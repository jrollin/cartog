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

/// SQL to create the sqlite-vec virtual table with the given embedding dimension.
fn rag_vec_schema(dim: usize) -> String {
    format!("CREATE VIRTUAL TABLE IF NOT EXISTS symbol_vec USING vec0(embedding float[{dim}])")
}

/// Default database filename, stored in the project root.
pub const DB_FILE: &str = ".cartog.db";

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
const SCHEMA_VERSION: u32 = 3;

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

    if current >= SCHEMA_VERSION && has_hash_cols {
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

    // Store the new schema version
    if let Err(e) = conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    ) {
        warn!(error = %e, "failed to store schema version");
    }
}

/// Check stored embedding dimension against requested dimension.
/// If they differ, drop the vector table and clear the embedding map.
///
/// Returns rusqlite's `Result` so the caller (`Database::open`) can wrap
/// any failure into `DbError::EmbeddingDimension` with precise context.
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

    if let Some(old_dim) = stored_dim {
        if old_dim != effective_dim {
            tracing::warn!(
                old = old_dim,
                new = effective_dim,
                "Embedding dimension changed — clearing vector index. Run `cartog rag index` to re-embed."
            );
            conn.execute("DROP TABLE IF EXISTS symbol_vec", [])?;
            conn.execute("DELETE FROM symbol_embedding_map", [])?;
        }
    }

    conn.execute_batch(&rag_vec_schema(effective_dim))?;

    conn.execute(
        "INSERT OR REPLACE INTO metadata (key, value) VALUES ('embedding_dimension', ?1)",
        params![effective_dim.to_string()],
    )?;

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
        let conn = Connection::open(db_path).map_err(|source| DbError::Open {
            path: db_path.to_path_buf(),
            source,
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;",
        )
        .map_err(DbError::Pragma)?;
        conn.execute_batch(SCHEMA).map_err(DbError::Schema)?;
        conn.execute_batch(RAG_SCHEMA).map_err(DbError::RagSchema)?;
        backup_before_destructive_migration(&conn, db_path)?;
        migrate(&conn);
        handle_embedding_dimension(&conn, embedding_dim).map_err(DbError::EmbeddingDimension)?;
        Ok(Self { conn })
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
        Ok(Self { conn })
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
    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO metadata (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // ── Files ──

    /// Insert or update file metadata.
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
    pub fn clear_edges_for_file(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        Ok(())
    }

    /// Remove all symbols, edges, and RAG data for a file (before re-indexing it).
    pub fn clear_file_data(&self, path: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.clear_rag_data_for_file(path)?;
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE file_path = ?1", params![path])?;
        tx.commit()?;
        Ok(())
    }

    /// Remove a file and all its symbols and edges from the index.
    pub fn remove_file(&self, path: &str) -> Result<()> {
        self.clear_file_data(path)?;
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
        tx.commit()?;
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
        let mut del_out = self
            .conn
            .prepare_cached("DELETE FROM edges WHERE source_id = ?1")?;
        let mut null_in = self
            .conn
            .prepare_cached("UPDATE edges SET target_id = NULL WHERE target_id = ?1")?;
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
        drop(del_out);
        drop(null_in);
        drop(del_vec);
        drop(del_map);
        drop(del_content);
        drop(del_sym);
        tx.commit()?;
        Ok(())
    }

    /// Delete a single symbol and cascade to edges, content, and embeddings.
    pub fn delete_symbol(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn
            .execute("DELETE FROM edges WHERE source_id = ?1", params![id])?;
        self.conn.execute(
            "UPDATE edges SET target_id = NULL WHERE target_id = ?1",
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
        tx.commit()?;
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
        let mut unresolved_stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.source_id
             FROM edges e WHERE e.target_id IS NULL",
        )?;

        let unresolved: Vec<(i64, String, String, String)> = unresolved_stmt
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        self.resolve_edge_batch(&unresolved)
    }

    /// 6-tier heuristic resolution for a batch of unresolved edges.
    fn resolve_edge_batch(&self, unresolved: &[(i64, String, String, String)]) -> Result<u32> {
        let mut resolved = 0u32;

        let tx = self.conn.unchecked_transaction()?;

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

        let mut update_stmt = self
            .conn
            .prepare("UPDATE edges SET target_id = ?1 WHERE id = ?2")?;

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

        tx.commit()?;
        Ok(resolved)
    }

    /// Compute and store in-degree centrality for all symbols.
    ///
    /// In-degree = number of resolved incoming edges (calls, imports, inherits, etc.).
    /// Higher in-degree means the symbol is referenced more across the codebase.
    /// Resets all in-degree values to 0 first, then batch-updates from the edges table.
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
    pub fn invalidate_edges_targeting(
        &self,
        dirty_files: &std::collections::HashSet<String>,
    ) -> Result<u32> {
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After file re-indexing, edges from unchanged files may point to
        // symbol IDs that no longer exist (removed or renamed symbols).
        // Set these dangling references to NULL so they can be re-resolved.
        let n = self.conn.execute(
            "UPDATE edges SET target_id = NULL
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
        if dirty_files.is_empty() {
            return Ok(0);
        }
        // After invalidation, the set of unresolved edges is naturally scoped:
        // only edges from dirty files (freshly extracted) or targeting dirty files
        // (just invalidated) have target_id = NULL.
        // Reuse the same 2-pass resolution.
        self.resolve_edges()
    }

    /// Recompute in-degree centrality only for symbols in/around dirty files.
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

    /// Index statistics.
    pub fn stats(&self) -> Result<IndexStats> {
        let num_files: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let num_symbols: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let num_edges: u32 = self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        let num_resolved: u32 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE target_id IS NOT NULL",
            [],
            |row| row.get(0),
        )?;

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
            languages,
            symbol_kinds,
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
        let mut stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO symbol_content (symbol_id, content, header, normalized_name)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (symbol_id, name, content, header) in items {
            let normalized = normalize_symbol_name(name);
            stmt.execute(params![symbol_id, content, header, normalized])?;
        }
        tx.commit()?;
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

    /// Return all edges with `target_id IS NULL` (unresolved after heuristic pass).
    pub fn unresolved_edges(&self) -> Result<Vec<UnresolvedEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path, e.line
             FROM edges e
             WHERE e.target_id IS NULL",
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

    /// Update a single edge's target_id.
    pub fn update_edge_target(&self, edge_id: i64, target_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE edges SET target_id = ?1 WHERE id = ?2",
            params![target_id, edge_id],
        )?;
        Ok(())
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
    pub languages: Vec<(String, u32)>,
    pub symbol_kinds: Vec<(String, u32)>,
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
        // rusqlite::Error::SqliteFailure. The `Open` variant should preserve
        // the path we asked it to touch.
        let bad_path = std::path::PathBuf::from("/dev/null/definitely/not/a/db.sqlite");
        let err = Database::open(&bad_path, DEFAULT_EMBEDDING_DIM).unwrap_err();
        match err {
            DbError::Open { path, .. } => assert_eq!(path, bad_path),
            other => panic!("expected DbError::Open, got {other:?}"),
        }
    }
}
