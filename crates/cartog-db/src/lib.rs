//! SQLite persistence layer for the cartog code graph.
//!
//! Stores symbols, edges, and file metadata in a single SQLite database.
//! Provides graph traversal queries (callees, refs, impact, hierarchy),
//! full-text search via FTS5, vector KNN search via sqlite-vec, and a
//! 6-tier heuristic edge resolution algorithm.
#![doc = ""]
#![doc = include_str!("../README.md")]

use anyhow::{Context, Result};
use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sqlite_vec::sqlite3_vec_init;
use tracing::{info, warn};

use cartog_core::{Edge, EdgeKind, EdgeProvenance, FileInfo, Symbol, SymbolKind, Visibility};

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

    /// The main database file does not exist at the resolved path. Returned by
    /// [`Database::open_existing`] so callers can branch on "no index yet"
    /// without materializing a `.cartog/` for an un-opted-in project.
    #[error("no cartog index at {path}")]
    NotFound { path: std::path::PathBuf },

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

const SQL_INSERT_EDGE: &str = "INSERT INTO edges
     (source_id, target_name, target_id, kind, file_path, line, resolution_state, resolution_source)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";

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
    -- Which tier/source resolved target_id (EdgeProvenance::as_str), or NULL for
    -- unresolved edges and rows resolved before provenance tracking existed.
    resolution_source TEXT,
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
-- Per-file edge delete (clear_file_data_in_tx); without it the DELETE full-scans
-- edges per file, making --force/first-index O(files×edges). idx_edges_unresolved
-- is partial (state=0) so it can't serve deletes of resolved edges.
CREATE INDEX IF NOT EXISTS idx_edges_file ON edges(file_path);
-- Tier-2 import-path lookups; kind-only index scans all imports edges per call (#109).
CREATE INDEX IF NOT EXISTS idx_edges_kind_target ON edges(kind, target_name);
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
            current.extend(c.to_lowercase());
        } else if c.is_alphanumeric() {
            current.extend(c.to_lowercase());
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
    // Safety: sqlite3_vec_init matches the C ABI sqlite3_auto_extension expects.
    INIT.call_once(|| unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    });
}

/// Current schema version. Increment when adding migrations.
const SCHEMA_VERSION: u32 = 7;

/// Public mirror of the private `SCHEMA_VERSION` for callers outside this crate
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

/// Read a single `metadata` value by key from a cartog SQLite file at `path`,
/// without the full [`Database::open`] machinery. Mirrors
/// [`read_schema_version_at`]; used by `cartog push`/`pull` to read the
/// `last_commit` provenance row off a closed DB file.
///
/// Returns `Ok(None)` when the file is a cartog DB but lacks the row, or when
/// it has no `metadata` table at all (not a cartog DB). Returns `Err` only on
/// genuine SQLite errors (corrupt file, permission denied, etc.).
pub fn read_metadata_at(path: &std::path::Path, key: &str) -> anyhow::Result<Option<String>> {
    use anyhow::Context;
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open {} read-only for metadata read", path.display()))?;
    match conn.query_row(
        "SELECT value FROM metadata WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, Option<String>>(0),
    ) {
        // Row present; value may be a string or SQL NULL (a corrupt/hand-edited
        // row) — both collapse to "no usable value", same as a missing row.
        Ok(v) => Ok(v),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        // Missing `metadata` table entirely (non-cartog SQLite file): treat as
        // absent rather than an error, matching read_schema_version's stored=0.
        Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
            if msg.contains("no such table: metadata") =>
        {
            Ok(None)
        }
        Err(e) => Err(e).with_context(|| format!("read metadata[{key}] from {}", path.display())),
    }
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
    // Same idea for v6: ensure the resolution_source column exists.
    let has_resolution_source = conn
        .prepare("SELECT resolution_source FROM edges LIMIT 0")
        .is_ok();

    if current >= SCHEMA_VERSION
        && has_hash_cols
        && has_resolution_state
        && has_query_log
        && has_resolution_source
    {
        return;
    }

    // Fresh-DB fast path: the SCHEMA bootstrap just created every table at the
    // current shape, so all columns/tables exist but no schema_version row is
    // stamped yet (current was read as 1 via unwrap_or). Stamp the version and
    // skip the ladder, avoiding the needless v2→3 wipe and the
    // resolution_source "duplicate column" WARN on every fresh open.
    // Require an empty symbols table AND all four probes: a real pre-versioning
    // v1 DB has rows, and a crash-mid-migration DB is missing a column, so
    // neither is misclassified as fresh.
    let no_version_row = conn
        .query_row(
            "SELECT 1 FROM metadata WHERE key = 'schema_version'",
            [],
            |_| Ok(()),
        )
        .is_err();
    let symbols_empty = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get::<_, i64>(0))
        .map(|c| c == 0)
        .unwrap_or(false);
    if no_version_row
        && symbols_empty
        && has_hash_cols
        && has_resolution_state
        && has_query_log
        && has_resolution_source
    {
        if let Err(e) = conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        ) {
            warn!(error = %e, "failed to stamp fresh-DB schema version");
        }
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

    // Migration 5 → 6: edges.resolution_source records WHICH tier/source resolved
    // each edge. Additive, nullable. Pre-v6 resolved edges have an indistinguishable
    // tier, so they stay NULL ("unknown / pre-provenance") rather than guess a sentinel.
    if current < 6 || !has_resolution_source {
        info!("schema v6: adding edges.resolution_source column");
        // Surface a failed ALTER (matches the schema-version write below): the
        // probe guard re-runs the migration on the next open, so this is logged
        // rather than fatal, consistent with the other additive migrations.
        if let Err(e) = conn.execute("ALTER TABLE edges ADD COLUMN resolution_source TEXT", []) {
            warn!(error = %e, "failed to add edges.resolution_source column");
        }
    }

    // Migration 6 → 7: symbol-ID leaf-name escaping for injectivity.
    // The ID format gained separator-escaping for composite leaf names (dotted
    // import paths, `.`/`:`-bearing markdown headings) so distinct symbols can no
    // longer collide to one ID. Existing rows carry the old (collidable) IDs, so
    // clear the index for a full rebuild — mirrors the v2→3 stable-ID wipe.
    if current < 7 {
        info!("schema v7: symbol-ID escaping — clearing index for full rebuild");
        for table in &["symbol_content", "edges", "symbols", "files"] {
            let _ = conn.execute(&format!("DELETE FROM {table}"), []);
        }
        let _ = conn.execute("DELETE FROM symbol_vec", []);
        let _ = conn.execute("DELETE FROM symbol_embedding_map", []);
        let _ = conn.execute("DELETE FROM metadata WHERE key = 'last_commit'", []);
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

    // Mirrors the destructive conditions in `migrate()`: the 2→3 stable-id wipe
    // (`current < 3 || !has_hash_cols`) and the 6→7 symbol-id-escaping wipe
    // (`current < 7`). Either clears every indexed row, so back up first.
    let will_wipe = current < 7 || !has_hash_cols;
    if !will_wipe {
        return Ok(());
    }

    // Back up if ANY wiped table holds data, not just `symbols`: a partially
    // indexed DB (e.g. edges/content written before symbols) would otherwise
    // skip the backup and lose those rows to the wipe. A missing table errors
    // the EXISTS probe, which `unwrap_or(false)` treats as empty.
    let has_rows = |table: &str| -> bool {
        conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table})"), [], |r| {
            r.get::<_, bool>(0)
        })
        .unwrap_or(false)
    };
    let any_indexed = [
        "symbols",
        "edges",
        "files",
        "symbol_content",
        "symbol_embedding_map",
    ]
    .iter()
    .any(|t| has_rows(t));
    if !any_indexed {
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

    let symbol_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
        .unwrap_or(0);
    info!(
        backup = %backup_path.display(),
        old_version = current,
        new_version = SCHEMA_VERSION,
        symbols = symbol_count,
        "schema migration will clear indexed data — created backup"
    );

    Ok(())
}

// The `Database` inherent impl is split across `store/` submodules for
// navigability; each file holds one cohesive cluster of methods.
mod store;
pub use store::queries::PathHop;
pub use store::rag::KindScope;

/// An unresolved edge from the database (used by LSP resolution).
#[derive(Debug, Clone)]
pub struct UnresolvedEdge {
    pub edge_id: i64,
    pub target_name: String,
    pub file_path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
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
    // SQLite raises SQLITE_ERROR (primary code 1) with a message starting
    // "no such table: <name>". Match on the variant + the message inside it
    // rather than `e.to_string()` so a future change to rusqlite's Display
    // wrapper doesn't break the dispatch silently.
    matches!(
        e,
        rusqlite::Error::SqliteFailure(_, Some(msg)) if msg.contains("no such table")
    )
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

/// Build an [`Edge`] from six consecutive columns starting at `base`:
/// `source_id, target_name, target_id, kind, file_path, line, resolution_source`.
///
/// Shared by every edge-returning query so the field reads, the warn-on-unknown
/// decode, and the column ordering stay in one place. Callers that prepend an
/// `id` column pass `base = 1`; the bare-projection impact CTE passes `base = 0`.
fn edge_from_row(row: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<Edge> {
    let kind_str = row.get::<_, String>(base + 3)?;
    let kind = kind_str.parse().unwrap_or_else(|_| {
        warn!(kind = %kind_str, "unknown edge kind, defaulting to references");
        EdgeKind::References
    });

    let provenance = match row.get::<_, Option<String>>(base + 6)? {
        Some(s) => s.parse::<EdgeProvenance>().ok().or_else(|| {
            warn!(source = %s, "unknown edge provenance, dropping to None");
            None
        }),
        None => None,
    };

    Ok(Edge {
        source_id: row.get(base)?,
        target_name: row.get(base + 1)?,
        target_id: row.get(base + 2)?,
        kind,
        file_path: row.get(base + 4)?,
        line: row.get(base + 5)?,
        provenance,
    })
}

fn row_to_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<Edge> {
    edge_from_row(row, 1)
}

#[cfg(test)]
mod tests;
