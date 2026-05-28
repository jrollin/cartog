//! SQLite persistence layer for the cartog code graph.
//!
//! Stores symbols, edges, and file metadata in a single SQLite database.
//! Provides graph traversal queries (callees, refs, impact, hierarchy),
//! full-text search via FTS5, vector KNN search via sqlite-vec, and a
//! 6-tier heuristic edge resolution algorithm.

use anyhow::{Context, Result};
use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sqlite_vec::sqlite3_vec_init;
use tracing::{info, warn};

use cartog_core::{Edge, EdgeKind, FileInfo, Symbol, SymbolKind, Visibility};

/// Typed errors for the database-open and schema-migration paths.
///
/// The rest of the query API still returns `anyhow::Result` for now;
/// this enum exists so callers (the binary, MCP server, plugin authors)
/// can pattern-match on the actionable failure modes around opening a
/// database — especially distinguishing a corrupt file from a missing
/// one from a schema incompatibility. A `From<DbError>` impl on
/// `anyhow::Error` is provided automatically by the trait blanket, so
/// existing `?`-based call sites keep working unchanged.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// Failure opening or creating the SQLite file itself (permission
    /// denied, path missing, disk full, etc.).
    #[error("failed to open database at {path}: {source}")]
    Open {
        path: std::path::PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// Failure preparing the on-disk layout (e.g. could not create the
    /// `.cartog/` parent directory).
    #[error("failed to prepare database directory {path}: {source}")]
    PrepareDir {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Could not apply one of the startup PRAGMAs (journal_mode, WAL, …).
    #[error("failed to set startup pragmas: {0}")]
    Pragma(#[source] rusqlite::Error),

    /// Could not apply the `CREATE TABLE IF NOT EXISTS` schema bootstrap.
    #[error("failed to create schema: {0}")]
    Schema(#[source] rusqlite::Error),

    /// Could not create or migrate the RAG (FTS + vector) tables.
    #[error("failed to create RAG schema: {0}")]
    RagSchema(#[source] rusqlite::Error),

    /// Pre-migration backup via `VACUUM INTO` failed.
    #[error("failed to back up database before destructive migration to {path}: {source}")]
    BackupFailed {
        path: std::path::PathBuf,
        #[source]
        source: rusqlite::Error,
    },

    /// Embedding-dimension reconciliation failed (the stored `symbol_vec`
    /// shape didn't match the requested one and we couldn't rebuild it).
    #[error("embedding dimension migration failed: {0}")]
    EmbeddingDimension(#[source] rusqlite::Error),

    /// Read-only attach found a `schema_version` on disk that this binary
    /// doesn't know how to query. The primary writer was upgraded to a
    /// newer cartog; the read-only client should exit cleanly and let the
    /// user restart against the new version.
    #[error(
        "schema_version mismatch: this binary expects {expected}, DB has {stored} \
         (a different cartog process upgraded the schema; restart this session)"
    )]
    SchemaDrift { expected: u32, stored: u32 },

    /// A catch-all for other rusqlite-level failures inside `open` —
    /// use more specific variants whenever they fit.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Result alias for the typed-error helpers below.
pub type DbResult<T> = std::result::Result<T, DbError>;

const SQL_INSERT_SYMBOL: &str = "INSERT OR REPLACE INTO symbols
     (id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
      parent_id, signature, visibility, is_async, docstring, content_hash, subtree_hash)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)";

const SQL_INSERT_EDGE: &str =
    "INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS symbols (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    file_path TEXT NOT NULL,
    start_line INTEGER,
    end_line INTEGER,
    start_byte INTEGER,
    end_byte INTEGER,
    parent_id TEXT,
    signature TEXT,
    visibility TEXT,
    is_async BOOLEAN DEFAULT FALSE,
    docstring TEXT,
    in_degree INTEGER DEFAULT 0,
    content_hash TEXT,
    subtree_hash TEXT
);

CREATE TABLE IF NOT EXISTS edges (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id TEXT NOT NULL,
    target_name TEXT NOT NULL,
    target_id TEXT,
    kind TEXT NOT NULL,
    file_path TEXT NOT NULL,
    line INTEGER,
    -- 0 = unresolved (heuristic + LSP not yet definitive), 1 = resolved,
    -- 2 = unresolvable (LSP definitively returned no definition: typo, dyn dispatch, macro),
    -- 3 = external (LSP located the target outside the indexed root: stdlib, deps, node_modules).
    resolution_state INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (source_id) REFERENCES symbols(id)
);

CREATE TABLE IF NOT EXISTS files (
    path TEXT PRIMARY KEY,
    last_modified REAL,
    hash TEXT,
    language TEXT,
    num_symbols INTEGER DEFAULT 0
);

CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT
);

-- query_log feeds `cartog stats --savings` / `cartog savings`. One row per
-- successful read tool call (CLI or MCP). No query payload is stored — just
-- which tool, when, and the call surface — to keep the local-first promise.
CREATE TABLE IF NOT EXISTS query_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tool TEXT NOT NULL,
    source TEXT NOT NULL,
    ts INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_query_log_tool ON query_log(tool);
CREATE INDEX IF NOT EXISTS idx_query_log_ts ON query_log(ts);

CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
CREATE INDEX IF NOT EXISTS idx_symbols_kind ON symbols(kind);
CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);
CREATE INDEX IF NOT EXISTS idx_symbols_parent ON symbols(parent_id);
-- Composite: speeds up same-directory edge resolution
-- (WHERE name = ? AND file_path LIKE ?) in `resolve_edges_pass`.
CREATE INDEX IF NOT EXISTS idx_symbols_name_file ON symbols(name, file_path);
CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_name);
CREATE INDEX IF NOT EXISTS idx_edges_target_id ON edges(target_id);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
-- idx_edges_unresolved (partial index on resolution_state=0) is created
-- post-migration in Database::open so pre-v4 DBs without the column don't
-- blow up at SCHEMA-load time.
"#;

/// Schema for RAG semantic search tables.
///
/// - `symbol_content`: stores raw source code for each symbol (extracted via byte offsets)
/// - `symbol_fts`: FTS5 virtual table for keyword/BM25 search over symbol names and content
/// - `symbol_embedding_map`: maps integer rowids (for sqlite-vec) to symbol IDs
/// - `symbol_vec`: sqlite-vec virtual table for vector KNN search (384-dim float32)
const RAG_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS symbol_content (
    symbol_id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    header TEXT NOT NULL,
    normalized_name TEXT NOT NULL DEFAULT ''
);

CREATE VIRTUAL TABLE IF NOT EXISTS symbol_fts USING fts5(
    symbol_name,
    normalized_name,
    content,
    content=symbol_content,
    content_rowid=rowid
);

-- Triggers to keep FTS5 in sync with symbol_content
CREATE TRIGGER IF NOT EXISTS symbol_content_ai AFTER INSERT ON symbol_content BEGIN
    INSERT INTO symbol_fts(rowid, symbol_name, normalized_name, content)
    VALUES (new.rowid, (SELECT name FROM symbols WHERE id = new.symbol_id), new.normalized_name, new.content);
END;

CREATE TRIGGER IF NOT EXISTS symbol_content_ad AFTER DELETE ON symbol_content BEGIN
    INSERT INTO symbol_fts(symbol_fts, rowid, symbol_name, normalized_name, content)
    VALUES ('delete', old.rowid, (SELECT name FROM symbols WHERE id = old.symbol_id), old.normalized_name, old.content);
END;

CREATE TABLE IF NOT EXISTS symbol_embedding_map (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol_id TEXT NOT NULL UNIQUE
);

CREATE INDEX IF NOT EXISTS idx_embedding_map_symbol ON symbol_embedding_map(symbol_id);
"#;

/// Default embedding dimension (BGE-small-en-v1.5).
pub const DEFAULT_EMBEDDING_DIM: usize = 384;

/// Identity of the embedding stack that produced the vectors stored in
/// `symbol_vec`. Persisted in the `metadata` table so we can detect when the
/// user swaps provider or model and silently invalidates the existing index
/// even when the dimension happens to stay the same.
///
/// Dimension alone is not enough: two different models can share a dim
/// (e.g. a local 384-dim BGE and an Ollama 384-dim variant), and queries
/// against vectors generated by the other model return garbage similarity
/// scores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingFingerprint {
    /// Provider class identifier (`"local"`, `"ollama"`, …).
    pub provider: String,
    /// Specific model identifier within that provider.
    pub model: String,
    /// Embedding vector dimension.
    pub dimension: usize,
}

/// Metadata keys for the embedding fingerprint.
const EMBED_PROVIDER_KEY: &str = "embedding_provider";
const EMBED_MODEL_KEY: &str = "embedding_model";

/// SQL to create the sqlite-vec virtual table with the given embedding dimension.
fn rag_vec_schema(dim: usize) -> String {
    format!("CREATE VIRTUAL TABLE IF NOT EXISTS symbol_vec USING vec0(embedding float[{dim}])")
}

/// Default directory for cartog-generated artifacts, at the project root.
/// Holds the SQLite database and its destructive-migration backups.
pub const DB_DIR: &str = ".cartog";

/// Default SQLite database filename, stored inside [`DB_DIR`].
pub const DB_FILENAME: &str = "db.sqlite";

/// Legacy database filename at the project root, kept for backwards-compatibility
/// lookups. Never written to for new projects: use `DB_DIR`/`DB_FILENAME` instead.
pub const LEGACY_DB_FILE: &str = ".cartog.db";

/// Milliseconds a connection waits on a locked database before giving up.
///
/// WAL removes reader-vs-writer contention but not writer-vs-writer or
/// reader-vs-checkpoint contention. Without a `busy_timeout` SQLite fails
/// immediately with `SQLITE_BUSY`; this gives bounded retry instead. Applied
/// to every on-disk connection.
pub const BUSY_TIMEOUT_MS: u32 = 5000;

#[cfg(test)]
thread_local! {
    /// Test-only fault injection: when set to true, `reconcile_embedding_fingerprint`
    /// returns SQLITE_FULL between the model write and the dimension write.
    /// Cleared (swapped to false) on read so each fire is one-shot.
    static RECONCILE_FAIL_AFTER_MODEL: std::sync::atomic::AtomicBool =
        const { std::sync::atomic::AtomicBool::new(false) };
}

/// Run `PRAGMA wal_checkpoint(TRUNCATE)` on the SQLite file at `path`.
/// No-op for missing files. Used before moving the DB to flush the WAL.
pub fn checkpoint_wal(path: &std::path::Path) -> anyhow::Result<()> {
    use anyhow::Context;
    if !path.exists() {
        return Ok(());
    }
    let conn = Connection::open(path)
        .with_context(|| format!("open {} for WAL checkpoint", path.display()))?;
    conn.execute_batch(&format!(
        "PRAGMA busy_timeout={BUSY_TIMEOUT_MS};
         PRAGMA wal_checkpoint(TRUNCATE);"
    ))
    .with_context(|| format!("PRAGMA wal_checkpoint(TRUNCATE) on {}", path.display()))?;
    Ok(())
}

/// Maximum number of results returned by [`Database::search`].
/// Enforced here and referenced by CLI and MCP layers.
pub const MAX_SEARCH_LIMIT: u32 = 100;

/// Split a symbol name into lowercase words for FTS5 indexing.
///
/// Handles camelCase, PascalCase, snake_case, SCREAMING_SNAKE_CASE, and
/// mixed conventions. Examples:
/// - `validateToken` → `"validate token"`
/// - `DatabaseConnection` → `"database connection"`
/// - `validate_token` → `"validate token"`
/// - `TOKEN_EXPIRY` → `"token expiry"`
/// - `getHTTPResponse` → `"get http response"`
/// - `__init__` → `"init"`
pub fn normalize_symbol_name(name: &str) -> String {
    let mut words = Vec::new();
    let mut current = String::new();

    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();

    for i in 0..len {
        let c = chars[i];

        if c == '_' || c == '-' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }

        if c.is_uppercase() {
            let next_is_lower = i + 1 < len && chars[i + 1].is_lowercase();
            let prev_is_lower = !current.is_empty() && chars[i - 1].is_lowercase();

            if prev_is_lower {
                // camelCase boundary: `validateT` → split before T
                words.push(std::mem::take(&mut current));
            } else if !current.is_empty() && next_is_lower {
                // SCREAMING to PascalCase boundary: `HTTPResponse` → split before R
                words.push(std::mem::take(&mut current));
            }
            current.push(c.to_lowercase().next().unwrap());
        } else if c.is_alphanumeric() {
            current.push(c.to_lowercase().next().unwrap());
        } else {
            // Non-alphanumeric (other than _ and -): treat as separator
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        }
    }

    if !current.is_empty() {
        words.push(current);
    }

    words.join(" ")
}

pub struct Database {
    conn: Connection,
    /// Set when this `Database` was opened via [`Database::open_readonly`].
    /// Captures the `metadata` snapshot at attach time so a later promotion
    /// (Phase 5) can detect drift before switching to read-write mode. `None`
    /// for read-write opens.
    ///
    /// Invariant: `pinned.is_some() == is_read_only()`. Both flow from the
    /// same opening path, and the equivalence is what callers rely on.
    pinned: Option<PinnedAttach>,
}

/// Snapshot of write-mode-relevant metadata captured by a read-only attach.
/// Compared against the on-disk values when the reader decides whether it
/// can still safely serve queries against the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedAttach {
    pub schema_version: u32,
    pub embedding: Option<EmbeddingFingerprint>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

/// Register the sqlite-vec extension globally.
///
/// Must be called once before opening any database connections.
/// Safe to call multiple times (idempotent via `std::sync::Once`).
pub fn register_sqlite_vec() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    });
}

/// Current schema version. Increment when adding migrations.
const SCHEMA_VERSION: u32 = 5;

/// Public mirror of [`SCHEMA_VERSION`] for callers outside this crate
/// (e.g. `cartog pull` needs it to compare against a pulled DB and refuse
/// to load a future-versioned file). Kept in sync by construction.
pub const CURRENT_SCHEMA_VERSION: u32 = SCHEMA_VERSION;

/// Read the `schema_version` recorded in a cartog SQLite file at `path`,
/// without going through the full [`Database::open`] machinery (no
/// migrations, no fingerprint reconciliation). Used by `cartog pull` to
/// guard against pulling a future-versioned DB before clobbering the
/// local one.
///
/// Returns `Ok(0)` when the file exists but is not a cartog DB (no
/// `metadata` table, or no `schema_version` row). Returns `Err` only on
/// genuine SQLite errors (corrupt file, permission denied, etc.).
pub fn read_schema_version_at(path: &std::path::Path) -> anyhow::Result<u32> {
    use anyhow::Context;
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open {} read-only for schema check", path.display()))?;
    Ok(read_schema_version(&conn)?)
}

/// True when the `symbol_vec` virtual table exists in the open DB. Used by
/// the fast-path early returns in [`handle_embedding_dimension`] and
/// [`Database::reconcile_embedding_fingerprint`] so a previously-corrupted
/// DB (table dropped externally, or a pre-C4 cartog that crashed between
/// DROP and CREATE) is detected and rebuilt instead of silently passing
/// the metadata-only check.
fn symbol_vec_exists(conn: &Connection) -> std::result::Result<bool, rusqlite::Error> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name='symbol_vec'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .optional()
    .map(|v| v.is_some())
}

/// Read the on-disk `schema_version` for the read-only open paths.
/// A missing row (or missing `metadata` table — a non-cartog SQLite file
/// at the path) is treated as `stored = 0`, which surfaces to the caller
/// as `DbError::SchemaDrift { expected, stored: 0 }` rather than a raw
/// rusqlite error. Lets `cartog serve` print "another writer upgraded the
/// schema; restart this session" (the actionable message) instead of
/// "Query returned no rows" or "no such table: metadata".
fn read_schema_version(conn: &Connection) -> std::result::Result<u32, DbError> {
    match conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'schema_version'",
        [],
        |row| row.get::<_, u32>(0),
    ) {
        Ok(v) => Ok(v),
        // Missing row inside an existing table: stored=0.
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
        // Missing `metadata` table entirely (non-cartog SQLite file at the
        // path, or a partially-initialised DB): stored=0. rusqlite reports
        // this as a generic SqliteFailure; the message is the only stable
        // signal for "no such table" specifically.
        Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
            if msg.contains("no such table: metadata") =>
        {
            Ok(0)
        }
        Err(e) => Err(DbError::Sqlite(e)),
    }
}

/// Run schema migrations for existing databases.
///
/// Uses the `metadata` table to track the current schema version.
/// Each migration runs once and is idempotent. New databases start at
/// the latest version (SCHEMA already includes all columns).
fn migrate(conn: &Connection) {
    let current: u32 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(1); // pre-versioning databases are version 1

    // Check for partially-migrated v3: schema version bumped but columns missing.
    // Must run BEFORE the early return since current may already be >= SCHEMA_VERSION.
    let has_hash_cols = conn
        .prepare("SELECT content_hash FROM symbols LIMIT 0")
        .is_ok();
    // Same idea for v4: ensure the resolution_state column exists even if
    // schema_version was already bumped (e.g. partial migration crash).
    let has_resolution_state = conn
        .prepare("SELECT resolution_state FROM edges LIMIT 0")
        .is_ok();
    // Same idea for v5: ensure query_log exists even on partial migration.
    let has_query_log = conn.prepare("SELECT 1 FROM query_log LIMIT 0").is_ok();

    if current >= SCHEMA_VERSION && has_hash_cols && has_resolution_state && has_query_log {
        return;
    }

    // Migration 1 → 2: add in_degree column for centrality ranking
    if current < 2 {
        let _ = conn.execute(
            "ALTER TABLE symbols ADD COLUMN in_degree INTEGER DEFAULT 0",
            [],
        );
    }

    // Migration 2 → 3: stable symbol IDs + Merkle hash columns.
    if current < 3 || !has_hash_cols {
        info!("schema v3: stable symbol IDs — clearing index for full rebuild");
        let _ = conn.execute("ALTER TABLE symbols ADD COLUMN content_hash TEXT", []);
        let _ = conn.execute("ALTER TABLE symbols ADD COLUMN subtree_hash TEXT", []);
        // Clear all indexed data so next index rebuilds with stable IDs
        for table in &["symbol_content", "edges", "symbols", "files"] {
            let _ = conn.execute(&format!("DELETE FROM {table}"), []);
        }
        // Clear RAG data too — vector table first, then map
        let _ = conn.execute("DELETE FROM symbol_vec", []);
        let _ = conn.execute("DELETE FROM symbol_embedding_map", []);
        // Clear last_commit so incremental indexing doesn't skip anything
        let _ = conn.execute("DELETE FROM metadata WHERE key = 'last_commit'", []);
    }

    // Migration 3 → 4: edge resolution_state for the LSP "unresolvable" marker.
    // Non-destructive: column is additive, existing nulls become state=0
    // (will be re-attempted by LSP), existing target_ids become state=1.
    // The matching partial index is created in `Database::open` after this
    // function returns — keeps the SCHEMA bootstrap pre-migration safe.
    if current < 4 || !has_resolution_state {
        info!("schema v4: adding edges.resolution_state column");
        let _ = conn.execute(
            "ALTER TABLE edges ADD COLUMN resolution_state INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "UPDATE edges SET resolution_state = 1 WHERE target_id IS NOT NULL",
            [],
        );
    }

    // Migration 4 → 5: query_log table for `cartog stats --savings`.
    // Additive only; the SCHEMA bootstrap above already runs `CREATE TABLE IF
    // NOT EXISTS query_log`, so this branch is just the version bump for
    // databases that ran through `migrate()` on a pre-v5 binary.
    if current < 5 || !has_query_log {
        info!("schema v5: query_log table");
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS query_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tool TEXT NOT NULL,
                source TEXT NOT NULL,
                ts INTEGER NOT NULL
            )",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_log_tool ON query_log(tool)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_log_ts ON query_log(ts)",
            [],
        );
    }

    // Store the new schema version
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    ) {
        warn!(error = %e, "failed to store schema version");
    }
}

/// Retry backoff schedule for writes that race with another writer on the
/// embedding-dimension migration. Multiple cartog processes can each call
/// `Database::open` and contend on the same DB; `PRAGMA busy_timeout` only
/// covers single statements, not the full sequence here. Exhausting the
/// schedule (~2s total) returns the underlying error unchanged.
const MIGRATION_RETRY_BACKOFF_MS: &[u64] = &[50, 100, 250, 500, 1000];

/// Run a fallible rusqlite operation, retrying on `SQLITE_BUSY` /
/// `SQLITE_LOCKED` with the [`MIGRATION_RETRY_BACKOFF_MS`] schedule.
fn retry_busy<T, F>(mut op: F) -> std::result::Result<T, rusqlite::Error>
where
    F: FnMut() -> std::result::Result<T, rusqlite::Error>,
{
    let mut attempt = 0usize;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let busy = matches!(
                    e,
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error {
                            code: rusqlite::ErrorCode::DatabaseBusy
                                | rusqlite::ErrorCode::DatabaseLocked,
                            ..
                        },
                        _
                    )
                );
                if !busy || attempt >= MIGRATION_RETRY_BACKOFF_MS.len() {
                    return Err(e);
                }
                let delay_ms = MIGRATION_RETRY_BACKOFF_MS[attempt];
                tracing::debug!(
                    attempt = attempt + 1,
                    delay_ms,
                    "retrying embedding-dimension write after SQLITE_BUSY"
                );
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                attempt += 1;
            }
        }
    }
}

/// Check stored embedding dimension against requested dimension.
/// If they differ, drop the vector table and clear the embedding map.
///
/// Returns rusqlite's `Result` so the caller (`Database::open`) can wrap
/// any failure into `DbError::EmbeddingDimension` with precise context.
///
/// Writes are wrapped in [`retry_busy`] so a concurrent writer on the
/// same DB (another cartog process) doesn't crash this `Database::open`
/// with `SQLITE_BUSY`. When the stored dimension already matches the
/// effective one, the function returns without any DB writes at all.
fn handle_embedding_dimension(
    conn: &Connection,
    requested_dim: usize,
) -> std::result::Result<(), rusqlite::Error> {
    let stored_dim: Option<usize> = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
            [],
            |row| row.get::<_, i64>(0).map(|v| v as usize),
        )
        .ok();

    // When the caller passes the default dimension and a different dimension is
    // already stored, preserve the stored one. This avoids non-RAG commands
    // (which don't know the real provider dimension) from silently wiping a
    // vector index created by an Ollama provider with auto-detected dimension.
    let effective_dim = match stored_dim {
        Some(old) if requested_dim == DEFAULT_EMBEDDING_DIM && old != DEFAULT_EMBEDDING_DIM => old,
        _ => requested_dim,
    };

    // True early return: if the dim already matches AND the vector table
    // actually exists, nothing to write. The dim+table pair is the real
    // invariant; checking metadata alone misses the case where a previous
    // open crashed mid-migration and left the DB without `symbol_vec`
    // while metadata still claims a dimension.
    if stored_dim == Some(effective_dim) && symbol_vec_exists(conn)? {
        return Ok(());
    }

    // Wrap the wipe+rebuild sequence in a single transaction so a mid-
    // sequence failure (busy timeout exhausted, disk full, etc.) rolls
    // back atomically. Without this, a DROP that succeeds followed by an
    // INSERT that fails would leave the DB with no `symbol_vec` but
    // metadata pointing at the old dimension — the next open would skip
    // migration ("stored == requested") and queries against the missing
    // table would error forever.
    let schema = rag_vec_schema(effective_dim);
    let needs_wipe = stored_dim.is_some();
    retry_busy(|| {
        let tx = conn.unchecked_transaction()?;
        if needs_wipe {
            let old_dim = stored_dim.unwrap_or(0);
            tracing::warn!(
                old = old_dim,
                new = effective_dim,
                "Embedding dimension changed — clearing vector index. Run `cartog rag index` to re-embed."
            );
            tx.execute("DROP TABLE IF EXISTS symbol_vec", [])?;
            tx.execute("DELETE FROM symbol_embedding_map", [])?;
        }
        tx.execute_batch(&schema)?;
        tx.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('embedding_dimension', ?1)",
            params![effective_dim.to_string()],
        )?;
        tx.commit()
    })?;

    Ok(())
}

/// If the next migration will wipe existing data, copy the database to a
/// timestamped backup file first. No-op for in-memory or empty databases.
fn backup_before_destructive_migration(
    conn: &Connection,
    db_path: &std::path::Path,
) -> DbResult<()> {
    let current: u32 = conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(1);
    let has_hash_cols = conn
        .prepare("SELECT content_hash FROM symbols LIMIT 0")
        .is_ok();

    // Mirrors the condition in `migrate()` for the 2→3 wipe.
    let will_wipe = current < 3 || !has_hash_cols;
    if !will_wipe {
        return Ok(());
    }

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap_or(0);
    if symbol_count == 0 {
        return Ok(());
    }

    // Skip in-memory / URI-mode databases — nothing to back up.
    let path_str = db_path.to_string_lossy();
    if path_str.is_empty() || path_str == ":memory:" || path_str.starts_with("file:") {
        return Ok(());
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut backup_os = db_path.as_os_str().to_os_string();
    backup_os.push(format!(".pre-v{current}-{ts}.bak"));
    let backup_path = std::path::PathBuf::from(backup_os);

    // VACUUM INTO produces a consistent copy, safe alongside WAL.
    // Escape any single-quotes in the path literal.
    let escaped = backup_path.to_string_lossy().replace('\'', "''");
    conn.execute(&format!("VACUUM INTO '{escaped}'"), [])
        .map_err(|source| DbError::BackupFailed {
            path: backup_path.clone(),
            source,
        })?;

    info!(
        backup = %backup_path.display(),
        old_version = current,
        new_version = SCHEMA_VERSION,
        symbols = symbol_count,
        "schema migration will clear indexed data — created backup"
    );

    Ok(())
}

impl Database {
    /// Open or create the database at the given path.
    ///
    /// `embedding_dim` sets the vector dimension for the sqlite-vec table.
    /// If the stored dimension differs from the requested one, the vector index
    /// is cleared and recreated (a re-index via `cartog rag index` is needed).
    pub fn open(path: impl AsRef<std::path::Path>, embedding_dim: usize) -> DbResult<Self> {
        register_sqlite_vec();
        let db_path = path.as_ref();
        // SQLite::open fails on a missing parent tree, so materialize `.cartog/`.
        if let Some(parent) = db_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| DbError::PrepareDir {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(db_path).map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout={BUSY_TIMEOUT_MS};
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;"
        ))
        .map_err(DbError::Pragma)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Schema)?;
        conn.execute_batch(RAG_SCHEMA).map_err(DbError::RagSchema)?;
        backup_before_destructive_migration(&conn, db_path)?;
        migrate(&conn);
        // Partial index requires resolution_state (added in migration 3→4),
        // so create it after migrate() rather than from SCHEMA.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_edges_unresolved
                 ON edges(file_path) WHERE resolution_state = 0",
        )
        .map_err(DbError::Schema)?;
        handle_embedding_dimension(&conn, embedding_dim).map_err(DbError::EmbeddingDimension)?;
        Ok(Self { conn, pinned: None })
    }

    /// Open an existing on-disk database in **read-write** mode without
    /// running schema migrations or the embedding-fingerprint reconcile.
    /// Used by the Phase 5 promoter: a secondary that detected its primary
    /// died and validated the on-disk schema/fingerprint against its
    /// pinned snapshot before claiming the slot. Re-running the migration
    /// would re-trigger the SQLITE_BUSY race that the election was meant
    /// to prevent.
    ///
    /// Verifies that `schema_version` still matches `SCHEMA_VERSION` to
    /// guard against a race where another writer upgraded the schema
    /// between the secondary's attach and this promotion. Returns
    /// [`DbError::SchemaDrift`] in that case so the promoter aborts and
    /// the MCP process exits cleanly.
    pub fn open_existing_rw(path: impl AsRef<std::path::Path>) -> DbResult<Self> {
        register_sqlite_vec();
        let db_path = path.as_ref();
        let conn = Connection::open(db_path).map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout={BUSY_TIMEOUT_MS};
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;"
        ))
        .map_err(DbError::Pragma)?;

        let stored_schema = read_schema_version(&conn)?;
        if stored_schema != SCHEMA_VERSION {
            return Err(DbError::SchemaDrift {
                expected: SCHEMA_VERSION,
                stored: stored_schema,
            });
        }

        Ok(Self { conn, pinned: None })
    }

    /// Open an existing on-disk database in **read-only** mode for a
    /// secondary cartog process (Phase 4 read-only attach). Skips schema
    /// migrations and the embedding-fingerprint reconcile — the primary
    /// writer owns those.
    ///
    /// Behaviour:
    /// - Opens with `SQLITE_OPEN_READ_ONLY` so write attempts surface as
    ///   `SQLITE_READONLY` errors at runtime (a defense-in-depth backup
    ///   for the higher-level tool gating).
    /// - Reads the `metadata` snapshot (schema version + embedding
    ///   fingerprint) and stores it on the returned [`Database`] so the
    ///   promoter (Phase 5) can compare against the on-disk values later.
    /// - Returns [`DbError::SchemaDrift`] if the stored `schema_version`
    ///   doesn't match this binary's expected version — the primary
    ///   upgraded cartog underneath us and queries can't be trusted.
    pub fn open_readonly(path: impl AsRef<std::path::Path>) -> DbResult<Self> {
        use rusqlite::OpenFlags;
        register_sqlite_vec();
        let db_path = path.as_ref();
        let conn = Connection::open_with_flags(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        // busy_timeout is still useful: a long read can stall against a
        // writer mid-checkpoint. WAL keeps readers and writers from
        // blocking otherwise, but the timeout makes the bound explicit.
        conn.execute_batch(&format!("PRAGMA busy_timeout={BUSY_TIMEOUT_MS};"))
            .map_err(DbError::Pragma)?;

        let stored_schema = read_schema_version(&conn)?;
        if stored_schema != SCHEMA_VERSION {
            return Err(DbError::SchemaDrift {
                expected: SCHEMA_VERSION,
                stored: stored_schema,
            });
        }

        let stored_provider: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EMBED_PROVIDER_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        let stored_model: Option<String> = conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![EMBED_MODEL_KEY],
                |row| row.get(0),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        let stored_dim: Option<usize> = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get::<_, i64>(0).map(|v| v as usize),
            )
            .optional()
            .map_err(DbError::Sqlite)?;
        // Embedding fingerprint is recorded together (Phase 6b backfill).
        // If any field is missing the fingerprint is "unknown" — readers
        // can still serve graph queries, just can't validate against a
        // promoter swap later.
        let embedding = match (stored_provider, stored_model, stored_dim) {
            (Some(provider), Some(model), Some(dimension)) => Some(EmbeddingFingerprint {
                provider,
                model,
                dimension,
            }),
            _ => None,
        };

        Ok(Self {
            conn,
            pinned: Some(PinnedAttach {
                schema_version: stored_schema,
                embedding,
            }),
        })
    }

    /// Open an in-memory database (for tests and benchmarks).
    #[doc(hidden)]
    pub fn open_memory() -> DbResult<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(DbError::Pragma)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Schema)?;
        conn.execute_batch(RAG_SCHEMA).map_err(DbError::RagSchema)?;
        conn.execute_batch(&rag_vec_schema(DEFAULT_EMBEDDING_DIM))
            .map_err(DbError::RagSchema)?;
        migrate(&conn);
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_edges_unresolved
                 ON edges(file_path) WHERE resolution_state = 0",
        )
        .map_err(DbError::Schema)?;
        Ok(Self { conn, pinned: None })
    }

    /// True when this `Database` was opened via [`Self::open_readonly`].
    /// MCP tool gating (Phase 4) consults this to refuse the 2 write tools.
    pub fn is_read_only(&self) -> bool {
        self.pinned.is_some()
    }

    /// Snapshot captured at attach time when [`Self::open_readonly`] was
    /// used. `None` for read-write opens.
    pub fn pinned_attach(&self) -> Option<&PinnedAttach> {
        self.pinned.as_ref()
    }

    /// Cap the number of pages this DB connection can hold.
    ///
    /// Intended for tests that need to force a `SQLITE_FULL` error on a
    /// subsequent write (for example, to verify that a transaction rolls back
    /// cleanly). Production code should never call this.
    #[doc(hidden)]
    pub fn set_max_page_count_for_tests(&self, pages: u32) -> Result<()> {
        self.conn
            .execute_batch(&format!("PRAGMA max_page_count = {pages}"))?;
        Ok(())
    }

    /// Open a single SQLite transaction that the caller is expected to wrap
    /// around a multi-step indexing pipeline.
    ///
    /// Drop without `commit()` rolls back, so a panic mid-pipeline leaves the
    /// DB in its prior state.
    ///
    /// # Calling conventions inside the transaction
    ///
    /// Helpers fall into two categories:
    ///
    /// 1. **Batched writers must use the `_in_tx` variant.** Their non-`_in_tx`
    ///    wrapper issues its own `BEGIN` and would error out at runtime
    ///    (`cannot start a transaction within a transaction`). Examples:
    ///    [`Self::insert_symbols_in_tx`], [`Self::delete_symbols_in_tx`],
    ///    [`Self::insert_edges_in_tx`], [`Self::insert_symbol_contents_in_tx`],
    ///    [`Self::clear_file_data_in_tx`], [`Self::remove_file_in_tx`],
    ///    [`Self::resolve_edges_in_tx`], [`Self::resolve_edges_scoped_in_tx`].
    ///
    /// 2. **Single-statement helpers can be called directly.** They issue one
    ///    `self.conn.execute(...)` and participate transparently in the active
    ///    transaction. Examples used by `cartog-indexer`'s Phase 3 today:
    ///    [`Self::upsert_file`], [`Self::clear_edges_for_file`],
    ///    [`Self::set_metadata`], [`Self::compute_in_degrees`],
    ///    [`Self::compute_in_degrees_scoped`], [`Self::invalidate_edges_targeting`].
    ///    These are tagged with `// tx-safe: single statement` so the contract
    ///    survives drive-by edits.
    ///
    /// # Why `unchecked_transaction` rather than [`rusqlite::Connection::transaction`]
    ///
    /// `transaction()` requires `&mut Connection`, which would force every
    /// caller of `Database` to hold a mutable borrow for the entire pipeline.
    /// `unchecked_transaction()` works through `&Connection` and produces an
    /// equivalent [`rusqlite::Transaction`] with the same `DropBehavior::Rollback`
    /// default — only borrow-check ergonomics differ.
    ///
    /// # Errors
    ///
    /// Returns an error if SQLite cannot begin a transaction — typically
    /// because another transaction is already active on this connection.
    pub fn begin_indexing_tx(&self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self.conn.unchecked_transaction()?)
    }

    // ── Metadata ──

    /// Retrieve a metadata value by key.
    pub fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM metadata WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query metadata")
    }

    /// Store a metadata key-value pair (upserts on conflict).
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    /// Reconcile the stored embedding fingerprint with the one currently in
    /// use. Call right after `Database::open` from any code path that owns
    /// an `EmbeddingProvider` (indexer, watcher, MCP serve, `rag index`).
    ///
    /// Behavior:
    /// - All three fields (provider, model, dimension) match stored → no-op,
    ///   zero writes.
    /// - Any field differs → drop `symbol_vec`, clear `symbol_embedding_map`,
    ///   recreate the vector table at the new dimension, update all three
    ///   metadata keys. The user must run `cartog rag index` to repopulate.
    /// - DB has dimension but no provider/model (older cartog versions) →
    ///   backfill provider+model without wiping. The stored vectors stay
    ///   valid against whatever stack produced them; we just record the
    ///   identity going forward.
    ///
    /// Writes use [`retry_busy`] so a concurrent writer on the same DB does
    /// not crash this caller with `SQLITE_BUSY`.
    pub fn reconcile_embedding_fingerprint(&self, fp: &EmbeddingFingerprint) -> Result<()> {
        let stored_provider: Option<String> = self.get_metadata(EMBED_PROVIDER_KEY)?;
        let stored_model: Option<String> = self.get_metadata(EMBED_MODEL_KEY)?;
        let stored_dim: Option<usize> = self
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get::<_, i64>(0).map(|v| v as usize),
            )
            .optional()
            .context("Failed to query embedding dimension")?;

        // Full match AND `symbol_vec` actually exists on disk: zero writes.
        // The dim+table pair is the real invariant; checking metadata
        // alone misses the case where a previous open crashed mid-
        // migration and left the DB without `symbol_vec` while metadata
        // still claims a fingerprint.
        if stored_provider.as_deref() == Some(fp.provider.as_str())
            && stored_model.as_deref() == Some(fp.model.as_str())
            && stored_dim == Some(fp.dimension)
            && symbol_vec_exists(&self.conn)?
        {
            return Ok(());
        }

        // Backwards-compat: stored has dim from an older cartog (no provider/model
        // recorded yet). Treat first-time-with-provider as a backfill, not an
        // invalidation. Embeddings produced by the previous run are still valid
        // against whatever stack the user had configured then.
        let dim_matches = stored_dim == Some(fp.dimension);
        let is_backfill = dim_matches && stored_provider.is_none() && stored_model.is_none();

        if !is_backfill {
            tracing::warn!(
                old_provider = ?stored_provider,
                old_model = ?stored_model,
                old_dim = ?stored_dim,
                new_provider = %fp.provider,
                new_model = %fp.model,
                new_dim = fp.dimension,
                "Embedding fingerprint changed — clearing vector index. Run `cartog rag index` to re-embed."
            );
        }

        // Wrap the whole sequence in a transaction so a mid-sequence
        // failure (e.g. busy-retry exhausted on the third metadata write)
        // rolls back atomically. Otherwise the next open could see
        // partial state — e.g. provider/model match but dimension stale,
        // or symbol_vec dropped but metadata still pointing at the old
        // dim — and either skip migration or silently re-wipe.
        let schema = rag_vec_schema(fp.dimension);
        let do_wipe = !is_backfill;
        retry_busy(|| {
            let tx = self.conn.unchecked_transaction()?;
            if do_wipe {
                tx.execute("DROP TABLE IF EXISTS symbol_vec", [])?;
                tx.execute("DELETE FROM symbol_embedding_map", [])?;
            }
            tx.execute_batch(&schema)?;
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![EMBED_PROVIDER_KEY, fp.provider],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
                params![EMBED_MODEL_KEY, fp.model],
            )?;
            #[cfg(test)]
            if RECONCILE_FAIL_AFTER_MODEL
                .with(|b| b.swap(false, std::sync::atomic::Ordering::SeqCst))
            {
                return Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                    Some("injected mid-sequence failure".into()),
                ));
            }
            tx.execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('embedding_dimension', ?1)",
                params![fp.dimension.to_string()],
            )?;
            tx.commit()
        })
        .map_err(|e| anyhow::anyhow!("failed to reconcile embedding fingerprint: {e}"))?;

        Ok(())
    }

    // ── Files ──

    /// Insert or update file metadata.
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn upsert_file(&self, file: &FileInfo) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO files (path, last_modified, hash, language, num_symbols)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                file.path,
                file.last_modified,
                file.hash,
                file.language,
                file.num_symbols,
            ],
        )?;
        Ok(())
    }

    /// Look up stored metadata for a file.
    pub fn get_file(&self, path: &str) -> Result<Option<FileInfo>> {
        self.conn
            .query_row(
                "SELECT path, last_modified, hash, language, num_symbols FROM files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(FileInfo {
                        path: row.get(0)?,
                        last_modified: row.get(1)?,
                        hash: row.get(2)?,
                        language: row.get(3)?,
                        num_symbols: row.get(4)?,
                    })
                },
            )
            .optional()
            .context("Failed to query file")
    }

    /// Remove edges only for a file (used by Merkle diff which updates symbols surgically).
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn clear_edges_for_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        Ok(())
    }

    /// Remove all symbols, edges, and RAG data for a file (before re-indexing it).
    pub fn clear_file_data(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.clear_file_data_in_tx(path)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::clear_file_data`] but assumes the caller already holds an
    /// open transaction. Used by `cartog-indexer` to wrap the entire Phase 3
    /// pipeline atomically.
    pub fn clear_file_data_in_tx(&self, path: &str) -> Result<()> {
        self.clear_rag_data_for_file(path)?;
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE file_path = ?1", params![path])?;
        Ok(())
    }

    /// Remove a file and all its symbols and edges from the index.
    pub fn remove_file(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.remove_file_in_tx(path)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::remove_file`] but assumes the caller already holds an
    /// open transaction.
    pub fn remove_file_in_tx(&self, path: &str) -> Result<()> {
        self.clear_file_data_in_tx(path)?;
        self.conn
            .execute("DELETE FROM files WHERE path = ?1", params![path])?;
        Ok(())
    }

    // ── Symbols ──

    /// Insert or replace a single symbol.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_symbol(&self, sym: &Symbol) -> Result<()> {
        self.conn
            .prepare_cached(SQL_INSERT_SYMBOL)?
            .execute(params![
                sym.id,
                sym.name,
                sym.kind.as_str(),
                sym.file_path,
                sym.start_line,
                sym.end_line,
                sym.start_byte,
                sym.end_byte,
                sym.parent_id,
                sym.signature,
                sym.visibility.as_str(),
                sym.is_async,
                sym.docstring,
                sym.content_hash,
                sym.subtree_hash,
            ])?;
        Ok(())
    }

    /// Insert or replace multiple symbols in a single transaction.
    pub fn insert_symbols(&self, symbols: &[Symbol]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_symbols_in_tx(symbols)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_symbols`] but assumes the caller already holds an
    /// open transaction.
    pub fn insert_symbols_in_tx(&self, symbols: &[Symbol]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(SQL_INSERT_SYMBOL)?;
        for sym in symbols {
            stmt.execute(params![
                sym.id,
                sym.name,
                sym.kind.as_str(),
                sym.file_path,
                sym.start_line,
                sym.end_line,
                sym.start_byte,
                sym.end_byte,
                sym.parent_id,
                sym.signature,
                sym.visibility.as_str(),
                sym.is_async,
                sym.docstring,
                sym.content_hash,
                sym.subtree_hash,
            ])?;
        }
        Ok(())
    }

    /// Get stored symbol hashes for a file (for Merkle diff).
    /// Returns `(id, content_hash, subtree_hash)` tuples.
    #[allow(clippy::type_complexity)]
    pub fn get_symbol_hashes_for_file(
        &self,
        file_path: &str,
    ) -> Result<Vec<(String, Option<String>, Option<String>)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, content_hash, subtree_hash FROM symbols WHERE file_path = ?1")?;
        let rows = stmt
            .query_map(params![file_path], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Update only the position fields of a symbol (for moved-but-unchanged symbols).
    pub fn update_symbol_position(
        &self,
        id: &str,
        start_line: u32,
        end_line: u32,
        start_byte: u32,
        end_byte: u32,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE symbols SET start_line = ?2, end_line = ?3,
                    start_byte = ?4, end_byte = ?5 WHERE id = ?1",
            params![id, start_line, end_line, start_byte, end_byte],
        )?;
        Ok(())
    }

    /// Delete multiple symbols and cascade (edges, content, embeddings) in a
    /// single transaction.
    pub fn delete_symbols(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        self.delete_symbols_in_tx(ids)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::delete_symbols`] but assumes the caller already holds an
    /// open transaction.
    pub fn delete_symbols_in_tx(&self, ids: &[String]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut del_out = self
            .conn
            .prepare_cached("DELETE FROM edges WHERE source_id = ?1")?;
        // Reset state alongside target_id so the orphaned edge re-enters
        // unresolved_edges() instead of becoming a (NULL, state=1) zombie.
        let mut null_in = self.conn.prepare_cached(
            "UPDATE edges SET target_id = NULL, resolution_state = 0 WHERE target_id = ?1",
        )?;
        let mut del_vec = self.conn.prepare_cached(
            "DELETE FROM symbol_vec WHERE rowid IN \
             (SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1)",
        )?;
        let mut del_map = self
            .conn
            .prepare_cached("DELETE FROM symbol_embedding_map WHERE symbol_id = ?1")?;
        let mut del_content = self
            .conn
            .prepare_cached("DELETE FROM symbol_content WHERE symbol_id = ?1")?;
        let mut del_sym = self
            .conn
            .prepare_cached("DELETE FROM symbols WHERE id = ?1")?;
        for id in ids {
            del_out.execute(params![id])?;
            null_in.execute(params![id])?;
            del_vec.execute(params![id])?;
            del_map.execute(params![id])?;
            del_content.execute(params![id])?;
            del_sym.execute(params![id])?;
        }
        Ok(())
    }

    /// Delete a single symbol and cascade to edges, content, and embeddings.
    pub fn delete_symbol(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM edges WHERE source_id = ?1", params![id])?;
        self.conn.execute(
            "UPDATE edges SET target_id = NULL, resolution_state = 0 WHERE target_id = ?1",
            params![id],
        )?;
        let _ = self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid IN \
             (SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1)",
            params![id],
        );
        let _ = self.conn.execute(
            "DELETE FROM symbol_embedding_map WHERE symbol_id = ?1",
            params![id],
        );
        let _ = self.conn.execute(
            "DELETE FROM symbol_content WHERE symbol_id = ?1",
            params![id],
        );
        self.conn
            .execute("DELETE FROM symbols WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    // ── Edges ──

    /// Insert a single edge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_edge(&self, edge: &Edge) -> Result<()> {
        self.conn.prepare_cached(SQL_INSERT_EDGE)?.execute(params![
            edge.source_id,
            edge.target_name,
            edge.target_id,
            edge.kind.as_str(),
            edge.file_path,
            edge.line,
        ])?;
        Ok(())
    }

    /// Insert multiple edges in a single transaction.
    pub fn insert_edges(&self, edges: &[Edge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_edges_in_tx(edges)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_edges`] but assumes the caller already holds an
    /// open transaction.
    pub fn insert_edges_in_tx(&self, edges: &[Edge]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(SQL_INSERT_EDGE)?;
        for edge in edges {
            stmt.execute(params![
                edge.source_id,
                edge.target_name,
                edge.target_id,
                edge.kind.as_str(),
                edge.file_path,
                edge.line,
            ])?;
        }
        Ok(())
    }

    // ── Edge Resolution ──

    /// Resolve target_name → target_id for all unresolved edges.
    ///
    /// Runs two passes so that import edges resolved in pass 1 enable
    /// import-path resolution (tier 2) for non-import edges in pass 2.
    ///
    /// 6-tier priority resolution (per pass):
    /// 1. Same file — symbol with matching name in the same file
    /// 2. Import-path — follow resolved imports to find the target in the imported file
    /// 3. Same directory — symbol in a file in the same directory
    /// 4. Parent scope preference — when multiple global matches, prefer same parent scope
    /// 5. Unique project-wide match — exactly one symbol with that name globally
    /// 6. Class over constructor — when exactly 2 matches and one is a class, prefer class
    pub fn resolve_edges(&self) -> Result<u32> {
        let tx = self.conn.unchecked_transaction()?;
        let total = self.resolve_edges_in_tx()?;
        tx.commit()?;
        Ok(total)
    }

    /// Like [`Self::resolve_edges`] but assumes the caller already holds an
    /// open transaction.
    pub fn resolve_edges_in_tx(&self) -> Result<u32> {
        let mut total_resolved = 0u32;
        for _pass in 0..2 {
            let resolved = self.resolve_edges_pass()?;
            if resolved == 0 {
                break;
            }
            total_resolved += resolved;
        }
        Ok(total_resolved)
    }

    fn resolve_edges_pass(&self) -> Result<u32> {
        // Skip state=2 (unresolvable) edges: the heuristic can't resolve what
        // LSP definitively gave up on, and re-walking them every dirty reindex
        // burns measurable time on large repos. They re-enter via
        // `reset_unresolvable_for_names` when a matching symbol appears.
        let mut unresolved_stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.source_id
             FROM edges e WHERE e.target_id IS NULL AND e.resolution_state = 0",
        )?;

        let unresolved: Vec<(i64, String, String, String)> = unresolved_stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        self.resolve_edge_batch(&unresolved)
    }

    /// 6-tier heuristic resolution for a batch of unresolved edges.
    ///
    /// Caller is responsible for transaction wrapping — see the public
    /// [`Self::resolve_edges`] / [`Self::resolve_edges_scoped`] helpers.
    fn resolve_edge_batch(&self, unresolved: &[(i64, String, String, String)]) -> Result<u32> {
        let mut resolved = 0u32;

        let mut same_file_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path = ?2 LIMIT 1")?;

        let mut import_resolve_stmt = self.conn.prepare(
            "SELECT s.id FROM symbols s
             INNER JOIN edges ie ON ie.kind = 'imports' AND ie.target_name = ?1
                 AND ie.target_id IS NOT NULL
             INNER JOIN symbols is2 ON is2.id = ie.source_id AND is2.file_path = ?2
             INNER JOIN symbols resolved ON resolved.id = ie.target_id
             WHERE s.name = ?1 AND s.kind != 'import'
                 AND s.file_path = resolved.file_path
             LIMIT 1",
        )?;

        let mut same_dir_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path LIKE ?2 LIMIT 1")?;

        let mut parent_scope_stmt = self.conn.prepare(
            "SELECT s.id FROM symbols s
             INNER JOIN symbols source ON source.id = ?2
             WHERE s.name = ?1 AND s.parent_id = source.parent_id AND s.id != source.id
             LIMIT 1",
        )?;

        let mut anywhere_stmt = self
            .conn
            .prepare("SELECT id, kind FROM symbols WHERE name = ?1 AND kind != 'import' LIMIT 3")?;

        // Heuristic resolve must flip state=1 alongside target_id; otherwise
        // unresolved_edges() (state=0 filter) would still surface the edge to
        // the next LSP pass, wasting a server roundtrip on already-known answers.
        let mut update_stmt = self
            .conn
            .prepare("UPDATE edges SET target_id = ?1, resolution_state = 1 WHERE id = ?2")?;

        for (edge_id, target_name, edge_file, source_id) in unresolved {
            let simple_name = target_name.rsplit('.').next().unwrap_or(target_name);

            // 1) Same file
            let target_id: Option<String> = same_file_stmt
                .query_row(params![simple_name, edge_file], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, edge_id])?;
                resolved += 1;
                continue;
            }

            // 2) Import-path resolution
            let target_id: Option<String> = import_resolve_stmt
                .query_row(params![simple_name, edge_file], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, edge_id])?;
                resolved += 1;
                continue;
            }

            // 3) Same directory
            let dir = edge_file
                .rsplit_once('/')
                .map(|(d, _)| format!("{d}/%"))
                .unwrap_or_default();

            if !dir.is_empty() {
                let target_id: Option<String> = same_dir_stmt
                    .query_row(params![simple_name, dir], |row| row.get(0))
                    .optional()?;

                if let Some(tid) = target_id {
                    update_stmt.execute(params![tid, edge_id])?;
                    resolved += 1;
                    continue;
                }
            }

            // 4) Parent scope preference
            let target_id: Option<String> = parent_scope_stmt
                .query_row(params![simple_name, source_id], |row| row.get(0))
                .optional()?;

            if let Some(tid) = target_id {
                update_stmt.execute(params![tid, edge_id])?;
                resolved += 1;
                continue;
            }

            // 5+6) Project-wide: unique match, or class-over-constructor disambiguation
            let mut rows = anywhere_stmt.query(params![simple_name])?;
            let mut matches: Vec<(String, String)> = Vec::new();
            while let Some(row) = rows.next()? {
                matches.push((row.get(0)?, row.get(1)?));
                if matches.len() == 3 {
                    break;
                }
            }
            drop(rows);

            let resolved_id = match matches.len() {
                1 => Some(&matches[0].0),
                2 => disambiguate_two(&matches[0], &matches[1]),
                _ => None,
            };

            if let Some(tid) = resolved_id {
                update_stmt.execute(params![tid, edge_id])?;
                resolved += 1;
            }
        }

        Ok(resolved)
    }

    /// Compute and store in-degree centrality for all symbols.
    ///
    /// In-degree = number of resolved incoming edges (calls, imports, inherits, etc.).
    /// Higher in-degree means the symbol is referenced more across the codebase.
    /// Resets all in-degree values to 0 first, then batch-updates from the edges table.
    ///
    /// tx-safe: two unconditional statements participate in any active outer
    /// transaction — see [`Self::begin_indexing_tx`].
    pub fn compute_in_degrees(&self) -> Result<u32> {
        self.conn.execute("UPDATE symbols SET in_degree = 0", [])?;

        // CTE computes counts once; the UPDATE applies them.
        // Avoids a correlated subquery per symbol (O(n*m) → O(n+m)).
        let updated = self.conn.execute(
            "WITH counts AS (
                SELECT target_id, COUNT(*) AS cnt
                FROM edges WHERE target_id IS NOT NULL
                GROUP BY target_id
            )
            UPDATE symbols SET in_degree = (
                SELECT cnt FROM counts WHERE counts.target_id = symbols.id
            )
            WHERE id IN (SELECT target_id FROM counts)",
            [],
        )?;

        Ok(updated as u32)
    }

    // ── Scoped resolution (incremental indexing) ──

    /// Invalidate resolved edges that point into any of the dirty files.
    ///
    /// When a file is re-indexed, its symbols may have been renamed/removed.
    /// Edges from *unchanged* files that previously resolved to those symbols
    /// must be cleared so they can be re-resolved against the new symbol set.
    ///
    /// tx-safe: single statement — see [`Self::begin_indexing_tx`].
    pub fn invalidate_edges_targeting(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After file re-indexing, edges from unchanged files may point to
        // symbol IDs that no longer exist (removed or renamed symbols).
        // Set these dangling references to NULL AND reset resolution_state so
        // the heuristic + LSP passes get another shot. Without the state reset
        // the edge would stay at state=1 (resolved) but with target_id NULL —
        // permanently invisible to `unresolved_edges()`.
        let n = self.conn.execute(
            "UPDATE edges SET target_id = NULL, resolution_state = 0
             WHERE target_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM symbols WHERE symbols.id = edges.target_id)",
            [],
        )?;
        Ok(n as u32)
    }

    /// Resolve edges scoped to dirty files only.
    ///
    /// Processes: edges originating from dirty files (freshly extracted)
    /// and edges whose target was just invalidated (target_id set to NULL).
    /// Uses the same 6-tier heuristic as `resolve_edges`.
    /// Resolve edges after scoped invalidation.
    ///
    /// After `invalidate_edges_targeting` has cleared target_ids for edges
    /// pointing into dirty files, this re-resolves all currently unresolved edges.
    /// Fewer edges are unresolved compared to a first-time full resolve.
    pub fn resolve_edges_scoped(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        let tx = self.conn.unchecked_transaction()?;
        let total = self.resolve_edges_scoped_in_tx(dirty_files)?;
        tx.commit()?;
        Ok(total)
    }

    /// Like [`Self::resolve_edges_scoped`] but assumes the caller already
    /// holds an open transaction.
    pub fn resolve_edges_scoped_in_tx(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After invalidation, the set of unresolved edges is naturally scoped:
        // only edges from dirty files (freshly extracted) or targeting dirty files
        // (just invalidated) have target_id = NULL.
        // Reuse the same 2-pass resolution.
        self.resolve_edges_in_tx()
    }

    /// Recompute in-degree centrality only for symbols in/around dirty files.
    ///
    /// tx-safe: every internal statement participates in any active outer
    /// transaction — see [`Self::begin_indexing_tx`]. Does NOT open one of
    /// its own, unlike the batched `*_in_tx` helpers; outside an outer
    /// transaction the per-file resets are not atomic with the recompute.
    pub fn compute_in_degrees_scoped(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }

        // Reset in-degree for symbols in dirty files
        for file in dirty_files {
            self.conn.execute(
                "UPDATE symbols SET in_degree = 0 WHERE file_path = ?1",
                params![file],
            )?;
        }

        // Also reset symbols that are targets of edges from dirty files
        // (their in-degree may have changed)
        for file in dirty_files {
            self.conn.execute(
                "UPDATE symbols SET in_degree = 0
                 WHERE id IN (
                     SELECT DISTINCT e.target_id FROM edges e
                     WHERE e.file_path = ?1 AND e.target_id IS NOT NULL
                 )",
                params![file],
            )?;
        }

        // Recompute for all symbols with in_degree = 0 that have incoming edges
        let updated = self.conn.execute(
            "WITH counts AS (
                SELECT target_id, COUNT(*) AS cnt
                FROM edges WHERE target_id IS NOT NULL
                GROUP BY target_id
            )
            UPDATE symbols SET in_degree = (
                SELECT cnt FROM counts WHERE counts.target_id = symbols.id
            )
            WHERE in_degree = 0
              AND id IN (SELECT target_id FROM counts)",
            [],
        )?;

        Ok(updated as u32)
    }

    // ── Queries ──

    /// Search for symbols by name — case-insensitive, prefix match ranks before substring.
    ///
    /// `%` and `_` in `query` are treated as literals, not LIKE wildcards.
    /// Note: `LOWER()` in SQLite is ASCII-only, which is acceptable for code identifiers.
    /// Returns an error if `query` is empty or `limit` is zero.
    pub fn search(
        &self,
        query: &str,
        kind_filter: Option<SymbolKind>,
        file_filter: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Symbol>> {
        anyhow::ensure!(!query.is_empty(), "search query cannot be empty");
        anyhow::ensure!(limit > 0, "search limit must be at least 1");

        // Escape LIKE special characters so query is matched literally.
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let kind_str = kind_filter.map(|k| k.as_str());
        // Ranking: match_tier + kind_penalty.
        //   match_tier: 0 = exact, 1 = prefix, 2 = substring
        //   kind_penalty: definitions (function/method/class) = 0, variable = 3, import = 6
        // Definitions always rank above variables/imports across all match tiers:
        //   exact class=0, prefix function=1, substring method=2,
        //   exact variable=3, prefix variable=4, substring variable=5,
        //   exact import=6, ...
        // Within the same rank score, secondary sort by kind (fn < method < class)
        // then by file_path and start_line for determinism.
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line,
                    start_byte, end_byte, parent_id, signature, visibility,
                    is_async, docstring, in_degree, content_hash, subtree_hash,
                    (CASE
                       WHEN LOWER(name) = LOWER(?1)                    THEN 0
                       WHEN LOWER(name) LIKE LOWER(?2) || '%' ESCAPE '\\' THEN 1
                       ELSE                                                  2
                     END) +
                    (CASE kind
                       WHEN 'function' THEN 0
                       WHEN 'method'   THEN 0
                       WHEN 'class'    THEN 0
                       WHEN 'variable' THEN 3
                       WHEN 'import'   THEN 6
                       ELSE                 3
                     END) AS rank
             FROM symbols
             WHERE LOWER(name) LIKE '%' || LOWER(?2) || '%' ESCAPE '\\'
               AND (?3 IS NULL OR kind = ?3)
               AND (?4 IS NULL OR file_path = ?4)
             ORDER BY rank,
                      in_degree DESC,
                      CASE kind
                        WHEN 'function' THEN 0
                        WHEN 'method'   THEN 1
                        WHEN 'class'    THEN 2
                        ELSE                 3
                      END,
                      file_path, start_line
             LIMIT ?5",
        )?;
        // ?1 = raw query (exact equality), ?2 = escaped query (LIKE patterns), ?3 = kind, ?4 = file, ?5 = limit
        let rows = stmt
            .query_map(
                params![query, escaped, kind_str, file_filter, limit],
                row_to_symbol,
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Outline: all symbols in a file, ordered by line.
    pub fn outline(&self, file_path: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols WHERE file_path = ?1
             ORDER BY start_line",
        )?;
        let rows = stmt
            .query_map(params![file_path], row_to_symbol)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Find what a symbol calls (edges originating from symbols matching the name).
    pub fn callees(&self, name: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line
             FROM edges e
             JOIN symbols s ON e.source_id = s.id
             WHERE s.name = ?1 AND e.kind = 'calls'",
        )?;
        let rows = stmt
            .query_map(params![name], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// All references to a name, with the source symbol resolved.
    /// Optionally filter by edge kind.
    pub fn refs(
        &self,
        name: &str,
        kind_filter: Option<EdgeKind>,
    ) -> Result<Vec<(Edge, Option<Symbol>)>> {
        // Use a LEFT JOIN to resolve target_id → symbol name instead of a correlated subquery.
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(Edge, Option<Symbol>)> {
            let kind_str = row.get::<_, String>(4)?;
            let kind = kind_str.parse().unwrap_or(EdgeKind::References);
            let edge = Edge {
                source_id: row.get(1)?,
                target_name: row.get(2)?,
                target_id: row.get(3)?,
                kind,
                file_path: row.get(5)?,
                line: row.get(6)?,
            };
            let sym: Option<Symbol> = if row.get::<_, Option<String>>(7)?.is_some() {
                Some(row_to_symbol_offset(row, 7)?)
            } else {
                None
            };
            Ok((edge, sym))
        };

        let rows = if let Some(kind) = kind_filter {
            let mut stmt = self.conn.prepare_cached(
                "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line,
                        s.id, s.name, s.kind, s.file_path, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.parent_id, s.signature, s.visibility,
                        s.is_async, s.docstring, s.in_degree, s.content_hash, s.subtree_hash
                 FROM edges e
                 LEFT JOIN symbols s ON e.source_id = s.id
                 LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                 WHERE (e.target_name = ?1 OR sym2.name = ?1)
                   AND e.kind = ?2",
            )?;
            let rows = stmt
                .query_map(params![name, kind.as_str()], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        } else {
            let mut stmt = self.conn.prepare_cached(
                "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line,
                        s.id, s.name, s.kind, s.file_path, s.start_line, s.end_line,
                        s.start_byte, s.end_byte, s.parent_id, s.signature, s.visibility,
                        s.is_async, s.docstring, s.in_degree, s.content_hash, s.subtree_hash
                 FROM edges e
                 LEFT JOIN symbols s ON e.source_id = s.id
                 LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                 WHERE e.target_name = ?1 OR sym2.name = ?1",
            )?;
            let rows = stmt
                .query_map(params![name], map_row)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        Ok(rows)
    }

    /// Inheritance hierarchy rooted at a class.
    pub fn hierarchy(&self, class_name: &str) -> Result<Vec<(String, String)>> {
        // Returns (child, parent) pairs
        let mut stmt = self.conn.prepare(
            "SELECT s.name, e.target_name
             FROM edges e
             JOIN symbols s ON e.source_id = s.id
             WHERE e.kind = 'inherits'
               AND (s.name = ?1 OR e.target_name = ?1)",
        )?;
        let rows = stmt
            .query_map(params![class_name], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// File-level dependencies (imports from a file).
    pub fn file_deps(&self, file_path: &str) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind, e.file_path, e.line
             FROM edges e
             WHERE e.file_path = ?1 AND e.kind = 'imports'",
        )?;
        let rows = stmt
            .query_map(params![file_path], row_to_edge)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Transitive impact analysis: everything reachable within `depth` hops.
    ///
    /// Evaluated as a single recursive CTE rather than iterating `refs()` per
    /// frontier node — saves N round-trips and lets SQLite's planner amortize
    /// the LEFT JOINs. Each unique edge is returned once, labeled with the
    /// minimum depth at which it was reached.
    pub fn impact(&self, name: &str, max_depth: u32) -> Result<Vec<(Edge, u32)>> {
        if max_depth == 0 {
            return Ok(Vec::new());
        }

        let sql = "
            WITH RECURSIVE impacted(
                edge_id, source_id, target_name, target_id, kind,
                file_path, line, source_name, depth
            ) AS (
                SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                       e.file_path, e.line, s.name, 1
                FROM edges e
                LEFT JOIN symbols s ON e.source_id = s.id
                LEFT JOIN symbols sym2 ON e.target_id = sym2.id
                WHERE e.target_name = ?1 OR sym2.name = ?1

                UNION

                SELECT e.id, e.source_id, e.target_name, e.target_id, e.kind,
                       e.file_path, e.line, s.name, i.depth + 1
                FROM impacted i
                JOIN edges e
                  ON (e.target_name = i.source_name
                      OR EXISTS (
                          SELECT 1 FROM symbols t
                          WHERE t.id = e.target_id AND t.name = i.source_name
                      ))
                LEFT JOIN symbols s ON e.source_id = s.id
                WHERE i.source_name IS NOT NULL AND i.depth < ?2
            )
            SELECT source_id, target_name, target_id, kind, file_path, line,
                   MIN(depth) AS depth
            FROM impacted
            GROUP BY edge_id
            ORDER BY depth, edge_id
        ";

        let mut stmt = self.conn.prepare_cached(sql)?;
        let rows = stmt
            .query_map(params![name, max_depth], |row| {
                let kind_str: String = row.get(3)?;
                let kind = kind_str.parse().unwrap_or(EdgeKind::References);
                let edge = Edge {
                    source_id: row.get(0)?,
                    target_name: row.get(1)?,
                    target_id: row.get(2)?,
                    kind,
                    file_path: row.get(4)?,
                    line: row.get(5)?,
                };
                let depth: u32 = row.get(6)?;
                Ok((edge, depth))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// True when no symbols have been indexed yet (fresh/empty DB). Cheap —
    /// a single `EXISTS(SELECT 1 FROM symbols)` that stops at the first row.
    /// Used by query commands to distinguish "no index yet" from a no-match.
    pub fn is_empty(&self) -> Result<bool> {
        let exists: bool =
            self.conn
                .query_row("SELECT EXISTS(SELECT 1 FROM symbols)", [], |row| row.get(0))?;
        Ok(!exists)
    }

    /// Index statistics.
    pub fn stats(&self) -> Result<IndexStats> {
        let num_files: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let num_symbols: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        // Single GROUP BY over edges replaces what would otherwise be four
        // sequential full table scans (one COUNT(*) per bucket). The partial
        // index `idx_edges_unresolved` only covers state=0, so state=2 and
        // state=3 counts can't use it — one scan + a 4-row Vec is cheaper.
        let mut bucket_stmt = self
            .conn
            .prepare("SELECT resolution_state, COUNT(*) FROM edges GROUP BY resolution_state")?;
        let mut num_resolved: u32 = 0;
        let mut num_unresolvable: u32 = 0;
        let mut num_external: u32 = 0;
        let mut num_edges: u32 = 0;
        let rows = bucket_stmt.query_map([], |row| {
            let state: i64 = row.get(0)?;
            let count: u32 = row.get(1)?;
            Ok((state, count))
        })?;
        for row in rows {
            let (state, count) = row?;
            num_edges += count;
            match state {
                1 => num_resolved = count,
                2 => num_unresolvable = count,
                3 => num_external = count,
                _ => {} // state=0 (unresolved) or any future state
            }
        }

        let mut lang_stmt = self.conn.prepare(
            "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY COUNT(*) DESC",
        )?;
        let languages: Vec<(String, u32)> = lang_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut kind_stmt = self
            .conn
            .prepare("SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC")?;
        let symbol_kinds: Vec<(String, u32)> = kind_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(IndexStats {
            num_files,
            num_symbols,
            num_edges,
            num_resolved,
            num_unresolvable,
            num_external,
            languages,
            symbol_kinds,
        })
    }

    /// Record a successful query against the index for the `cartog stats --savings`
    /// / `cartog savings` retention hook.
    ///
    /// Best-effort: errors are swallowed (logged via `warn!`) so a failing
    /// write never aborts the user's actual query.
    ///
    /// **Read-only attach skips the write.** Secondary MCP servers opened via
    /// [`Self::open_readonly`] cannot write at all. As a result, queries
    /// served by a secondary are NOT reflected in `query_log` — there is no
    /// IPC that forwards them to the primary. `cartog stats --savings` on a
    /// machine that runs multiple MCP servers will therefore *undercount*
    /// secondary traffic, not overcount it. This is a deliberate trade-off:
    /// the alternative would be a separate per-process file with its own
    /// merge logic, which is more complexity than the retention hook needs.
    ///
    /// Stored fields: `tool` (e.g. `"search"`, `"refs"`, MCP-side already
    /// strips the `cartog_` prefix so CLI and MCP rows aggregate), `source`
    /// (`"cli"` or `"mcp"`), and a unix-seconds timestamp. The query payload
    /// itself is never recorded — see the privacy banner in README.
    pub fn log_query(&self, tool: &str, source: &str) {
        if self.is_read_only() {
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Err(e) = self.conn.execute(
            "INSERT INTO query_log (tool, source, ts) VALUES (?1, ?2, ?3)",
            params![tool, source, ts],
        ) {
            // Always warn for the individual failure (debuggable in traces),
            // and additionally emit a one-shot loud-error on the first
            // failure so a persistently-broken query_log (e.g. SQLITE_FULL
            // or a missing table on a manually-tampered DB) is visible even
            // when warns are filtered.
            warn!(error = %e, tool, source, "query_log insert failed");
            if !LOG_QUERY_FAILURE_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::error!(
                    error = %e,
                    "query_log is failing — `cartog stats --savings` will undercount. \
                     Check disk space and `cartog doctor`."
                );
            }
        }
    }

    /// Aggregate `query_log` for `cartog stats --savings` / `cartog savings`.
    /// Safe on read-only attach (it's a read). Returns an empty report when
    /// the `query_log` table is missing (the read-only attach path skips
    /// schema bootstrap, so a v5 DB that lost the table — manual drop, partial
    /// snapshot restore — would otherwise surface a `no such table` error).
    pub fn savings_breakdown(&self) -> Result<SavingsReport> {
        // Probe for the table once. Only treat "no such table" as the empty-
        // report case so real DB faults (corruption, locked, permissions)
        // still propagate to the caller.
        if let Err(e) = self.conn.prepare("SELECT 1 FROM query_log LIMIT 0") {
            if is_no_such_table(&e) {
                return Ok(empty_savings_report());
            }
            return Err(e.into());
        }

        let mut tool_stmt = self.conn.prepare(
            "SELECT tool, COUNT(*) FROM query_log GROUP BY tool ORDER BY COUNT(*) DESC, tool",
        )?;
        let by_tool: Vec<(String, u64)> = tool_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut src_stmt = self.conn.prepare(
            "SELECT source, COUNT(*) FROM query_log GROUP BY source ORDER BY COUNT(*) DESC, source",
        )?;
        let by_source: Vec<(String, u64)> = src_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let total_queries: u64 = by_tool.iter().map(|(_, c)| c).sum();
        let tokens_used_cartog = total_queries.saturating_mul(TOKENS_PER_QUERY_CARTOG as u64);
        let tokens_used_grep = total_queries.saturating_mul(TOKENS_PER_QUERY_GREP as u64);
        let estimated_tokens_saved = tokens_used_grep.saturating_sub(tokens_used_cartog);
        // 0–99 to keep the visual bar from flat-topping at 100% on degenerate
        // data and to leave room for "less than 1% rounding" cases.
        let percent_saved = (estimated_tokens_saved * 100)
            .checked_div(tokens_used_grep)
            .unwrap_or(0)
            .min(99) as u8;

        Ok(SavingsReport {
            by_tool,
            by_source,
            total_queries,
            tokens_used_cartog,
            tokens_used_grep,
            estimated_tokens_saved,
            percent_saved,
            baseline_delta: TOKENS_SAVED_PER_QUERY,
        })
    }

    /// Get all non-import symbols ordered by in-degree (highest first), then by file.
    ///
    /// Used by `cartog map` to produce a centrality-ranked codebase summary.
    pub fn top_symbols(&self, limit: u32) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols
             WHERE kind != 'import' AND kind != 'variable'
             ORDER BY in_degree DESC, file_path, start_line
             LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], row_to_symbol)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Returns `true` if at least one file has been indexed.
    ///
    /// Cheaper than [`Database::stats`] for the common "is the index empty?" check —
    /// SQLite can satisfy `LIMIT 1` with a single index seek rather than a full count.
    pub fn has_indexed_files(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM files LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Get symbols for a set of file paths, grouped by file, ordered by line.
    ///
    /// Optionally filter by symbol kind. Only returns symbols for files that
    /// exist in the index. Files with no matching symbols are omitted.
    /// SQLite variable limit per query. Chunking keeps us well under the default
    /// `SQLITE_MAX_VARIABLE_NUMBER` (999 in older builds, 32766 in newer).
    const FILE_CHUNK_SIZE: usize = 500;

    pub fn symbols_for_files(
        &self,
        file_paths: &[String],
        kind_filter: Option<SymbolKind>,
    ) -> Result<Vec<Symbol>> {
        if file_paths.is_empty() {
            return Ok(Vec::new());
        }

        let kind_str = kind_filter.map(|k| k.as_str().to_string());
        let mut all_results = Vec::new();

        for chunk in file_paths.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<_> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let kind_param_idx = chunk.len() + 1;

            let sql = format!(
                "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                        parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
                 FROM symbols
                 WHERE file_path IN ({})
                   AND (?{kind_param_idx} IS NULL OR kind = ?{kind_param_idx})
                 ORDER BY file_path, start_line",
                placeholders.join(", ")
            );
            let mut stmt = self.conn.prepare(&sql)?;

            let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|p| Box::new(p.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            param_values.push(Box::new(kind_str.clone()));

            let params: Vec<&dyn rusqlite::types::ToSql> =
                param_values.iter().map(|p| &**p).collect();
            let rows = stmt
                .query_map(&*params, row_to_symbol)?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all_results.extend(rows);
        }

        // Re-sort across chunks to maintain file_path, start_line order
        if file_paths.len() > Self::FILE_CHUNK_SIZE {
            all_results.sort_by(|a, b| {
                a.file_path
                    .cmp(&b.file_path)
                    .then(a.start_line.cmp(&b.start_line))
            });
        }

        Ok(all_results)
    }

    /// Get all indexed file paths, sorted alphabetically.
    pub fn all_files(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Load (path, hash) pairs for every indexed file in one query.
    ///
    /// Used by the parallel indexer to avoid per-file DB round trips when
    /// deciding whether a file needs re-parsing.
    pub fn all_file_hashes(&self) -> Result<std::collections::HashMap<String, String>> {
        let mut stmt = self.conn.prepare("SELECT path, hash FROM files")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    // ── RAG: Symbol Content ──

    /// Insert or replace symbol content (raw source + metadata header for embedding).
    ///
    /// `symbol_name` is used to compute a normalized form (camelCase/snake_case split)
    /// stored in the FTS5 index for better keyword matching.
    pub fn upsert_symbol_content(
        &self,
        symbol_id: &str,
        symbol_name: &str,
        content: &str,
        header: &str,
    ) -> Result<()> {
        let normalized = normalize_symbol_name(symbol_name);
        self.conn.execute(
            "INSERT OR REPLACE INTO symbol_content (symbol_id, content, header, normalized_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![symbol_id, content, header, normalized],
        )?;
        Ok(())
    }

    /// Insert multiple symbol contents in a single transaction.
    ///
    /// Tuples: `(symbol_id, symbol_name, content, header)`.
    pub fn insert_symbol_contents(&self, items: &[(String, String, String, String)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.insert_symbol_contents_in_tx(items)?;
        tx.commit()?;
        Ok(())
    }

    /// Like [`Self::insert_symbol_contents`] but assumes the caller already
    /// holds an open transaction.
    pub fn insert_symbol_contents_in_tx(
        &self,
        items: &[(String, String, String, String)],
    ) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO symbol_content (symbol_id, content, header, normalized_name)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (symbol_id, name, content, header) in items {
            let normalized = normalize_symbol_name(name);
            stmt.execute(params![symbol_id, content, header, normalized])?;
        }
        Ok(())
    }

    /// Remove symbol content for all symbols in a file.
    pub fn clear_symbol_content_for_file(&self, file_path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM symbol_content WHERE symbol_id IN
             (SELECT id FROM symbols WHERE file_path = ?1)",
            params![file_path],
        )?;
        Ok(())
    }

    /// Get the content + header for a symbol.
    pub fn get_symbol_content(&self, symbol_id: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT content, header FROM symbol_content WHERE symbol_id = ?1",
                params![symbol_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("Failed to query symbol content")
    }

    /// Batch fetch content + header for multiple symbols.
    ///
    /// Returns a map of `symbol_id → (content, header)` for all found symbols.
    pub fn get_symbol_contents_batch(
        &self,
        symbol_ids: &[String],
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        let mut result = std::collections::HashMap::with_capacity(symbol_ids.len());
        if symbol_ids.is_empty() {
            return Ok(result);
        }
        for chunk in symbol_ids.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<&str> = chunk.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT symbol_id, content, header FROM symbol_content WHERE symbol_id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (id, content, header) in rows {
                result.insert(id, (content, header));
            }
        }
        Ok(result)
    }

    // ── RAG: FTS5 Search ──

    /// Full-text search over symbol names and content using BM25 ranking.
    ///
    /// Returns symbol IDs ordered by relevance (best match first).
    pub fn fts5_search(&self, query: &str, limit: u32) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id
             FROM symbol_fts f
             JOIN symbol_content sc ON sc.rowid = f.rowid
             WHERE symbol_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query, limit], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ── RAG: Embedding Map ──

    /// Get or create an integer ID for a symbol in the embedding map.
    ///
    /// Returns the `id` (integer rowid) used as key in the vec0 virtual table.
    pub fn get_or_create_embedding_id(&self, symbol_id: &str) -> Result<i64> {
        // Try to get existing
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1",
                params![symbol_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            return Ok(id);
        }

        // Insert new
        self.conn.execute(
            "INSERT INTO symbol_embedding_map (symbol_id) VALUES (?1)",
            params![symbol_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Look up the symbol ID for an embedding map rowid.
    pub fn symbol_id_for_embedding(&self, embedding_id: i64) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT symbol_id FROM symbol_embedding_map WHERE id = ?1",
                params![embedding_id],
                |row| row.get(0),
            )
            .optional()
            .context("Failed to query embedding map")
    }

    /// Batch look up symbol IDs for multiple embedding map rowids.
    pub fn symbol_ids_for_embeddings(&self, embedding_ids: &[i64]) -> Result<Vec<(i64, String)>> {
        if embedding_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut all_results = Vec::with_capacity(embedding_ids.len());
        for chunk in embedding_ids.chunks(Self::FILE_CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT id, symbol_id FROM symbol_embedding_map WHERE id IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
                .iter()
                .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            all_results.extend(rows);
        }
        Ok(all_results)
    }

    // ── RAG: Vector Storage (sqlite-vec) ──

    /// Insert or replace an embedding vector for a symbol.
    ///
    /// `embedding_id` is the integer key from `symbol_embedding_map`.
    /// `embedding` is a 384-dim f32 vector serialized as little-endian bytes.
    pub fn upsert_embedding(&self, embedding_id: i64, embedding: &[u8]) -> Result<()> {
        // Delete existing entry if any (vec0 doesn't support REPLACE)
        self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid = ?1",
            params![embedding_id],
        )?;
        self.conn.execute(
            "INSERT INTO symbol_vec (rowid, embedding) VALUES (?1, ?2)",
            params![embedding_id, embedding],
        )?;
        Ok(())
    }

    /// Insert multiple embeddings in a single transaction.
    pub fn insert_embeddings(&self, items: &[(i64, Vec<u8>)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (id, embedding) in items {
            self.conn
                .execute("DELETE FROM symbol_vec WHERE rowid = ?1", params![id])?;
            self.conn.execute(
                "INSERT INTO symbol_vec (rowid, embedding) VALUES (?1, ?2)",
                params![id, embedding],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// KNN vector search: find the `limit` nearest neighbors to `query_embedding`.
    ///
    /// Returns `(embedding_id, distance)` pairs ordered by distance (ascending).
    pub fn vector_search(&self, query_embedding: &[u8], limit: u32) -> Result<Vec<(i64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT rowid, distance
             FROM symbol_vec
             WHERE embedding MATCH ?1
             ORDER BY distance
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![query_embedding, limit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count the number of embeddings stored.
    pub fn embedding_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM symbol_embedding_map", [], |row| {
                row.get(0)
            })?)
    }

    /// Check if a symbol already has an embedding.
    pub fn has_embedding(&self, symbol_id: &str) -> Result<bool> {
        let map_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM symbol_embedding_map WHERE symbol_id = ?1",
                params![symbol_id],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(id) = map_id {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM symbol_vec WHERE rowid = ?1)",
                params![id],
                |row| row.get(0),
            )?;
            Ok(exists)
        } else {
            Ok(false)
        }
    }

    /// Remove all RAG data (content, FTS, embeddings, embedding map) for symbols in a file.
    pub fn clear_rag_data_for_file(&self, file_path: &str) -> Result<()> {
        // Delete embeddings via the map
        self.conn.execute(
            "DELETE FROM symbol_vec WHERE rowid IN
             (SELECT em.id FROM symbol_embedding_map em
              JOIN symbols s ON em.symbol_id = s.id
              WHERE s.file_path = ?1)",
            params![file_path],
        )?;
        // Delete embedding map entries
        self.conn.execute(
            "DELETE FROM symbol_embedding_map WHERE symbol_id IN
             (SELECT id FROM symbols WHERE file_path = ?1)",
            params![file_path],
        )?;
        // Delete content (triggers will clean up FTS)
        self.clear_symbol_content_for_file(file_path)?;
        Ok(())
    }

    /// Get a symbol by its ID.
    pub fn get_symbol(&self, id: &str) -> Result<Option<Symbol>> {
        self.conn
            .query_row(
                "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                        parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
                 FROM symbols WHERE id = ?1",
                params![id],
                row_to_symbol,
            )
            .optional()
            .context("Failed to query symbol")
    }

    /// Get multiple symbols by their IDs, preserving order.
    pub fn get_symbols_by_ids(&self, ids: &[String]) -> Result<Vec<Symbol>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
                    parent_id, signature, visibility, is_async, docstring, in_degree,
                    content_hash, subtree_hash
             FROM symbols WHERE id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows: std::collections::HashMap<String, Symbol> = stmt
            .query_map(param_refs.as_slice(), row_to_symbol)?
            .filter_map(|r| r.ok())
            .map(|s| (s.id.clone(), s))
            .collect();
        // Preserve caller's ordering
        Ok(ids.iter().filter_map(|id| rows.get(id).cloned()).collect())
    }

    /// Get all symbol IDs that have content stored but no embedding yet.
    ///
    /// Variables are excluded — they are too numerous and low-signal for embedding.
    pub fn symbols_needing_embeddings(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind NOT IN (?1, ?2)
             AND NOT EXISTS (
                 SELECT 1 FROM symbol_embedding_map em
                 JOIN symbol_vec sv ON sv.rowid = em.id
                 WHERE em.symbol_id = sc.symbol_id
             )",
        )?;
        let rows = stmt
            .query_map(
                params![SymbolKind::Variable.as_str(), SymbolKind::Import.as_str(),],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count symbols that have content stored.
    pub fn symbol_content_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM symbol_content", [], |row| row.get(0))?)
    }

    /// Get all symbol IDs that have content stored (excluding variables and imports).
    pub fn all_content_symbol_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind NOT IN (?1, ?2)
             ORDER BY sc.symbol_id",
        )?;
        let rows = stmt
            .query_map(
                params![SymbolKind::Variable.as_str(), SymbolKind::Import.as_str(),],
                |row| row.get(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Clear all embedding data (for force re-embed).
    pub fn clear_all_embeddings(&self) -> Result<()> {
        self.conn.execute("DELETE FROM symbol_vec", [])?;
        self.conn.execute("DELETE FROM symbol_embedding_map", [])?;
        Ok(())
    }

    // ── LSP Resolution Helpers ──
    //
    // These helpers (`unresolved_edges`, `find_symbol_at_location`,
    // `update_edge_target`, `mark_edge_unresolvable`, `mark_edge_external`,
    // `edge_resolution_state`) are called from `cartog-lsp::lsp_resolve_edges`,
    // which itself runs inside `index_directory`'s outer indexing transaction
    // (see [`Self::begin_indexing_tx`]). They MUST remain transaction-free —
    // any future addition of `unchecked_transaction()` here would re-introduce
    // the Phase 3 atomicity bug at runtime ("cannot start a transaction
    // within a transaction").

    /// Return edges still waiting for resolution (`resolution_state = 0`).
    ///
    /// Edges marked `state = 2` (unresolvable) or `state = 3` (external) are
    /// excluded so a dirty reindex doesn't re-query the language server for
    /// edges it already classified. Both are sticky and re-enter the
    /// unresolved set only via [`Self::reset_unresolvable_for_names`] when a
    /// matching symbol is added, or [`Self::reset_all_unresolvable`] on
    /// `--force`. ([`Self::invalidate_edges_targeting`] only touches state=1
    /// rows because it filters on `target_id IS NOT NULL`, and state {2, 3}
    /// rows always have `target_id NULL`.)
    ///
    /// tx-safe: read-only single statement — see note above the section header.
    pub fn unresolved_edges(&self) -> Result<Vec<UnresolvedEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.line
             FROM edges e
             WHERE e.resolution_state = 0",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(UnresolvedEdge {
                edge_id: row.get(0)?,
                target_name: row.get(1)?,
                file_path: row.get(2)?,
                line: row.get(3)?,
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Find the tightest-enclosing symbol at a given file + line.
    ///
    /// tx-safe: read-only single statement — see the LSP-section header note.
    pub fn find_symbol_at_location(&self, file_path: &str, line: u32) -> Result<Option<String>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM symbols
                 WHERE file_path = ?1 AND start_line <= ?2 AND end_line >= ?2
                 ORDER BY (end_line - start_line) ASC
                 LIMIT 1",
                params![file_path, line],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Update a single edge's target_id and flip it to `resolution_state = 1`.
    ///
    /// tx-safe: single statement — see the LSP-section header note. If you
    /// ever batch this internally with `unchecked_transaction()`, also update
    /// `index_directory` so it does not call `lsp_resolve_edges` inside its
    /// outer transaction.
    pub fn update_edge_target(&self, edge_id: i64, target_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET target_id = ?1, resolution_state = 1 WHERE id = ?2",
            params![target_id, edge_id],
        )?;
        Ok(())
    }

    /// Test-only inspector: returns true when the edge is at `resolution_state = 2`.
    ///
    /// Exposed because the column is otherwise crate-private, and downstream
    /// integration tests (cartog-indexer) need a read-only way to assert the
    /// marker state without snapshotting raw SQL.
    pub fn is_edge_unresolvable(&self, edge_id: i64) -> Result<bool> {
        Ok(self.edge_resolution_state(edge_id)? == 2)
    }

    /// Test-only inspector: returns the raw `resolution_state` value for an edge.
    ///
    /// 0=unresolved, 1=resolved, 2=unresolvable, 3=external.
    pub fn edge_resolution_state(&self, edge_id: i64) -> Result<i64> {
        let state: i64 = self.conn.query_row(
            "SELECT resolution_state FROM edges WHERE id = ?1",
            params![edge_id],
            |row| row.get(0),
        )?;
        Ok(state)
    }

    /// Reset every edge at `resolution_state IN (2, 3)` back to `0`. Used by
    /// `cartog index --force` to honor the "retry everything" contract:
    /// without this, the heuristic + LSP would still skip permanently-marked
    /// edges (both unresolvable and external) even on a forced re-index.
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn reset_all_unresolvable(&self) -> Result<u32> {
        let n = self.conn.execute(
            "UPDATE edges SET resolution_state = 0 WHERE resolution_state IN (2, 3)",
            [],
        )?;
        Ok(n as u32)
    }

    /// Mark a single edge as `resolution_state = 2` (LSP definitively gave up).
    ///
    /// Callers MUST only invoke this after a definitive negative answer from
    /// the language server. Never call from a transient-error branch (server
    /// crash, didOpen failure, half-loaded warmup) — the marker is sticky
    /// across runs until [`Self::reset_unresolvable_for_names`] reopens it
    /// (on a matching new symbol) or [`Self::reset_all_unresolvable`] runs
    /// (`--force`).
    ///
    /// The `WHERE resolution_state = 0` guard preserves the invariant that
    /// state {2, 3} rows have `target_id IS NULL` — without it an accidental
    /// call on a state=1 (resolved) edge would silently flip the state while
    /// keeping the stale target, hiding a corrupted edge from
    /// [`Self::unresolved_edges`].
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn mark_edge_unresolvable(&self, edge_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET resolution_state = 2 WHERE id = ?1 AND resolution_state = 0",
            params![edge_id],
        )?;
        Ok(())
    }

    /// Mark a single edge as `resolution_state = 3` (LSP located the target
    /// outside the indexed root — stdlib, third-party deps, node_modules).
    ///
    /// Same stickiness contract as [`Self::mark_edge_unresolvable`]: only call
    /// after a definitive positive answer naming an out-of-root URI;
    /// reopened by the same name-keyed and force-reset paths. The
    /// `WHERE resolution_state = 0` guard preserves the `target_id IS NULL`
    /// invariant for state=3 rows.
    ///
    /// tx-safe: single statement — see the LSP-section header note.
    pub fn mark_edge_external(&self, edge_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET resolution_state = 3 WHERE id = ?1 AND resolution_state = 0",
            params![edge_id],
        )?;
        Ok(())
    }

    /// Reset `resolution_state` from {2, 3} → 0 for edges whose target_name is in `names`.
    ///
    /// Called from the indexer when new symbols are added: an edge that was
    /// previously "unresolvable" (no symbol with this name existed) or
    /// "external" (target lived outside the index — but the user just vendored
    /// it in-tree) may now be resolvable against the freshly-added target.
    /// Returns the number of edges reopened. No-op when `names` is empty.
    ///
    /// tx-safe: single statement. Names are batched to honor SQLite's default
    /// 999-parameter limit; only rows at state 2 or 3 are touched so the write
    /// set stays tiny even on a large rename.
    pub fn reset_unresolvable_for_names(&self, names: &[String]) -> Result<u32> {
        if names.is_empty() {
            return Ok(0);
        }
        const CHUNK: usize = 500;
        let mut total: u32 = 0;
        for chunk in names.chunks(CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "UPDATE edges
                 SET resolution_state = 0
                 WHERE resolution_state IN (2, 3)
                   AND target_name IN ({placeholders})"
            );
            let params: Vec<&dyn rusqlite::ToSql> =
                chunk.iter().map(|n| n as &dyn rusqlite::ToSql).collect();
            let n = self.conn.execute(&sql, params.as_slice())?;
            total += n as u32;
        }
        Ok(total)
    }
}

/// An unresolved edge from the database (used by LSP resolution).
#[derive(Debug, Clone)]
pub struct UnresolvedEdge {
    pub edge_id: i64,
    pub target_name: String,
    pub file_path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexStats {
    pub num_files: u32,
    pub num_symbols: u32,
    pub num_edges: u32,
    pub num_resolved: u32,
    /// Edges at `resolution_state = 2` (LSP definitively gave up: typo, dyn dispatch, macro).
    pub num_unresolvable: u32,
    /// Edges at `resolution_state = 3` (LSP located the target outside the indexed root).
    pub num_external: u32,
    pub languages: Vec<(String, u32)>,
    pub symbol_kinds: Vec<(String, u32)>,
}

/// Per-tool query counts + token-savings estimate for `cartog stats --savings`.
///
/// Carries both sides of the comparison (cartog vs grep+read) so the CLI can
/// render a "with / without / saved" breakdown that's actually informative —
/// the flat delta on its own under-explains where the number comes from.
#[derive(Debug, Clone, Serialize)]
pub struct SavingsReport {
    /// `(tool_name, count)` sorted by count descending, then tool name.
    pub by_tool: Vec<(String, u64)>,
    /// `(source, count)` for `"cli"` / `"mcp"`.
    pub by_source: Vec<(String, u64)>,
    /// Sum of all per-tool counts.
    pub total_queries: u64,
    /// Estimated tokens cartog used for `total_queries` reads.
    pub tokens_used_cartog: u64,
    /// Estimated tokens an equivalent grep+read flow would have used.
    pub tokens_used_grep: u64,
    /// `tokens_used_grep - tokens_used_cartog`. Same as the old
    /// `estimated_tokens_saved` field; kept for JSON back-compat.
    pub estimated_tokens_saved: u64,
    /// Integer percent of `tokens_used_grep` saved (0–99). Caps at 99 so
    /// the bar never visually flat-tops at 100% on degenerate data.
    pub percent_saved: u8,
    /// Per-query baseline token delta (grep − cartog). Exposed so the CLI
    /// can name the figure in the footer.
    pub baseline_delta: u32,
}

/// Per-query token cost for cartog. Measured: ~280 tokens for a typical
/// navigation query (`where is X used?`, `what does X call?`) including the
/// structured response payload.
pub const TOKENS_PER_QUERY_CARTOG: u32 = 280;

/// Per-query token cost for an equivalent grep + read flow. Measured: a
/// grep sweep plus reading the surrounding ~50 lines of each hit averages
/// ~1,700 tokens to answer the same navigation question.
pub const TOKENS_PER_QUERY_GREP: u32 = 1_700;

/// Per-query token delta (`grep − cartog`). Coarse on purpose; refining
/// per-tool would require richer per-call accounting and isn't worth it
/// pre-v1. Sources: benchmarks/queries.rs (see `crates/cartog/benches/`).
pub const TOKENS_SAVED_PER_QUERY: u32 = TOKENS_PER_QUERY_GREP - TOKENS_PER_QUERY_CARTOG;

/// One-shot flag flipped the first time `log_query` fails. Surfaces a loud
/// error so a persistently-broken `query_log` (SQLITE_FULL, missing table)
/// is visible even when `warn!` is filtered. Process-scoped on purpose: the
/// goal is one user-visible message per cartog invocation, not per row.
static LOG_QUERY_FAILURE_REPORTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Zero-state [`SavingsReport`] used when no queries have been logged yet
/// (or when the `query_log` table is missing on a read-only attach).
fn empty_savings_report() -> SavingsReport {
    SavingsReport {
        by_tool: Vec::new(),
        by_source: Vec::new(),
        total_queries: 0,
        tokens_used_cartog: 0,
        tokens_used_grep: 0,
        estimated_tokens_saved: 0,
        percent_saved: 0,
        baseline_delta: TOKENS_SAVED_PER_QUERY,
    }
}

/// Returns true when a rusqlite error specifically indicates a missing table,
/// not any other prepare failure. Used by `savings_breakdown` to distinguish
/// "query_log doesn't exist yet" (return empty report) from real DB faults
/// (propagate).
fn is_no_such_table(e: &rusqlite::Error) -> bool {
    // SQLite reports missing tables as a generic Error (extended code 1)
    // whose message starts with `no such table:`. Match on the message text
    // because the rusqlite error variant for prepare-time errors does not
    // carry the offending object name.
    e.to_string().contains("no such table")
}

// ── Row Mapping Helpers ──

fn row_to_symbol(row: &rusqlite::Row<'_>) -> rusqlite::Result<Symbol> {
    row_to_symbol_offset(row, 0)
}

fn row_to_symbol_offset(row: &rusqlite::Row<'_>, off: usize) -> rusqlite::Result<Symbol> {
    let kind_str = row.get::<_, String>(off + 2)?;
    let kind = kind_str.parse().unwrap_or_else(|_| {
        warn!(kind = %kind_str, "unknown symbol kind, defaulting to variable");
        SymbolKind::Variable
    });

    let vis_str = row.get::<_, Option<String>>(off + 10)?.unwrap_or_default();

    Ok(Symbol {
        id: row.get(off)?,
        name: row.get(off + 1)?,
        kind,
        file_path: row.get(off + 3)?,
        start_line: row.get(off + 4)?,
        end_line: row.get(off + 5)?,
        start_byte: row.get(off + 6)?,
        end_byte: row.get(off + 7)?,
        parent_id: row.get(off + 8)?,
        signature: row.get(off + 9)?,
        visibility: Visibility::from_str_lossy(&vis_str),
        is_async: row.get(off + 11)?,
        docstring: row.get(off + 12)?,
        in_degree: row.get(off + 13).unwrap_or(0),
        content_hash: row.get(off + 14).unwrap_or(None),
        subtree_hash: row.get(off + 15).unwrap_or(None),
    })
}

/// When exactly 2 global matches exist, try to pick one unambiguously.
/// This is a last-resort heuristic — only reached after same-file, import-path,
/// same-directory, and parent-scope tiers all fail.
///
/// Patterns:
/// - type def vs method (Java/TS constructor shares class name) → prefer type def
/// - function vs method (Ruby/Go top-level fn vs module method) → prefer function
fn disambiguate_two<'a>(a: &'a (String, String), b: &'a (String, String)) -> Option<&'a String> {
    match kind_priority(&a.1).cmp(&kind_priority(&b.1)) {
        std::cmp::Ordering::Greater => Some(&a.0),
        std::cmp::Ordering::Less => Some(&b.0),
        std::cmp::Ordering::Equal => None,
    }
}

/// Higher priority = preferred in disambiguation.
/// Only values that differ trigger disambiguation; equal priorities → no resolution.
fn kind_priority(kind: &str) -> u8 {
    match kind {
        "class" | "interface" | "enum" | "type_alias" | "trait" => 3,
        "function" => 2,
        "method" => 1,
        _ => 0,
    }
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    let kind_str = row.get::<_, String>(4)?;
    let kind = kind_str.parse().unwrap_or_else(|_| {
        warn!(kind = %kind_str, "unknown edge kind, defaulting to references");
        EdgeKind::References
    });

    Ok(Edge {
        source_id: row.get(1)?,
        target_name: row.get(2)?,
        target_id: row.get(3)?,
        kind,
        file_path: row.get(5)?,
        line: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_symbol(name: &str, kind: SymbolKind, file: &str, line: u32) -> Symbol {
        Symbol::new(name, kind, file, line, line + 5, 0, 100, None)
    }

    // ── normalize_symbol_name tests ──

    #[test]
    fn test_normalize_snake_case() {
        assert_eq!(normalize_symbol_name("validate_token"), "validate token");
        assert_eq!(
            normalize_symbol_name("get_current_user"),
            "get current user"
        );
        assert_eq!(normalize_symbol_name("_private_method"), "private method");
        assert_eq!(normalize_symbol_name("__init__"), "init");
    }

    #[test]
    fn test_normalize_camel_case() {
        assert_eq!(normalize_symbol_name("validateToken"), "validate token");
        assert_eq!(normalize_symbol_name("getCurrentUser"), "get current user");
        assert_eq!(normalize_symbol_name("findByToken"), "find by token");
    }

    #[test]
    fn test_normalize_pascal_case() {
        assert_eq!(
            normalize_symbol_name("DatabaseConnection"),
            "database connection"
        );
        assert_eq!(normalize_symbol_name("AuthService"), "auth service");
        assert_eq!(normalize_symbol_name("TokenError"), "token error");
    }

    #[test]
    fn test_normalize_screaming_snake() {
        assert_eq!(normalize_symbol_name("TOKEN_EXPIRY"), "token expiry");
        assert_eq!(normalize_symbol_name("MAX_RETRY_COUNT"), "max retry count");
    }

    #[test]
    fn test_normalize_acronyms() {
        assert_eq!(
            normalize_symbol_name("getHTTPResponse"),
            "get http response"
        );
        assert_eq!(normalize_symbol_name("parseJSON"), "parse json");
        assert_eq!(normalize_symbol_name("HTMLParser"), "html parser");
    }

    #[test]
    fn test_normalize_single_word() {
        assert_eq!(normalize_symbol_name("validate"), "validate");
        assert_eq!(normalize_symbol_name("Token"), "token");
    }

    #[test]
    fn test_normalize_empty_and_special() {
        assert_eq!(normalize_symbol_name(""), "");
        assert_eq!(normalize_symbol_name("_"), "");
        assert_eq!(normalize_symbol_name("___"), "");
    }

    #[test]
    fn test_insert_and_query_symbols() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("my_func", SymbolKind::Function, "test.py", 10);
        db.insert_symbol(&sym).unwrap();

        let outline = db.outline("test.py").unwrap();
        assert_eq!(outline.len(), 1);
        assert_eq!(outline[0].name, "my_func");
    }

    #[test]
    fn is_empty_reflects_symbol_presence() {
        let db = Database::open_memory().unwrap();
        assert!(db.is_empty().unwrap(), "fresh DB should be empty");
        db.insert_symbol(&test_symbol("f", SymbolKind::Function, "a.py", 1))
            .unwrap();
        assert!(!db.is_empty().unwrap(), "DB with a symbol is not empty");
    }

    #[test]
    fn test_insert_and_query_edges() {
        let db = Database::open_memory().unwrap();
        let caller = test_symbol("caller_fn", SymbolKind::Function, "a.py", 1);
        let callee = test_symbol("callee_fn", SymbolKind::Function, "b.py", 1);
        db.insert_symbol(&caller).unwrap();
        db.insert_symbol(&callee).unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "callee_fn".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let refs = db.refs("callee_fn", None).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0.source_id, caller.id);
    }

    #[test]
    fn test_edge_resolution() {
        let db = Database::open_memory().unwrap();
        let sym_a = test_symbol("process", SymbolKind::Function, "a.py", 1);
        let sym_b = test_symbol("helper", SymbolKind::Function, "a.py", 20);
        db.insert_symbols(&[sym_a.clone(), sym_b.clone()]).unwrap();

        let edge = Edge {
            source_id: sym_a.id.clone(),
            target_name: "helper".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);
    }

    #[test]
    fn test_stats() {
        let db = Database::open_memory().unwrap();
        let file = FileInfo {
            path: "test.py".to_string(),
            last_modified: 0.0,
            hash: "abc".to_string(),
            language: "python".to_string(),
            num_symbols: 2,
        };
        db.upsert_file(&file).unwrap();
        let sym = test_symbol("foo", SymbolKind::Function, "test.py", 1);
        db.insert_symbol(&sym).unwrap();

        let stats = db.stats().unwrap();
        assert_eq!(stats.num_files, 1);
        assert_eq!(stats.num_symbols, 1);
    }

    #[test]
    fn savings_breakdown_empty_returns_zero() {
        let db = Database::open_memory().unwrap();
        let r = db.savings_breakdown().unwrap();
        assert_eq!(r.total_queries, 0);
        assert_eq!(r.tokens_used_cartog, 0);
        assert_eq!(r.tokens_used_grep, 0);
        assert_eq!(r.estimated_tokens_saved, 0);
        assert_eq!(r.percent_saved, 0);
        assert!(r.by_tool.is_empty());
        assert!(r.by_source.is_empty());
        assert_eq!(r.baseline_delta, TOKENS_SAVED_PER_QUERY);
    }

    #[test]
    fn log_query_persists_rows_aggregated_by_tool_and_source() {
        let db = Database::open_memory().unwrap();
        db.log_query("search", "cli");
        db.log_query("search", "cli");
        db.log_query("refs", "cli");
        db.log_query("search", "mcp");
        db.log_query("impact", "mcp");

        let r = db.savings_breakdown().unwrap();
        assert_eq!(r.total_queries, 5);
        // With/without/saved derived from the per-query constants.
        assert_eq!(r.tokens_used_cartog, 5 * TOKENS_PER_QUERY_CARTOG as u64);
        assert_eq!(r.tokens_used_grep, 5 * TOKENS_PER_QUERY_GREP as u64);
        assert_eq!(r.estimated_tokens_saved, 5 * TOKENS_SAVED_PER_QUERY as u64);
        // ~83% saved given 280 vs 1700 baseline.
        assert_eq!(r.percent_saved, 83);

        // by_tool sorted by count desc, then name
        let tool_counts: Vec<_> = r.by_tool.iter().map(|(t, c)| (t.as_str(), *c)).collect();
        assert_eq!(tool_counts, vec![("search", 3), ("impact", 1), ("refs", 1)]);

        let src_counts: Vec<_> = r.by_source.iter().map(|(s, c)| (s.as_str(), *c)).collect();
        assert_eq!(src_counts, vec![("cli", 3), ("mcp", 2)]);
    }

    #[test]
    fn log_query_noop_on_read_only_attach() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let primary = Database::open(&db_path, 384).unwrap();
            primary.log_query("search", "cli"); // primary write succeeds
        }

        let reader = Database::open_readonly(&db_path).unwrap();
        assert!(reader.is_read_only());
        // log_query on read-only attach must silently no-op (no panic, no insert).
        reader.log_query("search", "mcp");
        reader.log_query("refs", "mcp");

        let r = reader.savings_breakdown().unwrap();
        // Only the primary's row is visible — secondary writes were dropped.
        assert_eq!(r.total_queries, 1);
        assert_eq!(r.by_tool, vec![("search".to_string(), 1)]);
    }

    #[test]
    fn test_resolve_edges_same_dir_priority() {
        let db = Database::open_memory().unwrap();

        // "helper" exists in same dir (src/utils.py) and elsewhere (lib/utils.py)
        let caller = test_symbol("process", SymbolKind::Function, "src/main.py", 1);
        let same_dir = test_symbol("helper", SymbolKind::Function, "src/utils.py", 1);
        let other_dir = test_symbol("helper", SymbolKind::Function, "lib/utils.py", 1);
        db.insert_symbols(&[caller.clone(), same_dir.clone(), other_dir.clone()])
            .unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "helper".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "src/main.py".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);

        // Verify it resolved to the same-directory symbol
        let refs = db.refs("helper", None).unwrap();
        let call_edge = refs
            .iter()
            .find(|(e, _)| e.kind == EdgeKind::Calls)
            .unwrap();
        assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &same_dir.id);
    }

    #[test]
    fn test_resolve_edges_ambiguous_no_resolve() {
        let db = Database::open_memory().unwrap();

        // "helper" in two different directories, caller in a third
        let caller = test_symbol("process", SymbolKind::Function, "app/main.py", 1);
        let sym1 = test_symbol("helper", SymbolKind::Function, "pkg_a/utils.py", 1);
        let sym2 = test_symbol("helper", SymbolKind::Function, "pkg_b/utils.py", 1);
        db.insert_symbols(&[caller.clone(), sym1, sym2]).unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "helper".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "app/main.py".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        // Should NOT resolve because "helper" is ambiguous (2 matches globally)
        assert_eq!(resolved, 0);
    }

    #[test]
    fn test_resolve_edges_same_file_priority() {
        let db = Database::open_memory().unwrap();

        // "helper" in same file AND in another file
        let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
        let same_file = test_symbol("helper", SymbolKind::Function, "a.py", 20);
        let other_file = test_symbol("helper", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[caller.clone(), same_file.clone(), other_file])
            .unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "helper".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "a.py".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);

        // Verify same-file symbol was chosen
        let refs = db.refs("helper", None).unwrap();
        let call_edge = refs
            .iter()
            .find(|(e, _)| e.kind == EdgeKind::Calls)
            .unwrap();
        assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &same_file.id);
    }

    #[test]
    fn test_resolve_edges_class_over_constructor() {
        let db = Database::open_memory().unwrap();

        // Java pattern: Logger class + Logger() constructor method in same file
        let caller = test_symbol("handleLogin", SymbolKind::Method, "auth/Service.java", 10);
        let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
        let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
        db.insert_symbols(&[caller.clone(), logger_class.clone(), logger_ctor])
            .unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "Logger".to_string(),
            target_id: None,
            kind: EdgeKind::References,
            file_path: "auth/Service.java".to_string(),
            line: 12,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);

        let refs = db.refs("Logger", None).unwrap();
        let ref_edge = refs
            .iter()
            .find(|(e, _)| e.kind == EdgeKind::References)
            .unwrap();
        assert_eq!(ref_edge.0.target_id.as_ref().unwrap(), &logger_class.id);
    }

    #[test]
    fn test_resolve_edges_class_over_constructor_still_ambiguous_with_three() {
        let db = Database::open_memory().unwrap();

        // Three matches: class + ctor + function — should NOT resolve
        let caller = test_symbol("main", SymbolKind::Function, "app.java", 1);
        let sym_class = test_symbol("Foo", SymbolKind::Class, "a/Foo.java", 1);
        let sym_ctor = test_symbol("Foo", SymbolKind::Method, "a/Foo.java", 5);
        let sym_func = test_symbol("Foo", SymbolKind::Function, "b/Foo.java", 1);
        db.insert_symbols(&[caller.clone(), sym_class, sym_ctor, sym_func])
            .unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "Foo".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "app.java".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 0);
    }

    #[test]
    fn test_resolve_edges_multipass_import_then_call() {
        let db = Database::open_memory().unwrap();

        // File auth/service.java imports Logger from util/Logger.java
        // and also calls Logger.info() — a reference to Logger
        let import_sym = test_symbol("util.Logger", SymbolKind::Import, "auth/service.java", 1);
        let caller = test_symbol("authenticate", SymbolKind::Method, "auth/service.java", 10);
        let logger_class = test_symbol("Logger", SymbolKind::Class, "util/Logger.java", 1);
        let logger_ctor = test_symbol("Logger", SymbolKind::Method, "util/Logger.java", 5);
        db.insert_symbols(&[
            import_sym.clone(),
            caller.clone(),
            logger_class.clone(),
            logger_ctor,
        ])
        .unwrap();

        // Import edge: auth/service.java imports "Logger"
        let import_edge = Edge {
            source_id: import_sym.id.clone(),
            target_name: "Logger".to_string(),
            target_id: None,
            kind: EdgeKind::Imports,
            file_path: "auth/service.java".to_string(),
            line: 1,
        };
        db.insert_edge(&import_edge).unwrap();

        // Reference edge: authenticate() references Logger
        let ref_edge = Edge {
            source_id: caller.id.clone(),
            target_name: "Logger".to_string(),
            target_id: None,
            kind: EdgeKind::References,
            file_path: "auth/service.java".to_string(),
            line: 15,
        };
        db.insert_edge(&ref_edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        // Pass 1: import edge resolves via tier 6 (class over ctor)
        // Pass 2: reference edge resolves via tier 2 (import-path)
        assert_eq!(resolved, 2);

        let refs = db.refs("Logger", None).unwrap();
        let reference = refs
            .iter()
            .find(|(e, _)| e.kind == EdgeKind::References)
            .unwrap();
        assert_eq!(reference.0.target_id.as_ref().unwrap(), &logger_class.id);
    }

    #[test]
    fn test_resolve_edges_function_over_method() {
        let db = Database::open_memory().unwrap();

        // Ruby pattern: get_logger as top-level function AND as module method
        let caller = test_symbol("process", SymbolKind::Function, "app/main.rb", 1);
        let top_fn = test_symbol("get_logger", SymbolKind::Function, "utils/helpers.rb", 6);
        let mod_method = test_symbol("get_logger", SymbolKind::Method, "utils/logging.rb", 6);
        db.insert_symbols(&[caller.clone(), top_fn.clone(), mod_method])
            .unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "get_logger".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "app/main.rb".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);

        let refs = db.refs("get_logger", None).unwrap();
        let call_edge = refs
            .iter()
            .find(|(e, _)| e.kind == EdgeKind::Calls)
            .unwrap();
        assert_eq!(call_edge.0.target_id.as_ref().unwrap(), &top_fn.id);
    }

    #[test]
    fn test_resolve_edges_two_functions_still_ambiguous() {
        let db = Database::open_memory().unwrap();

        // Two functions with same name in different files — should NOT resolve
        let caller = test_symbol("main", SymbolKind::Function, "app.rb", 1);
        let fn1 = test_symbol("helper", SymbolKind::Function, "a/utils.rb", 1);
        let fn2 = test_symbol("helper", SymbolKind::Function, "b/utils.rb", 1);
        db.insert_symbols(&[caller.clone(), fn1, fn2]).unwrap();

        let edge = Edge {
            source_id: caller.id.clone(),
            target_name: "helper".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "app.rb".to_string(),
            line: 5,
        };
        db.insert_edge(&edge).unwrap();

        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 0);
    }

    #[test]
    fn test_callees_query() {
        let db = Database::open_memory().unwrap();

        let caller = test_symbol("process", SymbolKind::Function, "a.py", 1);
        let callee1 = test_symbol("fetch", SymbolKind::Function, "b.py", 1);
        let callee2 = test_symbol("save", SymbolKind::Function, "c.py", 1);
        db.insert_symbols(&[caller.clone(), callee1, callee2])
            .unwrap();

        db.insert_edges(&[
            Edge {
                source_id: caller.id.clone(),
                target_name: "fetch".to_string(),
                target_id: None,
                kind: EdgeKind::Calls,
                file_path: "a.py".to_string(),
                line: 5,
            },
            Edge {
                source_id: caller.id.clone(),
                target_name: "save".to_string(),
                target_id: None,
                kind: EdgeKind::Calls,
                file_path: "a.py".to_string(),
                line: 6,
            },
        ])
        .unwrap();

        let callees = db.callees("process").unwrap();
        assert_eq!(callees.len(), 2);
        let targets: Vec<&str> = callees.iter().map(|e| e.target_name.as_str()).collect();
        assert!(targets.contains(&"fetch"));
        assert!(targets.contains(&"save"));
    }

    #[test]
    fn test_impact_transitive() {
        let db = Database::open_memory().unwrap();

        let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
        let b = test_symbol("b", SymbolKind::Function, "b.py", 1);
        let c = test_symbol("c", SymbolKind::Function, "c.py", 1);
        db.insert_symbols(&[a.clone(), b.clone(), c.clone()])
            .unwrap();

        // b calls a, c calls b
        db.insert_edges(&[
            Edge {
                source_id: b.id.clone(),
                target_name: "a".to_string(),
                target_id: Some(a.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "b.py".to_string(),
                line: 5,
            },
            Edge {
                source_id: c.id.clone(),
                target_name: "b".to_string(),
                target_id: Some(b.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "c.py".to_string(),
                line: 5,
            },
        ])
        .unwrap();

        // Impact of "a" with depth 2 should find b (depth 1) and c (depth 2)
        let results = db.impact("a", 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].1, 1); // first hop
        assert_eq!(results[1].1, 2); // second hop
    }

    #[test]
    fn test_impact_depth_zero_returns_empty() {
        let db = Database::open_memory().unwrap();
        let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
        db.insert_symbols(&[a]).unwrap();
        assert!(db.impact("a", 0).unwrap().is_empty());
    }

    #[test]
    fn test_impact_cycle_terminates() {
        // Cycle: a → b → a. impact("a", 3) must not loop forever.
        let db = Database::open_memory().unwrap();
        let a = test_symbol("a", SymbolKind::Function, "a.py", 1);
        let b = test_symbol("b", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[a.clone(), b.clone()]).unwrap();
        db.insert_edges(&[
            Edge {
                source_id: a.id.clone(),
                target_name: "b".to_string(),
                target_id: Some(b.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "a.py".to_string(),
                line: 2,
            },
            Edge {
                source_id: b.id.clone(),
                target_name: "a".to_string(),
                target_id: Some(a.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "b.py".to_string(),
                line: 2,
            },
        ])
        .unwrap();

        // Each of the two edges is returned once, labeled with its shallowest depth.
        let results = db.impact("a", 5).unwrap();
        assert_eq!(results.len(), 2);
        for (_, depth) in &results {
            assert!(*depth >= 1 && *depth <= 5);
        }
    }

    #[test]
    fn test_impact_fanout_dedupes_by_edge() {
        // Two callers of `shared`, each also calling each other → diamond.
        // Each edge should appear once.
        let db = Database::open_memory().unwrap();
        let shared = test_symbol("shared", SymbolKind::Function, "s.py", 1);
        let x = test_symbol("x", SymbolKind::Function, "x.py", 1);
        let y = test_symbol("y", SymbolKind::Function, "y.py", 1);
        db.insert_symbols(&[shared.clone(), x.clone(), y.clone()])
            .unwrap();
        db.insert_edges(&[
            Edge {
                source_id: x.id.clone(),
                target_name: "shared".to_string(),
                target_id: Some(shared.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "x.py".to_string(),
                line: 1,
            },
            Edge {
                source_id: y.id.clone(),
                target_name: "shared".to_string(),
                target_id: Some(shared.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "y.py".to_string(),
                line: 1,
            },
            Edge {
                source_id: y.id.clone(),
                target_name: "x".to_string(),
                target_id: Some(x.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "y.py".to_string(),
                line: 2,
            },
        ])
        .unwrap();

        let results = db.impact("shared", 3).unwrap();
        // 3 distinct edges, each reported exactly once.
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_hierarchy_query() {
        let db = Database::open_memory().unwrap();

        let parent = test_symbol("Animal", SymbolKind::Class, "a.py", 1);
        let child = test_symbol("Dog", SymbolKind::Class, "a.py", 10);
        db.insert_symbols(&[parent, child.clone()]).unwrap();

        db.insert_edge(&Edge {
            source_id: child.id.clone(),
            target_name: "Animal".to_string(),
            target_id: None,
            kind: EdgeKind::Inherits,
            file_path: "a.py".to_string(),
            line: 10,
        })
        .unwrap();

        let pairs = db.hierarchy("Dog").unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "Dog");
        assert_eq!(pairs[0].1, "Animal");
    }

    #[test]
    fn test_file_deps_query() {
        let db = Database::open_memory().unwrap();

        let import_sym = test_symbol("os", SymbolKind::Import, "main.py", 1);
        db.insert_symbol(&import_sym).unwrap();

        db.insert_edge(&Edge {
            source_id: import_sym.id.clone(),
            target_name: "os".to_string(),
            target_id: None,
            kind: EdgeKind::Imports,
            file_path: "main.py".to_string(),
            line: 1,
        })
        .unwrap();

        let deps = db.file_deps("main.py").unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].target_name, "os");
    }

    #[test]
    fn test_remove_file_clears_all_data() {
        let db = Database::open_memory().unwrap();

        let sym = test_symbol("foo", SymbolKind::Function, "test.py", 1);
        db.insert_symbol(&sym).unwrap();
        db.insert_edge(&Edge {
            source_id: sym.id.clone(),
            target_name: "bar".to_string(),
            target_id: None,
            kind: EdgeKind::Calls,
            file_path: "test.py".to_string(),
            line: 5,
        })
        .unwrap();
        db.upsert_file(&FileInfo {
            path: "test.py".to_string(),
            last_modified: 0.0,
            hash: "abc".to_string(),
            language: "python".to_string(),
            num_symbols: 1,
        })
        .unwrap();

        db.remove_file("test.py").unwrap();

        assert!(db.outline("test.py").unwrap().is_empty());
        assert!(db.get_file("test.py").unwrap().is_none());
    }

    #[test]
    fn test_refs_with_kind_filter() {
        let db = Database::open_memory().unwrap();
        let parent = test_symbol("AuthService", SymbolKind::Class, "a.py", 1);
        let child = test_symbol("AdminService", SymbolKind::Class, "a.py", 20);
        let caller = test_symbol("login", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[parent.clone(), child.clone(), caller.clone()])
            .unwrap();

        db.insert_edges(&[
            Edge {
                source_id: child.id.clone(),
                target_name: "AuthService".to_string(),
                target_id: None,
                kind: EdgeKind::Inherits,
                file_path: "a.py".to_string(),
                line: 20,
            },
            Edge {
                source_id: caller.id.clone(),
                target_name: "AuthService".to_string(),
                target_id: None,
                kind: EdgeKind::Calls,
                file_path: "b.py".to_string(),
                line: 5,
            },
        ])
        .unwrap();

        // No filter → both edges
        let all = db.refs("AuthService", None).unwrap();
        assert_eq!(all.len(), 2);

        // Filter inherits only
        let inherits = db.refs("AuthService", Some(EdgeKind::Inherits)).unwrap();
        assert_eq!(inherits.len(), 1);
        assert_eq!(inherits[0].0.kind, EdgeKind::Inherits);

        // Filter calls only
        let calls = db.refs("AuthService", Some(EdgeKind::Calls)).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.kind, EdgeKind::Calls);

        // Filter with no matches
        let raises = db.refs("AuthService", Some(EdgeKind::Raises)).unwrap();
        assert!(raises.is_empty());
    }

    #[test]
    fn test_search_exact_match_ranks_first() {
        let db = Database::open_memory().unwrap();
        let exact = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
        let prefix = test_symbol("parse_config_file", SymbolKind::Function, "a.py", 10);
        let substr = test_symbol("get_parse_config", SymbolKind::Function, "a.py", 20);
        db.insert_symbols(&[exact.clone(), prefix, substr]).unwrap();

        let results = db.search("parse_config", None, None, 20).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].name, "parse_config");
    }

    #[test]
    fn test_search_definitions_outrank_variables() {
        let db = Database::open_memory().unwrap();
        // Variables with exact match on "token"
        let var1 = test_symbol("token", SymbolKind::Variable, "routes/auth.ts", 20);
        let var2 = test_symbol("token", SymbolKind::Variable, "routes/admin.ts", 11);
        // Class with prefix match
        let class = test_symbol("TokenError", SymbolKind::Class, "auth/tokens.ts", 14);
        // Function with substring match
        let func = test_symbol("validateToken", SymbolKind::Function, "auth/tokens.ts", 59);
        // Class with substring match
        let subclass = test_symbol("ExpiredTokenError", SymbolKind::Class, "auth/tokens.ts", 22);
        db.insert_symbols(&[var1, var2, class, func, subclass])
            .unwrap();

        let results = db.search("token", None, None, 20).unwrap();
        assert_eq!(results.len(), 5);
        // Definitions (class, function) should all rank above variables
        let def_names: Vec<&str> = results[..3].iter().map(|s| s.name.as_str()).collect();
        assert!(def_names.contains(&"TokenError"));
        assert!(def_names.contains(&"validateToken"));
        assert!(def_names.contains(&"ExpiredTokenError"));
        // Variables should be last
        assert_eq!(results[3].name, "token");
        assert_eq!(results[4].name, "token");
    }

    #[test]
    fn test_search_prefix_match() {
        let db = Database::open_memory().unwrap();
        let a = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
        let b = test_symbol("parse_args", SymbolKind::Function, "a.py", 10);
        let c = test_symbol("unrelated", SymbolKind::Function, "a.py", 20);
        db.insert_symbols(&[a, b, c]).unwrap();

        let results = db.search("parse", None, None, 20).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"parse_config"));
        assert!(names.contains(&"parse_args"));
    }

    #[test]
    fn test_search_substring_match() {
        let db = Database::open_memory().unwrap();
        let a = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
        let b = test_symbol("get_config", SymbolKind::Function, "a.py", 10);
        let c = test_symbol("unrelated", SymbolKind::Function, "a.py", 20);
        db.insert_symbols(&[a, b, c]).unwrap();

        let results = db.search("config", None, None, 20).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"parse_config"));
        assert!(names.contains(&"get_config"));
    }

    #[test]
    fn test_search_case_insensitive() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym).unwrap();

        let results = db.search("Parse", None, None, 20).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "parse_config");
    }

    #[test]
    fn test_search_kind_filter() {
        let db = Database::open_memory().unwrap();
        let func = test_symbol("parse_config", SymbolKind::Function, "a.py", 1);
        let class = test_symbol("parse_result", SymbolKind::Class, "a.py", 10);
        db.insert_symbols(&[func, class]).unwrap();

        let results = db
            .search("parse", Some(SymbolKind::Function), None, 20)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].kind, SymbolKind::Function);
    }

    #[test]
    fn test_search_file_filter() {
        let db = Database::open_memory().unwrap();
        let a = test_symbol("parse_config", SymbolKind::Function, "src/a.rs", 1);
        let b = test_symbol("parse_config", SymbolKind::Function, "src/b.rs", 1);
        db.insert_symbols(&[a, b]).unwrap();

        let results = db.search("parse", None, Some("src/a.rs"), 20).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "src/a.rs");
    }

    #[test]
    fn test_search_empty_query_returns_error() {
        let db = Database::open_memory().unwrap();
        let err = db.search("", None, None, 20).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_search_zero_limit_returns_error() {
        let db = Database::open_memory().unwrap();
        let err = db.search("parse", None, None, 0).unwrap_err();
        assert!(err.to_string().contains("at least 1"));
    }

    #[test]
    fn test_search_limit_caps_results() {
        let db = Database::open_memory().unwrap();
        // Insert 5 symbols all matching "fn"
        for i in 0..5u32 {
            let sym = test_symbol(&format!("fn_{i}"), SymbolKind::Function, "a.py", i * 10 + 1);
            db.insert_symbol(&sym).unwrap();
        }
        let results = db.search("fn", None, None, 3).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_limit_one_returns_top_ranked() {
        let db = Database::open_memory().unwrap();
        let exact = test_symbol("resolve", SymbolKind::Function, "a.py", 1);
        let prefix = test_symbol("resolve_edges", SymbolKind::Function, "a.py", 10);
        db.insert_symbols(&[exact, prefix]).unwrap();

        let results = db.search("resolve", None, None, 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "resolve");
    }

    #[test]
    fn test_search_wildcard_chars_treated_as_literals() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("get_foo", SymbolKind::Function, "a.py", 1);
        let unrelated = test_symbol("getXfoo", SymbolKind::Function, "a.py", 10);
        db.insert_symbols(&[sym, unrelated]).unwrap();

        // "get_foo" with literal underscore should NOT match "getXfoo"
        let results = db.search("get_foo", None, None, 20).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "get_foo");
    }

    #[test]
    fn test_search_percent_treated_as_literal() {
        let db = Database::open_memory().unwrap();
        // No symbol contains a literal %, so searching for "%" should return empty
        let sym = test_symbol("get_config", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym).unwrap();

        let results = db.search("%", None, None, 20).unwrap();
        assert!(results.is_empty(), "% should not act as a wildcard");
    }

    // ── RAG: Symbol Content Tests ──

    #[test]
    fn test_upsert_and_get_symbol_content() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("my_func", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym).unwrap();

        db.upsert_symbol_content(
            &sym.id,
            "my_func",
            "def my_func(): pass",
            "// File: a.py\n// Type: function\n// Name: my_func",
        )
        .unwrap();

        let result = db.get_symbol_content(&sym.id).unwrap();
        assert!(result.is_some());
        let (content, header) = result.unwrap();
        assert_eq!(content, "def my_func(): pass");
        assert!(header.contains("my_func"));
    }

    #[test]
    fn test_insert_symbol_contents_batch() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
        db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

        let items = vec![
            (
                sym1.id.clone(),
                "foo".to_string(),
                "def foo(): pass".to_string(),
                "header1".to_string(),
            ),
            (
                sym2.id.clone(),
                "bar".to_string(),
                "def bar(): pass".to_string(),
                "header2".to_string(),
            ),
        ];
        db.insert_symbol_contents(&items).unwrap();

        assert_eq!(db.symbol_content_count().unwrap(), 2);
        assert!(db.get_symbol_content(&sym1.id).unwrap().is_some());
        assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
    }

    #[test]
    fn test_clear_symbol_content_for_file() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

        db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
            .unwrap();
        db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
            .unwrap();
        assert_eq!(db.symbol_content_count().unwrap(), 2);

        db.clear_symbol_content_for_file("a.py").unwrap();
        assert_eq!(db.symbol_content_count().unwrap(), 1);
        assert!(db.get_symbol_content(&sym1.id).unwrap().is_none());
        assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
    }

    // ── RAG: FTS5 Tests ──

    #[test]
    fn test_fts5_search_by_content() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("validate_token", SymbolKind::Function, "auth.py", 1);
        db.insert_symbol(&sym).unwrap();

        db.upsert_symbol_content(
            &sym.id,
            "validate_token",
            "def validate_token(token: str) -> bool:\n    return token.is_valid()",
            "// File: auth.py",
        )
        .unwrap();

        // Search by content keyword
        let results = db.fts5_search("\"validate\"", 10).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0], sym.id);
    }

    #[test]
    fn test_fts5_search_no_match() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, "foo", "def foo(): pass", "header")
            .unwrap();

        let results = db.fts5_search("\"nonexistent_term_xyz\"", 10).unwrap();
        assert!(results.is_empty());
    }

    // ── RAG: Embedding Map Tests ──

    #[test]
    fn test_get_or_create_embedding_id() {
        let db = Database::open_memory().unwrap();

        let id1 = db.get_or_create_embedding_id("a.py:foo:1").unwrap();
        let id2 = db.get_or_create_embedding_id("a.py:foo:1").unwrap();
        let id3 = db.get_or_create_embedding_id("b.py:bar:5").unwrap();

        assert_eq!(id1, id2, "same symbol should return same ID");
        assert_ne!(id1, id3, "different symbols should get different IDs");
    }

    #[test]
    fn test_symbol_id_for_embedding() {
        let db = Database::open_memory().unwrap();
        let eid = db.get_or_create_embedding_id("test:sym:1").unwrap();

        let sym_id = db.symbol_id_for_embedding(eid).unwrap();
        assert_eq!(sym_id, Some("test:sym:1".to_string()));

        let none = db.symbol_id_for_embedding(99999).unwrap();
        assert!(none.is_none());
    }

    #[test]
    fn test_symbol_ids_for_embeddings_batch() {
        let db = Database::open_memory().unwrap();
        let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
        let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

        let results = db.symbol_ids_for_embeddings(&[eid1, eid2]).unwrap();
        assert_eq!(results.len(), 2);
    }

    // ── RAG: Vector Storage Tests ──

    #[test]
    fn test_upsert_and_search_embedding() {
        let db = Database::open_memory().unwrap();
        let eid = db.get_or_create_embedding_id("a:foo:1").unwrap();

        // Create a simple 384-dim vector
        let mut embedding = vec![0.0f32; 384];
        embedding[0] = 1.0;
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();

        db.upsert_embedding(eid, &bytes).unwrap();

        // Search with a similar vector
        let query = bytes.clone();
        let results = db.vector_search(&query, 5).unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, eid);
        assert!(
            results[0].1 < 0.01,
            "self-match should have near-zero distance"
        );
    }

    #[test]
    fn test_insert_embeddings_batch() {
        let db = Database::open_memory().unwrap();
        let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
        let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

        let make_vec = |val: f32| -> Vec<u8> {
            let v = vec![val; 384];
            v.iter().flat_map(|f| f.to_le_bytes()).collect()
        };

        let items = vec![(eid1, make_vec(0.1)), (eid2, make_vec(0.9))];
        db.insert_embeddings(&items).unwrap();

        assert_eq!(db.embedding_count().unwrap(), 2);
    }

    #[test]
    fn test_has_embedding() {
        let db = Database::open_memory().unwrap();
        assert!(!db.has_embedding("nonexistent").unwrap());

        let eid = db.get_or_create_embedding_id("a:foo:1").unwrap();
        // Map exists but no vector yet
        assert!(!db.has_embedding("a:foo:1").unwrap());

        // Insert vector
        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        db.upsert_embedding(eid, &bytes).unwrap();
        assert!(db.has_embedding("a:foo:1").unwrap());
    }

    #[test]
    fn test_clear_all_embeddings() {
        let db = Database::open_memory().unwrap();
        let eid1 = db.get_or_create_embedding_id("a:foo:1").unwrap();
        let eid2 = db.get_or_create_embedding_id("b:bar:2").unwrap();

        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        db.upsert_embedding(eid1, &bytes).unwrap();
        db.upsert_embedding(eid2, &bytes).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 2);

        db.clear_all_embeddings().unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
    }

    #[test]
    fn test_symbols_needing_embeddings() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
        db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

        // Add content for both
        db.upsert_symbol_content(&sym1.id, "foo", "def foo(): pass", "header")
            .unwrap();
        db.upsert_symbol_content(&sym2.id, "bar", "def bar(): pass", "header")
            .unwrap();

        // Both need embeddings initially
        let needing = db.symbols_needing_embeddings().unwrap();
        assert_eq!(needing.len(), 2);

        // Embed one
        let eid = db.get_or_create_embedding_id(&sym1.id).unwrap();
        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        db.upsert_embedding(eid, &bytes).unwrap();

        // Only one needs embedding now
        let needing = db.symbols_needing_embeddings().unwrap();
        assert_eq!(needing.len(), 1);
        assert_eq!(needing[0], sym2.id);
    }

    #[test]
    fn test_clear_rag_data_for_file() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

        db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
            .unwrap();
        db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
            .unwrap();

        let eid1 = db.get_or_create_embedding_id(&sym1.id).unwrap();
        let eid2 = db.get_or_create_embedding_id(&sym2.id).unwrap();
        let bytes: Vec<u8> = vec![0.0f32; 384]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        db.upsert_embedding(eid1, &bytes).unwrap();
        db.upsert_embedding(eid2, &bytes).unwrap();

        // Clear RAG data for a.py only
        db.clear_rag_data_for_file("a.py").unwrap();

        // a.py data gone
        assert!(db.get_symbol_content(&sym1.id).unwrap().is_none());
        assert!(!db.has_embedding(&sym1.id).unwrap());

        // b.py data intact
        assert!(db.get_symbol_content(&sym2.id).unwrap().is_some());
        assert!(db.has_embedding(&sym2.id).unwrap());
    }

    #[test]
    fn test_all_content_symbol_ids() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[sym1.clone(), sym2.clone()]).unwrap();

        db.upsert_symbol_content(&sym1.id, "foo", "content1", "header1")
            .unwrap();
        db.upsert_symbol_content(&sym2.id, "bar", "content2", "header2")
            .unwrap();

        let all = db.all_content_symbol_ids().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_symbols_needing_embeddings_excludes_variables() {
        let db = Database::open_memory().unwrap();
        let func = test_symbol("process", SymbolKind::Function, "a.py", 1);
        let var = test_symbol("MAX_RETRIES", SymbolKind::Variable, "a.py", 10);
        let cls = test_symbol("Service", SymbolKind::Class, "a.py", 20);
        db.insert_symbols(&[func.clone(), var.clone(), cls.clone()])
            .unwrap();

        // Add content for all three
        db.upsert_symbol_content(&func.id, "process", "def process(): pass", "header")
            .unwrap();
        db.upsert_symbol_content(&var.id, "MAX_RETRIES", "MAX_RETRIES = 3", "header")
            .unwrap();
        db.upsert_symbol_content(&cls.id, "Service", "class Service: pass", "header")
            .unwrap();

        // Only function and class should need embeddings (variable excluded)
        let needing = db.symbols_needing_embeddings().unwrap();
        assert_eq!(needing.len(), 2);
        assert!(!needing.contains(&var.id), "variables should be excluded");
        assert!(needing.contains(&func.id));
        assert!(needing.contains(&cls.id));
    }

    #[test]
    fn test_all_content_symbol_ids_excludes_variables() {
        let db = Database::open_memory().unwrap();
        let func = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let var = test_symbol("MY_VAR", SymbolKind::Variable, "a.py", 10);
        let method = test_symbol("bar", SymbolKind::Method, "a.py", 20);
        db.insert_symbols(&[func.clone(), var.clone(), method.clone()])
            .unwrap();

        db.upsert_symbol_content(&func.id, "foo", "def foo(): pass", "header")
            .unwrap();
        db.upsert_symbol_content(&var.id, "MY_VAR", "MY_VAR = 42", "header")
            .unwrap();
        db.upsert_symbol_content(&method.id, "bar", "def bar(self): pass", "header")
            .unwrap();

        let all = db.all_content_symbol_ids().unwrap();
        assert_eq!(all.len(), 2, "variables should be excluded");
        assert!(!all.contains(&var.id));
    }

    #[test]
    fn test_get_symbol_contents_batch() {
        let db = Database::open_memory().unwrap();
        let sym1 = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let sym2 = test_symbol("bar", SymbolKind::Function, "a.py", 10);
        let sym3 = test_symbol("baz", SymbolKind::Function, "a.py", 20);
        db.insert_symbols(&[sym1.clone(), sym2.clone(), sym3.clone()])
            .unwrap();

        db.upsert_symbol_content(&sym1.id, "foo", "def foo(): pass", "h1")
            .unwrap();
        db.upsert_symbol_content(&sym2.id, "bar", "def bar(): pass", "h2")
            .unwrap();
        // sym3 has no content

        let ids = vec![sym1.id.clone(), sym2.id.clone(), sym3.id.clone()];
        let map = db.get_symbol_contents_batch(&ids).unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key(&sym1.id));
        assert!(map.contains_key(&sym2.id));
        assert!(!map.contains_key(&sym3.id));
        assert_eq!(map[&sym1.id].0, "def foo(): pass");
    }

    #[test]
    fn test_get_symbol_contents_batch_empty() {
        let db = Database::open_memory().unwrap();
        let map = db.get_symbol_contents_batch(&[]).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    fn test_get_symbol_by_id() {
        let db = Database::open_memory().unwrap();
        let sym = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym).unwrap();

        let found = db.get_symbol(&sym.id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "foo");

        let not_found = db.get_symbol("nonexistent").unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_symbols_for_files_basic() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
        let s2 = test_symbol("func_b", SymbolKind::Function, "src/a.py", 10);
        let s3 = test_symbol("ClassC", SymbolKind::Class, "src/b.py", 1);
        let s4 = test_symbol("func_d", SymbolKind::Function, "src/c.py", 1);
        db.insert_symbols(&[s1, s2, s3, s4]).unwrap();

        // Query for two files
        let files = vec!["src/a.py".to_string(), "src/b.py".to_string()];
        let results = db.symbols_for_files(&files, None).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file_path, "src/a.py");
        assert_eq!(results[2].file_path, "src/b.py");
    }

    #[test]
    fn test_symbols_for_files_kind_filter() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
        let s2 = test_symbol("ClassB", SymbolKind::Class, "src/a.py", 10);
        db.insert_symbols(&[s1, s2]).unwrap();

        let files = vec!["src/a.py".to_string()];
        let results = db
            .symbols_for_files(&files, Some(SymbolKind::Function))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "func_a");
    }

    #[test]
    fn test_symbols_for_files_empty_input() {
        let db = Database::open_memory().unwrap();
        let results = db.symbols_for_files(&[], None).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_symbols_for_files_no_matching_files() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("func_a", SymbolKind::Function, "src/a.py", 1);
        db.insert_symbol(&s1).unwrap();

        let files = vec!["src/nonexistent.py".to_string()];
        let results = db.symbols_for_files(&files, None).unwrap();
        assert!(results.is_empty());
    }

    // ── In-degree centrality tests ──

    #[test]
    fn test_compute_in_degrees() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("func_a", SymbolKind::Function, "a.py", 1);
        let s2 = test_symbol("func_b", SymbolKind::Function, "b.py", 1);
        let s3 = test_symbol("func_c", SymbolKind::Function, "c.py", 1);
        db.insert_symbols(&[s1.clone(), s2.clone(), s3.clone()])
            .unwrap();

        // func_b calls func_a (2 call sites), func_c calls func_a (1 call site)
        let e1 = Edge::new(&s2.id, "func_a", EdgeKind::Calls, "b.py", 5);
        let e2 = Edge::new(&s2.id, "func_a", EdgeKind::Calls, "b.py", 10);
        let e3 = Edge::new(&s3.id, "func_a", EdgeKind::Calls, "c.py", 3);
        // func_c also calls func_b
        let e4 = Edge::new(&s3.id, "func_b", EdgeKind::Calls, "c.py", 7);
        db.insert_edges(&[e1, e2, e3, e4]).unwrap();
        db.resolve_edges().unwrap();
        db.compute_in_degrees().unwrap();

        let sym_a = db.get_symbol(&s1.id).unwrap().unwrap();
        let sym_b = db.get_symbol(&s2.id).unwrap().unwrap();
        let sym_c = db.get_symbol(&s3.id).unwrap().unwrap();

        assert_eq!(sym_a.in_degree, 3, "func_a should have 3 incoming edges");
        assert_eq!(sym_b.in_degree, 1, "func_b should have 1 incoming edge");
        assert_eq!(sym_c.in_degree, 0, "func_c should have 0 incoming edges");
    }

    #[test]
    fn test_compute_in_degrees_resets() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("func_a", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&s1).unwrap();

        // Manually set in_degree to 99
        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 99 WHERE id = ?1",
                params![s1.id],
            )
            .unwrap();

        // compute_in_degrees should reset to 0 (no edges)
        db.compute_in_degrees().unwrap();
        let sym = db.get_symbol(&s1.id).unwrap().unwrap();
        assert_eq!(sym.in_degree, 0);
    }

    #[test]
    fn test_top_symbols_ordered_by_centrality() {
        let db = Database::open_memory().unwrap();
        let s1 = test_symbol("hub", SymbolKind::Function, "a.py", 1);
        let s2 = test_symbol("leaf", SymbolKind::Function, "b.py", 1);
        let s3 = test_symbol("mid", SymbolKind::Function, "c.py", 1);
        db.insert_symbols(&[s1.clone(), s2.clone(), s3.clone()])
            .unwrap();

        // Set in-degrees directly for testing
        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 10 WHERE id = ?1",
                params![s1.id],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 1 WHERE id = ?1",
                params![s2.id],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 5 WHERE id = ?1",
                params![s3.id],
            )
            .unwrap();

        let top = db.top_symbols(10).unwrap();
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].name, "hub");
        assert_eq!(top[0].in_degree, 10);
        assert_eq!(top[1].name, "mid");
        assert_eq!(top[2].name, "leaf");
    }

    #[test]
    fn test_search_uses_in_degree_tiebreaker() {
        let db = Database::open_memory().unwrap();
        // Two functions with same name prefix, different centrality
        let s1 = test_symbol("parse_request", SymbolKind::Function, "a.py", 1);
        let s2 = test_symbol("parse_response", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[s1.clone(), s2.clone()]).unwrap();

        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 20 WHERE id = ?1",
                params![s1.id],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE symbols SET in_degree = 5 WHERE id = ?1",
                params![s2.id],
            )
            .unwrap();

        let results = db.search("parse", None, None, 10).unwrap();
        assert_eq!(results.len(), 2);
        // parse_request (in_degree=20) should come before parse_response (in_degree=5)
        assert_eq!(results[0].name, "parse_request");
        assert_eq!(results[1].name, "parse_response");
    }

    #[test]
    fn test_schema_version_stored() {
        let db = Database::open_memory().unwrap();
        let version = db.get_metadata("schema_version").unwrap();
        assert!(version.is_some());
        assert_eq!(version.unwrap(), SCHEMA_VERSION.to_string());
    }

    // ── Scoped edge resolution tests ──

    #[test]
    fn test_invalidate_dangling_edges_after_symbol_removal() {
        let db = Database::open_memory().unwrap();

        // File A: defines foo
        let sym_a = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym_a).unwrap();

        // File B: calls foo (edge from B to A)
        let sym_b = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        db.insert_symbol(&sym_b).unwrap();
        let edge = Edge::new(&sym_b.id, "foo", EdgeKind::Calls, "b.py", 5);
        db.insert_edge(&edge).unwrap();

        // Resolve: edge should point to sym_a
        let resolved = db.resolve_edges().unwrap();
        assert_eq!(resolved, 1);

        // Simulate: directly delete the symbol row (bypassing delete_symbol cascade)
        // to create a dangling edge reference
        db.conn
            .execute("DELETE FROM symbols WHERE id = ?1", params![sym_a.id])
            .unwrap();

        // Invalidate dangling edges
        let dirty = std::collections::HashSet::from(["a.py".to_string()]);
        let invalidated = db.invalidate_edges_targeting(&dirty).unwrap();
        assert_eq!(invalidated, 1);

        // Edge should now be unresolved
        let edges = db.callees("bar").unwrap();
        assert!(
            edges.iter().all(|e| e.target_id.is_none()),
            "edge should be unresolved after invalidation"
        );
    }

    #[test]
    fn test_scoped_resolution_after_symbol_changes() {
        let db = Database::open_memory().unwrap();

        // File A: defines foo
        let sym_a = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        db.insert_symbol(&sym_a).unwrap();

        // File B: calls foo
        let sym_b = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        db.insert_symbol(&sym_b).unwrap();
        db.insert_edge(&Edge::new(&sym_b.id, "foo", EdgeKind::Calls, "b.py", 5))
            .unwrap();

        // Resolve globally first
        db.resolve_edges().unwrap();

        // Simulate re-indexing a.py: delete_symbol nullifies edges, then re-insert
        db.delete_symbol(&sym_a.id).unwrap();
        db.insert_symbol(&sym_a).unwrap();

        // Scoped resolve should re-resolve the edge
        let dirty = std::collections::HashSet::from(["a.py".to_string()]);
        let re_resolved = db.resolve_edges_scoped(&dirty).unwrap();
        assert_eq!(re_resolved, 1);
    }

    #[test]
    fn test_compute_in_degrees_scoped() {
        let db = Database::open_memory().unwrap();

        let foo = test_symbol("foo", SymbolKind::Function, "a.py", 1);
        let bar = test_symbol("bar", SymbolKind::Function, "b.py", 1);
        let baz = test_symbol("baz", SymbolKind::Function, "c.py", 1);
        db.insert_symbol(&foo).unwrap();
        db.insert_symbol(&bar).unwrap();
        db.insert_symbol(&baz).unwrap();

        // bar calls foo, baz calls foo
        db.insert_edge(&Edge::new(&bar.id, "foo", EdgeKind::Calls, "b.py", 5))
            .unwrap();
        db.insert_edge(&Edge::new(&baz.id, "foo", EdgeKind::Calls, "c.py", 3))
            .unwrap();

        db.resolve_edges().unwrap();
        db.compute_in_degrees().unwrap();

        // foo should have in_degree = 2
        let results = db.search("foo", None, None, 10).unwrap();
        assert_eq!(results[0].in_degree, 2);

        // Now scope to just b.py
        let dirty = std::collections::HashSet::from(["b.py".to_string()]);
        db.compute_in_degrees_scoped(&dirty).unwrap();

        // foo should still have in_degree = 2 (recomputed correctly)
        let results = db.search("foo", None, None, 10).unwrap();
        assert_eq!(results[0].in_degree, 2);
    }

    // ── Embedding dimension migration tests ──

    #[test]
    fn test_open_stores_embedding_dimension() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        let db = Database::open(&db_path, 384).unwrap();
        let stored: String = db
            .get_metadata("embedding_dimension")
            .unwrap()
            .expect("dimension should be stored");
        assert_eq!(stored, "384");
    }

    #[test]
    fn test_open_with_different_dimension_clears_embeddings() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // First open with 384-dim
        {
            let db = Database::open(&db_path, 384).unwrap();
            let sym = Symbol::new("foo", SymbolKind::Function, "a.py", 1, 10, 0, 100, None);
            db.insert_symbol(&sym).unwrap();
            db.upsert_symbol_content(&sym.id, "foo", "def foo():", "header")
                .unwrap();
            let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
            let bytes = vec![0u8; 384 * 4];
            db.insert_embeddings(&[(eid, bytes)]).unwrap();
            assert_eq!(db.embedding_count().unwrap(), 1);
        }

        // Reopen with 768-dim — should auto-wipe embeddings
        {
            let db = Database::open(&db_path, 768).unwrap();
            assert_eq!(db.embedding_count().unwrap(), 0);
            let stored: String = db
                .get_metadata("embedding_dimension")
                .unwrap()
                .expect("dimension should be updated");
            assert_eq!(stored, "768");
        }
    }

    #[test]
    fn test_open_same_dimension_preserves_embeddings() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // First open
        {
            let db = Database::open(&db_path, 384).unwrap();
            let sym = Symbol::new("bar", SymbolKind::Function, "b.py", 1, 10, 0, 100, None);
            db.insert_symbol(&sym).unwrap();
            db.upsert_symbol_content(&sym.id, "bar", "def bar():", "header")
                .unwrap();
            let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
            let bytes = vec![0u8; 384 * 4];
            db.insert_embeddings(&[(eid, bytes)]).unwrap();
        }

        // Reopen with same dimension — embeddings preserved
        {
            let db = Database::open(&db_path, 384).unwrap();
            assert_eq!(db.embedding_count().unwrap(), 1);
        }
    }

    #[test]
    fn test_default_dim_preserves_stored_non_default() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // First open with non-default dimension (e.g. Ollama auto-detected 768)
        {
            let db = Database::open(&db_path, 768).unwrap();
            let sym = Symbol::new("baz", SymbolKind::Function, "c.py", 1, 10, 0, 100, None);
            db.insert_symbol(&sym).unwrap();
            db.upsert_symbol_content(&sym.id, "baz", "def baz():", "header")
                .unwrap();
            let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
            let bytes = vec![0u8; 768 * 4];
            db.insert_embeddings(&[(eid, bytes)]).unwrap();
        }

        // Reopen with DEFAULT_EMBEDDING_DIM (384) — must preserve 768-dim embeddings
        {
            let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
            assert_eq!(db.embedding_count().unwrap(), 1);
            let stored: i64 = db
                .conn
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(stored, 768);
        }
    }

    #[test]
    fn test_explicit_non_default_dim_wipes_different_stored() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // First open with 768
        {
            let db = Database::open(&db_path, 768).unwrap();
            let sym = Symbol::new("qux", SymbolKind::Function, "d.py", 1, 10, 0, 100, None);
            db.insert_symbol(&sym).unwrap();
            db.upsert_symbol_content(&sym.id, "qux", "def qux():", "header")
                .unwrap();
            let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
            let bytes = vec![0u8; 768 * 4];
            db.insert_embeddings(&[(eid, bytes)]).unwrap();
        }

        // Reopen with explicit 1536 — this IS a real dimension change, must wipe
        {
            let db = Database::open(&db_path, 1536).unwrap();
            assert_eq!(db.embedding_count().unwrap(), 0);
        }
    }

    #[test]
    fn test_reopen_same_dim_does_not_rewrite_metadata() {
        // True early-return guarantee: when stored dim already matches the
        // requested dim, `handle_embedding_dimension` should not touch the
        // metadata table. We assert this by snapshotting the row's content
        // before and after re-open and verifying no write occurred (rowid
        // would advance on INSERT OR REPLACE).
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        let _db = Database::open(&db_path, 384).unwrap();

        let rowid_before: i64 = {
            let conn = Connection::open(&db_path).unwrap();
            conn.query_row(
                "SELECT rowid FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        let _db = Database::open(&db_path, 384).unwrap();

        let rowid_after: i64 = {
            let conn = Connection::open(&db_path).unwrap();
            conn.query_row(
                "SELECT rowid FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        // INSERT OR REPLACE assigns a new rowid; identity here proves we
        // skipped the write entirely.
        assert_eq!(
            rowid_before, rowid_after,
            "same-dim reopen should not rewrite the embedding_dimension row"
        );
    }

    #[test]
    fn test_retry_busy_returns_on_non_busy_error() {
        // A non-busy error should propagate immediately, no retries.
        let attempts = std::cell::Cell::new(0);
        let result = retry_busy(|| -> std::result::Result<(), rusqlite::Error> {
            attempts.set(attempts.get() + 1);
            Err(rusqlite::Error::InvalidQuery)
        });
        assert!(matches!(result, Err(rusqlite::Error::InvalidQuery)));
        assert_eq!(attempts.get(), 1, "non-busy errors must not retry");
    }

    #[test]
    fn test_retry_busy_succeeds_after_transient_busy() {
        // Simulate a writer that returns BUSY on the first call and Ok on the second.
        let attempts = std::cell::Cell::new(0);
        let result = retry_busy(|| -> std::result::Result<u32, rusqlite::Error> {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                Err(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error {
                        code: rusqlite::ErrorCode::DatabaseBusy,
                        extended_code: 5,
                    },
                    Some("database is locked".to_string()),
                ))
            } else {
                Ok(42)
            }
        });
        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn test_retry_busy_exhausts_and_propagates() {
        // After backoff schedule is exhausted, the original BUSY error must surface.
        let attempts = std::cell::Cell::new(0);
        let result = retry_busy(|| -> std::result::Result<(), rusqlite::Error> {
            attempts.set(attempts.get() + 1);
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::DatabaseBusy,
                    extended_code: 5,
                },
                Some("database is locked".to_string()),
            ))
        });
        assert!(matches!(
            result,
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error {
                    code: rusqlite::ErrorCode::DatabaseBusy,
                    ..
                },
                _
            ))
        ));
        // 1 initial call + MIGRATION_RETRY_BACKOFF_MS.len() retries
        assert_eq!(attempts.get(), MIGRATION_RETRY_BACKOFF_MS.len() + 1);
    }

    // ── Embedding fingerprint tests (Phase 6b) ──

    fn fp(provider: &str, model: &str, dim: usize) -> EmbeddingFingerprint {
        EmbeddingFingerprint {
            provider: provider.to_string(),
            model: model.to_string(),
            dimension: dim,
        }
    }

    fn seed_embedding(db: &Database, dim: usize, sym_name: &str) {
        let sym = Symbol::new(sym_name, SymbolKind::Function, "f.py", 1, 10, 0, 100, None);
        db.insert_symbol(&sym).unwrap();
        db.upsert_symbol_content(&sym.id, sym_name, "def f():", "header")
            .unwrap();
        let eid = db.get_or_create_embedding_id(&sym.id).unwrap();
        let bytes = vec![0u8; dim * 4];
        db.insert_embeddings(&[(eid, bytes)]).unwrap();
    }

    #[test]
    fn test_fingerprint_match_is_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        let f = fp("local", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f).unwrap();
        seed_embedding(&db, 384, "foo");
        // Reconciling identical fingerprint must preserve embeddings.
        db.reconcile_embedding_fingerprint(&f).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 1);
    }

    #[test]
    fn test_fingerprint_provider_swap_wipes() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        let f1 = fp("local", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f1).unwrap();
        seed_embedding(&db, 384, "bar");
        assert_eq!(db.embedding_count().unwrap(), 1);

        // Same dim + model name, different provider class → wipe.
        let f2 = fp("ollama", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f2).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
        assert_eq!(
            db.get_metadata("embedding_provider").unwrap().as_deref(),
            Some("ollama")
        );
    }

    #[test]
    fn test_fingerprint_model_swap_wipes() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        let f1 = fp("local", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f1).unwrap();
        seed_embedding(&db, 384, "baz");
        assert_eq!(db.embedding_count().unwrap(), 1);

        // Same provider + dim, different model → still a swap, must wipe.
        let f2 = fp("local", "AllMiniLML6V2", 384);
        db.reconcile_embedding_fingerprint(&f2).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
        assert_eq!(
            db.get_metadata("embedding_model").unwrap().as_deref(),
            Some("AllMiniLML6V2")
        );
    }

    #[test]
    fn test_fingerprint_backfill_does_not_wipe() {
        // Simulate an older cartog DB: dimension recorded, provider/model not yet.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        seed_embedding(&db, 384, "qux");
        assert!(db.get_metadata("embedding_provider").unwrap().is_none());
        assert_eq!(db.embedding_count().unwrap(), 1);

        // First reconcile after upgrade: backfill provider/model without wiping.
        let f = fp("local", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f).unwrap();
        assert_eq!(
            db.embedding_count().unwrap(),
            1,
            "backfill must preserve existing embeddings"
        );
        assert_eq!(
            db.get_metadata("embedding_provider").unwrap().as_deref(),
            Some("local")
        );
        assert_eq!(
            db.get_metadata("embedding_model").unwrap().as_deref(),
            Some("BGE-small-en-v1.5")
        );
    }

    #[test]
    fn test_fingerprint_dim_change_wipes() {
        // A real dimension change must wipe even if provider/model also change.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        let f1 = fp("local", "BGE-small-en-v1.5", 384);
        db.reconcile_embedding_fingerprint(&f1).unwrap();
        seed_embedding(&db, 384, "quux");
        assert_eq!(db.embedding_count().unwrap(), 1);

        let f2 = fp("local", "BGELargeENV15", 1024);
        db.reconcile_embedding_fingerprint(&f2).unwrap();
        assert_eq!(db.embedding_count().unwrap(), 0);
        let stored_dim: i64 = db
            .conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM metadata WHERE key = 'embedding_dimension'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_dim, 1024);
        // A successful wipe must also recreate symbol_vec at the new dim.
        // Without this assertion, an early return between the DROP and the
        // CREATE in reconcile_embedding_fingerprint would pass the count +
        // metadata checks above while leaving the DB unusable for RAG.
        assert!(
            symbol_vec_exists(&db.conn).unwrap(),
            "successful reconcile must recreate symbol_vec"
        );
    }

    // ── Read-only attach tests (Phase 3) ──

    #[test]
    fn test_open_readonly_succeeds_and_marks_read_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // Primary creates and writes a fingerprint.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.reconcile_embedding_fingerprint(&fp("local", "BGE-small-en-v1.5", 384))
                .unwrap();
            seed_embedding(&db, 384, "foo");
        }

        // Reader attaches read-only.
        let reader = Database::open_readonly(&db_path).unwrap();
        assert!(reader.is_read_only(), "open_readonly must set the flag");
        let pinned = reader.pinned_attach().expect("read-only attach pins state");
        assert_eq!(pinned.schema_version, SCHEMA_VERSION);
        assert_eq!(
            pinned.embedding,
            Some(fp("local", "BGE-small-en-v1.5", 384))
        );
    }

    #[test]
    fn test_open_readonly_can_query_existing_data() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        {
            let db = Database::open(&db_path, 384).unwrap();
            let sym = Symbol::new(
                "callable",
                SymbolKind::Function,
                "a.py",
                1,
                10,
                0,
                100,
                None,
            );
            db.insert_symbol(&sym).unwrap();
        }

        let reader = Database::open_readonly(&db_path).unwrap();
        let count: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "reader sees primary's data");
    }

    #[test]
    fn test_open_readonly_refuses_writes() {
        // SQLITE_OPEN_READ_ONLY must turn any INSERT into SQLITE_READONLY at
        // runtime — defense-in-depth for the higher-level tool gating in
        // Phase 4.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let _db = Database::open(&db_path, 384).unwrap();
        }

        let reader = Database::open_readonly(&db_path).unwrap();
        let err = reader
            .conn
            .execute(
                "INSERT OR REPLACE INTO metadata (key, value) VALUES ('x', 'y')",
                [],
            )
            .unwrap_err();
        // The specific code is SQLITE_READONLY (8); rusqlite surfaces it as
        // SqliteFailure with the matching error code variant. We just check
        // that some error came back rather than match on the FFI integer.
        let msg = err.to_string();
        assert!(
            msg.contains("read") || msg.contains("readonly") || msg.contains("write"),
            "read-only DB write should fail with a read-only-flavored error, got: {msg}"
        );
    }

    #[test]
    fn test_open_readonly_detects_schema_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let db = Database::open(&db_path, 384).unwrap();
            // Simulate a future cartog: bump schema_version on disk.
            db.set_metadata("schema_version", "9999").unwrap();
        }

        let err = Database::open_readonly(&db_path).unwrap_err();
        match err {
            DbError::SchemaDrift { expected, stored } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(stored, 9999);
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn test_open_readonly_does_not_run_migrations() {
        // After open_readonly returns, no PRAGMAs or writes should have
        // landed beyond what was there before. We test the visible
        // consequence: an existing user-set metadata key is unchanged.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.set_metadata("user_marker", "untouched").unwrap();
        }
        let _reader = Database::open_readonly(&db_path).unwrap();
        // Re-open writable to verify the marker is still there and the
        // schema didn't get rewritten.
        let primary = Database::open(&db_path, 384).unwrap();
        assert_eq!(
            primary.get_metadata("user_marker").unwrap().as_deref(),
            Some("untouched")
        );
    }

    #[test]
    fn test_open_default_is_not_read_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        assert!(!db.is_read_only());
        assert!(db.pinned_attach().is_none());
    }

    // ── Promotion path: open_existing_rw (Phase 5) ──

    #[test]
    fn test_open_existing_rw_opens_writable_and_skips_migrations() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // Materialize with a user-set metadata marker we can re-read.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.set_metadata("marker", "preserved").unwrap();
        }

        let promoted = Database::open_existing_rw(&db_path).unwrap();
        assert!(!promoted.is_read_only(), "open_existing_rw is RW");
        assert!(promoted.pinned_attach().is_none(), "RW opens have no pin");
        // The marker survives (we didn't wipe anything).
        assert_eq!(
            promoted.get_metadata("marker").unwrap().as_deref(),
            Some("preserved")
        );
        // We can write — confirming it's a real RW handle.
        promoted.set_metadata("write_check", "ok").unwrap();
    }

    #[test]
    fn test_open_existing_rw_detects_schema_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.set_metadata("schema_version", "9999").unwrap();
        }
        let err = Database::open_existing_rw(&db_path).unwrap_err();
        match err {
            DbError::SchemaDrift { expected, stored } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(stored, 9999);
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn test_database_open_alone_does_not_change_fingerprint() {
        // Regression for the cartog rag search path: opening the DB (which
        // every CLI command does) must not touch the embedding fingerprint
        // unless reconcile_embedding_fingerprint is explicitly called.
        // Pre-fix, cmd_rag_search called reconcile on every invocation,
        // which could race a primary serve's writes if the user changed
        // provider in .cartog.toml since last index. After the fix,
        // cmd_rag_search opens RW but does NOT reconcile.
        //
        // This test asserts the invariant at the layer below: Database::open
        // does not, by itself, alter provider/model metadata. Combined with
        // the production code change (no reconcile call in cmd_rag_search),
        // a CLI search invocation cannot wipe symbol_vec.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let original_fp = fp("local", "BGE-small-en-v1.5", 384);
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.reconcile_embedding_fingerprint(&original_fp).unwrap();
            seed_embedding(&db, 384, "guard");
        }
        // Re-open as cmd_rag_search would (RW, no reconcile). Same dim,
        // so handle_embedding_dimension early-returns; nothing rewrites.
        {
            let _db = Database::open(&db_path, 384).unwrap();
        }
        // Fingerprint and embeddings intact.
        let db = Database::open(&db_path, 384).unwrap();
        assert_eq!(
            db.get_metadata("embedding_provider").unwrap().as_deref(),
            Some("local")
        );
        assert_eq!(
            db.get_metadata("embedding_model").unwrap().as_deref(),
            Some("BGE-small-en-v1.5")
        );
        assert_eq!(db.embedding_count().unwrap(), 1);
    }

    #[test]
    fn test_open_readonly_missing_schema_version_is_schema_drift() {
        // Regression: pre-fix, a metadata table without a schema_version
        // row surfaced as DbError::Sqlite(QueryReturnedNoRows) instead of
        // the actionable SchemaDrift. Callers (cartog serve) print
        // different messages for the two — drift is the right one ("the
        // primary upgraded cartog; restart this session"), the raw
        // rusqlite error is opaque.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // Create a DB with our schema, then delete the schema_version row.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.conn
                .execute("DELETE FROM metadata WHERE key = 'schema_version'", [])
                .unwrap();
        }
        let err = Database::open_readonly(&db_path).unwrap_err();
        match err {
            DbError::SchemaDrift { expected, stored } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(stored, 0, "missing row should surface as stored=0");
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn test_open_readonly_missing_metadata_table_is_schema_drift() {
        // Regression: a non-cartog SQLite file at the path (or a
        // partially-initialised DB where the `metadata` table is missing
        // entirely) used to surface as a raw rusqlite "no such table:
        // metadata" error instead of the actionable SchemaDrift. Fix:
        // read_schema_version catches that specific SqliteFailure and
        // returns stored=0.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        // Build a SQLite file that's NOT a cartog DB: empty schema.
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch("CREATE TABLE unrelated (x INTEGER);")
                .unwrap();
        }
        let err = Database::open_readonly(&db_path).unwrap_err();
        match err {
            DbError::SchemaDrift { expected, stored } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(stored, 0, "missing metadata table should be stored=0");
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn test_open_existing_rw_missing_schema_version_is_schema_drift() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.conn
                .execute("DELETE FROM metadata WHERE key = 'schema_version'", [])
                .unwrap();
        }
        let err = Database::open_existing_rw(&db_path).unwrap_err();
        match err {
            DbError::SchemaDrift { expected, stored } => {
                assert_eq!(expected, SCHEMA_VERSION);
                assert_eq!(stored, 0);
            }
            other => panic!("expected SchemaDrift, got {other:?}"),
        }
    }

    #[test]
    fn test_reconcile_rebuilds_when_metadata_matches_but_symbol_vec_missing() {
        // Defensive regression: if `symbol_vec` is missing for any reason
        // (external corruption, pre-C4 cartog that crashed mid-migration)
        // but metadata still claims the matching fingerprint, the fast-
        // path early return previously skipped the rebuild, leaving the
        // DB stuck. After the fix, the symbol_vec_exists() check forces
        // a rebuild.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let f = fp("local", "BGE-small-en-v1.5", 384);

        // 1. Establish a normal state.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.reconcile_embedding_fingerprint(&f).unwrap();
        }

        // 2. Drop the vector table out-of-band, simulating corruption.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.conn
                .execute("DROP TABLE IF EXISTS symbol_vec", [])
                .unwrap();
            // Metadata unchanged: still claims (local, BGE-small-en-v1.5, 384).
            assert_eq!(
                db.get_metadata("embedding_dimension").unwrap().as_deref(),
                Some("384")
            );
        }

        // 3. Re-reconcile with the same fingerprint. Pre-fix: early-return
        //    skipped rebuild → symbol_vec stayed missing forever. Post-fix:
        //    the symbol_vec_exists() check forces the rebuild.
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.reconcile_embedding_fingerprint(&f).unwrap();
            let exists: bool = db
                .conn
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE name='symbol_vec'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .unwrap()
                .is_some();
            assert!(
                exists,
                "reconcile must rebuild symbol_vec when missing, even on metadata match"
            );
        }
    }

    #[test]
    fn test_handle_embedding_dimension_rebuilds_when_symbol_vec_missing() {
        // Same defensive guarantee for the lower-level handle_embedding_dimension
        // fast-path. Open a DB, drop symbol_vec, re-open: the table must come
        // back even though stored_dim == requested_dim.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.conn
                .execute("DROP TABLE IF EXISTS symbol_vec", [])
                .unwrap();
        }
        let db = Database::open(&db_path, 384).unwrap();
        let exists: bool = db
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE name='symbol_vec'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_some();
        assert!(
            exists,
            "Database::open must rebuild symbol_vec when missing, even on metadata match"
        );
    }

    #[test]
    fn test_reconcile_fingerprint_rolls_back_on_midsequence_failure() {
        // Regression: pre-fix, each metadata write in
        // reconcile_embedding_fingerprint ran outside any transaction.
        // If the busy-retry on a later write exhausted (or any other
        // failure), the DB was left with partial state — e.g.
        // symbol_vec dropped, provider rewritten, dimension stale. The
        // next open would see (stored_dim != fp.dimension) → "wipe and
        // rebuild" but the embeddings would already be gone, and the
        // primary writer would silently keep operating against the
        // damaged DB.
        //
        // With the transaction wrapper, a mid-sequence failure rolls
        // back the entire reconcile. We exercise this by capping
        // max_page_count so a write in the middle of the sequence
        // fails with SQLITE_FULL.
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");

        // 1. Establish a known state with our own embedding rows.
        let initial_fp = fp("local", "BGE-small-en-v1.5", 384);
        {
            let db = Database::open(&db_path, 384).unwrap();
            db.reconcile_embedding_fingerprint(&initial_fp).unwrap();
            seed_embedding(&db, 384, "seed");
        }

        // 2. Force a deterministic mid-sequence failure via the
        //    RECONCILE_FAIL_AFTER_MODEL fault-injection hook (gated by
        //    #[cfg(test)]). Page-cap tricks don't reliably trigger
        //    SQLITE_FULL: SQLite reuses freed pages after DROP TABLE.
        let new_fp = fp("ollama", "nomic-embed-text-v2", 384);
        let outcome = {
            let db = Database::open(&db_path, 384).unwrap();
            RECONCILE_FAIL_AFTER_MODEL.with(|b| b.store(true, std::sync::atomic::Ordering::SeqCst));
            db.reconcile_embedding_fingerprint(&new_fp)
        };
        assert!(outcome.is_err(), "injected SQLITE_FULL must surface as Err");

        // 3. Failure path: the DB on disk must still reflect the INITIAL
        //    fingerprint, not a partial state.
        let post = Database::open(&db_path, 384).unwrap();
        let stored_provider = post.get_metadata("embedding_provider").unwrap();
        let stored_model = post.get_metadata("embedding_model").unwrap();
        let stored_dim_str = post.get_metadata("embedding_dimension").unwrap();
        let symbol_vec_exists = post
            .conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='symbol_vec'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .is_some();
        assert_eq!(
            stored_provider.as_deref(),
            Some("local"),
            "failed reconcile must roll back provider"
        );
        assert_eq!(
            stored_model.as_deref(),
            Some("BGE-small-en-v1.5"),
            "failed reconcile must roll back model"
        );
        assert_eq!(
            stored_dim_str.as_deref(),
            Some("384"),
            "failed reconcile must roll back dimension"
        );
        assert!(
            symbol_vec_exists,
            "failed reconcile must roll back symbol_vec drop"
        );
        assert_eq!(
            post.embedding_count().unwrap(),
            1,
            "failed reconcile must roll back the symbol_embedding_map DELETE"
        );
    }

    #[test]
    fn test_default_embedding_dim_constant() {
        assert_eq!(DEFAULT_EMBEDDING_DIM, 384);
    }

    #[test]
    fn test_destructive_migration_creates_backup() {
        // Build a legacy v2 database file: pre-hash-columns, with indexed data.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("legacy.db");

        {
            register_sqlite_vec();
            let conn = Connection::open(&db_path).unwrap();
            // Minimal legacy schema that the wipe code will operate on.
            conn.execute_batch(
                "CREATE TABLE symbols (
                    id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
                    start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
                    parent_id TEXT, signature TEXT, visibility TEXT,
                    is_async BOOLEAN, docstring TEXT, in_degree INTEGER DEFAULT 0
                 );
                 CREATE TABLE edges (
                    id INTEGER PRIMARY KEY AUTOINCREMENT, source_id TEXT, target_name TEXT,
                    target_id TEXT, kind TEXT, file_path TEXT, line INTEGER
                 );
                 CREATE TABLE files (path TEXT PRIMARY KEY, last_modified REAL, hash TEXT,
                                     language TEXT, num_symbols INTEGER);
                 CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO symbols (id, name, kind, file_path) VALUES ('s1', 'foo', 'function', 'a.py');
                 INSERT INTO metadata (key, value) VALUES ('schema_version', '2');",
            )
            .unwrap();
        }

        // Opening via the real entry point should back up the legacy file before wiping.
        let _db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

        let backups: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("legacy.db.pre-v")
            })
            .collect();
        assert_eq!(
            backups.len(),
            1,
            "expected exactly one pre-migration backup, found {}",
            backups.len()
        );
    }

    #[test]
    fn test_no_backup_for_fresh_database() {
        // A fresh DB should never produce a backup file.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("fresh.db");
        let _db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

        let backups: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".pre-v"))
            .collect();
        assert!(
            backups.is_empty(),
            "fresh DB should not create a backup file"
        );
    }

    #[test]
    fn test_busy_timeout_pragma_is_set() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("timeout.db");
        let db = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

        let timeout: i64 = db
            .conn
            .query_row("PRAGMA busy_timeout;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, BUSY_TIMEOUT_MS as i64);
    }

    #[test]
    fn test_busy_timeout_makes_second_writer_retry_instead_of_aborting() {
        // Regression for #42. A second writer blocked by a held write lock
        // should *wait* (bounded by busy_timeout) rather than abort instantly.
        // Proven deterministically: against the same held lock, a connection
        // with busy_timeout=0 fails immediately, one with a non-zero timeout
        // only fails after waiting that long. No inter-thread timing race.
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("concurrent.db");
        let _ = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();

        // Holder keeps an exclusive write lock for the whole test.
        let holder = Database::open(&db_path, DEFAULT_EMBEDDING_DIM).unwrap();
        holder
            .conn
            .execute_batch("BEGIN IMMEDIATE; INSERT INTO metadata (key, value) VALUES ('a', '1');")
            .unwrap();

        let attempt_write = |timeout_ms: u32| -> std::time::Duration {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(&format!("PRAGMA busy_timeout={timeout_ms};"))
                .unwrap();
            let start = std::time::Instant::now();
            let res = conn.execute("INSERT INTO metadata (key, value) VALUES ('b', '2');", []);
            assert!(res.is_err(), "write must fail while the lock is held");
            start.elapsed()
        };

        // busy_timeout=0: SQLite aborts immediately, no retry.
        assert!(
            attempt_write(0) < std::time::Duration::from_millis(150),
            "with busy_timeout=0 the writer must fail immediately"
        );
        // busy_timeout=300ms: SQLite retries for the full window before failing.
        assert!(
            attempt_write(300) >= std::time::Duration::from_millis(250),
            "with a non-zero busy_timeout the writer must retry, not abort"
        );

        holder.conn.execute_batch("COMMIT;").unwrap();
    }

    // ── Typed error surface ──

    #[test]
    fn test_db_error_wraps_into_anyhow() {
        // Callers that keep using anyhow::Result must still compose with DbError
        // transparently via `?`, thanks to the std::error::Error blanket impl.
        fn downstream() -> anyhow::Result<()> {
            let _db = Database::open_memory()?; // returns DbResult<Database>
            Ok(())
        }
        downstream().unwrap();
    }

    #[test]
    fn test_db_error_open_variant_has_path() {
        // Give Database::open a path inside a non-writable location to force
        // a failure. We accept either PrepareDir (mkdir failed on the parent)
        // or Open (SQLite refused), since the failure point depends on the
        // platform's handling of `/dev/null/…`.
        let bad_path = std::path::PathBuf::from("/dev/null/definitely/not/a/db.sqlite");
        let err = Database::open(&bad_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
        match err {
            DbError::Open { path, .. } => assert_eq!(path, bad_path),
            DbError::PrepareDir { path, .. } => {
                assert_eq!(path, bad_path.parent().unwrap());
            }
            other => panic!("expected DbError::Open or PrepareDir, got {other:?}"),
        }
    }

    // ── Phase 3 atomicity: indexing transaction primitive ──

    /// Build a minimal valid Symbol for transactional tests.
    fn tx_test_symbol(id: &str, file: &str) -> Symbol {
        Symbol {
            id: id.to_string(),
            name: id.to_string(),
            kind: SymbolKind::Function,
            file_path: file.to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: 0,
            parent_id: None,
            signature: None,
            visibility: Visibility::Public,
            is_async: false,
            docstring: None,
            in_degree: 0,
            content_hash: Some("h".to_string()),
            subtree_hash: Some("s".to_string()),
        }
    }

    #[test]
    fn test_indexing_tx_commit_persists_writes() {
        // Sanity: writes through *_in_tx variants under begin_indexing_tx
        // must persist after commit().
        let db = Database::open_memory().unwrap();
        let sym = tx_test_symbol("a.py:function:foo", "a.py");

        let tx = db.begin_indexing_tx().unwrap();
        db.insert_symbols_in_tx(std::slice::from_ref(&sym)).unwrap();
        tx.commit().unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "committed write must persist");
    }

    #[test]
    fn test_indexing_tx_rollback_drops_writes() {
        // Phase 3 atomicity: writes through *_in_tx variants must roll back
        // when the transaction is dropped without commit() — e.g. an `?`
        // bubbled up an error mid-pipeline, or a panic unwound the stack.
        let db = Database::open_memory().unwrap();
        let sym = tx_test_symbol("a.py:function:foo", "a.py");

        {
            let _tx = db.begin_indexing_tx().unwrap();
            db.insert_symbols_in_tx(std::slice::from_ref(&sym)).unwrap();
            // _tx dropped here without commit() — must roll back.
        }

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count, 0,
            "writes must roll back when the indexing transaction is dropped without commit"
        );
    }

    #[test]
    fn test_indexing_tx_partial_failure_rolls_back_full_pipeline() {
        // Phase 3 atomicity, end-to-end shape: simulate a multi-step pipeline
        // where step N fails after steps 1..N-1 already wrote. Without an
        // outer transaction, the prior writes would persist (the original
        // bug). With begin_indexing_tx wrapping the sequence, dropping `tx`
        // on the error path rolls every prior write back.
        let db = Database::open_memory().unwrap();

        // Seed one pre-existing symbol so we can verify it survives the
        // rollback path (a regression here would also wipe pre-existing
        // data, which is the worst flavor of the bug).
        let pre = tx_test_symbol("pre.py:function:keep", "pre.py");
        db.insert_symbols(std::slice::from_ref(&pre)).unwrap();

        // Run a "Phase 3 lookalike" that fails mid-way. The early `bail!`
        // means tx.commit() is unreachable; dropping `tx` on the error
        // path is exactly what we want to exercise.
        let result: Result<()> = (|| {
            let _tx = db.begin_indexing_tx()?;
            // Write a first batch.
            let batch1 = vec![tx_test_symbol("a.py:function:foo", "a.py")];
            db.insert_symbols_in_tx(&batch1)?;

            // Simulate a downstream failure after a successful early write.
            anyhow::bail!("simulated mid-pipeline failure");
        })();
        assert!(result.is_err(), "the pipeline must propagate its error");

        // The seed survives, the partial write does not.
        let names: Vec<String> = db
            .conn
            .prepare("SELECT id FROM symbols ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["pre.py:function:keep"],
            "pre-existing rows must survive; the partial write must roll back"
        );
    }

    #[test]
    fn test_public_wrapper_still_self_commits() {
        // The public, non-`_in_tx` API must remain usable on its own —
        // existing callers (mcp server, watch, search, etc.) don't open
        // transactions and must keep working unchanged.
        let db = Database::open_memory().unwrap();
        let sym = tx_test_symbol("a.py:function:foo", "a.py");

        // No outer transaction; the wrapper opens and commits its own.
        db.insert_symbols(std::slice::from_ref(&sym)).unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "public wrapper must persist without an outer tx");
    }

    #[test]
    fn test_partial_pipeline_without_outer_tx_persists_writes() {
        // Discriminator test: documents the *old* behavior. Without an
        // outer transaction, an error after a successful self-committing
        // write leaves that write persisted. This is exactly the bug the
        // outer transaction in `index_directory` fixes. If this assertion
        // ever flips, it means someone changed the public wrapper's
        // semantics — and `test_indexing_tx_partial_failure_rolls_back_full_pipeline`
        // would no longer be discriminating between buggy and fixed states.
        let db = Database::open_memory().unwrap();

        let result: Result<()> = (|| {
            // Each call commits independently.
            let batch1 = vec![tx_test_symbol("a.py:function:foo", "a.py")];
            db.insert_symbols(&batch1)?;
            anyhow::bail!("simulated mid-pipeline failure");
        })();
        assert!(result.is_err());

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "without an outer transaction, an early write persists despite a later error"
        );
    }

    // ── resolution_state (edge marker) tests ──

    fn resolution_state_of(db: &Database, edge_id: i64) -> i64 {
        db.conn
            .query_row(
                "SELECT resolution_state FROM edges WHERE id = ?1",
                params![edge_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn insert_test_edge(db: &Database, target_name: &str) -> i64 {
        let sym = test_symbol("src", SymbolKind::Function, "a.py", 1);
        db.insert_symbols(std::slice::from_ref(&sym)).unwrap();
        let edge = Edge::new(&sym.id, target_name, EdgeKind::Calls, "a.py", 1);
        db.insert_edge(&edge).unwrap();
        db.conn.last_insert_rowid()
    }

    #[test]
    fn test_new_edge_has_default_state_zero() {
        let db = Database::open_memory().unwrap();
        let id = insert_test_edge(&db, "missing_target");
        assert_eq!(resolution_state_of(&db, id), 0);
    }

    #[test]
    fn test_update_edge_target_flips_state_to_one() {
        let db = Database::open_memory().unwrap();
        let id = insert_test_edge(&db, "anything");
        db.update_edge_target(id, "some:symbol:id").unwrap();
        assert_eq!(resolution_state_of(&db, id), 1);
    }

    #[test]
    fn test_mark_edge_unresolvable_sets_state_to_two() {
        let db = Database::open_memory().unwrap();
        let id = insert_test_edge(&db, "anything");
        db.mark_edge_unresolvable(id).unwrap();
        assert_eq!(resolution_state_of(&db, id), 2);
    }

    #[test]
    fn test_unresolved_edges_excludes_state_two() {
        let db = Database::open_memory().unwrap();
        let _unresolved = insert_test_edge(&db, "still_unresolved");
        let burned = insert_test_edge(&db, "burned");
        db.mark_edge_unresolvable(burned).unwrap();

        let edges = db.unresolved_edges().unwrap();
        let names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
        assert!(names.contains(&"still_unresolved"));
        assert!(!names.contains(&"burned"));
    }

    #[test]
    fn test_reset_unresolvable_for_names_targets_only_matching() {
        let db = Database::open_memory().unwrap();
        let burned_foo = insert_test_edge(&db, "foo");
        let burned_bar = insert_test_edge(&db, "bar");
        db.mark_edge_unresolvable(burned_foo).unwrap();
        db.mark_edge_unresolvable(burned_bar).unwrap();

        let reopened = db
            .reset_unresolvable_for_names(&["foo".to_string()])
            .unwrap();
        assert_eq!(reopened, 1);
        assert_eq!(resolution_state_of(&db, burned_foo), 0);
        assert_eq!(resolution_state_of(&db, burned_bar), 2);
    }

    #[test]
    fn test_reset_unresolvable_for_names_empty_is_noop() {
        let db = Database::open_memory().unwrap();
        let n = db.reset_unresolvable_for_names(&[]).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn test_reset_unresolvable_for_names_does_not_touch_state_zero_or_one() {
        // The reset reopens state {2, 3} → state=0. Resolved (state=1) and
        // already-open (state=0) edges with matching names must be left alone.
        let db = Database::open_memory().unwrap();
        let still_open = insert_test_edge(&db, "foo"); // state=0
        let already_resolved = insert_test_edge(&db, "foo");
        db.update_edge_target(already_resolved, "some:id").unwrap(); // state=1

        db.reset_unresolvable_for_names(&["foo".to_string()])
            .unwrap();
        assert_eq!(resolution_state_of(&db, still_open), 0);
        assert_eq!(resolution_state_of(&db, already_resolved), 1);
    }

    #[test]
    fn test_mark_edge_external_sets_state_to_three() {
        let db = Database::open_memory().unwrap();
        let id = insert_test_edge(&db, "anything");
        db.mark_edge_external(id).unwrap();
        assert_eq!(resolution_state_of(&db, id), 3);
        assert_eq!(db.edge_resolution_state(id).unwrap(), 3);
    }

    #[test]
    fn test_unresolved_edges_excludes_state_three() {
        // External (state=3) edges must be skipped by the LSP retry loop, same
        // as state=2 — otherwise we re-query dep targets on every dirty run.
        let db = Database::open_memory().unwrap();
        let _open = insert_test_edge(&db, "still_open");
        let ext = insert_test_edge(&db, "external_dep");
        db.mark_edge_external(ext).unwrap();

        let edges = db.unresolved_edges().unwrap();
        let names: Vec<&str> = edges.iter().map(|e| e.target_name.as_str()).collect();
        assert!(names.contains(&"still_open"));
        assert!(!names.contains(&"external_dep"));
    }

    #[test]
    fn test_reset_all_unresolvable_resets_state_two_and_three() {
        // `cartog index --force` must clear BOTH definitive markers (2 and 3)
        // so a forced re-index honors the "retry everything" contract.
        let db = Database::open_memory().unwrap();
        let burned = insert_test_edge(&db, "burned");
        let external = insert_test_edge(&db, "external");
        db.mark_edge_unresolvable(burned).unwrap();
        db.mark_edge_external(external).unwrap();

        let reset = db.reset_all_unresolvable().unwrap();
        assert_eq!(reset, 2);
        assert_eq!(resolution_state_of(&db, burned), 0);
        assert_eq!(resolution_state_of(&db, external), 0);
    }

    #[test]
    fn test_reset_unresolvable_for_names_reopens_state_three() {
        // External edges must also reopen when a matching symbol is added —
        // this is the "vendored dependency in-tree" path.
        let db = Database::open_memory().unwrap();
        let ext_foo = insert_test_edge(&db, "foo");
        let ext_bar = insert_test_edge(&db, "bar");
        db.mark_edge_external(ext_foo).unwrap();
        db.mark_edge_external(ext_bar).unwrap();

        let reopened = db
            .reset_unresolvable_for_names(&["foo".to_string()])
            .unwrap();
        assert_eq!(reopened, 1);
        assert_eq!(resolution_state_of(&db, ext_foo), 0);
        assert_eq!(resolution_state_of(&db, ext_bar), 3);
    }

    #[test]
    fn test_stats_surfaces_external_and_unresolvable_counts() {
        let db = Database::open_memory().unwrap();
        let resolved = insert_test_edge(&db, "resolved_target");
        db.update_edge_target(resolved, "some:id").unwrap();
        let burned = insert_test_edge(&db, "burned");
        db.mark_edge_unresolvable(burned).unwrap();
        let external = insert_test_edge(&db, "external");
        db.mark_edge_external(external).unwrap();
        let _open = insert_test_edge(&db, "open");

        let stats = db.stats().unwrap();
        assert_eq!(stats.num_resolved, 1);
        assert_eq!(stats.num_unresolvable, 1);
        assert_eq!(stats.num_external, 1);
        assert_eq!(stats.num_edges, 4);
    }

    #[test]
    fn test_invalidate_edges_targeting_resets_state_when_target_disappears() {
        // When a symbol referenced by a resolved edge is removed, the edge
        // must drop back to (target_id NULL, state=0) so it re-enters the
        // unresolved set on the next pass.
        let db = Database::open_memory().unwrap();

        // Set up: source edge points to symbol "ghost" via update_edge_target,
        // then drop the symbol so the edge becomes dangling.
        let src = test_symbol("src", SymbolKind::Function, "a.py", 1);
        let target = test_symbol("ghost", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
        let edge = Edge::new(&src.id, "ghost", EdgeKind::Calls, "a.py", 1);
        db.insert_edge(&edge).unwrap();
        let eid = db.conn.last_insert_rowid();
        db.update_edge_target(eid, &target.id).unwrap();
        assert_eq!(resolution_state_of(&db, eid), 1);

        // Remove the target symbol — leaves edge.target_id pointing at nothing.
        db.conn
            .execute("DELETE FROM symbols WHERE id = ?1", params![target.id])
            .unwrap();

        let mut dirty = std::collections::HashSet::new();
        dirty.insert("b.py".to_string());
        db.invalidate_edges_targeting(&dirty).unwrap();

        assert_eq!(
            resolution_state_of(&db, eid),
            0,
            "dangling edge must return to state=0 so unresolved_edges() can see it"
        );
        let row: Option<String> = db
            .conn
            .query_row(
                "SELECT target_id FROM edges WHERE id = ?1",
                params![eid],
                |r| r.get(0),
            )
            .unwrap();
        assert!(row.is_none(), "target_id must be NULL after invalidation");
    }

    #[test]
    fn test_delete_symbol_resets_state_on_dangling_incoming_edges() {
        // Regression for the "(target_id=NULL, state=1) zombie" bug: when a
        // resolved target symbol is deleted, every edge pointing to it must
        // drop back to state=0 — otherwise the edge becomes invisible to both
        // unresolved_edges() (state=1 filter) and graph traversal (NULL target).
        let db = Database::open_memory().unwrap();
        let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
        let target = test_symbol("ghost", SymbolKind::Function, "b.py", 1);
        db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
        let edge = Edge::new(&src.id, "ghost", EdgeKind::Calls, "a.py", 1);
        db.insert_edge(&edge).unwrap();
        let eid = db.conn.last_insert_rowid();
        db.update_edge_target(eid, &target.id).unwrap();

        db.delete_symbol(&target.id).unwrap();

        assert_eq!(resolution_state_of(&db, eid), 0);
        let visible = db
            .unresolved_edges()
            .unwrap()
            .iter()
            .any(|e| e.edge_id == eid);
        assert!(
            visible,
            "orphaned edge must resurface in unresolved_edges()"
        );
    }

    #[test]
    fn test_delete_symbols_in_tx_resets_state_on_dangling_incoming_edges() {
        // Same invariant as test_delete_symbol_..., for the batched path used
        // by the indexer's Merkle-diff `removed` set.
        let db = Database::open_memory().unwrap();
        let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
        let t1 = test_symbol("ghost1", SymbolKind::Function, "b.py", 1);
        let t2 = test_symbol("ghost2", SymbolKind::Function, "c.py", 1);
        db.insert_symbols(&[src.clone(), t1.clone(), t2.clone()])
            .unwrap();
        let e1 = Edge::new(&src.id, "ghost1", EdgeKind::Calls, "a.py", 1);
        db.insert_edge(&e1).unwrap();
        let eid1 = db.conn.last_insert_rowid();
        db.update_edge_target(eid1, &t1.id).unwrap();
        let e2 = Edge::new(&src.id, "ghost2", EdgeKind::Calls, "a.py", 2);
        db.insert_edge(&e2).unwrap();
        let eid2 = db.conn.last_insert_rowid();
        db.update_edge_target(eid2, &t2.id).unwrap();

        db.delete_symbols(&[t1.id.clone(), t2.id.clone()]).unwrap();

        assert_eq!(resolution_state_of(&db, eid1), 0);
        assert_eq!(resolution_state_of(&db, eid2), 0);
    }

    #[test]
    fn test_heuristic_resolve_flips_state_to_one() {
        // Regression: resolve_edge_batch's UPDATE must set state=1 alongside
        // target_id. Otherwise heuristically-resolved edges stay state=0 and
        // get re-queried by LSP on the next pass — pure waste.
        let db = Database::open_memory().unwrap();
        let src = test_symbol("caller", SymbolKind::Function, "a.py", 1);
        let target = test_symbol("foo", SymbolKind::Function, "a.py", 10);
        db.insert_symbols(&[src.clone(), target.clone()]).unwrap();
        let edge = Edge::new(&src.id, "foo", EdgeKind::Calls, "a.py", 2);
        db.insert_edge(&edge).unwrap();
        let eid = db.conn.last_insert_rowid();
        assert_eq!(resolution_state_of(&db, eid), 0);

        db.resolve_edges().unwrap();

        assert_eq!(
            resolution_state_of(&db, eid),
            1,
            "heuristic resolve must set state=1 so LSP doesn't re-attack the edge"
        );
        assert!(
            db.unresolved_edges()
                .unwrap()
                .iter()
                .all(|e| e.edge_id != eid),
            "resolved edge must drop out of unresolved_edges()"
        );
    }

    #[test]
    fn test_partial_unresolved_index_exists() {
        // The partial index speeds up the unresolved_edges() query on large
        // repos. Verify it actually got created by inspecting sqlite_master.
        let db = Database::open_memory().unwrap();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='index' AND name='idx_edges_unresolved'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn test_resolution_state_default_via_insert_edges_batch() {
        // The batched insert path is the production hot path. Make sure
        // it honors the DEFAULT 0 just like single-row inserts do.
        let db = Database::open_memory().unwrap();
        let src = test_symbol("src", SymbolKind::Function, "a.py", 1);
        db.insert_symbols(std::slice::from_ref(&src)).unwrap();
        let edges = vec![
            Edge::new(&src.id, "x", EdgeKind::Calls, "a.py", 1),
            Edge::new(&src.id, "y", EdgeKind::Calls, "a.py", 2),
        ];
        db.insert_edges(&edges).unwrap();
        let states: Vec<i64> = db
            .conn
            .prepare("SELECT resolution_state FROM edges ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(states, vec![0, 0]);
    }

    #[test]
    fn test_migration_v3_to_v4_backfills_resolved_to_state_one() {
        // Simulate a pre-v4 database: open with v3-equivalent schema (no
        // resolution_state column, schema_version=3), insert edges with
        // and without target_ids, then re-open to trigger the migration.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v3.sqlite");

        {
            let conn = Connection::open(&path).unwrap();
            // Bootstrap a v3-shaped edges table by hand.
            conn.execute_batch(
                "CREATE TABLE symbols (
                    id TEXT PRIMARY KEY, name TEXT, kind TEXT, file_path TEXT,
                    start_line INTEGER, end_line INTEGER, start_byte INTEGER, end_byte INTEGER,
                    parent_id TEXT, signature TEXT, visibility TEXT, is_async BOOLEAN,
                    docstring TEXT, in_degree INTEGER DEFAULT 0,
                    content_hash TEXT, subtree_hash TEXT);
                 CREATE TABLE edges (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    source_id TEXT NOT NULL, target_name TEXT NOT NULL, target_id TEXT,
                    kind TEXT NOT NULL, file_path TEXT NOT NULL, line INTEGER);
                 CREATE TABLE files (path TEXT PRIMARY KEY);
                 CREATE TABLE metadata (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO metadata (key, value) VALUES ('schema_version', '3');
                 INSERT INTO symbols (id, name, kind, file_path) VALUES ('s:1', 'foo', 'function', 'a.py');
                 INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
                   VALUES ('s:1', 'foo', 's:1', 'calls', 'a.py', 1);
                 INSERT INTO edges (source_id, target_name, target_id, kind, file_path, line)
                   VALUES ('s:1', 'missing', NULL, 'calls', 'a.py', 2);",
            )
            .unwrap();
        }

        // Re-open through the production path so migrate() runs.
        let db = Database::open(&path, DEFAULT_EMBEDDING_DIM).unwrap();

        let resolved_state: i64 = db
            .conn
            .query_row(
                "SELECT resolution_state FROM edges WHERE target_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let unresolved_state: i64 = db
            .conn
            .query_row(
                "SELECT resolution_state FROM edges WHERE target_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(resolved_state, 1, "existing target_id NOT NULL → state=1");
        assert_eq!(unresolved_state, 0, "existing target_id NULL → state=0");

        let bumped: String = db
            .conn
            .query_row(
                "SELECT value FROM metadata WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bumped, SCHEMA_VERSION.to_string());
    }
}
