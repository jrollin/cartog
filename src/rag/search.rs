use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use std::sync::Mutex;

use crate::db::Database;
use crate::types::{Symbol, SymbolKind};

use super::embeddings::{embedding_to_bytes, EmbeddingEngine};
use super::reranker::CrossEncoderEngine;

/// Cached embedding engine — loaded once, reused across search calls.
static EMBEDDING_ENGINE: Mutex<Option<EmbeddingEngine>> = Mutex::new(None);

/// Cached cross-encoder engine — loaded once, reused across search calls.
/// Uses tri-state: None = not attempted, Some(None) = load failed, Some(Some(_)) = ready.
static RERANKER_ENGINE: Mutex<Option<Option<CrossEncoderEngine>>> = Mutex::new(None);

/// Get or initialize the cached embedding engine.
///
/// NOTE: The Mutex is held for the entire duration of model inference.
/// This is fine for single-threaded CLI and MCP usage (one query at a time).
/// If the MCP server becomes multi-threaded with concurrent queries,
/// this should be replaced with a pool or per-thread engine.
fn with_embedding_engine<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut EmbeddingEngine) -> Result<R>,
{
    let mut guard = EMBEDDING_ENGINE
        .lock()
        .map_err(|_| anyhow::anyhow!("embedding engine lock poisoned"))?;
    if guard.is_none() {
        *guard = Some(EmbeddingEngine::new()?);
    }
    f(guard.as_mut().unwrap())
}

/// Get or initialize the cached cross-encoder engine.
///
/// Returns None if model is not available (not downloaded) or lock is poisoned.
/// Uses tri-state caching: once a load attempt fails, it is not retried,
/// avoiding repeated filesystem/network probes on every search call.
fn with_reranker_engine<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&mut CrossEncoderEngine) -> R,
{
    let mut guard = RERANKER_ENGINE.lock().ok()?;
    if guard.is_none() {
        // First attempt: try to load, cache the result either way
        match CrossEncoderEngine::load() {
            Ok(engine) => *guard = Some(Some(engine)),
            Err(e) => {
                tracing::debug!(error = %e, "Cross-encoder not available, skipping re-ranking");
                *guard = Some(None); // Cache the failure — don't retry
                return None;
            }
        }
    }
    guard.as_mut().unwrap().as_mut().map(f)
}

/// A search result combining symbol metadata with relevance info.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub symbol: Symbol,
    pub content: Option<String>,
    pub rrf_score: f64,
    /// Cross-encoder re-ranking score (higher = more relevant). Present only when
    /// the cross-encoder model is available.
    pub rerank_score: Option<f64>,
    /// Which retrieval methods found this result.
    pub sources: Vec<String>,
}

/// Result of a hybrid search operation.
#[derive(Debug, Serialize)]
pub struct HybridSearchResult {
    pub results: Vec<SearchResult>,
    pub fts_count: u32,
    pub vec_count: u32,
    pub merged_count: u32,
}

/// Reciprocal Rank Fusion: merge multiple ranked lists into a single ranking.
///
/// `k = 60` is the standard constant from the original RRF paper (Cormack et al., 2009).
fn rrf_merge(ranked_lists: &[(&str, Vec<String>)], k: f64) -> Vec<(String, f64, Vec<String>)> {
    let mut scores: HashMap<String, (f64, Vec<String>)> = HashMap::new();

    for (source_name, list) in ranked_lists {
        let source = (*source_name).to_string();
        for (rank, id) in list.iter().enumerate() {
            let entry = scores
                .entry(id.clone())
                .or_insert_with(|| (0.0, Vec::new()));
            entry.0 += 1.0 / (k + rank as f64 + 1.0);
            if !entry.1.iter().any(|s| s == source_name) {
                entry.1.push(source.clone());
            }
        }
    }

    let mut results: Vec<(String, f64, Vec<String>)> = scores
        .into_iter()
        .map(|(id, (score, sources))| (id, score, sources))
        .collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Run hybrid search: FTS5 keyword + vector KNN, merged with RRF.
///
/// When `kind_filter` is set, results are filtered before applying `limit`,
/// so the caller always gets up to `limit` results of the requested kind.
pub fn hybrid_search(
    db: &Database,
    query: &str,
    limit: u32,
    kind_filter: Option<SymbolKind>,
) -> Result<HybridSearchResult> {
    let retrieval_limit = (limit * 3).max(20); // Over-retrieve for better merge

    // 1. FTS5 keyword search
    let fts_results = fts5_search_safe(db, query, retrieval_limit)?;
    let fts_count = fts_results.len() as u32;

    // 2. Vector search (if embeddings exist in the DB)
    let vec_results = if db.embedding_count()? > 0 {
        vector_search(db, query, retrieval_limit)?
    } else {
        Vec::new()
    };
    let vec_count = vec_results.len() as u32;

    // 3. RRF merge
    let ranked_lists: Vec<(&str, Vec<String>)> =
        vec![("fts5", fts_results), ("vector", vec_results)];
    let merged = rrf_merge(&ranked_lists, 60.0);
    let merged_count = merged.len() as u32;

    // 4. Hydrate all merged candidates with symbol data + content.
    let candidate_ids: Vec<String> = merged.iter().map(|(id, _, _)| id.clone()).collect();

    let symbols = db.get_symbols_by_ids(&candidate_ids)?;

    let score_map: HashMap<&str, (f64, &Vec<String>)> = merged
        .iter()
        .map(|(id, score, sources)| (id.as_str(), (*score, sources)))
        .collect();

    let symbol_map: HashMap<&str, &Symbol> = symbols.iter().map(|s| (s.id.as_str(), s)).collect();

    let empty_sources = Vec::new();
    let mut candidates: Vec<SearchResult> = Vec::new();
    for id in &candidate_ids {
        if let Some(sym) = symbol_map.get(id.as_str()) {
            let (score, sources) = score_map
                .get(id.as_str())
                .copied()
                .unwrap_or((0.0, &empty_sources));

            let content = db.get_symbol_content(id)?.map(|(c, _)| c);

            candidates.push(SearchResult {
                symbol: (*sym).clone(),
                content,
                rrf_score: score,
                rerank_score: None,
                sources: sources.clone(),
            });
        }
    }

    // 5. Cross-encoder re-ranking (if model is available).
    //    Cap at 50 candidates to bound latency.
    const RERANK_MAX: usize = 50;
    let rerank_slice = if candidates.len() > RERANK_MAX {
        &mut candidates[..RERANK_MAX]
    } else {
        &mut candidates[..]
    };
    with_reranker_engine(|engine| {
        rerank_candidates(engine, query, rerank_slice);
    });

    // 6. Apply kind filter + limit on (re-ranked) candidates.
    let mut results = Vec::new();
    for candidate in candidates {
        if results.len() >= limit as usize {
            break;
        }
        if let Some(ref filter) = kind_filter {
            if &candidate.symbol.kind != filter {
                continue;
            }
        }
        results.push(candidate);
    }

    Ok(HybridSearchResult {
        results,
        fts_count,
        vec_count,
        merged_count,
    })
}

/// Re-rank candidates in place using a cross-encoder.
///
/// Batches all (query, content) pairs for a single ONNX inference call,
/// then re-sorts by cross-encoder score descending.
/// Candidates without content retain their original order at the end.
fn rerank_candidates(
    engine: &mut CrossEncoderEngine,
    query: &str,
    candidates: &mut [SearchResult],
) {
    // Collect indices of candidates that have content (no cloning).
    let scoreable_indices: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.content.as_ref().map(|_| i))
        .collect();

    if scoreable_indices.is_empty() {
        return;
    }

    // Build doc refs from the candidates' content (borrow, not clone).
    let docs: Vec<&str> = scoreable_indices
        .iter()
        .map(|&i| candidates[i].content.as_deref().unwrap())
        .collect();

    match engine.score_batch(query, &docs) {
        Ok(scores) => {
            for (&idx, score) in scoreable_indices.iter().zip(scores.iter()) {
                candidates[idx].rerank_score = Some(*score as f64);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Cross-encoder batch scoring failed, keeping RRF order");
        }
    }

    // Stable sort: candidates with rerank_score come first (sorted by score desc),
    // then candidates without score (in original RRF order).
    candidates.sort_by(|a, b| match (a.rerank_score, b.rerank_score) {
        (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
}

/// FTS5 search with safe query escaping.
///
/// Tries three strategies in order, returning the first non-empty result:
/// 1. **Phrase**: `"validate token"` — exact adjacent match (highest precision)
/// 2. **AND**: `"validate" AND "token"` — all terms present (good precision)
/// 3. **OR**: `"validate" OR "token"` — any term present (highest recall, lowest precision)
///
/// Only FTS5 syntax errors trigger fallback; real DB errors are propagated.
fn fts5_search_safe(db: &Database, query: &str, limit: u32) -> Result<Vec<String>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // 1. Phrase search (exact adjacency)
    let phrase_query = format!("\"{}\"", query.replace('"', "\"\""));
    match db.fts5_search(&phrase_query, limit) {
        Ok(results) if !results.is_empty() => return Ok(results),
        Err(e) if !is_fts5_syntax_error(&e) => return Err(e),
        _ => {}
    }

    // 2. AND search (all terms present, any order)
    if terms.len() > 1 {
        let and_query = terms.join(" AND ");
        match db.fts5_search(&and_query, limit) {
            Ok(results) if !results.is_empty() => return Ok(results),
            Err(e) if !is_fts5_syntax_error(&e) => return Err(e),
            _ => {}
        }
    }

    // 3. OR search (any term present — broadest, lowest precision)
    let or_query = terms.join(" OR ");
    match db.fts5_search(&or_query, limit) {
        Ok(results) => Ok(results),
        Err(e) if !is_fts5_syntax_error(&e) => Err(e),
        _ => Ok(Vec::new()),
    }
}

/// Check if an error is an FTS5 query syntax error (expected, safe to retry).
fn is_fts5_syntax_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("fts5") || msg.contains("syntax") || msg.contains("parse")
}

/// Vector search: embed the query and find nearest neighbors.
fn vector_search(db: &Database, query: &str, limit: u32) -> Result<Vec<String>> {
    let query_embedding = with_embedding_engine(|engine| engine.embed(query))?;
    let query_bytes = embedding_to_bytes(&query_embedding);

    let nn_results = db.vector_search(&query_bytes, limit)?;

    // Map embedding IDs back to symbol IDs
    let embedding_ids: Vec<i64> = nn_results.iter().map(|(id, _)| *id).collect();
    let id_map = db.symbol_ids_for_embeddings(&embedding_ids)?;
    let id_lookup: HashMap<i64, String> = id_map.into_iter().collect();

    // Preserve distance ordering
    let symbol_ids: Vec<String> = nn_results
        .iter()
        .filter_map(|(eid, _)| id_lookup.get(eid).cloned())
        .collect();

    Ok(symbol_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SymbolKind;

    /// Create a symbol + content pair and insert into the database.
    fn insert_symbol_with_content(
        db: &Database,
        name: &str,
        kind: SymbolKind,
        file: &str,
        line: u32,
        content: &str,
    ) -> Symbol {
        let sym = Symbol::new(name, kind, file, line, line + 10, 0, content.len() as u32);
        db.insert_symbol(&sym).unwrap();
        let header = format!("// File: {file} | {kind} {name}", kind = sym.kind);
        db.upsert_symbol_content(&sym.id, name, content, &header)
            .unwrap();
        sym
    }

    // ── RRF merge unit tests ──

    #[test]
    fn test_rrf_merge_single_list() {
        let list = vec![(
            "fts5",
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        )];
        let merged = rrf_merge(&list, 60.0);

        assert_eq!(merged.len(), 3);
        // First item should have highest score
        assert_eq!(merged[0].0, "a");
        assert!(merged[0].1 > merged[1].1);
        assert!(merged[1].1 > merged[2].1);
    }

    #[test]
    fn test_rrf_merge_two_lists() {
        let lists = vec![
            (
                "fts5",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            (
                "vec",
                vec!["b".to_string(), "d".to_string(), "a".to_string()],
            ),
        ];
        let merged = rrf_merge(&lists, 60.0);

        // "b" appears rank 1 in fts5 + rank 0 in vec → highest combined score
        // "a" appears rank 0 in fts5 + rank 2 in vec
        assert_eq!(merged[0].0, "b"); // rank 1 + rank 0
        assert_eq!(merged[1].0, "a"); // rank 0 + rank 2

        // Check sources
        let b = merged.iter().find(|(id, _, _)| id == "b").unwrap();
        assert!(b.2.contains(&"fts5".to_string()));
        assert!(b.2.contains(&"vec".to_string()));
    }

    #[test]
    fn test_rrf_merge_no_overlap() {
        let lists = vec![
            ("fts5", vec!["a".to_string(), "b".to_string()]),
            ("vec", vec!["c".to_string(), "d".to_string()]),
        ];
        let merged = rrf_merge(&lists, 60.0);

        assert_eq!(merged.len(), 4);
        // Items at rank 0 should tie, then rank 1 should tie
        let scores: Vec<f64> = merged.iter().map(|(_, s, _)| *s).collect();
        assert!((scores[0] - scores[1]).abs() < f64::EPSILON);
        assert!((scores[2] - scores[3]).abs() < f64::EPSILON);
    }

    #[test]
    fn test_rrf_merge_empty() {
        let lists: Vec<(&str, Vec<String>)> = vec![("fts5", vec![]), ("vec", vec![])];
        let merged = rrf_merge(&lists, 60.0);
        assert!(merged.is_empty());
    }

    // ── hybrid_search integration tests (FTS5-only, no model needed) ──
    //
    // These tests populate an in-memory DB with realistic code symbols and assert
    // on ranking order, precision, and edge cases. They serve as regression baselines:
    // if you change the search pipeline, failing tests indicate a quality change.

    /// Shared corpus: a realistic mix of symbols across a Python codebase.
    /// Used by multiple tests to verify ranking against a consistent dataset.
    fn seed_python_corpus(db: &Database) {
        insert_symbol_with_content(
            db,
            "AuthService",
            SymbolKind::Class,
            "auth/service.py",
            1,
            "class AuthService:\n    def authenticate(self, username, password):\n        token = generate_token(username)\n        return token",
        );
        insert_symbol_with_content(
            db,
            "validate_token",
            SymbolKind::Function,
            "auth/tokens.py",
            10,
            "def validate_token(token: str) -> bool:\n    if token.is_expired():\n        raise TokenError('expired')\n    return True",
        );
        insert_symbol_with_content(
            db,
            "generate_token",
            SymbolKind::Function,
            "auth/tokens.py",
            20,
            "def generate_token(username: str) -> str:\n    payload = {'sub': username}\n    return jwt.encode(payload, SECRET_KEY)",
        );
        insert_symbol_with_content(
            db,
            "UserRepository",
            SymbolKind::Class,
            "models/user.py",
            1,
            "class UserRepository:\n    def find_by_email(self, email: str) -> User:\n        return self.db.query(User).filter(email=email).first()",
        );
        insert_symbol_with_content(
            db,
            "send_email",
            SymbolKind::Function,
            "notifications/email.py",
            5,
            "def send_email(to: str, subject: str, body: str) -> None:\n    smtp = connect_smtp()\n    smtp.send(to, subject, body)",
        );
    }

    // ── Per-language smoke tests ──

    #[test]
    fn test_hybrid_search_python_ranking() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        // "validate token" should rank validate_token #1 (both terms in name+content)
        let result = hybrid_search(&db, "validate token", 10, None).unwrap();
        assert!(result.fts_count > 0, "FTS5 should find results");
        assert_eq!(result.vec_count, 0, "no embeddings → no vector results");
        assert_eq!(result.results[0].symbol.name, "validate_token");
        assert!(result.results[0].sources.contains(&"fts5".to_string()));

        // generate_token matches "token" but NOT "validate" — must rank below validate_token
        if let Some(gen_pos) = result
            .results
            .iter()
            .position(|r| r.symbol.name == "generate_token")
        {
            assert!(
                gen_pos > 0,
                "generate_token should rank below validate_token"
            );
        }

        // "authenticate" should find AuthService (content match)
        let result = hybrid_search(&db, "authenticate", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "AuthService");

        // send_email should NOT appear for an auth-related query
        let names: Vec<&str> = result
            .results
            .iter()
            .map(|r| r.symbol.name.as_str())
            .collect();
        assert!(
            !names.contains(&"send_email"),
            "unrelated symbol should not appear for 'authenticate'"
        );
    }

    #[test]
    fn test_hybrid_search_typescript_ranking() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "UserService",
            SymbolKind::Class,
            "src/services/user.ts",
            1,
            "export class UserService {\n  async findById(id: string): Promise<User> {\n    return this.repository.findOne(id);\n  }\n}",
        );
        insert_symbol_with_content(
            &db,
            "createRouter",
            SymbolKind::Function,
            "src/routes/index.ts",
            5,
            "export function createRouter(app: Express): Router {\n  const router = Router();\n  router.get('/users', listUsers);\n  return router;\n}",
        );
        insert_symbol_with_content(
            &db,
            "DatabaseConnection",
            SymbolKind::Class,
            "src/db/connection.ts",
            1,
            "export class DatabaseConnection {\n  private pool: Pool;\n  async connect(config: DbConfig): Promise<void> {\n    this.pool = await createPool(config);\n  }\n}",
        );

        // "connect" matches DatabaseConnection's content; the others don't mention "connect"
        let result = hybrid_search(&db, "connect", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "DatabaseConnection");
        assert_eq!(
            result.results.len(),
            1,
            "only DatabaseConnection contains 'connect'"
        );

        // "router" should rank createRouter #1
        let result = hybrid_search(&db, "router", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "createRouter");
    }

    #[test]
    fn test_hybrid_search_rust_ranking() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "extract",
            SymbolKind::Method,
            "src/languages/python.rs",
            15,
            "fn extract(&self, source: &str, file_path: &str) -> Result<ExtractionResult> {\n    let tree = self.parser.parse(source)?;\n    let mut symbols = Vec::new();\n    walk_tree(&tree, &mut symbols);\n    Ok(ExtractionResult { symbols, edges: vec![] })\n}",
        );
        insert_symbol_with_content(
            &db,
            "Database",
            SymbolKind::Class,
            "src/db.rs",
            20,
            "pub struct Database {\n    conn: Connection,\n}\nimpl Database {\n    pub fn open(path: impl AsRef<Path>) -> Result<Self> {\n        let conn = Connection::open(path)?;\n        Ok(Self { conn })\n    }\n}",
        );
        insert_symbol_with_content(
            &db,
            "resolve_edges",
            SymbolKind::Method,
            "src/db.rs",
            100,
            "pub fn resolve_edges(&self) -> Result<u32> {\n    // Match target_name to symbols: same file > same dir > unique project match\n    let mut resolved = 0;\n    resolved\n}",
        );

        // "extract symbols" — both terms in extract's content; Database/resolve_edges don't have "extract"
        let result = hybrid_search(&db, "extract symbols", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "extract");

        // "resolve edges" — only resolve_edges has both terms
        let result = hybrid_search(&db, "resolve edges", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "resolve_edges");

        // "Database" should not return extract or resolve_edges as #1
        let result = hybrid_search(&db, "Database", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "Database");
    }

    #[test]
    fn test_hybrid_search_go_ranking() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "HandleRequest",
            SymbolKind::Function,
            "handlers/auth.go",
            10,
            "func HandleRequest(w http.ResponseWriter, r *http.Request) {\n\ttoken := r.Header.Get(\"Authorization\")\n\tif !ValidateToken(token) {\n\t\thttp.Error(w, \"unauthorized\", 401)\n\t}\n}",
        );
        insert_symbol_with_content(
            &db,
            "Repository",
            SymbolKind::Class,
            "models/repository.go",
            5,
            "type Repository struct {\n\tdb *sql.DB\n}\n\nfunc (r *Repository) FindByID(id string) (*User, error) {\n\trow := r.db.QueryRow(\"SELECT * FROM users WHERE id = ?\", id)\n\treturn scanUser(row)\n}",
        );

        // "handle request" — HandleRequest has both terms in name+content
        let result = hybrid_search(&db, "handle request", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "HandleRequest");

        // Repository should not appear for "handle request" (no shared terms)
        let names: Vec<&str> = result
            .results
            .iter()
            .map(|r| r.symbol.name.as_str())
            .collect();
        assert!(!names.contains(&"Repository"));
    }

    #[test]
    fn test_hybrid_search_ruby_ranking() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "SessionManager",
            SymbolKind::Class,
            "lib/session_manager.rb",
            1,
            "class SessionManager\n  def initialize(store)\n    @store = store\n  end\n\n  def create_session(user)\n    token = SecureRandom.hex(32)\n    @store.set(token, user.id)\n    token\n  end\nend",
        );
        insert_symbol_with_content(
            &db,
            "migrate",
            SymbolKind::Method,
            "db/migrate.rb",
            5,
            "def migrate(version:)\n  pending = migrations.select { |m| m.version > version }\n  pending.each { |m| m.up }\nend",
        );

        // "session" — SessionManager has it in name+content, migrate doesn't
        let result = hybrid_search(&db, "session", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "SessionManager");
        let names: Vec<&str> = result
            .results
            .iter()
            .map(|r| r.symbol.name.as_str())
            .collect();
        assert!(
            !names.contains(&"migrate"),
            "unrelated symbol should not appear"
        );

        // "migrate" — exact name match
        let result = hybrid_search(&db, "migrate", 10, None).unwrap();
        assert_eq!(result.results[0].symbol.name, "migrate");
    }

    // ── Precision and ranking tests ──

    #[test]
    fn test_ranking_relevant_above_irrelevant() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        // "token" appears in validate_token and generate_token content, NOT in send_email
        let result = hybrid_search(&db, "token", 10, None).unwrap();
        let names: Vec<&str> = result
            .results
            .iter()
            .map(|r| r.symbol.name.as_str())
            .collect();
        assert!(
            names.contains(&"validate_token"),
            "validate_token should appear for 'token'"
        );
        assert!(
            names.contains(&"generate_token"),
            "generate_token should appear for 'token'"
        );
        assert!(
            !names.contains(&"send_email"),
            "send_email should NOT appear for 'token'"
        );
    }

    #[test]
    fn test_ranking_multi_term_beats_single_term() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        // "validate token" as a phrase matches validate_token exactly (FTS5 splits
        // underscores into separate tokens). generate_token doesn't match the phrase
        // because "validate" is not in its content.
        let result = hybrid_search(&db, "validate token", 10, None).unwrap();
        assert_eq!(
            result.results[0].symbol.name, "validate_token",
            "symbol matching both terms as phrase should rank #1"
        );

        // Now test OR ranking: "generate token" — generate_token and AuthService both
        // contain "generate" and "token". Both should appear in top results.
        let result = hybrid_search(&db, "generate token", 10, None).unwrap();
        let top_names: Vec<&str> = result
            .results
            .iter()
            .take(3)
            .map(|r| r.symbol.name.as_str())
            .collect();
        assert!(
            top_names.contains(&"generate_token"),
            "generate_token should be in top 3 for 'generate token', got: {top_names:?}"
        );
        // validate_token should also appear (has "token") but ranked lower
        if let Some(val) = result
            .results
            .iter()
            .find(|r| r.symbol.name == "validate_token")
        {
            assert!(
                result.results[0].rrf_score >= val.rrf_score,
                "phrase match should score >= single-term match"
            );
        }
    }

    // ── FTS5 normalized name tests ──
    //
    // The normalized_name column in the FTS5 index splits camelCase/PascalCase/snake_case
    // into individual words, enabling keyword matching across naming conventions.

    #[test]
    fn test_fts5_camel_case_matches_via_normalized_name() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "DatabaseConnection",
            SymbolKind::Class,
            "db.ts",
            1,
            "export class DatabaseConnection { }",
        );

        // "database" matches via normalized_name column ("database connection")
        let result = hybrid_search(&db, "database", 10, None).unwrap();
        assert_eq!(
            result.results.len(),
            1,
            "normalized_name should split PascalCase — 'database' should match 'DatabaseConnection'"
        );
        assert_eq!(result.results[0].symbol.name, "DatabaseConnection");
    }

    #[test]
    fn test_fts5_camel_case_multi_term() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "validateToken",
            SymbolKind::Function,
            "auth.ts",
            1,
            "function validateToken(t: string) { }",
        );
        insert_symbol_with_content(
            &db,
            "generateToken",
            SymbolKind::Function,
            "auth.ts",
            10,
            "function generateToken(user: string) { }",
        );

        // "validate token" as phrase matches normalized_name "validate token" exactly
        let result = hybrid_search(&db, "validate token", 10, None).unwrap();
        assert!(
            !result.results.is_empty(),
            "phrase 'validate token' should match validateToken via normalized_name"
        );
        assert_eq!(result.results[0].symbol.name, "validateToken");
    }

    #[test]
    fn test_fts5_screaming_snake_case_matches() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "TOKEN_EXPIRY",
            SymbolKind::Variable,
            "config.py",
            1,
            "TOKEN_EXPIRY = 3600",
        );

        let result = hybrid_search(&db, "token expiry", 10, None).unwrap();
        assert_eq!(
            result.results.len(),
            1,
            "normalized_name should split SCREAMING_SNAKE — 'token expiry' should match 'TOKEN_EXPIRY'"
        );
    }

    #[test]
    fn test_fts5_limitation_no_substring_match() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "validate_token",
            SymbolKind::Function,
            "auth.py",
            1,
            "def validate_token(token): pass",
        );

        // FTS5 is token-based, not substring-based.
        // "valid" does NOT match "validate" or "validate_token".
        // Use `cartog search` for substring matching.
        let result = hybrid_search(&db, "valid", 10, None).unwrap();
        assert!(
            result.results.is_empty(),
            "FTS5 does not do substring matching — 'valid' should not match 'validate_token'. \
             Use `cartog search` for substring matching."
        );
    }

    // ── AND fallback test ──

    #[test]
    fn test_fts5_and_fallback_non_adjacent_terms() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "process_request",
            SymbolKind::Function,
            "server.py",
            1,
            "def process_request(req):\n    validated = validate(req)\n    response = build_response(validated)\n    return response",
        );
        insert_symbol_with_content(
            &db,
            "build_response",
            SymbolKind::Function,
            "server.py",
            10,
            "def build_response(data):\n    return Response(data=data, status=200)",
        );

        // "validate response" — no symbol has these words adjacent (phrase won't match).
        // AND fallback: process_request has both "validate" and "response" in content.
        // build_response has only "response" — should rank below process_request.
        let result = hybrid_search(&db, "validate response", 10, None).unwrap();
        assert!(
            !result.results.is_empty(),
            "AND fallback should find results"
        );
        assert_eq!(
            result.results[0].symbol.name, "process_request",
            "symbol containing both terms should rank #1 via AND fallback"
        );
    }

    // ── Kind filter test ──

    #[test]
    fn test_hybrid_search_kind_filter() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        // Without filter: "token" matches functions and possibly classes
        let all = hybrid_search(&db, "token", 10, None).unwrap();
        assert!(all.results.len() >= 2);

        // With kind=Function filter: only functions returned, still respects limit
        let funcs = hybrid_search(&db, "token", 10, Some(SymbolKind::Function)).unwrap();
        for r in &funcs.results {
            assert_eq!(r.symbol.kind, SymbolKind::Function);
        }

        // With kind=Class: AuthService mentions "token" in content
        let classes = hybrid_search(&db, "token", 10, Some(SymbolKind::Class)).unwrap();
        for r in &classes.results {
            assert_eq!(r.symbol.kind, SymbolKind::Class);
        }
    }

    #[test]
    fn test_kind_filter_respects_limit() {
        let db = Database::open_memory().unwrap();
        // Insert 5 functions and 5 classes, all mentioning "handler"
        for i in 0..5 {
            insert_symbol_with_content(
                &db,
                &format!("handle_func_{i}"),
                SymbolKind::Function,
                "handlers.py",
                i * 20,
                &format!("def handle_func_{i}(request): return handler_response({i})"),
            );
            insert_symbol_with_content(
                &db,
                &format!("HandlerClass{i}"),
                SymbolKind::Class,
                "handlers.py",
                i * 20 + 10,
                &format!(
                    "class HandlerClass{i}:\n    def handle(self): return handler_result({i})"
                ),
            );
        }

        // Request 3 functions — should get exactly 3 despite 10 total matches
        let result = hybrid_search(&db, "handler", 3, Some(SymbolKind::Function)).unwrap();
        assert_eq!(
            result.results.len(),
            3,
            "kind filter + limit should return exactly 3"
        );
        for r in &result.results {
            assert_eq!(r.symbol.kind, SymbolKind::Function);
        }
    }

    // ── Cross-language test ──

    #[test]
    fn test_hybrid_search_cross_language() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "validate",
            SymbolKind::Function,
            "auth.py",
            1,
            "def validate(token: str) -> bool:\n    return check_signature(token)",
        );
        insert_symbol_with_content(
            &db,
            "validate",
            SymbolKind::Function,
            "src/auth.ts",
            1,
            "export function validate(token: string): boolean {\n  return checkSignature(token);\n}",
        );
        insert_symbol_with_content(
            &db,
            "validate",
            SymbolKind::Function,
            "auth.go",
            1,
            "func validate(token string) bool {\n\treturn checkSignature(token)\n}",
        );

        let result = hybrid_search(&db, "validate", 10, None).unwrap();
        assert_eq!(
            result.results.len(),
            3,
            "should find validate in all 3 languages"
        );
        for r in &result.results {
            assert_eq!(r.symbol.name, "validate");
        }
    }

    // ── Edge cases ──

    #[test]
    fn test_hybrid_search_no_results() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "foo",
            SymbolKind::Function,
            "a.py",
            1,
            "def foo(): pass",
        );

        let result = hybrid_search(&db, "zzz_nonexistent_term", 10, None).unwrap();
        assert!(result.results.is_empty());
        assert_eq!(result.fts_count, 0);
        assert_eq!(result.vec_count, 0);
    }

    #[test]
    fn test_hybrid_search_content_returned() {
        let db = Database::open_memory().unwrap();
        let content = "def greet(name: str) -> str:\n    return f'Hello, {name}!'";
        insert_symbol_with_content(&db, "greet", SymbolKind::Function, "hello.py", 1, content);

        let result = hybrid_search(&db, "greet", 10, None).unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].content.as_deref(), Some(content));
    }

    #[test]
    fn test_hybrid_search_respects_limit() {
        let db = Database::open_memory().unwrap();
        for i in 0..10 {
            insert_symbol_with_content(
                &db,
                &format!("handler_{i}"),
                SymbolKind::Function,
                "handlers.py",
                i * 15,
                &format!("def handler_{i}(request):\n    return response(handler={i})"),
            );
        }

        let result = hybrid_search(&db, "handler", 3, None).unwrap();
        assert_eq!(
            result.results.len(),
            3,
            "should return exactly limit results"
        );
        assert!(result.fts_count > 3, "FTS should over-retrieve");
    }

    // ── Rerank sorting tests ──

    fn make_result(
        name: &str,
        rrf: f64,
        rerank: Option<f64>,
        content: Option<&str>,
    ) -> SearchResult {
        SearchResult {
            symbol: Symbol::new(name, SymbolKind::Function, "test.py", 1, 10, 0, 100),
            content: content.map(|s| s.to_string()),
            rrf_score: rrf,
            rerank_score: rerank,
            sources: vec!["fts5".to_string()],
        }
    }

    #[test]
    fn test_rerank_sort_reorders_by_score_descending() {
        let mut candidates = [
            make_result("low", 0.9, Some(1.0), Some("low content")),
            make_result("high", 0.5, Some(9.0), Some("high content")),
            make_result("mid", 0.7, Some(5.0), Some("mid content")),
        ];

        // Simulate what rerank_candidates does after scoring: just sort
        candidates.sort_by(|a, b| match (a.rerank_score, b.rerank_score) {
            (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        assert_eq!(candidates[0].symbol.name, "high");
        assert_eq!(candidates[1].symbol.name, "mid");
        assert_eq!(candidates[2].symbol.name, "low");
    }

    #[test]
    fn test_rerank_sort_scored_before_unscored() {
        let mut candidates = [
            make_result("no_content", 0.9, None, None),
            make_result("scored", 0.3, Some(2.0), Some("content")),
            make_result("also_no_content", 0.8, None, None),
        ];

        candidates.sort_by(|a, b| match (a.rerank_score, b.rerank_score) {
            (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        assert_eq!(candidates[0].symbol.name, "scored");
        // Unscored maintain relative order (stable sort)
        assert_eq!(candidates[1].symbol.name, "no_content");
        assert_eq!(candidates[2].symbol.name, "also_no_content");
    }

    #[test]
    fn test_rerank_sort_all_unscored_preserves_order() {
        let mut candidates = [
            make_result("first", 0.9, None, None),
            make_result("second", 0.5, None, None),
            make_result("third", 0.3, None, None),
        ];

        candidates.sort_by(|a, b| match (a.rerank_score, b.rerank_score) {
            (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        // No rerank scores → original (RRF) order preserved
        assert_eq!(candidates[0].symbol.name, "first");
        assert_eq!(candidates[1].symbol.name, "second");
        assert_eq!(candidates[2].symbol.name, "third");
    }

    #[test]
    fn test_hybrid_search_rerank_score_consistency() {
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "process_data",
            SymbolKind::Function,
            "data.py",
            1,
            "def process_data(items):\n    return [transform(i) for i in items]",
        );

        let result = hybrid_search(&db, "process data", 10, None).unwrap();
        assert!(!result.results.is_empty());

        // Re-ranking depends on whether the cross-encoder model is downloadable.
        // In CI / offline environments, rerank_score will be None.
        // In environments with the model, results with content will have a rerank_score.
        let has_rerank = result.results.iter().any(|r| r.rerank_score.is_some());
        if has_rerank {
            for r in &result.results {
                if r.content.is_some() {
                    assert!(
                        r.rerank_score.is_some(),
                        "rerank_score should be set when cross-encoder is available"
                    );
                }
            }
        } else {
            for r in &result.results {
                assert!(
                    r.rerank_score.is_none(),
                    "rerank_score should be None without cross-encoder model"
                );
            }
        }
    }
}
