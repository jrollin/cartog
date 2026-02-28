use anyhow::{Context, Result};
use rusqlite::ffi::sqlite3_auto_extension;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sqlite_vec::sqlite3_vec_init;
use tracing::warn;

use crate::types::{Edge, EdgeKind, FileInfo, Symbol, SymbolKind, Visibility};

const SQL_INSERT_SYMBOL: &str = "INSERT OR REPLACE INTO symbols
     (id, name, kind, file_path, start_line, end_line, start_byte, end_byte,
      parent_id, signature, visibility, is_async, docstring)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";

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
    docstring TEXT
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

/// SQL to create the sqlite-vec virtual table (must run after sqlite-vec extension is loaded).
const RAG_VEC_SCHEMA: &str =
    "CREATE VIRTUAL TABLE IF NOT EXISTS symbol_vec USING vec0(embedding float[384])";

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

impl Database {
    /// Open or create the database at the given path.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open(path.as_ref()).context("Failed to open database")?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-65536;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=268435456;",
        )
        .context("Failed to set pragmas")?;
        conn.execute_batch(SCHEMA)
            .context("Failed to create schema")?;
        conn.execute_batch(RAG_SCHEMA)
            .context("Failed to create RAG schema")?;
        conn.execute_batch(RAG_VEC_SCHEMA)
            .context("Failed to create sqlite-vec table")?;
        Ok(Self { conn })
    }

    /// Open an in-memory database (for tests and benchmarks).
    #[doc(hidden)]
    pub fn open_memory() -> Result<Self> {
        register_sqlite_vec();
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(RAG_SCHEMA)?;
        conn.execute_batch(RAG_VEC_SCHEMA)?;
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

    /// Remove all symbols, edges, and RAG data for a file (before re-indexing it).
    pub fn clear_file_data(&self, path: &str) -> Result<()> {
        self.clear_rag_data_for_file(path)?;
        self.conn
            .execute("DELETE FROM edges WHERE file_path = ?1", params![path])?;
        self.conn
            .execute("DELETE FROM symbols WHERE file_path = ?1", params![path])?;
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
            ])?;
        }
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
    /// Priority: exact match in same file > same directory > unique project-wide match.
    pub fn resolve_edges(&self) -> Result<u32> {
        let mut resolved = 0u32;

        let mut unresolved_stmt = self.conn.prepare(
            "SELECT e.id, e.target_name, e.file_path
             FROM edges e WHERE e.target_id IS NULL",
        )?;

        let unresolved: Vec<(i64, String, String)> = unresolved_stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let tx = self.conn.unchecked_transaction()?;

        let mut same_file_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path = ?2 LIMIT 1")?;
        let mut same_dir_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 AND file_path LIKE ?2 LIMIT 1")?;
        let mut anywhere_stmt = self
            .conn
            .prepare("SELECT id FROM symbols WHERE name = ?1 LIMIT 2")?;
        let mut update_stmt = self
            .conn
            .prepare("UPDATE edges SET target_id = ?1 WHERE id = ?2")?;

        for (edge_id, target_name, edge_file) in &unresolved {
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

            // 2) Same directory
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

            // 3) Unique project-wide match — fetch at most 2 rows; resolve only if exactly 1
            let mut rows = anywhere_stmt.query(params![simple_name])?;
            let first = rows.next()?.and_then(|r| r.get::<_, String>(0).ok());
            let has_second = rows.next()?.is_some();
            if let (Some(tid), false) = (first, has_second) {
                update_stmt.execute(params![tid, edge_id])?;
                resolved += 1;
            }
        }

        tx.commit()?;
        Ok(resolved)
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
                    is_async, docstring,
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
                      CASE kind
                        WHEN 'function' THEN 0
                        WHEN 'method'   THEN 1
                        WHEN 'class'    THEN 2
                        ELSE                 3
                      END,
                      file_path, start_line
             LIMIT ?5",
        )?;
        // rank is column 13 — row_to_symbol reads columns 0–12 and ignores it
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
                    parent_id, signature, visibility, is_async, docstring
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
                        s.is_async, s.docstring
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
                        s.is_async, s.docstring
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
    pub fn impact(&self, name: &str, max_depth: u32) -> Result<Vec<(Edge, u32)>> {
        let mut results = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut frontier: Vec<(String, u32)> = vec![(name.to_string(), 0)];

        while let Some((current, depth)) = frontier.pop() {
            if depth >= max_depth || visited.contains(&current) {
                continue;
            }
            visited.insert(current.clone());

            let refs = self.refs(&current, None)?;
            for (edge, sym) in refs {
                results.push((edge, depth + 1));
                if let Some(s) = sym {
                    if !visited.contains(&s.name) {
                        frontier.push((s.name, depth + 1));
                    }
                }
            }
        }

        Ok(results)
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

    /// Returns `true` if at least one file has been indexed.
    ///
    /// Cheaper than [`stats`] for the common "is the index empty?" check —
    /// SQLite can satisfy `LIMIT 1` with a single index seek rather than a full count.
    pub fn has_indexed_files(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM files LIMIT 1", [], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Get all indexed file paths, sorted alphabetically.
    pub fn all_files(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT path FROM files ORDER BY path")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
        let placeholders: Vec<&str> = symbol_ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT symbol_id, content, header FROM symbol_content WHERE symbol_id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = symbol_ids
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
        // Use a temporary approach for variable-length IN clause
        let placeholders: Vec<String> = embedding_ids.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT id, symbol_id FROM symbol_embedding_map WHERE id IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = embedding_ids
            .iter()
            .map(|id| Box::new(*id) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
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
                        parent_id, signature, visibility, is_async, docstring
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
        let mut result = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(sym) = self.get_symbol(id)? {
                result.push(sym);
            }
        }
        Ok(result)
    }

    /// Get all symbol IDs that have content stored but no embedding yet.
    ///
    /// Variables are excluded — they are too numerous and low-signal for embedding.
    pub fn symbols_needing_embeddings(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind != ?1
             AND NOT EXISTS (
                 SELECT 1 FROM symbol_embedding_map em
                 JOIN symbol_vec sv ON sv.rowid = em.id
                 WHERE em.symbol_id = sc.symbol_id
             )",
        )?;
        let rows = stmt
            .query_map(params![SymbolKind::Variable.as_str()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count symbols that have content stored.
    pub fn symbol_content_count(&self) -> Result<u32> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM symbol_content", [], |row| row.get(0))?)
    }

    /// Get all symbol IDs that have content stored (excluding variables).
    pub fn all_content_symbol_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT sc.symbol_id FROM symbol_content sc
             JOIN symbols s ON s.id = sc.symbol_id
             WHERE s.kind != ?1
             ORDER BY sc.symbol_id",
        )?;
        let rows = stmt
            .query_map(params![SymbolKind::Variable.as_str()], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Clear all embedding data (for force re-embed).
    pub fn clear_all_embeddings(&self) -> Result<()> {
        self.conn.execute("DELETE FROM symbol_vec", [])?;
        self.conn.execute("DELETE FROM symbol_embedding_map", [])?;
        Ok(())
    }
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
    })
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
        Symbol::new(name, kind, file, line, line + 5, 0, 100)
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
}
