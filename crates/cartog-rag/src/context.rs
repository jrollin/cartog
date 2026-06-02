//! One-shot task-context builder: fuse semantic seeds, structural neighbors,
//! and centrality into a token-budgeted bundle for an agent's task.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::Serialize;

use cartog_core::Symbol;
use cartog_db::Database;

use crate::provider::{EmbeddingProvider, RerankerProvider};
use crate::search::{hybrid_search_tuned, KindFilter, SearchTuning};

/// Why a symbol is in a [`TaskContext`] bundle. Variants are ordered
/// strongest-first, so `Ord` ranks `Seed < Neighbor < Central`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextReason {
    /// Surfaced directly by semantic/keyword search.
    Seed,
    /// A 1-hop callee or caller of a seed.
    Neighbor,
    /// A high-centrality definition in a seed's file.
    Central,
}

/// One symbol in a task-context bundle.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct ContextEntry {
    pub symbol: Symbol,
    pub reason: ContextReason,
    /// Combined relevance score; higher ranks first.
    pub score: f64,
    /// Body, attached to top entries until the token budget is spent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// A task-context bundle: ranked, deduplicated entries within a token budget.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct TaskContext {
    pub task: String,
    pub entries: Vec<ContextEntry>,
    /// Approximate tokens occupied by the attached bodies (`bytes / 4`, min 1
    /// per non-empty body).
    pub approx_tokens: u32,
}

/// Tunables for [`build_task_context`].
#[derive(Debug, Clone, Copy)]
pub struct ContextOptions {
    /// Semantic seeds to retrieve.
    pub seed_count: u32,
    /// Top seeds whose 1-hop neighbors are pulled in.
    pub expand_count: usize,
    /// Centrality candidates considered (filtered to seed files).
    pub central_count: u32,
    pub tuning: SearchTuning,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            seed_count: 8,
            expand_count: 3,
            central_count: 20,
            tuning: SearchTuning::default(),
        }
    }
}

/// Rough tokens-per-byte for budget accounting (code averages ~4 chars/token).
const BYTES_PER_TOKEN: usize = 4;

/// Score bands keep Seeds above Neighbors above Central regardless of raw signal.
const SEED_BASE: f64 = 2.0;
const NEIGHBOR_BASE: f64 = 1.0;
const CENTRAL_BASE: f64 = 0.5;

/// Build a task-context bundle for `task`.
///
/// Pipeline: (1) [`hybrid_search_tuned`] for semantic seeds; (2) 1-hop callees
/// and callers of the top seeds; (3) high-centrality definitions in seed files;
/// (4) dedup by symbol id keeping the strongest reason, rank, then greedily
/// attach bodies until `token_budget` is spent.
///
/// Degrades to keyword-only seeds when no embeddings are indexed, and to RRF
/// ranking when `reranker` is `None`.
///
/// # Errors
/// Returns an error if a database query or the search pipeline fails.
#[must_use = "the task-context bundle is the result; ignoring it wastes the query"]
pub fn build_task_context<E: EmbeddingProvider + ?Sized>(
    db: &Database,
    task: &str,
    token_budget: u32,
    embedding_provider: &mut E,
    reranker: Option<&mut dyn RerankerProvider>,
    opts: &ContextOptions,
) -> Result<TaskContext> {
    let seeds = hybrid_search_tuned(
        db,
        task,
        opts.seed_count,
        KindFilter::CodeOnly,
        embedding_provider,
        reranker,
        &opts.tuning,
    )?;

    // id → (symbol, reason, score). On a stronger reason, adopt the new
    // reason AND its score (so the score always matches the winning reason);
    // on the same reason, keep the higher score.
    let mut picked: HashMap<String, (Symbol, ContextReason, f64)> = HashMap::new();
    let consider = |map: &mut HashMap<String, (Symbol, ContextReason, f64)>,
                    sym: Symbol,
                    reason: ContextReason,
                    score: f64| {
        map.entry(sym.id.clone())
            .and_modify(|e| {
                if reason < e.1 {
                    e.1 = reason;
                    e.2 = score;
                } else if reason == e.1 && score > e.2 {
                    e.2 = score;
                }
            })
            .or_insert((sym, reason, score));
    };

    let seed_files: HashSet<&str> = seeds
        .results
        .iter()
        .map(|r| r.symbol.file_path.as_str())
        .collect();

    for (rank, r) in seeds.results.iter().enumerate() {
        let base = r.rerank_score.unwrap_or(r.rrf_score);
        let score = SEED_BASE + base + rank_bonus(rank);
        consider(&mut picked, r.symbol.clone(), ContextReason::Seed, score);
    }

    // Expand the top seeds' 1-hop call neighborhood, keyed on the seed's exact
    // id so an overloaded name doesn't pull in the wrong symbol's neighbors.
    // Callees + callers, resolved `calls` edges only.
    for r in seeds.results.iter().take(opts.expand_count) {
        let boost = picked.get(&r.symbol.id).map_or(0.0, |e| e.2);
        let neighbor_ids: Vec<String> = db
            .callee_ids_of(&r.symbol.id)?
            .into_iter()
            .chain(db.caller_ids_of(&r.symbol.id)?)
            .collect();
        for sym in db.get_symbols_by_ids(&neighbor_ids)? {
            let score = NEIGHBOR_BASE + neighbor_weight(boost, sym.in_degree);
            consider(&mut picked, sym, ContextReason::Neighbor, score);
        }
    }

    // Centrality: high in-degree definitions living in a seed's file.
    if !seed_files.is_empty() {
        for sym in db.top_symbols(opts.central_count)? {
            if seed_files.contains(sym.file_path.as_str()) {
                let score = CENTRAL_BASE + centrality_weight(sym.in_degree);
                consider(&mut picked, sym, ContextReason::Central, score);
            }
        }
    }

    let mut entries: Vec<ContextEntry> = picked
        .into_values()
        .map(|(symbol, reason, score)| ContextEntry {
            symbol,
            reason,
            score,
            content: None,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.symbol.in_degree.cmp(&a.symbol.in_degree))
            .then(a.symbol.id.cmp(&b.symbol.id))
    });

    let approx_tokens = hydrate_within_budget(db, &mut entries, token_budget)?;

    Ok(TaskContext {
        task: task.to_string(),
        entries,
        approx_tokens,
    })
}

/// Small descending bonus so seeds keep their search order within the band.
fn rank_bonus(rank: usize) -> f64 {
    1.0 / (rank as f64 + 1.0)
}

/// Neighbor score lift from its seed's score and its own centrality.
fn neighbor_weight(seed_score: f64, in_degree: u32) -> f64 {
    seed_score * 0.1 + (f64::from(in_degree)).ln_1p() * 0.1
}

/// Central score lift from centrality alone.
fn centrality_weight(in_degree: u32) -> f64 {
    (f64::from(in_degree)).ln_1p() * 0.1
}

/// Attach bodies to ranked entries until `token_budget` is spent. Returns the
/// approximate tokens consumed. Entries past the budget keep `content: None`.
fn hydrate_within_budget(
    db: &Database,
    entries: &mut [ContextEntry],
    token_budget: u32,
) -> Result<u32> {
    let budget = token_budget as usize;
    let mut spent = 0usize;
    for entry in entries.iter_mut() {
        if spent >= budget {
            break;
        }
        if let Some((content, _)) = db.get_symbol_content(&entry.symbol.id)? {
            // Non-empty bodies cost at least 1 token so tiny (<4-byte) bodies
            // can't slip the cap for free.
            let cost = (content.len() / BYTES_PER_TOKEN).max(1);
            if spent + cost > budget {
                // Entries are score-sorted: a higher-ranked body that doesn't
                // fit means stop, rather than letting a lower-ranked smaller
                // body jump ahead of it.
                break;
            }
            spent += cost;
            entry.content = Some(content);
        }
    }
    Ok(spent as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_utils::MockEmbeddingProvider;
    use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind};

    fn insert(db: &Database, name: &str, kind: SymbolKind, file: &str, content: &str) -> Symbol {
        let sym = Symbol::new(name, kind, file, 1, 11, 0, content.len() as u32, None);
        db.insert_symbol(&sym).unwrap();
        let header = format!("// {kind} {name}");
        db.upsert_symbol_content(&sym.id, name, content, &header)
            .unwrap();
        sym
    }

    /// `authenticate → generate_token` over a `calls` edge in one auth file.
    fn seed_auth(db: &Database) -> (Symbol, Symbol) {
        let auth = insert(
            db,
            "authenticate",
            SymbolKind::Function,
            "auth.py",
            "def authenticate(user, pw):\n    return generate_token(user)",
        );
        let gen = insert(
            db,
            "generate_token",
            SymbolKind::Function,
            "auth.py",
            "def generate_token(user):\n    return jwt.encode(user)",
        );
        db.insert_edges(&[Edge {
            source_id: auth.id.clone(),
            target_name: "generate_token".to_string(),
            target_id: Some(gen.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "auth.py".to_string(),
            line: 2,
        }])
        .unwrap();
        (auth, gen)
    }

    #[test]
    fn context_surfaces_seeds_and_their_neighbors() {
        let db = Database::open_memory().unwrap();
        seed_auth(&db);
        let ctx = build_task_context(
            &db,
            "authenticate user",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();

        let names: Vec<&str> = ctx.entries.iter().map(|e| e.symbol.name.as_str()).collect();
        assert!(names.contains(&"authenticate"), "seed present: {names:?}");
        assert!(
            names.contains(&"generate_token"),
            "1-hop callee present: {names:?}"
        );
    }

    #[test]
    fn context_ranks_seeds_above_neighbors() {
        let db = Database::open_memory().unwrap();
        seed_auth(&db);
        let ctx = build_task_context(
            &db,
            "authenticate",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        // Entries are score-sorted; the first must be a Seed.
        assert_eq!(ctx.entries[0].reason, ContextReason::Seed);
    }

    #[test]
    fn context_has_no_duplicate_symbols() {
        let db = Database::open_memory().unwrap();
        seed_auth(&db);
        let ctx = build_task_context(
            &db,
            "token",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        let mut ids: Vec<&str> = ctx.entries.iter().map(|e| e.symbol.id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(before, ids.len(), "no symbol id should repeat");
    }

    #[test]
    fn context_tiny_budget_keeps_entries_without_bodies() {
        let db = Database::open_memory().unwrap();
        seed_auth(&db);
        let ctx = build_task_context(
            &db,
            "authenticate",
            0, // no room for any body
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(!ctx.entries.is_empty(), "seeds still returned");
        assert!(
            ctx.entries.iter().all(|e| e.content.is_none()),
            "zero budget attaches no bodies"
        );
        assert_eq!(ctx.approx_tokens, 0);
    }

    #[test]
    fn context_empty_index_returns_empty_bundle() {
        let db = Database::open_memory().unwrap();
        let ctx = build_task_context(
            &db,
            "anything",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        assert!(ctx.entries.is_empty());
    }

    #[test]
    fn context_neighbors_exclude_non_call_edges() {
        // A symbol that only IMPORTS the seed must not be classified as a call
        // Neighbor. (It may still appear as a Seed if keyword search surfaces
        // it, but never via the import edge as a neighbor.)
        let db = Database::open_memory().unwrap();
        let (auth, _gen) = seed_auth(&db);
        // Name + content share NO term with the query so search won't seed it;
        // its only link to the seed is an import edge.
        let importer = insert(
            &db,
            "zzz_loader",
            SymbolKind::Function,
            "config.py",
            "def zzz_loader():\n    return 1",
        );
        db.insert_edges(&[Edge {
            source_id: importer.id.clone(),
            target_name: "authenticate".to_string(),
            target_id: Some(auth.id.clone()),
            kind: EdgeKind::Imports,
            file_path: "config.py".to_string(),
            line: 2,
        }])
        .unwrap();

        let ctx = build_task_context(
            &db,
            "authenticate",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        let names: Vec<&str> = ctx.entries.iter().map(|e| e.symbol.name.as_str()).collect();
        assert!(
            !names.contains(&"zzz_loader"),
            "an import-only caller is not pulled in as a call neighbor: {names:?}"
        );
    }

    #[test]
    fn context_budget_prioritizes_higher_ranked_bodies() {
        // Top-ranked seed has a body that exceeds the budget; a lower-ranked
        // neighbor's smaller body must NOT jump ahead of it.
        let db = Database::open_memory().unwrap();
        let big = "x".repeat(400); // ~100 tokens
        let seed = insert(&db, "validate_token", SymbolKind::Function, "t.py", &big);
        let small = insert(
            &db,
            "helper",
            SymbolKind::Function,
            "t.py",
            "def helper(): pass",
        );
        db.insert_edges(&[Edge {
            source_id: seed.id.clone(),
            target_name: "helper".to_string(),
            target_id: Some(small.id.clone()),
            kind: EdgeKind::Calls,
            file_path: "t.py".to_string(),
            line: 1,
        }])
        .unwrap();

        // Budget of 10 tokens: too small for the 100-token seed body.
        let ctx = build_task_context(
            &db,
            "validate_token",
            10,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions::default(),
        )
        .unwrap();
        // The seed ranks first; since its body didn't fit, hydration stops and
        // the lower-ranked helper does NOT get a body either.
        assert_eq!(ctx.entries[0].symbol.name, "validate_token");
        assert!(
            ctx.entries.iter().all(|e| e.content.is_none()),
            "no body jumps ahead of the unfittable top-ranked entry"
        );
    }

    #[test]
    fn context_expands_neighbors_by_exact_seed_id_not_name() {
        // Two symbols share the name `process` in different files. Only the
        // seed in a.py calls `target_a`; the b.py overload calls `target_b`.
        // Expansion keyed on the seed's exact id must pull target_a, not
        // target_b (which a name-based fan-out would wrongly include).
        let db = Database::open_memory().unwrap();
        let seed = insert(
            &db,
            "process",
            SymbolKind::Function,
            "a.py",
            "def process(): target_a()",
        );
        let other = insert(
            &db,
            "process",
            SymbolKind::Function,
            "b.py",
            "def process(): target_b()",
        );
        let target_a = insert(
            &db,
            "target_a",
            SymbolKind::Function,
            "a.py",
            "def target_a(): pass",
        );
        let target_b = insert(
            &db,
            "target_b",
            SymbolKind::Function,
            "b.py",
            "def target_b(): pass",
        );
        assert_ne!(seed.id, other.id, "overloads have distinct ids");
        db.insert_edges(&[
            Edge {
                source_id: seed.id.clone(),
                target_name: "target_a".to_string(),
                target_id: Some(target_a.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "a.py".to_string(),
                line: 1,
            },
            Edge {
                source_id: other.id.clone(),
                target_name: "target_b".to_string(),
                target_id: Some(target_b.id.clone()),
                kind: EdgeKind::Calls,
                file_path: "b.py".to_string(),
                line: 1,
            },
        ])
        .unwrap();

        // Confirm the DB helper resolves callees by the exact seed id.
        assert_eq!(
            db.callee_ids_of(&seed.id).unwrap(),
            vec![target_a.id.clone()]
        );
        assert_eq!(
            db.callee_ids_of(&other.id).unwrap(),
            vec![target_b.id.clone()]
        );

        let ctx = build_task_context(
            &db,
            "process",
            6000,
            &mut MockEmbeddingProvider::new(384),
            None,
            &ContextOptions {
                expand_count: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let names: Vec<&str> = ctx.entries.iter().map(|e| e.symbol.name.as_str()).collect();
        // The top seed's exact callee is present; the other overload's callee
        // is only present if its `process` was itself a seed — assert at least
        // that expansion is id-scoped by checking target_a came in as a neighbor.
        let target_a_entry = ctx.entries.iter().find(|e| e.symbol.id == target_a.id);
        assert!(
            target_a_entry.is_some(),
            "seed's exact callee target_a is expanded: {names:?}"
        );
        assert_eq!(
            target_a_entry.unwrap().reason,
            ContextReason::Neighbor,
            "target_a enters as a call neighbor"
        );
    }
}
