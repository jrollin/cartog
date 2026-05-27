use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{EdgeKindFilter, SymbolKindFilter};
use crate::config::CartogConfig;
use cartog_core::{EdgeKind, SymbolKind};
use cartog_db::{Database, MAX_SEARCH_LIMIT};
use cartog_indexer as indexer;
use cartog_rag as rag;
use cartog_watch::{self as watch, WatchConfig};

pub mod ide;
pub mod init;
pub mod remote;

/// Stderr progress reporter for long-running CLI commands.
///
/// On a TTY it renders an animated spinner whose label tracks the current
/// phase. On a non-TTY (the Claude Code SessionStart hook, CI, piped output)
/// it prints a plain line on each phase change plus a periodic heartbeat, so a
/// multi-minute first index is never silent. Use [`Spinner::set_phase`] from a
/// progress callback to update the label/heartbeat.
struct Spinner {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    fn start(label: &'static str) -> Option<Self> {
        let is_tty = std::io::stderr().is_terminal();
        // Non-TTY callers (CI, pipes, scripts capturing stderr) stay silent by
        // default — only opt in via CARTOG_PROGRESS=1, which the Claude Code
        // SessionStart hook sets so its long first index isn't a silent wait.
        if !is_tty && std::env::var_os("CARTOG_PROGRESS").is_none() {
            return None;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(Mutex::new(label.to_string()));
        let stop_clone = Arc::clone(&stop);
        let phase_clone = Arc::clone(&phase);
        let handle = std::thread::spawn(move || {
            if is_tty {
                Self::run_tty(&stop_clone, &phase_clone);
            } else {
                Self::run_plain(&stop_clone, &phase_clone);
            }
        });
        Some(Self {
            stop,
            phase,
            handle: Some(handle),
        })
    }

    /// Update the displayed phase. On a non-TTY this prints a new line
    /// immediately so each phase boundary is visible in the hook log.
    fn set_phase(&self, phase: impl Into<String>) {
        if let Ok(mut p) = self.phase.lock() {
            *p = phase.into();
        }
    }

    fn run_tty(stop: &AtomicBool, phase: &Mutex<String>) {
        const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut i = 0usize;
        let start = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let elapsed = start.elapsed().as_secs();
            let label = phase.lock().map(|p| p.clone()).unwrap_or_default();
            let mut err = std::io::stderr().lock();
            // \r + clear-to-eol + frame + label + elapsed
            let _ = write!(err, "\r\x1b[K{} {label} ({elapsed}s)", FRAMES[i]);
            let _ = err.flush();
            drop(err);
            i = (i + 1) % FRAMES.len();
            std::thread::sleep(Duration::from_millis(100));
        }
        // Clear the spinner line on exit.
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[K");
        let _ = err.flush();
    }

    /// Non-TTY heartbeat: emit a line whenever the phase changes, plus one
    /// every 5s while a phase is still running, so the hook output is never
    /// silent for minutes. No carriage returns or escape codes — plain log.
    fn run_plain(stop: &AtomicBool, phase: &Mutex<String>) {
        let start = std::time::Instant::now();
        let mut last_label = String::new();
        let mut last_emit = std::time::Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let label = phase.lock().map(|p| p.clone()).unwrap_or_default();
            let changed = label != last_label;
            if changed || last_emit.elapsed() >= Duration::from_secs(5) {
                let elapsed = start.elapsed().as_secs();
                eprintln!("  {label}… ({elapsed}s)");
                last_label = label;
                last_emit = std::time::Instant::now();
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Capitalize the first character of a phase label for CLI display. Phase
/// wording itself is owned by `ProgressUpdate::label()` in the indexer/rag
/// crates; the spinner only adjusts presentation.
fn capitalize_phase(label: String) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => label,
    }
}

/// Build a progress callback that drives `spinner`'s phase label from a
/// `label_of` projection. Returns the callback plus the `Arc<Spinner>` the
/// caller must keep alive for the duration of the work, then pass to
/// [`stop_spinner`]. Centralizes the Arc lifecycle so the explicit
/// `Arc::into_inner` stop is reliable (no stray clone keeps the count above 1).
fn spinner_callback<U>(
    spinner: &Option<Arc<Spinner>>,
    label_of: fn(&U) -> String,
) -> Option<impl Fn(U)> {
    spinner.as_ref().map(|s| {
        let s = Arc::clone(s);
        move |u: U| s.set_phase(capitalize_phase(label_of(&u)))
    })
}

/// Stop and join a spinner created via [`Spinner::start`] + `Arc::new`. The
/// callback built by [`spinner_callback`] must already be dropped so the Arc
/// strong count is 1 and `Arc::into_inner` succeeds.
fn stop_spinner(spinner: Option<Arc<Spinner>>) {
    if let Some(s) = spinner.and_then(Arc::into_inner) {
        s.stop();
    }
}

fn open_db(path: &Path, embedding_dim: usize) -> Result<Database> {
    Database::open(path, embedding_dim).context("Failed to open cartog database")
}

/// Estimate token count from a string using chars/4 approximation.
#[cfg(test)]
fn estimate_tokens(s: &str) -> u32 {
    (s.len() as u32).div_ceil(4)
}

/// Truncate a string to fit within a token budget, appending a truncation notice.
fn truncate_to_budget(s: &str, max_tokens: u32) -> String {
    let max_bytes = (max_tokens as usize) * 4;
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // Find a char boundary at or before max_bytes, leaving room for notice
    let notice = "\n... (truncated to fit token budget)";
    let target = max_bytes.saturating_sub(notice.len());
    // UTF-8 chars are at most 4 bytes, so we only need to check 4 positions back.
    let cut = (target.saturating_sub(3)..=target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    let mut out = s[..cut].to_string();
    out.push_str(notice);
    out
}

/// Print `data` as pretty JSON if `json` is true, otherwise call `human_fmt`.
/// When `token_budget` is Some, truncate human-readable output to fit.
fn output<T: Serialize>(
    data: &T,
    json: bool,
    token_budget: Option<u32>,
    human_fmt: impl FnOnce(&T) -> String,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(data)?);
    } else {
        let text = human_fmt(data);
        match token_budget {
            Some(budget) => print!("{}", truncate_to_budget(&text, budget)),
            None => print!("{}", text),
        }
    }
    Ok(())
}

/// Hint suffix appended to "no result" messages when the index is empty, so a
/// fresh user can tell "you haven't indexed yet" from a genuine no-match.
/// Returns `""` when the index has symbols (the common case).
fn empty_index_hint(db: &Database) -> &'static str {
    match db.is_empty() {
        Ok(true) => " (index is empty — run 'cartog index .' first)",
        _ => "",
    }
}

/// Suggestion suffix for "no result" messages: when a navigation command
/// (refs/callees/impact/hierarchy) finds no exact match but the fuzzy search
/// surfaces similarly-named symbols, list them so the user can correct a typo
/// or partial name. Returns `""` when the index is empty (the empty-index hint
/// covers that) or when there are no near matches.
fn did_you_mean(db: &Database, name: &str) -> String {
    if name.is_empty() || matches!(db.is_empty(), Ok(true)) {
        return String::new();
    }
    let candidates = match db.search(name, None, None, 5) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    // An exact match means the symbol exists but genuinely has no edges/results;
    // suggesting it would be noise.
    if candidates.iter().any(|s| s.name == name) || candidates.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = candidates.iter().map(|s| s.name.as_str()).collect();
    format!(" — did you mean: {}?", names.join(", "))
}

/// Build or rebuild the code graph index.
pub fn cmd_index(
    db_path: &Path,
    path: &str,
    force: bool,
    lsp: bool,
    json: bool,
    embedding_dim: usize,
) -> Result<()> {
    let root = Path::new(path);
    let db = open_db(db_path, embedding_dim)?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Indexing").map(Arc::new)
    };
    let cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let cb_ref: Option<indexer::ProgressCallback<'_>> =
        cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let result = indexer::index_directory(&db, root, force, lsp, cb_ref, None);
    drop(cb);
    stop_spinner(spinner);
    let result = result?;

    // No-op run: nothing was added or removed this pass. The delta counters
    // are all zero, so the standard "0 symbols, 0 edges" line reads like a
    // failure. Report DB state instead — "up to date" when the index has
    // content, or "no indexable files" for an empty/unsupported tree.
    if !json && result.files_indexed == 0 && result.files_removed == 0 {
        let s = db.stats()?;
        if s.num_symbols == 0 {
            println!("No indexable files found under '{path}'.");
        } else {
            println!(
                "Index up to date ({} files, {} symbols unchanged)",
                s.num_files, s.num_symbols
            );
        }
        return Ok(());
    }

    output(&result, json, None, |r| {
        let lsp_part = if r.edges_lsp_resolved > 0
            || r.edges_marked_unresolvable > 0
            || r.edges_marked_external > 0
        {
            let mut s = format!(
                " ({} heuristic + {} LSP",
                r.edges_resolved, r.edges_lsp_resolved
            );
            if r.edges_marked_unresolvable > 0 {
                s.push_str(&format!(
                    ", {} marked unresolvable",
                    r.edges_marked_unresolvable
                ));
            }
            if r.edges_marked_external > 0 {
                s.push_str(&format!(", {} external", r.edges_marked_external));
            }
            s.push(')');
            s
        } else {
            String::new()
        };
        let sym_detail = if r.symbols_modified > 0 || r.symbols_unchanged > 0 {
            format!(
                " ({} new, {} modified, {} unchanged, {} removed)",
                r.symbols_added, r.symbols_modified, r.symbols_unchanged, r.symbols_removed
            )
        } else {
            String::new()
        };
        let unsupported = if r.files_unsupported > 0 {
            let breakdown = r
                .unsupported_by_ext
                .iter()
                .take(5)
                .map(|(ext, n)| format!("{n} .{ext}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "\n  {} files in unsupported languages not indexed ({breakdown})",
                r.files_unsupported
            )
        } else {
            String::new()
        };
        format!(
            "Indexed {} files ({} skipped, {} removed)\n  {} symbols{}, {} edges ({} resolved{}){}\n",
            r.files_indexed,
            r.files_skipped,
            r.files_removed,
            r.symbols_added + r.symbols_modified + r.symbols_unchanged,
            sym_detail,
            r.edges_added,
            r.edges_resolved + r.edges_lsp_resolved,
            lsp_part,
            unsupported,
        )
    })
}

/// Show symbols and structure of a file.
pub fn cmd_outline(
    db_path: &Path,
    file: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let symbols = db.outline(file)?;
    let file = file.to_string();
    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!("No symbols found in {file}{}\n", empty_index_hint(&db));
        }
        let mut out = String::new();
        for sym in syms {
            let indent = if sym.parent_id.is_some() { "  " } else { "" };
            let async_prefix = if sym.is_async { "async " } else { "" };
            match sym.kind {
                SymbolKind::Import => {
                    let text = sym.signature.as_deref().unwrap_or(&sym.name);
                    out.push_str(&format!("{indent}{text}  L{}\n", sym.start_line));
                }
                _ => {
                    let sig = sym.signature.as_deref().unwrap_or("");
                    out.push_str(&format!(
                        "{indent}{async_prefix}{kind} {name}{sig}  L{start}-{end}\n",
                        kind = sym.kind,
                        name = sym.name,
                        start = sym.start_line,
                        end = sym.end_line,
                    ));
                }
            }
        }
        out
    })
}

/// Find what a symbol calls.
pub fn cmd_callees(
    db_path: &Path,
    name: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let edges = db.callees(name)?;
    let name = name.to_string();
    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!(
                "No callees found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  {file}:{line}\n",
                target = edge.target_name,
                file = edge.file_path,
                line = edge.line,
            ));
        }
        out
    })
}

/// Transitive impact analysis — what breaks if this changes?
pub fn cmd_impact(
    db_path: &Path,
    name: &str,
    depth: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let results = db.impact(name, depth)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct ImpactEntry {
        edge: cartog_core::Edge,
        depth: u32,
    }

    let items: Vec<ImpactEntry> = results
        .into_iter()
        .map(|(edge, d)| ImpactEntry { edge, depth: d })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No impact found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            let indent = "  ".repeat(entry.depth as usize);
            out.push_str(&format!(
                "{indent}{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = entry.edge.source_id,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// All references to a symbol (calls, imports, inherits, references, raises).
pub fn cmd_refs(
    db_path: &Path,
    name: &str,
    kind: Option<EdgeKindFilter>,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = kind.map(EdgeKind::from);
    let results = db.refs(name, kind_filter)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct RefEntry {
        edge: cartog_core::Edge,
        source: Option<cartog_core::Symbol>,
    }

    let items: Vec<RefEntry> = results
        .into_iter()
        .map(|(edge, sym)| RefEntry { edge, source: sym })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No references found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            let source_name = entry
                .source
                .as_ref()
                .map(|s| s.name.as_str())
                .unwrap_or(&entry.edge.source_id);
            out.push_str(&format!(
                "{kind}  {source}  {file}:{line}\n",
                kind = entry.edge.kind,
                source = source_name,
                file = entry.edge.file_path,
                line = entry.edge.line,
            ));
        }
        out
    })
}

/// Show inheritance hierarchy for a class.
pub fn cmd_hierarchy(
    db_path: &Path,
    name: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let pairs = db.hierarchy(name)?;
    let name = name.to_string();

    #[derive(Serialize)]
    struct HierarchyEntry {
        child: String,
        parent: String,
    }

    let items: Vec<HierarchyEntry> = pairs
        .into_iter()
        .map(|(child, parent)| HierarchyEntry { child, parent })
        .collect();

    output(&items, json, token_budget, |items| {
        if items.is_empty() {
            return format!(
                "No hierarchy found for '{name}'{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        let mut out = String::new();
        for entry in items {
            out.push_str(&format!("{} -> {}\n", entry.child, entry.parent));
        }
        out
    })
}

/// File-level import dependencies.
pub fn cmd_deps(
    db_path: &Path,
    file: &str,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let edges = db.file_deps(file)?;
    let file = file.to_string();

    output(&edges, json, token_budget, |edges| {
        if edges.is_empty() {
            return format!(
                "No dependencies found for '{file}'{}\n",
                empty_index_hint(&db)
            );
        }
        let mut out = String::new();
        for edge in edges {
            out.push_str(&format!(
                "{target}  L{line}\n",
                target = edge.target_name,
                line = edge.line
            ));
        }
        out
    })
}

/// Search for symbols by name (case-insensitive prefix + substring match).
#[allow(clippy::too_many_arguments)]
pub fn cmd_search(
    db_path: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    file: Option<&str>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    let limit = limit.min(MAX_SEARCH_LIMIT);
    let symbols = db.search(query, kind_filter, file, limit)?;
    let query = query.to_string();

    output(&symbols, json, token_budget, |syms| {
        if syms.is_empty() {
            return format!(
                "No symbols found matching '{query}'{}\n",
                empty_index_hint(&db)
            );
        }
        let mut out = String::new();
        for sym in syms {
            out.push_str(&format!(
                "{kind}  {name}  {file}:{line}\n",
                kind = sym.kind,
                name = sym.name,
                file = sym.file_path,
                line = sym.start_line,
            ));
        }
        out
    })
}

/// Upload the local index DB to S3-compatible storage.
pub fn cmd_push(
    db_path: &Path,
    config: &CartogConfig,
    cli_remote: Option<&str>,
    json: bool,
) -> Result<()> {
    remote::push_index(db_path, config, cli_remote, json)
}

/// Download an index DB from S3-compatible storage into the local project.
pub fn cmd_pull(
    db_path: &Path,
    config: &CartogConfig,
    cli_remote: Option<&str>,
    force: bool,
    no_sign_request: bool,
    json: bool,
) -> Result<()> {
    remote::pull_index(db_path, config, cli_remote, force, no_sign_request, json)
}

/// Index statistics summary.
pub fn cmd_stats(db_path: &Path, json: bool, embedding_dim: usize) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let stats = db.stats()?;

    output(&stats, json, None, |stats| {
        let mut out = String::new();
        out.push_str(&format!("Files:    {}\n", stats.num_files));
        out.push_str(&format!("Symbols:  {}\n", stats.num_symbols));
        let mut edge_parts = vec![format!("{} resolved", stats.num_resolved)];
        if stats.num_external > 0 {
            edge_parts.push(format!("{} external", stats.num_external));
        }
        if stats.num_unresolvable > 0 {
            edge_parts.push(format!("{} unresolvable", stats.num_unresolvable));
        }
        out.push_str(&format!(
            "Edges:    {} ({})\n",
            stats.num_edges,
            edge_parts.join(", ")
        ));
        if !stats.languages.is_empty() {
            out.push_str("Languages:\n");
            for (lang, count) in &stats.languages {
                out.push_str(&format!("  {lang}: {count} files\n"));
            }
        }
        if !stats.symbol_kinds.is_empty() {
            out.push_str("Symbols by kind:\n");
            for (kind, count) in &stats.symbol_kinds {
                out.push_str(&format!("  {kind}: {count}\n"));
            }
        }
        out
    })
}

/// Token-budget-aware codebase summary: file tree + top symbols ranked by centrality.
pub fn cmd_map(db_path: &Path, tokens: u32, json: bool, embedding_dim: usize) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let files = db.all_files()?;

    if files.is_empty() {
        if json {
            println!("{{}}");
        } else {
            println!("No files indexed. Run 'cartog index .' first.");
        }
        return Ok(());
    }

    if json {
        // For JSON, return structured data without budget constraints
        let symbols = db.top_symbols(200)?;

        #[derive(Serialize)]
        struct MapResult {
            files: Vec<String>,
            top_symbols: Vec<cartog_core::Symbol>,
        }

        let result = MapResult {
            files,
            top_symbols: symbols,
        };
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    // Human-readable: build file tree, then fill remaining budget with symbols
    let budget_bytes = (tokens as usize) * 4;

    // Phase 1: file tree
    let mut out = String::new();
    out.push_str(&format!("# Codebase Map ({} files)\n\n", files.len()));
    for file in &files {
        out.push_str(&format!("  {file}\n"));
    }

    let tree_bytes = out.len();
    let remaining = budget_bytes.saturating_sub(tree_bytes);

    if remaining < 100 {
        print!("{}", truncate_to_budget(&out, tokens));
        return Ok(());
    }

    // Phase 2: top symbols by centrality, grouped by file
    out.push_str("\n# Top Symbols (by reference count)\n\n");

    let symbols = db.top_symbols(500)?;
    let mut current_file = "";

    for sym in &symbols {
        if out.len() >= budget_bytes {
            break;
        }

        if sym.file_path != current_file {
            let header = format!("\n{}:\n", sym.file_path);
            if out.len() + header.len() > budget_bytes {
                break;
            }
            out.push_str(&header);
            current_file = &sym.file_path;
        }

        let sig = sym.signature.as_deref().unwrap_or("");
        let line = format!(
            "  {kind} {name}{sig}  L{start}-{end}  ({refs} refs)\n",
            kind = sym.kind,
            name = sym.name,
            start = sym.start_line,
            end = sym.end_line,
            refs = sym.in_degree,
        );

        if out.len() + line.len() > budget_bytes {
            break;
        }
        out.push_str(&line);
    }

    print!("{out}");
    Ok(())
}

/// Show symbols affected by recent git changes.
pub fn cmd_changes(
    db_path: &Path,
    commits: u32,
    kind: Option<SymbolKindFilter>,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let root = std::env::current_dir()?;

    let changed_files = indexer::git_recently_changed_files(&root, commits)?;

    if changed_files.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No files changed in the last {commits} commits.");
        }
        return Ok(());
    }

    let kind_filter = match kind {
        Some(SymbolKindFilter::All) | None => None,
        Some(k) => Some(cartog_core::SymbolKind::from(k)),
    };
    let symbols = db.symbols_for_files(&changed_files, kind_filter)?;

    let result = cartog_core::ChangesResult {
        changed_files,
        symbols,
    };

    output(&result, json, token_budget, |r| {
        let mut out = format!(
            "{} files changed in last {} commits, {} symbols affected\n\n",
            r.changed_files.len(),
            commits,
            r.symbols.len()
        );
        let mut current_file = "";
        for sym in &r.symbols {
            if sym.file_path != current_file {
                current_file = &sym.file_path;
                out.push_str(&format!("{current_file}:\n"));
            }
            let sig = sym.signature.as_deref().unwrap_or("");
            out.push_str(&format!(
                "  {kind} {name}{sig}  L{start}-{end}\n",
                kind = sym.kind,
                name = sym.name,
                start = sym.start_line,
                end = sym.end_line,
            ));
        }
        let files_with_symbols: std::collections::HashSet<&str> =
            r.symbols.iter().map(|s| s.file_path.as_str()).collect();
        let unindexed: Vec<_> = r
            .changed_files
            .iter()
            .filter(|f| !files_with_symbols.contains(f.as_str()))
            .collect();
        if !unindexed.is_empty() {
            out.push_str(&format!(
                "\n{} changed files not in index:\n",
                unindexed.len()
            ));
            for f in unindexed {
                out.push_str(&format!("  {f}\n"));
            }
        }
        out
    })
}

// ── RAG Commands ──

/// Download the embedding model.
pub fn cmd_rag_setup(json: bool) -> Result<()> {
    let spinner = if json {
        None
    } else {
        // One-time notice so the multi-hundred-MB download isn't a silent wait.
        // Size matches docs/usage.md (embedding ~80MB + reranker ~1.1GB).
        eprintln!("Downloading embedding + re-ranker models (~1.2GB, one-time)…");
        Spinner::start("Downloading models")
    };
    // Download bi-encoder (embeddings)
    let embed_result = rag::setup::download_model();
    // Download cross-encoder (re-ranking)
    let rerank_result = rag::setup::download_cross_encoder();
    if let Some(s) = spinner {
        s.stop();
    }
    let embed_result = embed_result?;
    let rerank_result = rerank_result?;

    #[derive(serde::Serialize)]
    struct CombinedSetup {
        embedding: rag::setup::SetupResult,
        reranker: rag::setup::SetupResult,
    }

    let combined = CombinedSetup {
        embedding: embed_result,
        reranker: rerank_result,
    };

    output(&combined, json, None, |c| {
        format!(
            "Embedding model: {}\nRe-ranker model: {}\nModels ready. You can now run 'cartog rag index'.\n",
            c.embedding.model_dir, c.reranker.model_dir
        )
    })
}

/// Build embedding index for semantic search.
pub fn cmd_rag_index(
    db_path: &Path,
    path: &str,
    force: bool,
    json: bool,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    let root = Path::new(path);
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
        .context("failed to reconcile embedding fingerprint")?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Indexing code graph").map(Arc::new)
    };
    let ix_cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let ix_cb_ref: Option<indexer::ProgressCallback<'_>> =
        ix_cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let index_res = indexer::index_directory(&db, root, false, false, ix_cb_ref, None);
    drop(ix_cb);
    stop_spinner(spinner);
    let _index_result = index_res?;

    let spinner = if json {
        None
    } else {
        Spinner::start("Embedding symbols").map(Arc::new)
    };
    let rag_cb = spinner_callback(&spinner, rag::indexer::ProgressUpdate::label);
    let rag_cb_ref: Option<rag::indexer::ProgressCallback<'_>> =
        rag_cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let embed_res = rag::indexer::index_embeddings(&db, provider.as_mut(), force, rag_cb_ref, None);
    drop(rag_cb);
    stop_spinner(spinner);
    let result = embed_res?;

    output(&result, json, None, |r| {
        format!(
            "Embedded {} symbols ({} skipped, {} total with content)\n",
            r.symbols_embedded, r.symbols_skipped, r.total_content_symbols
        )
    })
}

/// Semantic search over code symbols.
#[allow(clippy::too_many_arguments)]
pub fn cmd_rag_search(
    db_path: &Path,
    query: &str,
    kind: Option<SymbolKindFilter>,
    limit: u32,
    json: bool,
    token_budget: Option<u32>,
    provider_config: &rag::EmbeddingProviderConfig,
    tuning: &rag::search::SearchTuning,
) -> Result<()> {
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    // NOTE: `cartog rag search` deliberately does NOT call
    // `reconcile_embedding_fingerprint`. The reconcile is destructive
    // (drops `symbol_vec` on mismatch) and can race a primary
    // `cartog serve` writer if the user's `.cartog.toml` changed since
    // last index. Search is read-only by nature; if the fingerprint
    // mismatches, the user gets the embeddings produced by the previous
    // provider — possibly poor results, but no data loss. Re-embedding
    // is `cartog rag index`'s job, which DOES reconcile.
    let kind_filter = match kind {
        Some(SymbolKindFilter::All) => rag::search::KindFilter::All,
        Some(k) => rag::search::KindFilter::Exact(cartog_core::SymbolKind::from(k)),
        None => rag::search::KindFilter::CodeOnly,
    };

    // Lazy reranker: the cross-encoder ONNX model is loaded only if retrieval
    // produced enough candidates for `rerank_min` to fire. For a one-shot CLI
    // command that may return fewer than `rerank_min` hits, this avoids
    // ~100-200ms of model-load latency + memory on every invocation.
    let reranker_factory = if provider_config.reranker_provider == "none" {
        None
    } else {
        let name = provider_config.reranker_provider.clone();
        Some(move || rag::create_reranker_provider(&name))
    };
    let search_result = rag::search::hybrid_search_tuned_lazy(
        &db,
        query,
        limit,
        kind_filter,
        provider.as_mut(),
        reranker_factory,
        tuning,
    )?;
    let query = query.to_string();

    output(&search_result, json, token_budget, |sr| {
        if sr.results.is_empty() {
            let mut out = format!("No results found for '{query}'\n");
            if sr.fts_count == 0 && sr.vec_count == 0 {
                out.push_str("Hint: run 'cartog rag index' to build the semantic search index.\n");
            }
            return out;
        }
        let mut out = format!(
            "Found {} results (FTS: {}, vector: {}, merged: {})\n\n",
            sr.results.len(),
            sr.fts_count,
            sr.vec_count,
            sr.merged_count
        );
        for (i, r) in sr.results.iter().enumerate() {
            let sources = r.sources.join("+");
            let rerank_str = r
                .rerank_score
                .map(|s| format!(" rerank={s:.2}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{}. {} {}  {}:{}-{}  [{}] score={:.4}{rerank_str}\n",
                i + 1,
                r.symbol.kind,
                r.symbol.name,
                r.symbol.file_path,
                r.symbol.start_line,
                r.symbol.end_line,
                sources,
                r.rrf_score,
            ));
            if let Some(ref content) = r.content {
                let preview: String = content
                    .lines()
                    .take(3)
                    .map(|l| format!("    {l}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                out.push_str(&format!("{preview}\n\n"));
            }
        }
        out
    })
}

/// Display the current configuration with default-value indicators.
pub fn cmd_config(
    config: &CartogConfig,
    config_path: Option<&Path>,
    config_rejected: bool,
    db_path: &Path,
    json: bool,
) -> Result<()> {
    // When the config file was found but rejected at parse time, `config`
    // is the empty default — displaying it as the active config would
    // silently lie. Show an explicit error and bail with the path so the
    // user knows which file to fix.
    if config_rejected {
        // Invariant: ConfigLoad::Rejected always carries a path, and main
        // propagates it as `config_path`. Bail explicitly so this surfaces
        // even if the invariant ever changes.
        let p = config_path
            .ok_or_else(|| anyhow::anyhow!("config rejected but path missing — invariant break"))?;
        anyhow::bail!(
            "configuration file {} was rejected (see earlier stderr for the \
             underlying reason). `cartog config` cannot display a meaningful \
             view until the file is fixed.",
            p.display()
        );
    }
    use crate::config::{
        DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
        DEFAULT_RERANKER_PROVIDER,
    };

    let embed = config.embedding.as_ref();
    let ollama = embed.and_then(|e| e.ollama.as_ref());
    let local = embed.and_then(|e| e.local.as_ref());
    let reranker = config.reranker.as_ref();
    let rag = config.rag.as_ref();
    let tuning_defaults = cartog_rag::search::SearchTuning::default();
    // `to_search_tuning()` applies the clamps (retrieval_multiplier.max(1),
    // rerank_min.min(rerank_max)) so what we show matches what the search
    // pipeline will actually use.
    let effective_tuning = rag.map(|r| r.to_search_tuning()).unwrap_or(tuning_defaults);

    let rag_value = |set: Option<u32>, effective: u32, default: u32| ValueDisplay {
        value: effective.to_string(),
        is_default: set.is_none(),
        default: default.to_string(),
    };

    let display = ConfigDisplay {
        config_file: config_path.map(|p| p.to_string_lossy().into_owned()),
        db_path: db_path.to_string_lossy().into_owned(),
        embedding: EmbeddingDisplay {
            provider: ValueDisplay {
                value: embed.map_or(DEFAULT_EMBEDDING_PROVIDER.into(), |e| {
                    e.provider().to_string()
                }),
                is_default: embed.map_or(true, |e| e.provider.is_none()),
                default: DEFAULT_EMBEDDING_PROVIDER.into(),
            },
            model: embed.and_then(|e| e.model.clone()),
            dimension: embed.and_then(|e| e.dimension),
            local: LocalEmbeddingDisplay {
                query_prefix: local.and_then(|l| l.query_prefix.clone()),
                document_prefix: local.and_then(|l| l.document_prefix.clone()),
            },
            ollama: OllamaDisplay {
                base_url: ValueDisplay {
                    value: ollama
                        .map_or(DEFAULT_OLLAMA_BASE_URL.into(), |o| o.base_url().to_string()),
                    is_default: ollama.map_or(true, |o| o.base_url.is_none()),
                    default: DEFAULT_OLLAMA_BASE_URL.into(),
                },
                model: ValueDisplay {
                    value: ollama.map_or(DEFAULT_OLLAMA_MODEL.into(), |o| o.model().to_string()),
                    is_default: ollama.map_or(true, |o| o.model.is_none()),
                    default: DEFAULT_OLLAMA_MODEL.into(),
                },
            },
        },
        reranker: RerankerDisplay {
            provider: ValueDisplay {
                value: reranker.map_or(DEFAULT_RERANKER_PROVIDER.into(), |r| {
                    r.provider().to_string()
                }),
                is_default: reranker.map_or(true, |r| r.provider.is_none()),
                default: DEFAULT_RERANKER_PROVIDER.into(),
            },
        },
        rag: RagDisplay {
            retrieval_multiplier: rag_value(
                rag.and_then(|r| r.retrieval_multiplier),
                effective_tuning.retrieval_multiplier,
                tuning_defaults.retrieval_multiplier,
            ),
            retrieval_floor: rag_value(
                rag.and_then(|r| r.retrieval_floor),
                effective_tuning.retrieval_floor,
                tuning_defaults.retrieval_floor,
            ),
            rerank_max: rag_value(
                rag.and_then(|r| r.rerank_max),
                effective_tuning.rerank_max,
                tuning_defaults.rerank_max,
            ),
            rerank_min: rag_value(
                rag.and_then(|r| r.rerank_min),
                effective_tuning.rerank_min,
                tuning_defaults.rerank_min,
            ),
        },
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&display)?);
    } else {
        print!("{}", format_config_human(&display));
    }
    Ok(())
}

fn format_value(v: &ValueDisplay) -> String {
    if v.is_default {
        format!("{} (default)", v.value)
    } else {
        format!("{} (default: {})", v.value, v.default)
    }
}

fn format_optional(v: &Option<String>) -> &str {
    match v {
        Some(s) => s.as_str(),
        None => "-",
    }
}

fn format_config_human(d: &ConfigDisplay) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    writeln!(
        out,
        "Config file: {}",
        d.config_file.as_deref().unwrap_or("none")
    )
    .unwrap();
    writeln!(out, "Database:    {}", d.db_path).unwrap();

    writeln!(out, "\n[embedding]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.embedding.provider)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_optional(&d.embedding.model)
    )
    .unwrap();
    writeln!(
        out,
        "  dimension:         {}",
        d.embedding.dimension.map_or("-".into(), |v| v.to_string())
    )
    .unwrap();

    writeln!(out, "\n[embedding.local]").unwrap();
    writeln!(
        out,
        "  query_prefix:      {}",
        format_optional(&d.embedding.local.query_prefix)
    )
    .unwrap();
    writeln!(
        out,
        "  document_prefix:   {}",
        format_optional(&d.embedding.local.document_prefix)
    )
    .unwrap();

    writeln!(out, "\n[embedding.ollama]").unwrap();
    writeln!(
        out,
        "  base_url:          {}",
        format_value(&d.embedding.ollama.base_url)
    )
    .unwrap();
    writeln!(
        out,
        "  model:             {}",
        format_value(&d.embedding.ollama.model)
    )
    .unwrap();

    writeln!(out, "\n[reranker]").unwrap();
    writeln!(
        out,
        "  provider:          {}",
        format_value(&d.reranker.provider)
    )
    .unwrap();

    writeln!(out, "\n[rag]").unwrap();
    writeln!(
        out,
        "  retrieval_multiplier: {}",
        format_value(&d.rag.retrieval_multiplier)
    )
    .unwrap();
    writeln!(
        out,
        "  retrieval_floor:      {}",
        format_value(&d.rag.retrieval_floor)
    )
    .unwrap();
    writeln!(
        out,
        "  rerank_max:           {}",
        format_value(&d.rag.rerank_max)
    )
    .unwrap();
    writeln!(
        out,
        "  rerank_min:           {}",
        format_value(&d.rag.rerank_min)
    )
    .unwrap();

    out
}

#[derive(Serialize)]
struct ConfigDisplay {
    config_file: Option<String>,
    db_path: String,
    embedding: EmbeddingDisplay,
    reranker: RerankerDisplay,
    rag: RagDisplay,
}

#[derive(Serialize)]
struct RagDisplay {
    retrieval_multiplier: ValueDisplay,
    retrieval_floor: ValueDisplay,
    rerank_max: ValueDisplay,
    rerank_min: ValueDisplay,
}

#[derive(Serialize)]
struct EmbeddingDisplay {
    provider: ValueDisplay,
    model: Option<String>,
    dimension: Option<usize>,
    local: LocalEmbeddingDisplay,
    ollama: OllamaDisplay,
}

#[derive(Serialize)]
struct LocalEmbeddingDisplay {
    query_prefix: Option<String>,
    document_prefix: Option<String>,
}

#[derive(Serialize)]
struct OllamaDisplay {
    base_url: ValueDisplay,
    model: ValueDisplay,
}

#[derive(Serialize)]
struct RerankerDisplay {
    provider: ValueDisplay,
}

#[derive(Serialize)]
struct ValueDisplay {
    value: String,
    is_default: bool,
    default: String,
}

// ── Doctor Command ──

#[derive(Serialize)]
struct CheckResult {
    name: String,
    status: CheckStatus,
    message: String,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum CheckStatus {
    Ok,
    Warn,
    Error,
}

impl CheckStatus {
    fn icon(self) -> &'static str {
        match self {
            CheckStatus::Ok => "+",
            CheckStatus::Warn => "!",
            CheckStatus::Error => "x",
        }
    }
}

#[derive(Serialize)]
struct DoctorReport {
    checks: Vec<CheckResult>,
    summary: DoctorSummary,
}

#[derive(Serialize)]
struct DoctorSummary {
    total: usize,
    ok: usize,
    warn: usize,
    error: usize,
}

fn check_git_repo() -> CheckResult {
    let mut dir = std::env::current_dir().unwrap_or_default();
    loop {
        if dir.join(".git").exists() {
            return CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: format!("git repository at {}", dir.display()),
            };
        }
        if !dir.pop() {
            break;
        }
    }
    CheckResult {
        name: "git".into(),
        status: CheckStatus::Error,
        message: "not inside a git repository".into(),
    }
}

fn check_config(config_path: Option<&Path>, rejected: bool) -> CheckResult {
    match (config_path, rejected) {
        (Some(p), true) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Error,
            message: format!(
                "{} was REJECTED (see stderr at startup for the reason). \
                 cartog is running with defaults; other check rows below \
                 reflect defaults, not your config file.",
                p.display()
            ),
        },
        (Some(p), false) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Ok,
            message: format!("loaded from {}", p.display()),
        },
        (None, _) => CheckResult {
            name: "config".into(),
            status: CheckStatus::Warn,
            message: "no .cartog.toml found (using defaults)".into(),
        },
    }
}

fn check_database(db_path: &Path, embedding_dim: usize) -> CheckResult {
    if !db_path.exists() {
        return CheckResult {
            name: "database".into(),
            status: CheckStatus::Warn,
            message: format!(
                "database not found at {}, run 'cartog index'",
                db_path.display()
            ),
        };
    }
    match Database::open(db_path, embedding_dim) {
        Ok(db) => match db.stats() {
            Ok(stats) if stats.num_files > 0 => CheckResult {
                name: "database".into(),
                status: CheckStatus::Ok,
                message: format!(
                    "{} files, {} symbols at {}",
                    stats.num_files,
                    stats.num_symbols,
                    db_path.display()
                ),
            },
            Ok(_) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "database exists but is empty, run 'cartog index' ({})",
                    db_path.display()
                ),
            },
            Err(e) => CheckResult {
                name: "database".into(),
                status: CheckStatus::Error,
                message: format!("failed to query database at {}: {e}", db_path.display()),
            },
        },
        Err(e) => CheckResult {
            name: "database".into(),
            status: CheckStatus::Error,
            message: format!("failed to open database at {}: {e}", db_path.display()),
        },
    }
}

/// Parse "http://host:port" into a "host:port" string for TCP probing.
fn parse_host_port(url: &str) -> Option<String> {
    let without_scheme = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let host_port = without_scheme.trim_end_matches('/');
    if host_port.contains(':') {
        Some(host_port.to_string())
    } else {
        Some(format!("{host_port}:80"))
    }
}

fn check_embedding_provider(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    match config.provider.as_str() {
        "local" => {
            if rag::is_embedding_model_cached() {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Ok,
                    message: "local model cached".into(),
                }
            } else {
                CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                }
            }
        }
        "ollama" => {
            let base_url = config
                .base_url
                .as_deref()
                .unwrap_or(rag::providers::DEFAULT_OLLAMA_BASE_URL);
            match parse_host_port(base_url) {
                Some(addr) => {
                    let resolve_result: Result<std::net::SocketAddr, _> =
                        std::net::ToSocketAddrs::to_socket_addrs(&addr.as_str())
                            .map(|mut addrs| addrs.next())
                            .and_then(|opt| {
                                opt.ok_or_else(|| {
                                    std::io::Error::new(
                                        std::io::ErrorKind::AddrNotAvailable,
                                        format!("no addresses resolved for {addr}"),
                                    )
                                })
                            });
                    let socket_addr = match resolve_result {
                        Ok(sa) => sa,
                        Err(e) => {
                            return CheckResult {
                                name: "embedding".into(),
                                status: CheckStatus::Error,
                                message: format!("cannot resolve ollama host '{addr}': {e}"),
                            };
                        }
                    };
                    match std::net::TcpStream::connect_timeout(&socket_addr, Duration::from_secs(3))
                    {
                        Ok(_) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Ok,
                            message: format!("ollama reachable at {base_url}"),
                        },
                        Err(e) => CheckResult {
                            name: "embedding".into(),
                            status: CheckStatus::Error,
                            message: format!("cannot reach ollama at {base_url}: {e}"),
                        },
                    }
                }
                None => CheckResult {
                    name: "embedding".into(),
                    status: CheckStatus::Error,
                    message: format!("cannot parse ollama URL: {base_url}"),
                },
            }
        }
        other => CheckResult {
            name: "embedding".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

fn check_reranker(config: &rag::EmbeddingProviderConfig) -> CheckResult {
    match config.reranker_provider.as_str() {
        "none" => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Ok,
            message: "disabled".into(),
        },
        "local" => {
            if rag::is_reranker_model_cached() {
                CheckResult {
                    name: "reranker".into(),
                    status: CheckStatus::Ok,
                    message: "local model cached".into(),
                }
            } else {
                CheckResult {
                    name: "reranker".into(),
                    status: CheckStatus::Warn,
                    message: "local model not downloaded, run 'cartog rag setup'".into(),
                }
            }
        }
        other => CheckResult {
            name: "reranker".into(),
            status: CheckStatus::Error,
            message: format!("unknown provider '{other}'"),
        },
    }
}

/// Doctor check for the optional `[remote]` S3-compatible sync.
///
/// Status semantics:
/// - **Ok** when `[remote]` is unset (the default — feature is inert; no
///   network traffic happens unless the user opts in). We do not warn here:
///   the absence of remote config is the expected baseline.
/// - **Ok** when `[remote].url` resolves and a HEAD against the configured
///   object succeeds (200 or 404 — both prove the bucket + creds work).
/// - **Warn** for any reachability failure (creds missing, wrong region,
///   network unreachable, 403). Push/pull would fail with the same error;
///   doctor surfaces it before the user discovers it the hard way.
/// - **Error** only when the feature was disabled at build time but a
///   `[remote]` section exists — config will be silently ignored otherwise.
fn check_remote(config: &CartogConfig, config_rejected: bool) -> CheckResult {
    // When the config file itself was rejected, the `config.remote` view is
    // always None (default). Reporting "not configured" here would be
    // misleading — the user might have had a perfectly valid [remote]
    // section before some other unrelated key got rejected. Surface this
    // explicitly so doctor doesn't lie.
    if config_rejected {
        return CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: "[remote] status unknown — config file was rejected; \
                      fix the config and re-run doctor"
                .into(),
        };
    }
    let remote = match config.remote.as_ref() {
        Some(r) => r,
        None => {
            return CheckResult {
                name: "remote".into(),
                status: CheckStatus::Ok,
                message: "not configured (local-only)".into(),
            }
        }
    };

    if remote.url.as_deref().unwrap_or("").is_empty() {
        return CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: "[remote] section present but `url` is empty".into(),
        };
    }

    #[cfg(not(feature = "remote-s3"))]
    {
        let _ = remote; // url presence already checked above
        CheckResult {
            name: "remote".into(),
            status: CheckStatus::Error,
            message: "[remote] configured but cartog was built without `remote-s3` feature".into(),
        }
    }

    #[cfg(feature = "remote-s3")]
    match remote::check_remote_reachable(remote) {
        Ok(()) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Ok,
            message: format!("{} reachable", remote.url.as_deref().unwrap_or("<unset>")),
        },
        Err(e) => CheckResult {
            name: "remote".into(),
            status: CheckStatus::Warn,
            message: format!("unreachable: {e}"),
        },
    }
}

fn build_report(checks: Vec<CheckResult>) -> DoctorReport {
    let ok = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Ok)
        .count();
    let warn = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let error = checks
        .iter()
        .filter(|c| c.status == CheckStatus::Error)
        .count();

    DoctorReport {
        summary: DoctorSummary {
            total: checks.len(),
            ok,
            warn,
            error,
        },
        checks,
    }
}

fn format_report_human(report: &DoctorReport) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    for check in &report.checks {
        writeln!(
            out,
            "  [{}] {}: {}",
            check.status.icon(),
            check.name,
            check.message,
        )
        .unwrap();
    }
    writeln!(out).unwrap();

    let s = &report.summary;
    if s.error > 0 {
        writeln!(
            out,
            "{} checks passed, {} warnings, {} errors",
            s.ok, s.warn, s.error
        )
        .unwrap();
    } else if s.warn > 0 {
        writeln!(out, "{} checks passed, {} warnings", s.ok, s.warn).unwrap();
    } else {
        writeln!(out, "All {} checks passed", s.ok).unwrap();
    }

    out
}

/// Check that requirements are met and everything is working.
pub fn cmd_doctor(
    config: &CartogConfig,
    config_path: Option<&Path>,
    config_rejected: bool,
    db_path: &Path,
    json: bool,
    embedding_dim: usize,
    provider_config: &rag::EmbeddingProviderConfig,
) -> Result<()> {
    let checks = vec![
        check_git_repo(),
        check_config(config_path, config_rejected),
        check_database(db_path, embedding_dim),
        check_embedding_provider(provider_config),
        check_reranker(provider_config),
        check_remote(config, config_rejected),
    ];

    let report = build_report(checks);

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", format_report_human(&report));
    }

    if report.summary.error > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Watch for file changes and auto-re-index.
#[allow(clippy::too_many_arguments)]
pub fn cmd_watch(
    db_path: &Path,
    path: &str,
    debounce: u64,
    rag: bool,
    rag_delay: u64,
    provider_config: rag::EmbeddingProviderConfig,
    json: bool,
) -> Result<()> {
    let mut config = WatchConfig::new(PathBuf::from(path));
    config.debounce = Duration::from_secs(debounce);
    config.rag = rag;
    config.rag_delay = Duration::from_secs(rag_delay);
    config.rag_config = provider_config;
    config.json_events = json;
    // pid_lock_dir/slot must be both-or-neither: a sandboxed host with no
    // resolvable state dir falls back to untracked mode rather than hard-
    // failing on the inverse half-config check in validate_pid_lock_config.
    config.pid_lock_dir = crate::state::default_state_dir();
    config.pid_lock_slot = config
        .pid_lock_dir
        .as_ref()
        .map(|_| crate::state::slot_for_db("watch", db_path));

    let db_path_str = db_path.to_string_lossy();
    watch::run_watch(config, &db_path_str)
}

mod self_cmd;
pub use self_cmd::{cmd_self_migrate_db, cmd_self_rollback, cmd_self_update, cmd_self_version};

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn capitalized_index_phase_labels() {
        use indexer::ProgressUpdate as U;
        let cap = |u: &U| capitalize_phase(u.label());
        assert_eq!(cap(&U::Walking), "Scanning files");
        assert_eq!(cap(&U::Parsing { total: 12 }), "Parsing 12 files");
        assert_eq!(cap(&U::Storing { total: 5 }), "Storing 5 files");
        assert_eq!(cap(&U::ResolvingLsp), "Resolving edges with LSP");
    }

    #[test]
    fn capitalized_rag_phase_labels() {
        use rag::indexer::ProgressUpdate as U;
        let cap = |u: &U| capitalize_phase(u.label());
        assert_eq!(cap(&U::Preparing), "Preparing");
        assert_eq!(
            cap(&U::Embedding {
                processed: 64,
                total: 256
            }),
            "Embedding 64/256"
        );
        assert_eq!(cap(&U::Storing), "Storing embeddings");
    }

    #[test]
    fn capitalize_phase_handles_empty() {
        assert_eq!(capitalize_phase(String::new()), "");
    }

    #[test]
    fn empty_index_hint_present_on_fresh_db() {
        // Non-empty case is covered by cartog-db's is_empty_reflects_symbol_presence.
        let db = Database::open_memory().unwrap();
        assert!(empty_index_hint(&db).contains("cartog index"));
    }

    fn db_with_symbol(name: &str) -> Database {
        use cartog_core::{FileInfo, Symbol};
        let db = Database::open_memory().unwrap();
        db.upsert_file(&FileInfo {
            path: "a.rs".into(),
            last_modified: 0.0,
            hash: "h".into(),
            language: "rust".into(),
            num_symbols: 1,
        })
        .unwrap();
        let sym = Symbol::new(name, SymbolKind::Class, "a.rs", 1, 2, 0, 10, None);
        db.insert_symbols(&[sym]).unwrap();
        db
    }

    #[test]
    fn did_you_mean_suggests_near_matches() {
        let db = db_with_symbol("ReviewResult");
        let hint = did_you_mean(&db, "Revie");
        assert!(hint.contains("did you mean"), "got: {hint}");
        assert!(hint.contains("ReviewResult"), "got: {hint}");
    }

    #[test]
    fn did_you_mean_silent_on_exact_match() {
        // An exact match means the symbol exists but has no edges — no suggestion.
        let db = db_with_symbol("ReviewResult");
        assert_eq!(did_you_mean(&db, "ReviewResult"), "");
    }

    #[test]
    fn did_you_mean_silent_on_empty_index() {
        let db = Database::open_memory().unwrap();
        assert_eq!(did_you_mean(&db, "Whatever"), "");
    }

    #[test]
    fn did_you_mean_silent_when_no_candidates() {
        let db = db_with_symbol("ReviewResult");
        assert_eq!(did_you_mean(&db, "ZZZnomatch"), "");
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn test_truncate_to_budget_within_limit() {
        let text = "short text";
        let result = truncate_to_budget(text, 100);
        assert_eq!(result, text);
    }

    #[test]
    fn test_truncate_to_budget_exceeds_limit() {
        let text = "a".repeat(200);
        let result = truncate_to_budget(&text, 10);
        assert!(result.len() <= 40 + 50); // budget bytes + notice
        assert!(result.ends_with("... (truncated to fit token budget)"));
    }

    #[test]
    fn test_truncate_to_budget_exact_boundary() {
        let text = "abcd"; // 4 bytes = 1 token
        let result = truncate_to_budget(text, 1);
        assert_eq!(result, "abcd");
    }

    // ── Config display tests ──

    fn default_config_display() -> ConfigDisplay {
        use crate::config::{
            DEFAULT_EMBEDDING_PROVIDER, DEFAULT_OLLAMA_BASE_URL, DEFAULT_OLLAMA_MODEL,
            DEFAULT_RERANKER_PROVIDER,
        };
        ConfigDisplay {
            config_file: None,
            db_path: "/tmp/test.db".into(),
            embedding: EmbeddingDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_EMBEDDING_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_EMBEDDING_PROVIDER.into(),
                },
                model: None,
                dimension: None,
                local: LocalEmbeddingDisplay {
                    query_prefix: None,
                    document_prefix: None,
                },
                ollama: OllamaDisplay {
                    base_url: ValueDisplay {
                        value: DEFAULT_OLLAMA_BASE_URL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_BASE_URL.into(),
                    },
                    model: ValueDisplay {
                        value: DEFAULT_OLLAMA_MODEL.into(),
                        is_default: true,
                        default: DEFAULT_OLLAMA_MODEL.into(),
                    },
                },
            },
            reranker: RerankerDisplay {
                provider: ValueDisplay {
                    value: DEFAULT_RERANKER_PROVIDER.into(),
                    is_default: true,
                    default: DEFAULT_RERANKER_PROVIDER.into(),
                },
            },
            rag: {
                let t = cartog_rag::search::SearchTuning::default();
                let v = |n: u32| ValueDisplay {
                    value: n.to_string(),
                    is_default: true,
                    default: n.to_string(),
                };
                RagDisplay {
                    retrieval_multiplier: v(t.retrieval_multiplier),
                    retrieval_floor: v(t.retrieval_floor),
                    rerank_max: v(t.rerank_max),
                    rerank_min: v(t.rerank_min),
                }
            },
        }
    }

    #[test]
    fn test_format_config_human_all_defaults() {
        let d = default_config_display();
        let out = format_config_human(&d);
        assert!(out.contains("Config file: none"));
        assert!(out.contains("Database:    /tmp/test.db"));
        assert!(out.contains("local (default)"));
        assert!(out.contains("model:             -"));
        assert!(out.contains("dimension:         -"));
        assert!(out.contains("query_prefix:      -"));
        assert!(out.contains("document_prefix:   -"));
    }

    #[test]
    fn test_format_config_human_custom_values() {
        let mut d = default_config_display();
        d.config_file = Some("/project/.cartog.toml".into());
        d.embedding.provider = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        d.embedding.model = Some("nomic-embed-text".into());
        d.embedding.dimension = Some(768);

        let out = format_config_human(&d);
        assert!(out.contains("Config file: /project/.cartog.toml"));
        assert!(out.contains("ollama (default: local)"));
        assert!(out.contains("model:             nomic-embed-text"));
        assert!(out.contains("dimension:         768"));
    }

    #[test]
    fn test_format_value_default() {
        let v = ValueDisplay {
            value: "local".into(),
            is_default: true,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "local (default)");
    }

    #[test]
    fn test_format_value_overridden() {
        let v = ValueDisplay {
            value: "ollama".into(),
            is_default: false,
            default: "local".into(),
        };
        assert_eq!(format_value(&v), "ollama (default: local)");
    }

    #[test]
    fn test_format_optional_some() {
        let v = Some("value".to_string());
        assert_eq!(format_optional(&v), "value");
    }

    #[test]
    fn test_format_optional_none() {
        let v: Option<String> = None;
        assert_eq!(format_optional(&v), "-");
    }

    #[test]
    fn test_config_display_json_serialization() {
        let d = default_config_display();
        let json = serde_json::to_value(&d).unwrap();
        assert_eq!(json["db_path"], "/tmp/test.db");
        assert_eq!(json["embedding"]["provider"]["value"], "local");
        assert_eq!(json["embedding"]["provider"]["is_default"], true);
        assert!(json["config_file"].is_null());
        assert!(json["embedding"]["model"].is_null());
    }

    #[test]
    fn test_truncate_to_budget_unicode() {
        // Each emoji is 4 bytes
        let text = "Hello 🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍";
        let result = truncate_to_budget(text, 5);
        assert!(result.ends_with("... (truncated to fit token budget)"));
        // Should not panic on char boundary issues
    }

    // ── Doctor check tests ──

    #[test]
    #[serial]
    fn test_check_git_repo_inside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let subdir = dir.path().join("sub");
        std::fs::create_dir(&subdir).unwrap();

        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&subdir).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Ok);
        assert_eq!(result.name, "git");
    }

    #[test]
    #[serial]
    fn test_check_git_repo_outside_git() {
        let dir = tempfile::TempDir::new().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        let result = check_git_repo();
        std::env::set_current_dir(original).unwrap();

        assert_eq!(result.status, CheckStatus::Error);
    }

    #[test]
    fn test_check_config_present() {
        let result = check_config(Some(Path::new("/project/.cartog.toml")), false);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains(".cartog.toml"));
    }

    #[test]
    fn test_check_config_absent() {
        let result = check_config(None, false);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("defaults"));
    }

    #[test]
    fn test_check_config_rejected_reports_error() {
        let result = check_config(Some(Path::new("/project/.cartog.toml")), true);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("REJECTED"));
    }

    #[test]
    fn test_check_database_missing() {
        let result = check_database(Path::new("/nonexistent/path.db"), 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("not found"));
    }

    #[test]
    fn test_check_database_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let _db = Database::open(&db_path, 384).unwrap();
        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.message.contains("empty"));
    }

    #[test]
    fn test_check_reranker_disabled() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "none".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("disabled"));
    }

    #[test]
    fn test_check_reranker_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            reranker_provider: "foobar".into(),
            ..Default::default()
        };
        let result = check_reranker(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("foobar"));
    }

    #[test]
    fn test_check_embedding_unknown_provider() {
        let config = rag::EmbeddingProviderConfig {
            provider: "unknown".into(),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("unknown"));
    }

    #[test]
    fn test_check_embedding_ollama_unreachable() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("http://127.0.0.1:19999".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot reach"));
    }

    #[test]
    fn test_check_status_icons() {
        assert_eq!(CheckStatus::Ok.icon(), "+");
        assert_eq!(CheckStatus::Warn.icon(), "!");
        assert_eq!(CheckStatus::Error.icon(), "x");
    }

    #[test]
    fn test_parse_host_port_standard() {
        assert_eq!(
            parse_host_port("http://localhost:11434"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_port() {
        assert_eq!(
            parse_host_port("http://example.com"),
            Some("example.com:80".into())
        );
    }

    #[test]
    fn test_parse_host_port_https() {
        assert_eq!(
            parse_host_port("https://example.com:443"),
            Some("example.com:443".into())
        );
    }

    #[test]
    fn test_parse_host_port_trailing_slash() {
        assert_eq!(
            parse_host_port("http://localhost:11434/"),
            Some("localhost:11434".into())
        );
    }

    #[test]
    fn test_parse_host_port_no_scheme() {
        assert_eq!(parse_host_port("localhost:11434"), None);
    }

    #[test]
    fn test_doctor_report_json_serialization() {
        let report = DoctorReport {
            checks: vec![
                CheckResult {
                    name: "git".into(),
                    status: CheckStatus::Ok,
                    message: "git repository".into(),
                },
                CheckResult {
                    name: "config".into(),
                    status: CheckStatus::Warn,
                    message: "no config".into(),
                },
            ],
            summary: DoctorSummary {
                total: 2,
                ok: 1,
                warn: 1,
                error: 0,
            },
        };
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["checks"][0]["status"], "ok");
        assert_eq!(json["checks"][1]["status"], "warn");
        assert_eq!(json["summary"]["total"], 2);
        assert_eq!(json["summary"]["ok"], 1);
    }

    // ── build_report tests ──

    #[test]
    fn test_build_report_all_ok() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 2);
        assert_eq!(report.summary.ok, 2);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    #[test]
    fn test_build_report_mixed() {
        let checks = vec![
            CheckResult {
                name: "a".into(),
                status: CheckStatus::Ok,
                message: "fine".into(),
            },
            CheckResult {
                name: "b".into(),
                status: CheckStatus::Warn,
                message: "meh".into(),
            },
            CheckResult {
                name: "c".into(),
                status: CheckStatus::Error,
                message: "bad".into(),
            },
        ];
        let report = build_report(checks);
        assert_eq!(report.summary.total, 3);
        assert_eq!(report.summary.ok, 1);
        assert_eq!(report.summary.warn, 1);
        assert_eq!(report.summary.error, 1);
    }

    #[test]
    fn test_build_report_empty() {
        let report = build_report(vec![]);
        assert_eq!(report.summary.total, 0);
        assert_eq!(report.summary.ok, 0);
        assert_eq!(report.summary.warn, 0);
        assert_eq!(report.summary.error, 0);
    }

    // ── format_report_human tests ──

    #[test]
    fn test_format_report_human_all_ok() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "git repository".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Ok,
                message: "42 files".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[+] git: git repository"));
        assert!(out.contains("[+] db: 42 files"));
        assert!(out.contains("All 2 checks passed"));
    }

    #[test]
    fn test_format_report_human_with_warnings() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "missing".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[!] config: missing"));
        assert!(out.contains("1 checks passed, 1 warnings"));
        assert!(!out.contains("errors"));
    }

    #[test]
    fn test_format_report_human_with_errors() {
        let report = build_report(vec![
            CheckResult {
                name: "git".into(),
                status: CheckStatus::Ok,
                message: "ok".into(),
            },
            CheckResult {
                name: "embed".into(),
                status: CheckStatus::Warn,
                message: "not cached".into(),
            },
            CheckResult {
                name: "db".into(),
                status: CheckStatus::Error,
                message: "broken".into(),
            },
        ]);
        let out = format_report_human(&report);
        assert!(out.contains("[x] db: broken"));
        assert!(out.contains("1 checks passed, 1 warnings, 1 errors"));
    }

    // ── check_database with indexed data ──

    #[test]
    fn test_check_database_with_data() {
        let dir = tempfile::TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path, 384).unwrap();
        // Insert a minimal file so stats.num_files > 0
        db.upsert_file(&cartog_core::FileInfo {
            path: "test.py".into(),
            last_modified: 0.0,
            hash: "abc123".into(),
            language: "python".into(),
            num_symbols: 0,
        })
        .unwrap();
        drop(db);

        let result = check_database(&db_path, 384);
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.message.contains("1 files"));
    }

    // ── check_embedding_provider local variants ──

    #[test]
    fn test_check_embedding_local_cached() {
        // This test reflects actual machine state — the local model is cached on CI/dev
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_embedding_provider(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local"
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "embedding");
    }

    #[test]
    fn test_check_reranker_local() {
        let config = rag::EmbeddingProviderConfig::default();
        let result = check_reranker(&config);
        // Either Ok (cached) or Warn (not cached) — never Error for "local"
        assert_ne!(result.status, CheckStatus::Error);
        assert_eq!(result.name, "reranker");
    }

    // ── check_embedding_provider ollama with bad URL ──

    #[test]
    fn test_check_embedding_ollama_bad_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: Some("not-a-url".into()),
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        assert_eq!(result.status, CheckStatus::Error);
        assert!(result.message.contains("cannot parse"));
    }

    // ── check_embedding_provider ollama with default URL (unreachable in test) ──

    #[test]
    fn test_check_embedding_ollama_default_url() {
        let config = rag::EmbeddingProviderConfig {
            provider: "ollama".into(),
            base_url: None,
            ..Default::default()
        };
        let result = check_embedding_provider(&config);
        // On machines without ollama running, this will be Error
        // On machines with ollama running, this will be Ok
        assert_eq!(result.name, "embedding");
        assert!(
            result.status == CheckStatus::Ok || result.status == CheckStatus::Error,
            "ollama check should be Ok or Error, not Warn"
        );
    }
}
