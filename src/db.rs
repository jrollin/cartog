use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
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

/// Default database filename, stored in the project root.
pub const DB_FILE: &str = ".cartog.db";

/// Maximum number of results returned by [`Database::search`].
/// Enforced here and referenced by CLI and MCP layers.
pub const MAX_SEARCH_LIMIT: u32 = 100;

pub struct Database {
    conn: Connection,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database").finish_non_exhaustive()
    }
}

impl Database {
    /// Open or create the database at the given path.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
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
        Ok(Self { conn })
    }

    /// Open an in-memory database (for tests and benchmarks).
    #[doc(hidden)]
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
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

    /// Remove all symbols and edges for a file (before re-indexing it).
    pub fn clear_file_data(&self, path: &str) -> Result<()> {
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
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file_path, start_line, end_line,
                    start_byte, end_byte, parent_id, signature, visibility,
                    is_async, docstring,
                    CASE
                      WHEN LOWER(name) = LOWER(?1)                    THEN 0
                      WHEN LOWER(name) LIKE LOWER(?2) || '%' ESCAPE '\\' THEN 1
                      ELSE                                                  2
                    END AS rank
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
}
