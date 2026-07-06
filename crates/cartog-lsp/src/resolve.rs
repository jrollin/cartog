//! Drain/apply split for LSP edge resolution: the per-language drain produces a
//! DB-free [`LangOutcomes`], and [`apply_lang_outcomes`] replays it against the
//! single DB writer. Keeping the two apart lets per-language drains run
//! concurrently while one applier owns the writer.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use anyhow::Result;

use cartog_core::PROGRESS_STRIDE;
use cartog_db::{Database, UnresolvedEdge};

use crate::client::is_request_timeout;
use crate::find_column_in_line;
use crate::manager::{DefinitionLocation, DefinitionOutcome, LspManager, DEFINITION_BATCH_WINDOW};
use crate::{LspCancel, LspProgress, LspResolveStats};

/// One language's drained LSP outcomes, before any DB write.
#[derive(Default)]
pub(crate) struct LangOutcomes {
    /// `(edge_id, in-root location)`; applier resolves each to a symbol.
    pub in_root: Vec<(i64, DefinitionLocation)>,
    /// Definitive "no definition"; marked unresolvable only if ≥1 edge resolved.
    pub pending_unresolvable: Vec<i64>,
    /// Edge column not found on its line — cartog can't form an LSP query for it,
    /// so it's unresolvable-by-LSP. Distinct from `pending_unresolvable` (server
    /// said "no def"): this is a deterministic cartog-side fact, marked whenever
    /// the server stayed alive (no resolved-gate).
    pub pending_unlocatable: Vec<i64>,
    /// Target outside the indexed root (stdlib/deps).
    pub pending_external: Vec<i64>,
    /// Process died mid-drain — answers may be corrupt, so `apply` marks nothing
    /// beyond the in-root successes drained before the death.
    pub server_died: bool,
    /// Mute-server abort — the drained answers are trustworthy and get committed;
    /// only the un-queried tail is skipped. Unlike `server_died`, which distrusts all.
    pub stopped_early: bool,
}

/// Consecutive timed-out request windows (each = one 10s batch deadline) before
/// a live-but-mute server is abandoned. Windows not files: the bound stays
/// ~`LIMIT × 10s` however the edges distribute.
const UNRESPONSIVE_WINDOW_LIMIT: usize = 3;

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

    // Unlocatable: a deterministic cartog-side fact (column not found on the
    // line), independent of server health, so no resolved-gate like external —
    // commit whenever the process stayed alive.
    if !outcomes.server_died {
        let mut n = 0u32;
        for edge_id in &outcomes.pending_unlocatable {
            if let Err(e) = db.mark_edge_unresolvable(*edge_id) {
                tracing::debug!("failed to mark unlocatable edge {edge_id} unresolvable: {e:#}");
                continue;
            }
            n += 1;
        }
        if n > 0 {
            tracing::debug!("LSP: {language} marked {n} unlocatable edges unresolvable");
        }
        marked_unresolvable += n;
    }

    // External: a half-loaded server can't fabricate a concrete out-of-root URI,
    // so no resolved-gate — commit whenever the process stayed alive (a mute
    // stop mid-drain still leaves the drained answers trustworthy).
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

/// Thread-safe progress emitter: workers `tick()` per edge; the sole emitting
/// thread `emit()`s a monotonic `(done, total)` to the user callback. Monotonic
/// (fetch_max-style) so out-of-order worker deltas never tick backward.
pub(crate) struct ProgressSink<'a> {
    total: u32,
    processed: AtomicU32,
    last_emitted: AtomicU32,
    cb: Option<LspProgress<'a>>,
}

impl<'a> ProgressSink<'a> {
    pub fn new(total: u32, cb: Option<LspProgress<'a>>) -> Self {
        Self {
            total,
            processed: AtomicU32::new(0),
            last_emitted: AtomicU32::new(u32::MAX),
            cb,
        }
    }

    /// Count one processed edge; returns the new running total.
    pub fn tick(&self) -> u32 {
        self.processed.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn processed(&self) -> u32 {
        self.processed.load(Ordering::Relaxed)
    }

    /// Emit `done` if it advances the high-water mark (dedup + monotonic).
    pub fn emit(&self, done: u32) {
        let Some(cb) = self.cb else { return };
        loop {
            let last = self.last_emitted.load(Ordering::Relaxed);
            if last != u32::MAX && done <= last {
                return;
            }
            if self
                .last_emitted
                .compare_exchange(last, done, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                cb(done, self.total);
                return;
            }
        }
    }

    /// Emit the terminal tick unconditionally (bypasses the monotonic guard) so
    /// the callback always lands on the final value, even when it equals a prior
    /// emit (e.g. the no-server case collapsing to a single `(0, total)`).
    pub fn emit_final(&self, done: u32) {
        if let Some(cb) = self.cb {
            if self.last_emitted.swap(done, Ordering::Relaxed) != done {
                cb(done, self.total);
            }
        }
    }
}

/// One language's drain result: its outcomes plus whether a server started.
pub(crate) struct LangResult {
    pub language: String,
    pub outcomes: LangOutcomes,
    pub started: bool,
}

/// Drain one language's edges via its LSP server into a DB-free [`LangOutcomes`].
/// Takes no `&Database` (compile-enforced DB-free), so it is safe to run on a
/// worker thread. Returns `Err(CANCELLED_MSG)` if the cancel probe trips.
#[allow(clippy::too_many_arguments)]
pub(crate) fn drain_language(
    root: &Path,
    manager: &mut LspManager,
    language: &str,
    edges: &[UnresolvedEdge],
    sink: &ProgressSink<'_>,
    cancel: Option<LspCancel<'_>>,
) -> Result<LangResult> {
    let check_cancel = || -> Result<()> {
        if cancel.is_some_and(|c| c()) {
            anyhow::bail!(cartog_core::CANCELLED_MSG);
        }
        Ok(())
    };

    let mut out = LangOutcomes::default();
    check_cancel()?;
    #[cfg(test)]
    if test_hooks::should_panic(language) {
        panic!("test-injected drain panic for {language}");
    }
    let started = match manager.start(language) {
        Ok(()) => true,
        Err(e) => {
            tracing::info!("LSP: {language} — {e:#} ({} unresolved edges)", edges.len());
            return Ok(LangResult {
                language: language.to_string(),
                outcomes: out,
                started: false,
            });
        }
    };

    // Group edges by file for batched didOpen.
    let mut by_file: HashMap<&str, Vec<&UnresolvedEdge>> = HashMap::new();
    for edge in edges {
        by_file.entry(&edge.file_path).or_default().push(edge);
    }
    tracing::info!(
        "LSP: resolving {} unresolved {language} edges across {} files...",
        edges.len(),
        by_file.len()
    );

    // Sort so the mute-abort decision is deterministic run-to-run (by_file is a
    // HashMap; consecutive-window counting would otherwise depend on hash order).
    let mut by_file: Vec<(&str, Vec<&UnresolvedEdge>)> = by_file.into_iter().collect();
    by_file.sort_by_key(|(path, _)| *path);

    let mut consecutive_timeout_windows = 0usize;
    for (file_path, file_edges) in by_file {
        check_cancel()?;
        let abs_path = root.join(file_path);
        let content = match std::fs::read_to_string(&abs_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("cannot read {file_path}: {e}");
                continue;
            }
        };

        if let Err(e) = manager.open_file(language, file_path, &content) {
            tracing::debug!("didOpen failed for {file_path}: {e:#}");
            // Server stopped reading stdin: alive but stuck (is_alive true), so
            // stop early — drained answers stay trusted, like the mute-server path.
            // Drop the client so a warm manager can't reuse a wedged one.
            if crate::client::is_write_timeout(&e) {
                tracing::warn!(
                    "{language} LSP server stopped reading stdin (didOpen write timed out) — \
                     remaining {language} edges resolved via heuristics only. Rerun with \
                     --no-lsp to skip LSP entirely."
                );
                out.stopped_early = true;
                manager.drop_client(language);
                break;
            }
            if !manager.is_alive(language) {
                tracing::warn!(
                    "{language} LSP server died during didOpen — remaining {language} edges \
                     resolved via heuristics only. Rerun with --no-lsp to skip LSP entirely."
                );
                out.server_died = true;
                break;
            }
            continue;
        }

        let lines: Vec<&str> = content.lines().collect();

        // Pair each edge whose target column we can locate with its LSP position
        // in ONE vec so the edge↔position↔outcome correspondence can't drift.
        // An edge whose target_name doesn't appear as a locatable word on its
        // recorded line (a whole-expression or multi-line target) has no column,
        // so it can never be queried: bucket it as unresolvable-by-LSP instead of
        // silently dropping it, or reopen_heuristic_exhausted re-walks it
        // fruitlessly on every pass.
        let mut batch: Vec<(&UnresolvedEdge, (u32, u32))> = Vec::with_capacity(file_edges.len());
        for &edge in &file_edges {
            match find_column_in_line(&lines, edge.line, &edge.target_name) {
                Some(col) => batch.push((edge, (edge.line.saturating_sub(1), col))),
                None => out.pending_unlocatable.push(edge.edge_id),
            }
        }
        let positions: Vec<(u32, u32)> = batch.iter().map(|&(_, pos)| pos).collect();

        let outcomes = match manager.definitions_batch(language, file_path, &positions, cancel) {
            Ok(o) => o,
            Err(e) if cartog_core::is_cancelled(&e) => {
                let _ = manager.close_file(language, file_path);
                return Err(e);
            }
            Err(e) => {
                tracing::debug!("definition batch failed for {file_path}: {e:#}");
                // Server stopped reading stdin (see the open_file arm). Skip
                // close_file: the write is wedged, so a didClose would only queue
                // behind the parked job. Drop the client instead (kill unblocks it).
                if crate::client::is_write_timeout(&e) {
                    tracing::warn!(
                        "{language} LSP server stopped reading stdin (definition write timed \
                         out) — remaining {language} edges resolved via heuristics only. Rerun \
                         with --no-lsp to skip LSP entirely."
                    );
                    out.stopped_early = true;
                    manager.drop_client(language);
                    break;
                }
                if !manager.is_alive(language) {
                    tracing::warn!(
                        "{language} LSP server died — remaining {language} edges resolved \
                         via heuristics only. Rerun with --no-lsp to skip LSP entirely."
                    );
                    out.server_died = true;
                }
                let _ = manager.close_file(language, file_path);
                if out.server_died {
                    break;
                }
                continue;
            }
        };

        // Outcomes are position-ordered, so chunking by window size rebuilds the
        // batch windows; an all-timeout window burned its full deadline. Enough
        // consecutive ones (across files) = mute server → abort, keep the rest.
        for (window, batch_slice) in outcomes
            .chunks(DEFINITION_BATCH_WINDOW)
            .zip(batch.chunks(DEFINITION_BATCH_WINDOW))
        {
            if window
                .iter()
                .all(|o| matches!(o, Err(e) if is_request_timeout(e)))
            {
                consecutive_timeout_windows += 1;
            } else {
                consecutive_timeout_windows = 0;
            }

            for ((edge, _pos), outcome) in batch_slice.iter().zip(window) {
                let done = sink.tick();
                if done % PROGRESS_STRIDE == 0 {
                    sink.emit(done);
                }
                match outcome {
                    Ok(Some(DefinitionOutcome::InRoot(loc))) => out.in_root.push((
                        edge.edge_id,
                        DefinitionLocation {
                            file_path: loc.file_path.clone(),
                            line: loc.line,
                        },
                    )),
                    Ok(Some(DefinitionOutcome::External)) => {
                        out.pending_external.push(edge.edge_id)
                    }
                    Ok(None) => out.pending_unresolvable.push(edge.edge_id),
                    Err(e) => {
                        tracing::debug!(
                            "definition failed for {} at {file_path}:{}: {e:#}",
                            edge.target_name,
                            edge.line
                        );
                        if !manager.is_alive(language) {
                            tracing::warn!(
                                "{language} LSP server died — remaining {language} edges \
                                 resolved via heuristics only. Rerun with --no-lsp to skip \
                                 LSP entirely."
                            );
                            out.server_died = true;
                            break;
                        }
                    }
                }
            }
            if out.server_died {
                break;
            }
            if consecutive_timeout_windows >= UNRESPONSIVE_WINDOW_LIMIT {
                tracing::warn!(
                    "{language} LSP server is unresponsive ({UNRESPONSIVE_WINDOW_LIMIT} \
                     consecutive request windows timed out) — remaining {language} edges \
                     resolved via heuristics only. Rerun with --no-lsp to skip LSP entirely."
                );
                out.stopped_early = true;
                break;
            }
        }

        let _ = manager.close_file(language, file_path);
        if out.server_died || out.stopped_early {
            break;
        }
    }

    Ok(LangResult {
        language: language.to_string(),
        outcomes: out,
        started,
    })
}

/// Resolve `cap` for a pass: `0` = auto (`min(languages, 4)`); else clamp to
/// `1..=languages`.
pub(crate) fn effective_cap(requested: usize, languages: usize) -> usize {
    let langs = languages.max(1);
    if requested == 0 {
        langs.min(4)
    } else {
        requested.clamp(1, langs)
    }
}

/// Serial resolution: drain each language with the (shared or owned) manager,
/// applying its outcomes immediately. Used for the shared-manager path and
/// `cap <= 1` — byte-identical to the pre-fan-out behavior.
pub(crate) fn resolve_serial(
    db: &Database,
    root: &Path,
    manager: &mut LspManager,
    by_language: &HashMap<String, Vec<UnresolvedEdge>>,
    sink: &ProgressSink<'_>,
    cancel: Option<LspCancel<'_>>,
) -> Result<LspResolveStats> {
    let mut stats = LspResolveStats::default();
    for (language, edges) in by_language {
        if cancel.is_some_and(|c| c()) {
            anyhow::bail!(cartog_core::CANCELLED_MSG);
        }
        let res = drain_language(root, manager, language, edges, sink, cancel)?;
        stats.any_server_started |= res.started;
        let (r, u, x) = apply_lang_outcomes(db, language, &res.outcomes)?;
        stats.resolved += r;
        stats.marked_unresolvable += u;
        stats.marked_external += x;
    }
    Ok(stats)
}

/// Concurrent resolution: a fixed pool of `cap` worker threads drains languages
/// (each owning its own single-language `LspManager`) into a channel; the
/// calling thread is the sole DB writer, applying collected outcomes in
/// deterministic sorted-language order. On cancel or worker panic, applies
/// nothing (the caller's transaction rolls back byte-clean).
pub(crate) fn resolve_parallel(
    db: &Database,
    root: &Path,
    overrides: &HashMap<String, Vec<String>>,
    by_language: HashMap<String, Vec<UnresolvedEdge>>,
    sink: &ProgressSink<'_>,
    cap: usize,
    cancel: Option<LspCancel<'_>>,
) -> Result<LspResolveStats> {
    let queue: Mutex<std::collections::VecDeque<(String, Vec<UnresolvedEdge>)>> =
        Mutex::new(by_language.into_iter().collect());
    let (tx, rx) = std::sync::mpsc::channel::<Result<LangResult>>();

    // Workers: own one single-language manager at a time, drain, send, drop the
    // manager (RAII shutdown) before popping the next — peak resident servers
    // never exceeds `cap`.
    // catch_unwind inside each worker so a drain panic becomes a sent error,
    // never an unwind — std::thread::scope re-panics on block exit if ANY
    // spawned thread panicked, even when joined, so panics must not escape here.
    std::thread::scope(|scope| {
        for _ in 0..cap {
            let queue = &queue;
            let tx = tx.clone();
            scope.spawn(move || loop {
                let next = queue.lock().expect("work queue poisoned").pop_front();
                let Some((language, edges)) = next else { break };
                let mut manager = LspManager::with_overrides(root, overrides.clone());
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    drain_language(root, &mut manager, &language, &edges, sink, cancel)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("LSP worker panicked for {language}")));
                let _ = tx.send(res);
            });
        }
        drop(tx); // close the channel once all clones are moved into workers
    });

    // Funnel: collect everything, then decide. Cancel wins over a panic/error;
    // either aborts before any apply so the surrounding tx rolls back clean.
    let mut results: Vec<LangResult> = Vec::new();
    let mut first_err: Option<anyhow::Error> = None;
    for msg in rx {
        match msg {
            Ok(res) => results.push(res),
            Err(e) => {
                if cartog_core::is_cancelled(&e) {
                    return Err(e);
                }
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if let Some(e) = first_err {
        tracing::warn!("LSP resolution failed: {e:#}");
        return Err(e);
    }

    // Apply in deterministic sorted-language order (single writer).
    results.sort_by(|a, b| a.language.cmp(&b.language));
    let mut stats = LspResolveStats::default();
    for res in &results {
        stats.any_server_started |= res.started;
        let (r, u, x) = apply_lang_outcomes(db, &res.language, &res.outcomes)?;
        stats.resolved += r;
        stats.marked_unresolvable += u;
        stats.marked_external += x;
    }
    Ok(stats)
}

/// Test-only hook to inject a worker panic for a given language.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::atomic::{AtomicBool, Ordering};
    static PANIC_ARMED: AtomicBool = AtomicBool::new(false);

    pub fn arm_panic() {
        PANIC_ARMED.store(true, Ordering::SeqCst);
    }
    pub fn disarm_panic() {
        PANIC_ARMED.store(false, Ordering::SeqCst);
    }
    pub(crate) fn should_panic(_language: &str) -> bool {
        PANIC_ARMED.load(Ordering::SeqCst)
    }
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
        let target = Symbol::new(
            "target",
            SymbolKind::Function,
            "target.py",
            10,
            20,
            0,
            100,
            None,
        );
        db.insert_symbols(&[caller.clone(), target]).unwrap();
        for i in 0..n {
            let e = Edge::new(
                &caller.id,
                "target",
                EdgeKind::Calls,
                "a.py",
                (i + 2) as u32,
            );
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
        assert_eq!(
            (resolved, marked),
            (0, 0),
            "no success → no unresolvable mark"
        );
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
    fn unlocatable_edges_are_marked_unresolvable() {
        // An edge whose column couldn't be found is unresolvable-by-LSP; its
        // count folds into marked_unresolvable alongside a real success.
        let (db, ids) = db_with_edges(2);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10)],
            pending_unlocatable: vec![ids[1]],
            ..Default::default()
        };
        let (resolved, marked_u, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u, marked_x), (1, 1, 0));
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 1, "resolved");
        assert_eq!(
            db.edge_resolution_state(ids[1]).unwrap(),
            2,
            "unlocatable → state=2"
        );
    }

    #[test]
    fn unlocatable_marked_even_with_zero_resolved() {
        // Unlike pending_unresolvable, unlocatable has no resolved-gate: it's a
        // deterministic cartog-side fact, marked whenever the server stayed alive.
        // server_died still suppresses it (untrusted pass).
        let (db, ids) = db_with_edges(1);
        let outcomes = LangOutcomes {
            pending_unlocatable: vec![ids[0]],
            ..Default::default()
        };
        let (resolved, marked_u, _) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u), (0, 1), "no gate on resolved");
        assert_eq!(db.edge_resolution_state(ids[0]).unwrap(), 2);

        let (db2, ids2) = db_with_edges(1);
        let dead = LangOutcomes {
            pending_unlocatable: vec![ids2[0]],
            server_died: true,
            ..Default::default()
        };
        let (_, marked_dead, _) = apply_lang_outcomes(&db2, "python", &dead).unwrap();
        assert_eq!(marked_dead, 0, "server death suppresses unlocatable marks");
        assert_eq!(
            db2.edge_resolution_state(ids2[0]).unwrap(),
            0,
            "stays state=0 on death"
        );
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
        assert_eq!(
            db.edge_resolution_state(ids[1]).unwrap(),
            2,
            "no-symbol → state=2"
        );
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
            ..Default::default()
        };
        let (resolved, marked_u, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u, marked_x), (1, 0, 0));
        assert_eq!(
            db.edge_resolution_state(ids[1]).unwrap(),
            0,
            "unmarked on death"
        );
        assert_eq!(
            db.edge_resolution_state(ids[2]).unwrap(),
            0,
            "unmarked on death"
        );
    }

    #[test]
    fn stopped_early_still_commits_drained_answers() {
        // A mute-server early stop (unlike a process death) keeps the answers
        // drained from responsive files: external/unresolvable are committed.
        let (db, ids) = db_with_edges(3);
        let outcomes = LangOutcomes {
            in_root: vec![in_root_at(ids[0], 10)],
            pending_unresolvable: vec![ids[1]],
            pending_external: vec![ids[2]],
            stopped_early: true,
            ..Default::default()
        };
        let (resolved, marked_u, marked_x) = apply_lang_outcomes(&db, "python", &outcomes).unwrap();
        assert_eq!((resolved, marked_u, marked_x), (1, 1, 1));
        assert_eq!(
            db.edge_resolution_state(ids[1]).unwrap(),
            2,
            "unresolvable kept"
        );
        assert_eq!(
            db.edge_resolution_state(ids[2]).unwrap(),
            3,
            "external kept"
        );
    }

    #[test]
    fn effective_cap_resolves_auto_and_clamps() {
        assert_eq!(effective_cap(0, 3), 3, "auto = min(langs, 4)");
        assert_eq!(effective_cap(0, 10), 4, "auto capped at 4");
        assert_eq!(effective_cap(10, 3), 3, "clamp to langs");
        assert_eq!(effective_cap(2, 5), 2);
        assert_eq!(effective_cap(0, 0), 1, "≥1 even with no languages");
    }

    #[test]
    fn progress_sink_is_monotonic_and_final_lands() {
        let ticks = std::sync::Mutex::new(Vec::new());
        let cb = |d, t| ticks.lock().unwrap().push((d, t));
        let sink = ProgressSink::new(10, Some(&cb));
        sink.emit(5);
        sink.emit(3); // backward → suppressed
        sink.emit(8);
        sink.emit_final(10);
        assert_eq!(*ticks.lock().unwrap(), vec![(5, 10), (8, 10), (10, 10)]);
    }

    /// Build a multi-language DB (python + ruby edges) for orchestration tests.
    fn db_two_languages() -> (Database, Vec<i64>) {
        let db = Database::open_memory().unwrap();
        let py = Symbol::new("pc", SymbolKind::Function, "a.py", 1, 5, 0, 100, None);
        let rb = Symbol::new("rc", SymbolKind::Function, "a.rb", 1, 5, 0, 100, None);
        db.insert_symbols(&[py.clone(), rb.clone()]).unwrap();
        db.insert_edge(&Edge::new(&py.id, "x", EdgeKind::Calls, "a.py", 2))
            .unwrap();
        db.insert_edge(&Edge::new(&rb.id, "y", EdgeKind::Calls, "a.rb", 2))
            .unwrap();
        let ids = db
            .unresolved_edges()
            .unwrap()
            .iter()
            .map(|e| e.edge_id)
            .collect();
        (db, ids)
    }

    #[test]
    fn parallel_with_no_servers_marks_nothing_and_matches_serial() {
        // No LSP servers in the test env, so every manager.start fails. Both
        // cap=1 (serial) and cap=4 (parallel) must mark nothing and agree.
        let _g = parallel_lock();
        let tmp = tempfile::tempdir().unwrap();
        let ov = fake_overrides();

        let (db1, ids1) = db_two_languages();
        let by1: HashMap<String, Vec<UnresolvedEdge>> = group(&db1);
        let sink1 = ProgressSink::new(2, None);
        let s1 = resolve_parallel(&db1, tmp.path(), &ov, by1, &sink1, 1, None).unwrap();

        let (db4, ids4) = db_two_languages();
        let by4 = group(&db4);
        let sink4 = ProgressSink::new(2, None);
        let s4 = resolve_parallel(&db4, tmp.path(), &ov, by4, &sink4, 4, None).unwrap();

        assert_eq!(
            (s1.resolved, s1.marked_unresolvable, s1.marked_external),
            (0, 0, 0)
        );
        assert_eq!(
            (s1.resolved, s1.marked_unresolvable, s1.marked_external),
            (s4.resolved, s4.marked_unresolvable, s4.marked_external)
        );
        for id in ids1.iter().chain(&ids4) {
            let db = if ids1.contains(id) { &db1 } else { &db4 };
            assert_eq!(db.edge_resolution_state(*id).unwrap(), 0, "stays state=0");
        }
    }

    #[test]
    fn parallel_cancel_returns_cancelled_and_marks_nothing() {
        let _g = parallel_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (db, ids) = db_two_languages();
        let by = group(&db);
        let sink = ProgressSink::new(2, None);
        let cancel = || true;
        let err = resolve_parallel(
            &db,
            tmp.path(),
            &fake_overrides(),
            by,
            &sink,
            4,
            Some(&cancel),
        )
        .expect_err("tripped cancel must abort");
        assert!(err.to_string().contains("cancelled"), "got: {err}");
        for id in &ids {
            assert_eq!(
                db.edge_resolution_state(*id).unwrap(),
                0,
                "nothing marked on cancel"
            );
        }
    }

    #[test]
    fn parallel_worker_panic_surfaces_distinct_error() {
        // Arm the test-only panic hook; the funnel must convert the worker panic
        // into a non-cancel error and apply nothing.
        let _g = parallel_lock();
        let tmp = tempfile::tempdir().unwrap();
        let (db, ids) = db_two_languages();
        let by = group(&db);
        let sink = ProgressSink::new(2, None);
        test_hooks::arm_panic();
        let res = resolve_parallel(&db, tmp.path(), &fake_overrides(), by, &sink, 4, None);
        test_hooks::disarm_panic();
        let err = res.expect_err("a worker panic must surface as an error");
        assert!(err.to_string().contains("panicked"), "got: {err}");
        assert!(!cartog_core::is_cancelled(&err), "panic is not a cancel");
        for id in &ids {
            assert_eq!(
                db.edge_resolution_state(*id).unwrap(),
                0,
                "nothing applied on panic"
            );
        }
    }

    fn group(db: &Database) -> HashMap<String, Vec<UnresolvedEdge>> {
        let mut by: HashMap<String, Vec<UnresolvedEdge>> = HashMap::new();
        for e in db.unresolved_edges().unwrap() {
            if let Some(lang) = cartog_core::detect_language(std::path::Path::new(&e.file_path)) {
                by.entry(lang.to_string()).or_default().push(e);
            }
        }
        by
    }

    /// Overrides pointing each test language at a non-existent binary so
    /// `manager.start` fails immediately (no 20s server-ready wait) and the test
    /// is hermetic regardless of which LSP servers are installed.
    fn fake_overrides() -> HashMap<String, Vec<String>> {
        ["python", "ruby"]
            .iter()
            .map(|l| (l.to_string(), vec!["cartog-no-such-lsp-binary".to_string()]))
            .collect()
    }

    // The panic hook is a process-global flag; serialize every resolve_parallel
    // test (they all run drain_language, which reads the flag) so an armed panic
    // can't leak into a concurrently-running sibling.
    static PARALLEL_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn parallel_lock() -> std::sync::MutexGuard<'static, ()> {
        PARALLEL_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A root with `n` one-edge files (each containing the target name on line
    /// 1) plus the matching [`UnresolvedEdge`]s, for drain_language tests.
    #[cfg(unix)]
    fn root_with_edge_files(n: usize) -> (tempfile::TempDir, Vec<UnresolvedEdge>) {
        let tmp = tempfile::tempdir().unwrap();
        let edges = (0..n)
            .map(|i| {
                let name = format!("f{i}.py");
                std::fs::write(tmp.path().join(&name), "target_fn()\n").unwrap();
                UnresolvedEdge {
                    edge_id: 1000 + i as i64,
                    target_name: "target_fn".to_string(),
                    file_path: name,
                    line: 1,
                }
            })
            .collect();
        (tmp, edges)
    }

    #[cfg(unix)]
    #[test]
    fn mute_server_stops_early_after_consecutive_timeout_windows() {
        use crate::manager::test_support::silent_fake_server;

        let _guard = parallel_lock(); // drain_language reads the global panic hook
                                      // One edge per file → one window per file, so the window limit is hit
                                      // after UNRESPONSIVE_WINDOW_LIMIT files.
        let n = UNRESPONSIVE_WINDOW_LIMIT + 2;
        let (tmp, edges) = root_with_edge_files(n);
        let mut mgr = LspManager::new(tmp.path());
        mgr.insert_client_for_test("python", silent_fake_server());
        mgr.set_definition_timeout_for_test("python", std::time::Duration::from_millis(200));

        let sink = ProgressSink::new(n as u32, None);
        let res = drain_language(tmp.path(), &mut mgr, "python", &edges, &sink, None).unwrap();

        assert!(res.outcomes.stopped_early, "a mute server must stop early");
        assert!(
            !res.outcomes.server_died,
            "a mute-but-alive server is not a death"
        );
        assert!(res.outcomes.in_root.is_empty());
        assert_eq!(
            sink.processed(),
            UNRESPONSIVE_WINDOW_LIMIT as u32,
            "drain must stop after the window limit, not visit all {n} files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn drain_language_stops_early_on_write_timeout() {
        use crate::manager::test_support::silent_fake_server;

        let _guard = parallel_lock(); // drain_language reads the global panic hook

        // One file whose content exceeds the ~64KB pipe buffer, so didOpen's write
        // wedges against a server that never reads stdin.
        let tmp = tempfile::tempdir().unwrap();
        let big = format!("target_fn()\n{}", "x".repeat(200_000));
        std::fs::write(tmp.path().join("f.py"), &big).unwrap();
        let edges = vec![UnresolvedEdge {
            edge_id: 1,
            target_name: "target_fn".to_string(),
            file_path: "f.py".to_string(),
            line: 1,
        }];

        let mut mgr = LspManager::new(tmp.path());
        mgr.insert_client_for_test("python", silent_fake_server());
        mgr.set_write_timeout_for_test("python", std::time::Duration::from_millis(300));

        let sink = ProgressSink::new(1, None);
        let started = std::time::Instant::now();
        let res = drain_language(tmp.path(), &mut mgr, "python", &edges, &sink, None).unwrap();
        let elapsed = started.elapsed();

        assert!(
            res.outcomes.stopped_early,
            "a server that stops reading stdin must stop the drain early"
        );
        assert!(!res.outcomes.server_died, "stuck-but-alive is not a death");
        // Bounded, not the 10s default deadline: the write timeout drove the stop.
        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "took {elapsed:?}"
        );
        // The wedged client must be dropped so a warm manager can't reuse it and
        // re-block on the next pass.
        assert!(
            !mgr.has_client_for_test("python"),
            "a wedged client must be dropped, not left for start() to reuse"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unlocatable_edges_are_bucketed_and_not_queried() {
        use crate::manager::test_support::{null_result_frames, scripted_fake_server};

        let _guard = parallel_lock(); // drain_language reads the global panic hook

        // One file, two edges: `findable` sits on its line; the second edge's
        // target `Pool::new` never appears on its line (line 2 is a different
        // expression), so no column can be computed — it lands in
        // pending_unlocatable and is never queried. Only the findable edge
        // reaches the server.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("f.py"), "findable()\nfoo.bar().baz()\n").unwrap();
        let edges = vec![
            UnresolvedEdge {
                edge_id: 1,
                target_name: "findable".to_string(),
                file_path: "f.py".to_string(),
                line: 1,
            },
            UnresolvedEdge {
                edge_id: 2,
                target_name: "Pool::new".to_string(),
                file_path: "f.py".to_string(),
                line: 2,
            },
        ];

        // The server answers exactly one request (id 1) — the one findable edge.
        let mut mgr = LspManager::new(tmp.path());
        mgr.insert_client_for_test("python", scripted_fake_server(&null_result_frames(&[1])));

        let sink = ProgressSink::new(2, None);
        let res = drain_language(tmp.path(), &mut mgr, "python", &edges, &sink, None).unwrap();

        assert_eq!(
            res.outcomes.pending_unlocatable,
            vec![2],
            "the unfindable edge is bucketed as unlocatable"
        );
        assert_eq!(
            res.outcomes.pending_unresolvable,
            vec![1],
            "only the findable edge was queried (server said no def)"
        );
        assert_eq!(
            sink.processed(),
            1,
            "only the findable edge is queried — the unlocatable one never is"
        );
    }

    #[cfg(unix)]
    #[test]
    fn replying_server_is_never_flagged_unresponsive() {
        use crate::manager::test_support::{null_result_frames, scripted_fake_server};

        let _guard = parallel_lock(); // drain_language reads the global panic hook
        let n = UNRESPONSIVE_WINDOW_LIMIT + 2;
        let (tmp, edges) = root_with_edge_files(n);
        // Ids run 1..=n (one request per file). Default 10s deadline on purpose:
        // a short one races fake-server startup under load (late reply = stale).
        let ids: Vec<i64> = (1..=n as i64).collect();
        let mut mgr = LspManager::new(tmp.path());
        mgr.insert_client_for_test("python", scripted_fake_server(&null_result_frames(&ids)));

        let sink = ProgressSink::new(n as u32, None);
        let res = drain_language(tmp.path(), &mut mgr, "python", &edges, &sink, None).unwrap();

        assert!(!res.outcomes.stopped_early && !res.outcomes.server_died);
        assert_eq!(res.outcomes.pending_unresolvable.len(), n);
        assert_eq!(sink.processed(), n as u32);
    }
}
