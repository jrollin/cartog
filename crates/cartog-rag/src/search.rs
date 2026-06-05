use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use cartog_core::{Symbol, SymbolKind};
use cartog_db::Database;

/// Filter for symbol kinds in search results.
#[derive(Debug, Clone)]
pub enum KindFilter {
    /// Return only symbols of this specific kind.
    Exact(SymbolKind),
    /// Return all symbols including documents.
    All,
    /// Return code symbols only (exclude documents). Default for rag search.
    CodeOnly,
}

impl KindFilter {
    /// Lower to the DB-layer [`KindScope`] for kind-biased retrieval.
    fn scope(&self) -> KindScope {
        match self {
            KindFilter::Exact(k) => KindScope::Exact(*k),
            KindFilter::All => KindScope::All,
            KindFilter::CodeOnly => KindScope::CodeOnly,
        }
    }
}

use super::provider::{embedding_to_bytes, EmbeddingProvider, RerankerProvider};
use cartog_db::KindScope;

/// Retrieval method that surfaced a search result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Keyword match via the FTS5 (BM25) index.
    Fts5,
    /// Semantic match via vector similarity.
    Vector,
}

impl Source {
    /// Map an internal ranked-list label to a [`Source`], if recognized.
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "fts5" => Some(Self::Fts5),
            "vector" => Some(Self::Vector),
            _ => None,
        }
    }

    /// Wire label for this source, matching the serialized form.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fts5 => "fts5",
            Self::Vector => "vector",
        }
    }
}

/// A search result combining symbol metadata with relevance info.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct SearchResult {
    pub symbol: Symbol,
    pub content: Option<String>,
    pub rrf_score: f64,
    /// Cross-encoder re-ranking score (higher = more relevant). Present only when
    /// the cross-encoder model is available.
    pub rerank_score: Option<f64>,
    /// Which retrieval methods found this result.
    pub sources: Vec<Source>,
}

/// Result of a hybrid search operation.
#[derive(Debug, Serialize, schemars::JsonSchema)]
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

/// Tunable parameters for the hybrid-search pipeline. Defaults match the
/// original hard-coded values so passing `SearchTuning::default()` preserves
/// historical behavior.
#[derive(Debug, Clone, Copy)]
pub struct SearchTuning {
    /// Over-retrieval multiplier. FTS and vector stages fetch
    /// `max(limit * retrieval_multiplier, retrieval_floor)` candidates so
    /// RRF + reranking have enough signal after filtering.
    pub retrieval_multiplier: u32,
    /// Minimum number of candidates to retrieve regardless of `limit`.
    pub retrieval_floor: u32,
    /// Maximum number of candidates scored by the cross-encoder. Bounds worst-
    /// case latency on wide queries.
    pub rerank_max: u32,
    /// Skip the cross-encoder entirely if fewer than this many candidates
    /// survived RRF merge. Keeps small-corpus queries fast.
    pub rerank_min: u32,
}

impl Default for SearchTuning {
    fn default() -> Self {
        Self {
            retrieval_multiplier: 3,
            retrieval_floor: 20,
            rerank_max: 50,
            rerank_min: 8,
        }
    }
}

/// Run hybrid search with default tuning. See [`hybrid_search_tuned`] for knobs.
pub fn hybrid_search<E: EmbeddingProvider + ?Sized>(
    db: &Database,
    query: &str,
    limit: u32,
    kind_filter: KindFilter,
    embedding_provider: &mut E,
    reranker: Option<&mut dyn RerankerProvider>,
) -> Result<HybridSearchResult> {
    hybrid_search_tuned(
        db,
        query,
        limit,
        kind_filter,
        embedding_provider,
        reranker,
        &SearchTuning::default(),
    )
}

/// Run hybrid search: FTS5 keyword + vector KNN, merged with RRF.
///
/// When `kind_filter` is set, results are filtered before applying `limit`,
/// so the caller always gets up to `limit` results of the requested kind.
/// `tuning` lets the caller override retrieval/rerank thresholds from config.
///
/// Eager reranker variant: the caller pre-loads the cross-encoder model.
/// Useful for long-lived processes (MCP server) that warm the reranker once
/// at startup and reuse it across many queries. For one-shot CLI commands
/// that may not need the reranker at all, prefer [`hybrid_search_tuned_lazy`]
/// which defers the ONNX model load until it knows it's actually needed.
pub fn hybrid_search_tuned<E: EmbeddingProvider + ?Sized>(
    db: &Database,
    query: &str,
    limit: u32,
    kind_filter: KindFilter,
    embedding_provider: &mut E,
    reranker: Option<&mut dyn RerankerProvider>,
    tuning: &SearchTuning,
) -> Result<HybridSearchResult> {
    let retrieval_limit = limit
        .saturating_mul(tuning.retrieval_multiplier)
        .max(tuning.retrieval_floor);

    // Bias retrieval to the wanted kind so the budget isn't all docs.
    let scope = kind_filter.scope();
    let fts_results = fts5_search_safe(db, query, retrieval_limit, scope)?;
    let fts_count = fts_results.len() as u32;

    let vec_results = if db.embedding_count()? > 0 {
        vector_search(db, query, retrieval_limit, scope, embedding_provider)?
    } else {
        Vec::new()
    };
    let vec_count = vec_results.len() as u32;

    let ranked_lists: Vec<(&str, Vec<String>)> =
        vec![("fts5", fts_results), ("vector", vec_results)];
    let merged = rrf_merge(&ranked_lists, 60.0);
    let merged_count = merged.len() as u32;

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
                sources: sources
                    .iter()
                    .filter_map(|s| Source::from_label(s))
                    .collect(),
            });
        }
    }

    let rerank_cap = tuning.rerank_max as usize;
    let rerank_min = tuning.rerank_min as usize;
    let rerank_slice = if candidates.len() > rerank_cap {
        &mut candidates[..rerank_cap]
    } else {
        &mut candidates[..]
    };
    match reranker {
        Some(r) if rerank_slice.len() >= rerank_min => {
            rerank_candidates(r, query, rerank_slice);
        }
        _ => {}
    }

    Ok(sort_filter_and_pack(
        candidates,
        (fts_count, vec_count, merged_count),
        limit,
        kind_filter,
    ))
}

/// Lazy variant: the reranker provider is constructed on demand, but only
/// when retrieval has produced at least `tuning.rerank_min` candidates.
/// Avoids loading the ONNX cross-encoder model (~100-200ms + memory) when
/// the result set is already too small to benefit from reranking.
pub fn hybrid_search_tuned_lazy<E, F>(
    db: &Database,
    query: &str,
    limit: u32,
    kind_filter: KindFilter,
    embedding_provider: &mut E,
    reranker_factory: Option<F>,
    tuning: &SearchTuning,
) -> Result<HybridSearchResult>
where
    E: EmbeddingProvider + ?Sized,
    F: FnOnce() -> Option<Box<dyn RerankerProvider>>,
{
    let retrieval_limit = limit
        .saturating_mul(tuning.retrieval_multiplier)
        .max(tuning.retrieval_floor);

    // Bias retrieval to the wanted kind so the budget isn't all docs.
    let scope = kind_filter.scope();
    let fts_results = fts5_search_safe(db, query, retrieval_limit, scope)?;
    let fts_count = fts_results.len() as u32;

    let vec_results = if db.embedding_count()? > 0 {
        vector_search(db, query, retrieval_limit, scope, embedding_provider)?
    } else {
        Vec::new()
    };
    let vec_count = vec_results.len() as u32;

    let ranked_lists: Vec<(&str, Vec<String>)> =
        vec![("fts5", fts_results), ("vector", vec_results)];
    let merged = rrf_merge(&ranked_lists, 60.0);
    let merged_count = merged.len() as u32;

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
                sources: sources
                    .iter()
                    .filter_map(|s| Source::from_label(s))
                    .collect(),
            });
        }
    }

    let rerank_cap = tuning.rerank_max as usize;
    let rerank_min = tuning.rerank_min as usize;
    let rerank_slice = if candidates.len() > rerank_cap {
        &mut candidates[..rerank_cap]
    } else {
        &mut candidates[..]
    };
    // Only touch the factory once we know we'd actually use its output.
    let maybe_reranker = reranker_factory
        .filter(|_| rerank_slice.len() >= rerank_min)
        .and_then(|f| f());
    if let Some(mut reranker) = maybe_reranker {
        rerank_candidates(reranker.as_mut(), query, rerank_slice);
    }

    Ok(sort_filter_and_pack(
        candidates,
        (fts_count, vec_count, merged_count),
        limit,
        kind_filter,
    ))
}

/// Shared tail of the two `hybrid_search_tuned*` entry points: sort by
/// (rerank_score → rrf_score → in_degree), apply `kind_filter`, truncate to
/// `limit`, pack into a `HybridSearchResult`.
/// Ranking order: rerank score desc, unscored last (fall back to RRF), ties by
/// in-degree desc. Shared with tests so they exercise the real comparator.
fn rerank_ordering(a: &SearchResult, b: &SearchResult) -> std::cmp::Ordering {
    let score_cmp = match (a.rerank_score, b.rerank_score) {
        (Some(sa), Some(sb)) => sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b
            .rrf_score
            .partial_cmp(&a.rrf_score)
            .unwrap_or(std::cmp::Ordering::Equal),
    };
    score_cmp.then(b.symbol.in_degree.cmp(&a.symbol.in_degree))
}

fn sort_filter_and_pack(
    mut candidates: Vec<SearchResult>,
    counts: (u32, u32, u32),
    limit: u32,
    kind_filter: KindFilter,
) -> HybridSearchResult {
    // 5b. Stable tiebreaker: within same score, prefer higher in-degree (more referenced).
    candidates.sort_by(rerank_ordering);

    // Filter by kind before capping to `limit` so we never return fewer than
    // `limit` qualifying results just because docs ranked above code. Reuse
    // `kind_in_scope` so this tail matches the retrieval/vector kind filter.
    let scope = kind_filter.scope();
    let results: Vec<SearchResult> = candidates
        .into_iter()
        .filter(|candidate| kind_in_scope(candidate.symbol.kind, scope))
        .take(limit as usize)
        .collect();

    HybridSearchResult {
        results,
        fts_count: counts.0,
        vec_count: counts.1,
        merged_count: counts.2,
    }
}

/// Re-rank candidates in place using a cross-encoder.
///
/// Batches all (query, content) pairs for a single ONNX inference call,
/// then re-sorts by cross-encoder score descending.
/// Candidates without content retain their original order at the end.
fn rerank_candidates(
    reranker: &mut dyn RerankerProvider,
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

    match reranker.score_batch(query, &docs) {
        Ok(scores) => {
            for (&idx, score) in scoreable_indices.iter().zip(scores.iter()) {
                candidates[idx].rerank_score = Some(*score as f64);
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Cross-encoder batch scoring failed, keeping RRF order");
        }
    }
    // Caller (hybrid_search) handles final sorting by rerank_score + RRF + in_degree.
}

/// FTS5 search with safe query escaping.
///
/// Tries three strategies in order, returning the first non-empty result:
/// 1. **Phrase**: `"validate token"` — exact adjacent match (highest precision)
/// 2. **AND**: `"validate" AND "token"` — all terms present (good precision)
/// 3. **OR**: `"validate" OR "token"` — any term present (highest recall, lowest precision)
///
/// Only FTS5 syntax errors trigger fallback; real DB errors are propagated.
fn fts5_search_safe(
    db: &Database,
    query: &str,
    limit: u32,
    scope: KindScope,
) -> Result<Vec<String>> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    // 1. Phrase search (exact adjacency)
    let phrase_query = format!("\"{}\"", query.replace('"', "\"\""));
    match db.fts5_search_kinded(&phrase_query, limit, scope) {
        Ok(results) if !results.is_empty() => return Ok(results),
        Err(e) if !is_fts5_syntax_error(&e) => return Err(e),
        _ => {}
    }

    // 2. AND search (all terms present, any order)
    if terms.len() > 1 {
        let and_query = terms.join(" AND ");
        match db.fts5_search_kinded(&and_query, limit, scope) {
            Ok(results) if !results.is_empty() => return Ok(results),
            Err(e) if !is_fts5_syntax_error(&e) => return Err(e),
            _ => {}
        }
    }

    // 3. OR search (any term present — broadest, lowest precision)
    let or_query = terms.join(" OR ");
    match db.fts5_search_kinded(&or_query, limit, scope) {
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

/// Post-filter predicate for vector hits (`vec0` KNN can't filter by kind).
fn kind_in_scope(kind: SymbolKind, scope: KindScope) -> bool {
    match scope {
        KindScope::All => true,
        KindScope::CodeOnly => kind != SymbolKind::Document && kind != SymbolKind::Import,
        KindScope::Exact(k) => kind == k,
    }
}

/// Vector search: embed the query and find nearest neighbors, biased to `scope`.
fn vector_search<E: EmbeddingProvider + ?Sized>(
    db: &Database,
    query: &str,
    limit: u32,
    scope: KindScope,
    provider: &mut E,
) -> Result<Vec<String>> {
    let query_embedding = provider.embed_query(query)?;
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

    // Post-filter by kind; `All` skips the lookup (unchanged behaviour).
    if matches!(scope, KindScope::All) {
        return Ok(symbol_ids);
    }
    let kinds: HashMap<String, SymbolKind> = db
        .get_symbols_by_ids(&symbol_ids)?
        .into_iter()
        .map(|s| (s.id, s.kind))
        .collect();
    Ok(symbol_ids
        .into_iter()
        .filter(|id| kinds.get(id).is_some_and(|&k| kind_in_scope(k, scope)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_utils::MockEmbeddingProvider;
    use cartog_core::SymbolKind;

    /// Create a symbol + content pair and insert into the database.
    fn insert_symbol_with_content(
        db: &Database,
        name: &str,
        kind: SymbolKind,
        file: &str,
        line: u32,
        content: &str,
    ) -> Symbol {
        let sym = Symbol::new(
            name,
            kind,
            file,
            line,
            line + 10,
            0,
            content.len() as u32,
            None,
        );
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
        let result = hybrid_search(
            &db,
            "validate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert!(result.fts_count > 0, "FTS5 should find results");
        assert_eq!(result.vec_count, 0, "no embeddings → no vector results");
        assert_eq!(result.results[0].symbol.name, "validate_token");
        assert!(result.results[0].sources.contains(&Source::Fts5));

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
        let result = hybrid_search(
            &db,
            "authenticate",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "connect",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert_eq!(result.results[0].symbol.name, "DatabaseConnection");
        assert_eq!(
            result.results.len(),
            1,
            "only DatabaseConnection contains 'connect'"
        );

        // "router" should rank createRouter #1
        let result = hybrid_search(
            &db,
            "router",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "extract symbols",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert_eq!(result.results[0].symbol.name, "extract");

        // "resolve edges" — only resolve_edges has both terms
        let result = hybrid_search(
            &db,
            "resolve edges",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert_eq!(result.results[0].symbol.name, "resolve_edges");

        // "Database" should not return extract or resolve_edges as #1
        let result = hybrid_search(
            &db,
            "Database",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "handle request",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "session",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "migrate",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert_eq!(result.results[0].symbol.name, "migrate");
    }

    // ── Precision and ranking tests ──

    #[test]
    fn test_ranking_relevant_above_irrelevant() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        // "token" appears in validate_token and generate_token content, NOT in send_email
        let result = hybrid_search(
            &db,
            "token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "validate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert_eq!(
            result.results[0].symbol.name, "validate_token",
            "symbol matching both terms as phrase should rank #1"
        );

        // Now test OR ranking: "generate token" — generate_token and AuthService both
        // contain "generate" and "token". Both should appear in top results.
        let result = hybrid_search(
            &db,
            "generate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "database",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "validate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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

        let result = hybrid_search(
            &db,
            "token expiry",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "valid",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "validate response",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let all = hybrid_search(
            &db,
            "token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert!(all.results.len() >= 2);

        // With kind=Function filter: only functions returned, still respects limit
        let funcs = hybrid_search(
            &db,
            "token",
            10,
            KindFilter::Exact(SymbolKind::Function),
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        for r in &funcs.results {
            assert_eq!(r.symbol.kind, SymbolKind::Function);
        }

        // With kind=Class: AuthService mentions "token" in content
        let classes = hybrid_search(
            &db,
            "token",
            10,
            KindFilter::Exact(SymbolKind::Class),
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
        let result = hybrid_search(
            &db,
            "handler",
            3,
            KindFilter::Exact(SymbolKind::Function),
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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

        let result = hybrid_search(
            &db,
            "validate",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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

        let result = hybrid_search(
            &db,
            "zzz_nonexistent_term",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert!(result.results.is_empty());
        assert_eq!(result.fts_count, 0);
        assert_eq!(result.vec_count, 0);
    }

    #[test]
    fn test_hybrid_search_content_returned() {
        let db = Database::open_memory().unwrap();
        let content = "def greet(name: str) -> str:\n    return f'Hello, {name}!'";
        insert_symbol_with_content(&db, "greet", SymbolKind::Function, "hello.py", 1, content);

        let result = hybrid_search(
            &db,
            "greet",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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

        let result = hybrid_search(
            &db,
            "handler",
            3,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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
            symbol: Symbol::new(name, SymbolKind::Function, "test.py", 1, 10, 0, 100, None),
            content: content.map(|s| s.to_string()),
            rrf_score: rrf,
            rerank_score: rerank,
            sources: vec![Source::Fts5],
        }
    }

    #[test]
    fn rerank_ordering_sorts_by_rerank_score_descending() {
        let mut candidates = [
            make_result("low", 0.9, Some(1.0), Some("low content")),
            make_result("high", 0.5, Some(9.0), Some("high content")),
            make_result("mid", 0.7, Some(5.0), Some("mid content")),
        ];

        candidates.sort_by(rerank_ordering);

        assert_eq!(candidates[0].symbol.name, "high");
        assert_eq!(candidates[1].symbol.name, "mid");
        assert_eq!(candidates[2].symbol.name, "low");
    }

    #[test]
    fn rerank_ordering_ranks_scored_before_unscored() {
        let mut candidates = [
            make_result("no_content", 0.9, None, None),
            make_result("scored", 0.3, Some(2.0), Some("content")),
            make_result("also_no_content", 0.8, None, None),
        ];

        candidates.sort_by(rerank_ordering);

        assert_eq!(candidates[0].symbol.name, "scored");
        // Unscored candidates fall back to RRF score: 0.9 before 0.8.
        assert_eq!(candidates[1].symbol.name, "no_content");
        assert_eq!(candidates[2].symbol.name, "also_no_content");
    }

    #[test]
    fn rerank_ordering_falls_back_to_rrf_score_when_all_unscored() {
        let mut candidates = [
            make_result("first", 0.9, None, None),
            make_result("second", 0.5, None, None),
            make_result("third", 0.3, None, None),
        ];

        candidates.sort_by(rerank_ordering);

        // No rerank scores → RRF score descending.
        assert_eq!(candidates[0].symbol.name, "first");
        assert_eq!(candidates[1].symbol.name, "second");
        assert_eq!(candidates[2].symbol.name, "third");
    }

    #[test]
    fn rerank_ordering_breaks_score_ties_by_in_degree() {
        // Two candidates with identical rerank score: higher in-degree wins.
        let mut a = make_result("less_referenced", 0.5, Some(5.0), Some("c"));
        let mut b = make_result("more_referenced", 0.5, Some(5.0), Some("c"));
        a.symbol.in_degree = 1;
        b.symbol.in_degree = 9;
        let mut candidates = [a, b];

        candidates.sort_by(rerank_ordering);

        assert_eq!(candidates[0].symbol.name, "more_referenced");
        assert_eq!(candidates[1].symbol.name, "less_referenced");
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

        let result = hybrid_search(
            &db,
            "process data",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
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

    // ── Lazy hybrid search (factory gating) ───────────────────────────

    use crate::provider::test_utils::MockRerankerProvider;
    use std::cell::Cell;

    #[test]
    fn lazy_search_invokes_factory_and_reranks_when_above_min() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        let called = Cell::new(false);
        // rerank_min=1 guarantees the seeded corpus clears the bar, so the
        // factory is built and its reranker scores the candidates.
        let tuning = SearchTuning {
            rerank_min: 1,
            ..SearchTuning::default()
        };
        let result = hybrid_search_tuned_lazy(
            &db,
            "validate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            Some(|| {
                called.set(true);
                Some(Box::new(MockRerankerProvider) as Box<dyn RerankerProvider>)
            }),
            &tuning,
        )
        .unwrap();

        assert!(
            called.get(),
            "factory must run when candidates >= rerank_min"
        );
        assert!(
            result.results.iter().any(|r| r.rerank_score.is_some()),
            "reranked candidates carry a rerank_score"
        );
    }

    #[test]
    fn lazy_search_skips_factory_when_below_min() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        let called = Cell::new(false);
        // rerank_min far above the 5-symbol corpus → factory never touched.
        let tuning = SearchTuning {
            rerank_min: 100,
            ..SearchTuning::default()
        };
        let result = hybrid_search_tuned_lazy(
            &db,
            "validate token",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            Some(|| {
                called.set(true);
                Some(Box::new(MockRerankerProvider) as Box<dyn RerankerProvider>)
            }),
            &tuning,
        )
        .unwrap();

        assert!(
            !called.get(),
            "factory must NOT run when candidates < rerank_min"
        );
        assert!(result.fts_count > 0, "retrieval still happened");
        assert!(
            result.results.iter().all(|r| r.rerank_score.is_none()),
            "no reranking → all rerank_score None"
        );
    }

    #[test]
    fn lazy_search_handles_no_factory() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        let result = hybrid_search_tuned_lazy(
            &db,
            "authenticate",
            10,
            KindFilter::All,
            &mut MockEmbeddingProvider::new(384),
            None::<fn() -> Option<Box<dyn RerankerProvider>>>,
            &SearchTuning::default(),
        )
        .unwrap();

        assert!(!result.results.is_empty(), "search returns results");
        assert!(
            result.results.iter().all(|r| r.rerank_score.is_none()),
            "no factory → no rerank scores"
        );
    }

    #[test]
    fn lazy_search_respects_kind_filter() {
        let db = Database::open_memory().unwrap();
        seed_python_corpus(&db);

        let result = hybrid_search_tuned_lazy(
            &db,
            "token",
            10,
            KindFilter::Exact(SymbolKind::Function),
            &mut MockEmbeddingProvider::new(384),
            None::<fn() -> Option<Box<dyn RerankerProvider>>>,
            &SearchTuning::default(),
        )
        .unwrap();

        assert!(
            result
                .results
                .iter()
                .all(|r| r.symbol.kind == SymbolKind::Function),
            "kind filter keeps only functions"
        );
    }

    // ── prose→0 regression: kind filtering at retrieval + result tail ──

    fn candidate(name: &str, kind: SymbolKind, rrf: f64) -> SearchResult {
        SearchResult {
            symbol: Symbol::new(name, kind, "test.py", 1, 10, 0, 100, None),
            content: Some("content".to_string()),
            rrf_score: rrf,
            rerank_score: None,
            sources: vec![Source::Fts5],
        }
    }

    #[test]
    fn sort_filter_and_pack_filters_before_limit() {
        // Top-ranked candidates are Documents; qualifying code sits lower. Under
        // CodeOnly + limit=2 we must still return the two code symbols, not [].
        let candidates = vec![
            candidate("readme", SymbolKind::Document, 0.9),
            candidate("guide", SymbolKind::Document, 0.8),
            candidate("do_thing", SymbolKind::Function, 0.7),
            candidate("do_method", SymbolKind::Method, 0.6),
        ];
        let out = sort_filter_and_pack(candidates, (4, 4, 4), 2, KindFilter::CodeOnly);
        let names: Vec<&str> = out.results.iter().map(|r| r.symbol.name.as_str()).collect();
        assert_eq!(names, vec!["do_thing", "do_method"]);
        // Counts stay pre-filter (diagnostic).
        assert_eq!((out.fts_count, out.vec_count, out.merged_count), (4, 4, 4));
    }

    #[test]
    fn sort_filter_and_pack_all_filtered_is_empty() {
        // Genuinely no code → empty is the correct, honest answer.
        let candidates = vec![
            candidate("a", SymbolKind::Document, 0.9),
            candidate("b", SymbolKind::Document, 0.8),
        ];
        let out = sort_filter_and_pack(candidates, (2, 2, 2), 5, KindFilter::CodeOnly);
        assert!(out.results.is_empty());
    }

    #[test]
    fn fts5_kinded_retrieval_excludes_documents() {
        // A prose word that appears in markdown docs and in code content. The
        // CodeOnly scope must retrieve the code symbol, not just the docs.
        let db = Database::open_memory().unwrap();
        for i in 0..10 {
            insert_symbol_with_content(
                &db,
                &format!("release_notes_{i}"),
                SymbolKind::Document,
                &format!("docs/CHANGELOG_{i}.md"),
                i * 5,
                "this release improves performance and fixes several issues",
            );
        }
        insert_symbol_with_content(
            &db,
            "improve_performance",
            SymbolKind::Function,
            "perf.py",
            1,
            "def improve_performance(): # improves performance, fixes issues",
        );

        // CodeOnly retrieval surfaces the function despite 10 doc matches.
        let code = db
            .fts5_search_kinded("\"performance\" OR \"fixes\"", 20, KindScope::CodeOnly)
            .unwrap();
        assert!(
            code.iter().any(|id| id.contains("improve_performance")),
            "code symbol must be retrieved under CodeOnly: {code:?}"
        );
        assert!(
            !code.iter().any(|id| id.contains("CHANGELOG")),
            "documents must be excluded under CodeOnly: {code:?}"
        );

        // All scope still returns documents (unchanged behaviour).
        let all = db
            .fts5_search_kinded("\"performance\" OR \"fixes\"", 20, KindScope::All)
            .unwrap();
        assert!(all.iter().any(|id| id.contains("CHANGELOG")));
    }

    #[test]
    fn prose_query_returns_code_when_docs_dominate() {
        // End-to-end: a doc-heavy corpus + a prose query under the default
        // CodeOnly must still surface code (the prose→0 bug returned nothing).
        let db = Database::open_memory().unwrap();
        for i in 0..12 {
            insert_symbol_with_content(
                &db,
                &format!("changelog_entry_{i}"),
                SymbolKind::Document,
                &format!("docs/notes_{i}.md"),
                i * 5,
                "this change improves the documentation and fixes several issues going forward",
            );
        }
        insert_symbol_with_content(
            &db,
            "fix_documentation_issues",
            SymbolKind::Function,
            "fixer.py",
            1,
            "def fix_documentation_issues(): # improves documentation, fixes issues",
        );

        let out = hybrid_search(
            &db,
            "this change improves the documentation and fixes several issues going forward",
            10,
            KindFilter::CodeOnly,
            &mut MockEmbeddingProvider::new(384),
            None,
        )
        .unwrap();
        assert!(
            out.results
                .iter()
                .any(|r| r.symbol.name == "fix_documentation_issues"),
            "prose query under CodeOnly must surface the code symbol, got: {:?}",
            out.results
                .iter()
                .map(|r| &r.symbol.name)
                .collect::<Vec<_>>()
        );
        assert!(
            out.results
                .iter()
                .all(|r| r.symbol.kind != SymbolKind::Document),
            "CodeOnly must not return documents"
        );
    }

    #[test]
    fn vector_path_kind_post_filter_excludes_documents() {
        // Exercises the vector arm (the FTS-only tests never build embeddings,
        // so kind_in_scope / the vector post-filter would otherwise be untested).
        let db = Database::open_memory().unwrap();
        insert_symbol_with_content(
            &db,
            "release_doc",
            SymbolKind::Document,
            "docs/NOTES.md",
            1,
            "notes about validation and tokens across the project",
        );
        insert_symbol_with_content(
            &db,
            "validate_token",
            SymbolKind::Function,
            "auth.py",
            1,
            "def validate_token(t): # validation of tokens",
        );

        // Build embeddings so embedding_count() > 0 and vector_search runs.
        let mut provider = MockEmbeddingProvider::new(384);
        crate::indexer::index_embeddings(&db, &mut provider, false, None, None).unwrap();
        assert!(
            db.embedding_count().unwrap() > 0,
            "embeddings must be built"
        );

        let out = hybrid_search(
            &db,
            "token validation",
            10,
            KindFilter::CodeOnly,
            &mut provider,
            None,
        )
        .unwrap();
        assert!(
            out.results
                .iter()
                .any(|r| r.symbol.name == "validate_token"),
            "code symbol must survive the vector + result kind filters"
        );
        assert!(
            out.results
                .iter()
                .all(|r| r.symbol.kind != SymbolKind::Document),
            "vector path must not surface documents under CodeOnly"
        );
    }
}
