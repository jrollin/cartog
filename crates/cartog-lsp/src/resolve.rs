//! Drain/apply split for LSP edge resolution: the per-language drain produces a
//! DB-free [`LangOutcomes`], and [`apply_lang_outcomes`] replays it against the
//! single DB writer. Keeping the two apart lets per-language drains run
//! concurrently while one applier owns the writer.

use anyhow::Result;

use cartog_db::Database;

use crate::manager::DefinitionLocation;

/// One language's drained LSP outcomes, before any DB write.
#[derive(Default)]
pub(crate) struct LangOutcomes {
    /// `(edge_id, in-root location)`; applier resolves each to a symbol.
    pub in_root: Vec<(i64, DefinitionLocation)>,
    /// Definitive "no definition"; marked unresolvable only if ≥1 edge resolved.
    pub pending_unresolvable: Vec<i64>,
    /// Target outside the indexed root (stdlib/deps).
    pub pending_external: Vec<i64>,
    /// Server died mid-drain — suppresses all marking for this language.
    pub server_died: bool,
}

/// Apply one language's outcomes to the DB, preserving the health gates.
/// Returns `(resolved, marked_unresolvable, marked_external)`.
pub(crate) fn apply_lang_outcomes(
    db: &Database,
    language: &str,
    outcomes: &LangOutcomes,
) -> Result<(u32, u32, u32)> {
    let mut resolved = 0u32;
    let mut marked_unresolvable = 0u32;
    let mut marked_external = 0u32;

    // A located line with no covering symbol is an extraction gap, not external
    // → unresolvable (keeps state=3 stdlib/deps-only).
    let mut extra_unresolvable: Vec<i64> = Vec::new();
    for (edge_id, loc) in &outcomes.in_root {
        match db.find_symbol_at_location(&loc.file_path, loc.line)? {
            Some(symbol_id) => match db.update_edge_target(*edge_id, &symbol_id) {
                Ok(()) => resolved += 1,
                Err(e) => tracing::debug!("failed to update edge {edge_id}: {e:#}"),
            },
            None => {
                tracing::debug!("no cartog symbol at {}:{}", loc.file_path, loc.line);
                extra_unresolvable.push(*edge_id);
            }
        }
    }

    // Unresolvable: gate on resolved > 0 (a half-loaded server fabricates
    // Ok(None) before its index is ready; don't burn good edges with state=2).
    if !outcomes.server_died && resolved > 0 {
        for edge_id in outcomes
            .pending_unresolvable
            .iter()
            .chain(&extra_unresolvable)
        {
            if let Err(e) = db.mark_edge_unresolvable(*edge_id) {
                tracing::debug!("failed to mark edge {edge_id} unresolvable: {e:#}");
                continue;
            }
            marked_unresolvable += 1;
        }
    } else {
        let n = outcomes.pending_unresolvable.len() + extra_unresolvable.len();
        if n > 0 {
            tracing::info!(
                "LSP: {language} produced {n} unresolvable answers but no successes — \
                 not marking (server may be half-loaded or unhealthy)"
            );
        }
    }

    // External: a half-loaded server can't fabricate a concrete out-of-root URI,
    // so no resolved-gate — commit whenever the server stayed alive.
    if !outcomes.server_died {
        for edge_id in &outcomes.pending_external {
            if let Err(e) = db.mark_edge_external(*edge_id) {
                tracing::debug!("failed to mark edge {edge_id} external: {e:#}");
                continue;
            }
            marked_external += 1;
        }
    } else if !outcomes.pending_external.is_empty() {
        tracing::info!(
            "LSP: {language} produced {} external answers but server died — not marking",
            outcomes.pending_external.len()
        );
    }

    Ok((resolved, marked_unresolvable, marked_external))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::DefinitionLocation;
    use cartog_core::{Edge, EdgeKind, Symbol, SymbolKind};

    /// Build an in-memory DB with `n` unresolved Calls edges from one caller,
    /// plus a target symbol at `target.py:10`. Returns `(db, edge_ids)`.
    fn db_with_edges(n: usize) -> (Database, Vec<i64>) {
        let db = Database::open_memory().unwrap();
        let caller = Symbol::new("caller", SymbolKind::Function, "a.py", 1, 5, 0, 100, None);
        let target = Symbol::new("target", SymbolKind::Function, "target.py", 10, 20, 0, 100, None);
        db.insert_symbols(&[caller.clone(), target]).unwrap();
        for i in 0..n {
            let e = Edge::new(&caller.id, "target", EdgeKind::Calls, "a.py", (i + 2) as u32);
            db.insert_edge(&e).unwrap();
        }
        let ids = db
            .unresolved_edges()
            .unwrap()
            .iter()
            .map(|e| e.edge_id)
            .collect();
        (db, ids)
    }

    fn in_root_at(edge_id: i64, line: u32) -> (i64, DefinitionLocation) {
        (
            edge_id,
            DefinitionLocation {
                file_path: "target.py".to_string(),
                line,
            },
        )
    }

    #[test]
    fn resolves_in_root_edges_to_their_symbol() {
        let (db, ids) = db_with_edges(2);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10), in_root_at(ids[1], 10)],
            ..Default::default()
        };
        let (resolved, u, x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, u, x), (2, 0, 0));
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 1);
        assert_eq!(db.edge_resolution_state(ids[1]).unwrap(), 1);
    }

    #[test]
    fn unresolvable_suppressed_when_no_edge_resolved() {
        // The lang-resolved gate: a half-loaded server produces only Ok(None);
        // with zero successes, nothing is marked (edges stay state=0 for retry).
        let (db, ids) = db_with_edges(1);
        let outcomes = LangOutcomes {
            pending_unresolvable: vec![ids[0]],
            ..Default::default()
        };
        let (resolved, marked, _) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked), (0, 0), "no success → no unresolvable mark");
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 0);
    }

    #[test]
    fn unresolvable_committed_when_an_edge_resolved() {
        let (db, ids) = db_with_edges(2);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10)],
            pending_unresolvable: vec![ids[1]],
            ..Default::default()
        };
        let (resolved, marked, _) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked), (1, 1));
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 1, "resolved");
        assert_eq!(db.edge_resolution_state(ids[1]).unwrap(), 2, "unresolvable");
    }

    #[test]
    fn in_root_with_no_symbol_at_line_is_unresolvable_not_external() {
        // A located line with no covering symbol falls to unresolvable (gated on
        // the sibling success), never external.
        let (db, ids) = db_with_edges(2);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10), in_root_at(ids[1], 999)],
            ..Default::default()
        };
        let (resolved, marked_u, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u, marked_x), (1, 1, 0));
        assert_eq!(db.edge_resolution_state(ids[1]).unwrap(), 2, "no-symbol → state=2");
    }

    #[test]
    fn external_committed_when_server_alive_even_with_zero_resolved() {
        // External has no resolved-gate: a stdlib-only file must still seal so it
        // does not re-query the LSP forever.
        let (db, ids) = db_with_edges(1);
        let outcomes = LangOutcomes {
            pending_external: vec![ids[0]],
            ..Default::default()
        };
        let (resolved, _, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_x), (0, 1));
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 3, "external");
    }

    #[test]
    fn server_died_suppresses_all_marking() {
        // A dead server marks nothing — neither unresolvable nor external — even
        // though in-root successes (written before the death) still count.
        let (db, ids) = db_with_edges(3);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10)],
            pending_unresolvable: vec![ids[1]],
            pending_external: vec![ids[2]],
            server_died: true,
        };
        let (resolved, marked_u, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u, marked_x), (1, 0, 0));
        assert_eq!(db.edge_resolution_state(ids[1]).unwrap(), 0, "unmarked on death");
        assert_eq!(db.edge_resolution_state(ids[2]).unwrap(), 0, "unmarked on death");
    }
}
