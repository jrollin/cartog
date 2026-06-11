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
pub mod mermaid;
pub mod remote;

/// True while a TTY spinner is painting the bottom line of stderr. The tracing
/// writer ([`SpinnerSafeWriter`]) reads this to clear the spinner line before
/// emitting a log, so a `\r`-rewritten spinner and a `\n`-terminated log no
/// longer garble each other — the spinner simply repaints on its next tick.
static SPINNER_ACTIVE: AtomicBool = AtomicBool::new(false);

/// `MakeWriter` for tracing that coexists with the spinner. When a spinner is
/// active it prefixes each record with `\r\x1b[K` (carriage return + clear to
/// end of line) so the log overwrites the spinner line cleanly; the spinner's
/// 100ms repaint then redraws below the log. Always writes to stderr.
pub struct SpinnerSafeWriter;

impl std::io::Write for SpinnerSafeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut err = std::io::stderr().lock();
        if SPINNER_ACTIVE.load(Ordering::Relaxed) {
            err.write_all(b"\r\x1b[K")?;
        }
        err.write_all(buf)?;
        // Report the caller's full buffer as written; the prefix is our own.
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().lock().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SpinnerSafeWriter {
    type Writer = SpinnerSafeWriter;
    fn make_writer(&'a self) -> Self::Writer {
        SpinnerSafeWriter
    }
}

/// Stderr progress reporter for long-running CLI commands.
///
/// On a TTY it renders an animated spinner whose label tracks the current
/// phase. On a non-TTY (the Claude Code SessionStart hook, CI, piped output)
/// it prints a plain line on each phase change plus a periodic heartbeat, so a
/// multi-minute first index is never silent. Use [`Spinner::set_phase`] from a
/// progress callback to update the label/heartbeat.
///
/// While a TTY spinner lives it sets [`SPINNER_ACTIVE`] so [`SpinnerSafeWriter`]
/// keeps concurrent tracing logs from colliding with the spinner line.
struct Spinner {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Set only on the TTY path, so `Drop` clears `SPINNER_ACTIVE` exactly once.
    tty: bool,
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
        // Only the TTY path paints a `\r`-rewritten line that logs can collide
        // with; the plain heartbeat is newline-terminated and needs no guard.
        if is_tty {
            SPINNER_ACTIVE.store(true, Ordering::Relaxed);
        }
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
            tty: is_tty,
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
        // Cleared after the painter joins (it has emitted its final line clear),
        // so later logs write plainly. `stop()` consumes self and also lands here.
        if self.tty {
            SPINNER_ACTIVE.store(false, Ordering::Relaxed);
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
    Database::open(path, embedding_dim).map_err(|e| open_db_error(path, e.into()))
}

/// Map a database-open failure to an actionable message naming the path and the
/// fix. Corruption ("not a database") and read-only mounts produce the most
/// confusing raw SQLite errors, so they get specific remediation; anything else
/// keeps a generic wrapper with the path. The original error is the cause.
fn open_db_error(path: &Path, err: anyhow::Error) -> anyhow::Error {
    let raw = err.to_string().to_ascii_lowercase();
    let p = path.display();
    let hint = if raw.contains("not a database") {
        format!(
            "database at {p} is corrupt or not a cartog database — \
             delete it and run `cartog index .` to rebuild"
        )
    } else if raw.contains("readonly") || raw.contains("read-only") {
        format!(
            "database at {p} is not writable — check the file and directory \
             permissions, or set [database].path to a writable location"
        )
    } else {
        format!("failed to open cartog database at {p}")
    };
    err.context(hint)
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
#[allow(clippy::too_many_arguments)] // thin CLI adapter over index_directory
pub fn cmd_index(
    db_path: &Path,
    path: &str,
    force: bool,
    lsp: bool,
    json: bool,
    embedding_dim: usize,
    redact: indexer::RedactionConfig,
    lsp_overrides: &std::collections::HashMap<String, Vec<String>>,
) -> Result<()> {
    let root = Path::new(path);
    let db = open_db(db_path, embedding_dim)?;

    // Stderr-only; `Spinner::start` self-gates (TTY or CARTOG_PROGRESS), so --json stdout stays clean.
    let spinner = Spinner::start("Indexing").map(Arc::new);
    let cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let cb_ref: Option<indexer::ProgressCallback<'_>> =
        cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let result =
        indexer::index_directory(&db, root, force, lsp, cb_ref, None, redact, lsp_overrides);
    drop(cb);
    stop_spinner(spinner);
    let result = result?;

    if !json && result.redaction_backfilled {
        eprintln!(
            "note: secret redaction was newly enabled; re-indexed all files to scrub stored content"
        );
    }

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

    output(&result, json, None, indexer::render_index_summary)
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
    // Don't count empty results — an empty-index call or a typo'd file path
    // didn't actually save the user any tokens vs grep + read.
    if !symbols.is_empty() {
        db.log_query("outline", "cli");
    }
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
    if !edges.is_empty() {
        db.log_query("callees", "cli");
    }
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
    if !results.is_empty() {
        db.log_query("impact", "cli");
    }
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

/// Find a call path between two symbols, with each hop's body inline.
pub fn cmd_trace(
    db_path: &Path,
    from: &str,
    to: &str,
    depth: u32,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    const MAX_TRACE_DEPTH: u32 = 20;
    let db = open_db(db_path, embedding_dim)?;
    // `file_path` is stored relative to the index root. The DB lives at
    // `<root>/.cartog/db.sqlite`, so the root is the db's grandparent — robust
    // regardless of the cwd `cartog trace` was launched from.
    let index_root = index_root_from_db_path(db_path);
    let path = db.trace(from, to, depth.min(MAX_TRACE_DEPTH))?;
    if path.is_some() {
        db.log_query("trace", "cli");
    }

    #[derive(Serialize)]
    struct HydratedHop {
        source_name: String,
        target_name: String,
        kind: cartog_core::EdgeKind,
        file_path: String,
        line: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    }

    // `{found, hops}` so `--json` distinguishes "no path" (found=false) from
    // "from == to" (found=true, hops=[]) — matching the cartog_trace MCP shape.
    #[derive(Serialize)]
    struct TraceResult {
        from: String,
        to: String,
        found: bool,
        hops: Vec<HydratedHop>,
    }

    let hops: Vec<HydratedHop> = path
        .iter()
        .flatten()
        .map(|h| HydratedHop {
            body: hop_body(&db, &index_root, &h.source_id),
            source_name: h.source_name.clone(),
            target_name: h.target_name.clone(),
            kind: h.kind,
            file_path: h.file_path.clone(),
            line: h.line,
        })
        .collect();

    let result = TraceResult {
        from: from.to_string(),
        to: to.to_string(),
        found: path.is_some(),
        hops,
    };
    output(&result, json, token_budget, |r| {
        if !r.found {
            return format!(
                "No call path from '{from}' to '{to}'{}{}{}\n",
                empty_index_hint(&db),
                did_you_mean(&db, &r.from),
                did_you_mean(&db, &r.to),
            );
        }
        if r.hops.is_empty() {
            return format!("'{}' is the target.\n", r.from);
        }
        let mut out = String::new();
        for hop in &r.hops {
            out.push_str(&format!(
                "{src} → {dst}  {file}:{line}\n",
                src = hop.source_name,
                dst = hop.target_name,
                file = hop.file_path,
                line = hop.line,
            ));
            if let Some(body) = &hop.body {
                out.push_str(body);
                out.push('\n');
            }
        }
        out
    })
}

/// Index root for a DB at `<root>/.cartog/db.sqlite` — the db's grandparent.
/// Falls back to the db's own parent (legacy `.cartog.db` layout), then `.`.
fn index_root_from_db_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .and_then(Path::parent)
        .or_else(|| db_path.parent())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Body of the symbol with id `source_id` — the exact symbol on the call path.
/// Prefers stored RAG content (redaction-aware), else reads source by byte
/// range from `root`-relative `file_path`. `None` when neither is available
/// (header-only hop).
fn hop_body(db: &Database, root: &Path, source_id: &str) -> Option<String> {
    if let Some((content, _)) = db.get_symbol_content(source_id).ok().flatten() {
        return Some(content);
    }
    let sym = db
        .get_symbols_by_ids(std::slice::from_ref(&source_id.to_string()))
        .ok()?
        .into_iter()
        .next()?;
    source_slice(
        root,
        &sym.file_path,
        sym.start_byte as usize,
        sym.end_byte as usize,
    )
}

/// Read `path` (resolved against `root`; absolute paths pass through) and return
/// the `[start, end)` byte slice, snapped to char boundaries. `None` on any read
/// or range error.
fn source_slice(root: &Path, path: &str, mut start: usize, mut end: usize) -> Option<String> {
    let src = std::fs::read_to_string(root.join(path)).ok()?;
    if start >= end || end > src.len() {
        return None;
    }
    while start < end && !src.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !src.is_char_boundary(end) {
        end -= 1;
    }
    Some(src[start..end].to_string())
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
    if !results.is_empty() {
        db.log_query("refs", "cli");
    }
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
    mermaid: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let pairs = db.hierarchy(name)?;
    if !pairs.is_empty() {
        db.log_query("hierarchy", "cli");
    }
    let name = name.to_string();

    // --json wins if both flags are set (matches the documented behavior).
    if mermaid && !json {
        // Surface the same diagnostic the plain branch shows so users running
        // --mermaid on a typo or empty index get the did-you-mean hint
        // alongside the bare `graph TD` document.
        if pairs.is_empty() {
            eprintln!(
                "No hierarchy found for '{name}'{}{}",
                empty_index_hint(&db),
                did_you_mean(&db, &name)
            );
        }
        print!("{}", mermaid::render_hierarchy(&pairs));
        return Ok(());
    }

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
    mermaid: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let edges = db.file_deps(file)?;
    if !edges.is_empty() {
        db.log_query("deps", "cli");
    }
    let file = file.to_string();

    if mermaid && !json {
        // Surface the same diagnostic the plain branch shows so users running
        // --mermaid against an unindexed file or empty index get the hint.
        if edges.is_empty() {
            eprintln!(
                "No dependencies found for '{file}'{}",
                empty_index_hint(&db)
            );
        }
        let targets: Vec<(String, u32)> = edges
            .iter()
            .map(|e| (e.target_name.clone(), e.line))
            .collect();
        print!("{}", mermaid::render_deps(&file, &targets));
        return Ok(());
    }

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
    if !symbols.is_empty() {
        db.log_query("search", "cli");
    }
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

mod savings;
use savings::{render_savings, savings_scope_label};

/// Index statistics summary.
pub fn cmd_stats(
    db_path: &Path,
    json: bool,
    token_budget: Option<u32>,
    embedding_dim: usize,
    savings: bool,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;

    if savings {
        let report = db.savings_breakdown()?;
        let scope = savings_scope_label(db_path);
        return output(&report, json, token_budget, |r| render_savings(&scope, r));
    }

    let stats = db.stats()?;
    output(&stats, json, token_budget, |stats| {
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
        if stats.num_files == 0 {
            out.push_str("\nIndex is empty — run `cartog index .` to build the code graph.\n");
        }
        out
    })
}

/// Token-budget-aware codebase summary: file tree + top symbols ranked by centrality.
pub fn cmd_map(
    db_path: &Path,
    tokens: u32,
    json: bool,
    mermaid: bool,
    embedding_dim: usize,
) -> Result<()> {
    let db = open_db(db_path, embedding_dim)?;
    let files = db.all_files()?;

    if files.is_empty() {
        if json {
            println!("{{}}");
        } else if mermaid {
            // Tell the user to index before pasting the (empty) diagram.
            eprintln!("No files indexed. Run `cartog index .` first.");
            println!("graph TD\n    repo[\"Repo (empty)\"]");
        } else {
            println!("No files indexed. Run 'cartog index .' first.");
        }
        return Ok(());
    }

    // Log AFTER the empty-files guard so no-op calls on an unindexed repo
    // don't inflate `cartog savings`.
    db.log_query("map", "cli");

    if mermaid && !json {
        // Honor the token budget by walking files until we exhaust it. The
        // emitted lines look like:
        //   repo --> f_<sane>_<hash8>["<label>"]
        //   f_<sane>_<hash8> --> s_<sane>_<hash8>["<name> (<kind>)"]
        // so per-file overhead is roughly `len(path) * 2 + len(prefix+hash) * 2 + 30`
        // and per-leaf overhead is roughly `len(path) + len(name) * 2 + len(kind) + 50`.
        // The constants are deliberately conservative — better to underfill
        // than overshoot the documented `--tokens` budget.
        let budget_bytes = (tokens as usize) * 4;
        let mut included_files: Vec<&str> = Vec::new();
        let mut size = "graph TD\n    repo[\"Repo\"]\n".len();
        // f_<sanitized>_<hash8> has at least len(path)+13 bytes of ID overhead.
        const FILE_ID_OVERHEAD: usize = 13;
        // s_<sanitized>_<hash8> for a "<file>::<name>" raw key — even longer.
        const SYM_ID_OVERHEAD: usize = 13;
        for f in &files {
            let edge_cost = f.len() * 2 + FILE_ID_OVERHEAD + 30;
            if size + edge_cost > budget_bytes && !included_files.is_empty() {
                break;
            }
            size += edge_cost;
            included_files.push(f.as_str());
        }
        // HashSet so per-symbol membership is O(1), not O(N).
        let included_set: std::collections::HashSet<&str> =
            included_files.iter().copied().collect();
        // Add top symbols per file until budget runs out.
        let symbols = db.top_symbols(500)?;
        let mut symbols_by_file: std::collections::BTreeMap<String, Vec<(String, String)>> =
            std::collections::BTreeMap::new();
        for sym in &symbols {
            if !included_set.contains(sym.file_path.as_str()) {
                continue;
            }
            // The actual emitted leaf carries the file path inside the
            // sym ID (`s_<sanitize(file::name)>_<hash>`), plus the file ID
            // again on the source side of `-->`. Account for both.
            let leaf_cost = sym.name.len() * 2 + sym.file_path.len() + SYM_ID_OVERHEAD * 2 + 50;
            if size + leaf_cost > budget_bytes {
                break;
            }
            size += leaf_cost;
            symbols_by_file
                .entry(sym.file_path.clone())
                .or_default()
                .push((sym.name.clone(), sym.kind.as_str().to_string()));
        }
        let included_owned: Vec<String> = included_files.iter().map(|s| (*s).to_string()).collect();
        let symbols_vec: Vec<(String, Vec<(String, String)>)> =
            symbols_by_file.into_iter().collect();
        print!("{}", mermaid::render_map(&included_owned, &symbols_vec));
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

    // Log AFTER the git call succeeds; otherwise non-git directories inflate
    // the savings counter via the `?` propagating an error.
    let changed_files = indexer::git_recently_changed_files(&root, commits)?;
    db.log_query("changes", "cli");

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
pub fn cmd_rag_setup(json: bool, provider_config: &rag::EmbeddingProviderConfig) -> Result<()> {
    let reranker_model = provider_config.reranker_model.as_deref();
    let spinner = if json {
        None
    } else {
        // One-time notice so the multi-hundred-MB download isn't a silent wait.
        eprintln!(
            "Downloading embedding (~80MB) + re-ranker ({}) models, one-time…",
            reranker_model.unwrap_or(rag::DEFAULT_RERANKER_MODEL)
        );
        Spinner::start("Downloading models")
    };
    // Download bi-encoder (embeddings)
    let embed_result = rag::setup::download_model();
    // Download cross-encoder (re-ranking) — the configured model, not always the default.
    let rerank_result =
        rag::setup::download_cross_encoder(reranker_model, provider_config.intra_threads);
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
    redact: indexer::RedactionConfig,
) -> Result<()> {
    let root = Path::new(path);
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;
    db.reconcile_embedding_fingerprint(&rag::fingerprint_of(provider.as_ref()))
        .context("failed to reconcile embedding fingerprint")?;

    // Progress on stderr; `Spinner::start` self-gates (TTY or CARTOG_PROGRESS).
    let spinner = Spinner::start("Indexing code graph").map(Arc::new);
    let ix_cb = spinner_callback(&spinner, indexer::ProgressUpdate::label);
    let ix_cb_ref: Option<indexer::ProgressCallback<'_>> =
        ix_cb.as_ref().map(|f| f as &(dyn Fn(_) + Send + Sync));
    let index_res = indexer::index_directory(
        &db,
        root,
        false,
        false,
        ix_cb_ref,
        None,
        redact,
        &std::collections::HashMap::new(),
    );
    drop(ix_cb);
    stop_spinner(spinner);
    let _index_result = index_res?;

    let spinner = Spinner::start("Embedding symbols").map(Arc::new);
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
        let model = provider_config.reranker_model.clone();
        let threads = provider_config.intra_threads;
        Some(move || rag::create_reranker_provider(&name, model.as_deref(), threads))
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
    db.log_query("rag_search", "cli");
    let query = query.to_string();
    // Gate the "build the index" hint on whether embeddings actually exist, not
    // on the result counts: kind-filtered retrieval can legitimately yield
    // fts/vec_count == 0 for a code query that only matched docs, even with a
    // fully-built index.
    let embeddings_built = db.embedding_count().map(|n| n > 0).unwrap_or(false);

    output(&search_result, json, token_budget, |sr| {
        render_rag_search(sr, &query, embeddings_built)
    })
}

/// Render the human-readable `rag search` output.
///
/// `embeddings_built` gates the index hints: `vec_count == 0` alone is not a
/// reliable signal — kind-filtered retrieval can legitimately yield zero
/// vector hits on a fully-built index.
fn render_rag_search(
    sr: &rag::search::HybridSearchResult,
    query: &str,
    embeddings_built: bool,
) -> String {
    if sr.results.is_empty() {
        let mut out = format!("No results found for '{query}'\n");
        if !embeddings_built {
            out.push_str("Hint: run 'cartog rag index' to build the semantic search index.\n");
        }
        return out;
    }
    let mut out = format!(
        "Found {} results (FTS: {}, vector: {}, merged: {})\n",
        sr.results.len(),
        sr.fts_count,
        sr.vec_count,
        sr.merged_count
    );
    if !embeddings_built {
        out.push_str(
            "Hint: keyword (FTS) matches only — run 'cartog rag index' to enable semantic search.\n",
        );
    }
    out.push('\n');
    for (i, r) in sr.results.iter().enumerate() {
        let sources = r
            .sources
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("+");
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
}

/// Build a token-budgeted task-context bundle for a natural-language task.
pub fn cmd_context(
    db_path: &Path,
    task: &str,
    tokens: u32,
    json: bool,
    provider_config: &rag::EmbeddingProviderConfig,
    tuning: &rag::search::SearchTuning,
) -> Result<()> {
    let mut provider = rag::create_embedding_provider(provider_config)?;
    let db = open_db(db_path, provider.dimension())?;

    // Build the bundle in its own scope so the provider/reranker borrows end
    // before the `output` closure (which re-borrows `&db`) runs.
    let ctx = {
        let mut reranker = if provider_config.reranker_provider == "none" {
            None
        } else {
            rag::create_reranker_provider(
                &provider_config.reranker_provider,
                provider_config.reranker_model.as_deref(),
                provider_config.intra_threads,
            )
        };
        let opts = rag::context::ContextOptions {
            tuning: *tuning,
            ..Default::default()
        };
        // `match` (not `as_deref_mut`) keeps the reranker borrow scoped to the
        // call so it drops before `output` re-borrows `db`.
        match reranker.as_mut() {
            Some(r) => rag::context::build_task_context(
                &db,
                task,
                tokens,
                provider.as_mut(),
                Some(r.as_mut()),
                &opts,
            ),
            None => {
                rag::context::build_task_context(&db, task, tokens, provider.as_mut(), None, &opts)
            }
        }?
    };
    if !ctx.entries.is_empty() {
        db.log_query("context", "cli");
    }
    let task = task.to_string();

    output(&ctx, json, None, |ctx| {
        if ctx.entries.is_empty() {
            return format!("No context found for '{task}'{}\n", empty_index_hint(&db));
        }
        let mut out = format!(
            "Context for '{task}' ({} symbols, ~{} tokens)\n\n",
            ctx.entries.len(),
            ctx.approx_tokens
        );
        for entry in &ctx.entries {
            out.push_str(&format!(
                "[{reason:?}] {kind} {name}  {file}:{line}\n",
                reason = entry.reason,
                kind = entry.symbol.kind,
                name = entry.symbol.name,
                file = entry.symbol.file_path,
                line = entry.symbol.start_line,
            ));
            if let Some(body) = &entry.content {
                for l in body.lines() {
                    out.push_str(&format!("    {l}\n"));
                }
                out.push('\n');
            }
        }
        out
    })
}

mod config_display;
pub use config_display::cmd_config;

mod doctor;
pub use doctor::cmd_doctor;

/// Watch for file changes and auto-re-index.
#[allow(clippy::too_many_arguments)]
pub fn cmd_watch(
    db_path: &Path,
    path: &str,
    debounce: u64,
    rag_override: Option<bool>,
    rag_delay: u64,
    provider_config: rag::EmbeddingProviderConfig,
    redact: indexer::RedactionConfig,
    json: bool,
) -> Result<()> {
    let mut config = WatchConfig::new(PathBuf::from(path));
    config.debounce = Duration::from_secs(debounce);
    config.rag_override = rag_override;
    config.rag_delay = Duration::from_secs(rag_delay);
    config.rag_config = provider_config;
    config.redact = redact;
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
pub use self_cmd::{
    cmd_self_migrate_db, cmd_self_rollback, cmd_self_update, cmd_self_version, UpdateMode,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn fts_only_search_result() -> rag::search::HybridSearchResult {
        rag::search::HybridSearchResult {
            results: vec![rag::search::SearchResult {
                symbol: cartog_core::Symbol::new(
                    "foo",
                    cartog_core::SymbolKind::Function,
                    "a.py",
                    1,
                    10,
                    0,
                    100,
                    None,
                ),
                content: None,
                rrf_score: 0.016,
                rerank_score: None,
                sources: vec![],
            }],
            fts_count: 1,
            vec_count: 0,
            merged_count: 1,
        }
    }

    #[test]
    fn rag_search_render_hints_when_results_found_but_embeddings_missing() {
        let out = render_rag_search(&fts_only_search_result(), "foo", false);
        assert!(
            out.contains("cartog rag index"),
            "FTS-only results without embeddings must hint at 'cartog rag index'; got:\n{out}"
        );
    }

    #[test]
    fn rag_search_render_no_hint_when_embeddings_built() {
        let out = render_rag_search(&fts_only_search_result(), "foo", true);
        assert!(
            !out.contains("Hint:"),
            "vec_count == 0 with a built index is legitimate, no hint expected; got:\n{out}"
        );
    }

    #[test]
    fn index_root_is_db_grandparent() {
        // New layout: <root>/.cartog/db.sqlite → root.
        let root = index_root_from_db_path(Path::new("/proj/.cartog/db.sqlite"));
        assert_eq!(root, Path::new("/proj"));
    }

    #[test]
    fn source_slice_resolves_relative_path_against_root() {
        // file_path is stored relative to the index root; reading must join it
        // to the root, not the process cwd.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
        // Byte range covering "def f()".
        let body = source_slice(root, "a.py", 0, 7).expect("reads relative to root");
        assert_eq!(body, "def f()");
        // A path that doesn't exist under root → None (not a cwd-relative read).
        assert!(source_slice(root, "missing.py", 0, 5).is_none());
    }

    #[test]
    fn capitalized_index_phase_labels() {
        use indexer::ProgressUpdate as U;
        let cap = |u: &U| capitalize_phase(u.label());
        assert_eq!(cap(&U::Walking), "Scanning files");
        assert_eq!(cap(&U::Parsing { done: 0, total: 12 }), "Parsing 12 files");
        assert_eq!(
            cap(&U::Parsing { done: 4, total: 12 }),
            "Parsing 4/12 files"
        );
        assert_eq!(cap(&U::Storing { done: 0, total: 5 }), "Storing 5 files");
        assert_eq!(cap(&U::Storing { done: 3, total: 5 }), "Storing 3/5 files");
        assert_eq!(
            cap(&U::ResolvingLsp { done: 0, total: 9 }),
            "Resolving 9 edges with LSP"
        );
        assert_eq!(
            cap(&U::ResolvingLsp { done: 3, total: 9 }),
            "Resolving 3/9 edges with LSP"
        );
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
    fn open_db_error_corrupt_names_path_and_rebuild() {
        let e = anyhow::anyhow!("file is not a database");
        let msg = open_db_error(Path::new("/p/.cartog/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/.cartog/db.sqlite"), "names path: {msg}");
        assert!(msg.contains("corrupt"), "{msg}");
        assert!(msg.contains("cartog index"), "{msg}");
    }

    #[test]
    fn open_db_error_readonly_names_path_and_permissions() {
        let e = anyhow::anyhow!("attempt to write a readonly database");
        let msg = open_db_error(Path::new("/p/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/db.sqlite"), "{msg}");
        assert!(msg.contains("permission"), "{msg}");
    }

    #[test]
    fn open_db_error_generic_keeps_path() {
        let e = anyhow::anyhow!("disk full");
        let msg = open_db_error(Path::new("/p/db.sqlite"), e).to_string();
        assert!(msg.contains("/p/db.sqlite"), "{msg}");
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

    #[test]
    fn test_truncate_to_budget_unicode() {
        // Each emoji is 4 bytes
        let text = "Hello 🌍🌍🌍🌍🌍🌍🌍🌍🌍🌍";
        let result = truncate_to_budget(text, 5);
        assert!(result.ends_with("... (truncated to fit token budget)"));
        // Should not panic on char boundary issues
    }

    proptest::proptest! {
        /// `s[..cut]` would panic if `cut` landed mid-codepoint.
        #[test]
        fn truncate_never_panics(s in ".*", budget in 0u32..64) {
            let _ = truncate_to_budget(&s, budget);
        }

        /// Within budget → returned verbatim, no notice. Budget is derived from
        /// the string so every case exercises the in-budget branch (a fixed
        /// budget range would reject most strings via prop_assume).
        #[test]
        fn truncate_within_budget_is_verbatim(s in ".{0,200}", slack in 0u32..50) {
            let budget = (s.len() as u32).div_ceil(4) + slack;
            proptest::prop_assert_eq!(truncate_to_budget(&s, budget), s);
        }

        /// When truncation fires, the kept content stays within the byte budget.
        #[test]
        fn truncate_respects_byte_budget(s in ".{0,500}", budget in 0u32..200) {
            let max_bytes = (budget as usize) * 4;
            proptest::prop_assume!(s.len() > max_bytes);
            let notice = "\n... (truncated to fit token budget)";
            let out = truncate_to_budget(&s, budget);
            proptest::prop_assert!(out.ends_with(notice), "truncated output must carry the notice");
            let content = &out[..out.len() - notice.len()];
            proptest::prop_assert!(
                content.len() <= max_bytes,
                "kept {} content bytes > {} budget",
                content.len(),
                max_bytes
            );
        }
    }

    // ── cmd_* command bodies over a real indexed DB ───────────────────
    //
    // Drive the read commands end-to-end against a temp DB populated from a
    // small Python fixture. The commands print to stdout (so output content
    // can't be asserted directly), but calling them exercises the real query,
    // the human/JSON formatter closures, the empty-result did-you-mean paths,
    // and the token-budget branch — returning Ok/Err is the observable
    // contract. Query-log side effects are verified via savings_breakdown.

    const CMD_FIXTURE_SRC: &str = "\
class Animal:
    def speak(self):
        return helper()


class Dog(Animal):
    def speak(self):
        return helper()


def helper():
    return 42


def main():
    d = Dog()
    return d.speak()
";

    /// Index `CMD_FIXTURE_SRC` as `lib.py` and return the DB path. The TempDir
    /// is returned so the caller keeps it alive for the test's duration. The
    /// index root is a named subdir: the walker prunes dot-prefixed dirs, and
    /// a bare TempDir name starts with ".tmp".
    fn indexed_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().join("project");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("lib.py"), CMD_FIXTURE_SRC).unwrap();
        let db_path = tmp.path().join("cartog.db");
        let db = Database::open(&db_path, 384).unwrap();
        indexer::index_directory(
            &db,
            &root,
            true,
            false,
            None,
            None,
            indexer::RedactionConfig::disabled(),
            &std::collections::HashMap::new(),
        )
        .expect("fixture indexes");
        drop(db);
        (tmp, db_path)
    }

    /// Logged query count — a delta proves a command hit the query layer
    /// (commands print to stdout, so rendered content can't be asserted here).
    fn queries_logged(db_path: &std::path::Path) -> u64 {
        Database::open(db_path, 384)
            .unwrap()
            .savings_breakdown()
            .unwrap()
            .total_queries
    }

    #[test]
    fn cmd_outline_runs_a_query_for_a_populated_file() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_outline(&db, "lib.py", false, None, 384).expect("outline ok");
        assert_eq!(
            queries_logged(&db),
            before + 1,
            "outline of a populated file must run exactly one query"
        );
    }

    #[test]
    fn cmd_outline_json_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_outline(&db, "lib.py", true, None, 384).expect("outline --json ok");
    }

    #[test]
    fn cmd_outline_unknown_file_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_outline(&db, "missing.py", false, None, 384).expect("outline of unknown file is ok");
    }

    #[test]
    fn cmd_refs_runs_a_query_per_invocation_with_and_without_kind_filter() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_refs(&db, "helper", None, false, None, 384).expect("refs ok");
        cmd_refs(&db, "helper", Some(EdgeKindFilter::Calls), false, None, 384)
            .expect("refs --kind calls ok");
        assert_eq!(
            queries_logged(&db),
            before + 2,
            "each refs invocation must run a query"
        );
    }

    #[test]
    fn cmd_refs_near_miss_name_takes_the_did_you_mean_branch_without_error() {
        let (_tmp, db) = indexed_db();
        // Empty result triggers the did_you_mean / empty_index_hint branch.
        cmd_refs(&db, "helpe", None, false, None, 384).expect("refs of near-miss name is ok");
    }

    #[test]
    fn cmd_callees_logs_a_query_only_when_it_finds_results() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_callees(&db, "main", false, None, 384).expect("callees ok");
        let after_hit = queries_logged(&db);
        cmd_callees(&db, "no_such_symbol", false, None, 384).expect("empty callees is ok");
        let after_miss = queries_logged(&db);

        assert_eq!(after_hit, before + 1, "a callees hit logs one query");
        assert_eq!(
            after_miss, after_hit,
            "an empty callees result must not log a query"
        );
    }

    #[test]
    fn cmd_impact_plain_and_json_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_impact(&db, "helper", 3, false, None, 384).expect("impact ok");
        cmd_impact(&db, "helper", 3, true, None, 384).expect("impact --json ok");
    }

    #[test]
    fn cmd_trace_logs_a_query_only_when_a_path_is_found() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_trace(&db, "speak", "helper", 8, false, None, 384).expect("trace ok");
        let after_hit = queries_logged(&db);
        cmd_trace(&db, "speak", "no_such_symbol", 8, false, None, 384)
            .expect("no-path trace is ok");
        let after_miss = queries_logged(&db);

        assert_eq!(after_hit, before + 1, "a found path logs one query");
        assert_eq!(
            after_miss, after_hit,
            "a no-path result must not log a query"
        );
    }

    #[test]
    fn cmd_trace_json_branch_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_trace(&db, "speak", "helper", 8, true, None, 384).expect("trace --json ok");
    }

    // No CLI test for `cmd_context`: it builds a real embedding provider
    // (ONNX model), so it can't run model-independently in CI. The fusion
    // logic is covered by `cartog_rag::context` unit tests (MockEmbeddingProvider)
    // and the `cartog_context` MCP tool test (test_provider).

    #[test]
    fn cmd_hierarchy_plain_json_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_hierarchy(&db, "Dog", false, false, None, 384).expect("hierarchy ok");
        cmd_hierarchy(&db, "Dog", true, false, None, 384).expect("hierarchy --json ok");
        cmd_hierarchy(&db, "Dog", false, true, None, 384).expect("hierarchy --mermaid ok");
    }

    #[test]
    fn cmd_deps_plain_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_deps(&db, "lib.py", false, false, None, 384).expect("deps ok");
        cmd_deps(&db, "lib.py", false, true, None, 384).expect("deps --mermaid ok");
    }

    #[test]
    fn cmd_search_runs_a_query_for_each_filter_and_budget_branch() {
        let (_tmp, db) = indexed_db();
        let before = queries_logged(&db);
        cmd_search(&db, "Anim", None, None, 30, false, None, 384).expect("search ok");
        cmd_search(
            &db,
            "speak",
            Some(SymbolKindFilter::Method),
            Some("lib.py"),
            30,
            false,
            None,
            384,
        )
        .expect("search with kind + file filter ok");
        // Token-budget branch.
        cmd_search(&db, "e", None, None, 30, false, Some(50), 384).expect("search --tokens ok");
        assert_eq!(
            queries_logged(&db),
            before + 3,
            "each search invocation must run a query"
        );
    }

    #[test]
    fn cmd_search_empty_result_does_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_search(&db, "zzz_no_match", None, None, 30, false, None, 384)
            .expect("empty search is ok");
    }

    #[test]
    fn cmd_stats_plain_json_and_savings_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_stats(&db, false, None, 384, false).expect("stats ok");
        cmd_stats(&db, true, None, 384, false).expect("stats --json ok");
        cmd_stats(&db, false, None, 384, true).expect("stats --savings ok");
    }

    #[test]
    fn cmd_map_plain_json_and_mermaid_branches_do_not_error() {
        let (_tmp, db) = indexed_db();
        cmd_map(&db, 1000, false, false, 384).expect("map ok");
        cmd_map(&db, 1000, true, false, 384).expect("map --json ok");
        cmd_map(&db, 1000, false, true, 384).expect("map --mermaid ok");
    }
}
