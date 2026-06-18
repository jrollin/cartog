//! CLI progress plumbing: a TTY/non-TTY [`Spinner`], the tracing [`SpinnerSafeWriter`]
//! that coexists with it, the Ctrl-C [`install_cancel_probe`], and the
//! [`spinner_callback`]/[`stop_spinner`] lifecycle helpers that bridge an
//! indexer/rag progress stream to the spinner's phase label.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
pub(crate) struct Spinner {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Set only on the TTY path, so `Drop` clears `SPINNER_ACTIVE` exactly once.
    tty: bool,
}

impl Spinner {
    pub(crate) fn start(label: &'static str) -> Option<Self> {
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
    pub(crate) fn set_phase(&self, phase: impl Into<String>) {
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

    pub(crate) fn stop(mut self) {
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

/// Install a Ctrl-C handler that flips an `AtomicBool`, returning a probe that
/// reads it. One-shot: `set_handler` can only succeed once per process, so call
/// this from a single command invocation. Best-effort — a failed install (a
/// handler already exists) leaves the probe stuck `false`, i.e. non-cancellable.
pub(crate) fn install_cancel_probe() -> impl Fn() -> bool + Send + Sync + 'static {
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst));
    move || interrupted.load(Ordering::SeqCst)
}

/// Capitalize the first character of a phase label for CLI display. Phase
/// wording itself is owned by `ProgressUpdate::label()` in the indexer/rag
/// crates; the spinner only adjusts presentation.
pub(crate) fn capitalize_phase(label: String) -> String {
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
pub(crate) fn spinner_callback<U>(
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
pub(crate) fn stop_spinner(spinner: Option<Arc<Spinner>>) {
    if let Some(s) = spinner.and_then(Arc::into_inner) {
        s.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cartog_indexer as indexer;
    use cartog_rag as rag;

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
}
