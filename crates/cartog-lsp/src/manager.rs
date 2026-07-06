use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::client::LspClient;
use super::servers::{find_servers, is_binary_available};

/// Default max seconds to wait for an LSP server to finish loading its project model.
/// Override with `CARTOG_LSP_READY_TIMEOUT_SECS`.
const DEFAULT_READY_TIMEOUT_SECS: u64 = 20;

/// How many `textDocument/definition` requests `definitions_batch` keeps in
/// flight at once. Caps in-flight memory and stdin backpressure while still
/// overlapping round-trips; large files resolve in windows of this size. Each
/// window shares one batch deadline, so the drain treats it as one timeout unit.
pub(crate) const DEFINITION_BATCH_WINDOW: usize = 64;

/// Read the ready-timeout from env, falling back to the default.
fn ready_timeout_secs() -> u64 {
    std::env::var("CARTOG_LSP_READY_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_READY_TIMEOUT_SECS)
}

/// Open (or create, truncating) a per-server log file in the system temp dir.
/// Returns `Stdio::null()` if we can't open the file so LSP startup is never
/// blocked by a logging issue. `key` is the binary name for spec-driven servers
/// and the language name for command overrides, so several `docker`-based
/// override servers don't collide on a single `docker.log`.
fn open_lsp_log(key: &str) -> Stdio {
    let dir = std::env::temp_dir().join("cartog-lsp");
    if std::fs::create_dir_all(&dir).is_err() {
        return Stdio::null();
    }
    // Sanitize the key for filename safety.
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.log"));
    match OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => {
            tracing::info!(path = %path.display(), "LSP stderr logged to file");
            Stdio::from(f)
        }
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "LSP stderr log open failed; discarding");
            Stdio::null()
        }
    }
}

/// Seconds to wait for $/progress notifications before switching to probe-based detection.
const PROGRESS_DETECT_SECS: u64 = 5;

/// Manages running LSP server instances, one per language.
pub struct LspManager {
    root: PathBuf,
    clients: HashMap<String, (LspClient, &'static str)>, // (client, language_id)
    /// Per-language command overrides (`[lsp.<lang>] command = [...]`). `argv[0]`
    /// is the program, the rest are args; `${ROOT}` in any element expands to
    /// the project root. When present for a language, `start()` spawns this
    /// instead of looking up a [`ServerSpec`](crate::servers::ServerSpec) on PATH.
    overrides: HashMap<String, Vec<String>>,
}

impl LspManager {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self::with_overrides(root, HashMap::new())
    }

    /// Like [`new`](Self::new) but with per-language command overrides. Each
    /// value is the argv to launch that language's server (`argv[0]` is the
    /// program); `${ROOT}` in any element expands to `root`. Overrides take
    /// precedence over the built-in [`ServerSpec`](crate::servers::ServerSpec) PATH lookup in `start()`.
    #[must_use]
    pub fn with_overrides(root: &Path, overrides: HashMap<String, Vec<String>>) -> Self {
        Self {
            root: root.to_path_buf(),
            clients: HashMap::new(),
            overrides,
        }
    }

    /// Ensure the manager's root matches the given path.
    /// If different, shut down all servers (they were initialized for a different project root).
    pub fn ensure_root(&mut self, root: &Path) {
        if self.root != root {
            tracing::info!("LSP: project root changed, restarting servers");
            self.shutdown_all();
            self.root = root.to_path_buf();
        }
    }

    /// Start a language server for the given cartog language.
    /// Returns Ok(()) if started successfully, Err if server not available or failed to init.
    pub fn start(&mut self, language: &str) -> Result<()> {
        if self.clients.contains_key(language) {
            return Ok(());
        }

        if let Some(argv) = self.overrides.get(language) {
            return self.start_override(language, argv.clone());
        }

        let candidates = find_servers(language);
        if candidates.is_empty() {
            bail!("no LSP server configured for {language}");
        }

        // Try each candidate in order, use the first one available on PATH
        let spec = candidates.iter().find(|s| is_binary_available(s.binary));

        let spec = match spec {
            Some(s) => s,
            None => {
                // Show install hints for all candidates
                let hints: Vec<_> = candidates
                    .iter()
                    .map(|s| format!("{}: {}", s.binary, s.install_hint))
                    .collect();
                bail!(
                    "no LSP server found on PATH. Install one of:\n  {}",
                    hints.join("\n  ")
                );
            }
        };

        let child = Command::new(spec.binary)
            .args(spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(open_lsp_log(spec.binary))
            .current_dir(&self.root)
            .spawn()
            .with_context(|| format!("failed to spawn {}", spec.binary))?;

        let mut client = LspClient::new(child)?;

        tracing::info!("LSP: waiting for {} to load project...", spec.binary);
        // Native server: cartog's PID is the real parent, so monitoring it is correct.
        self.initialize(&mut client, Some(std::process::id()))?;

        self.clients
            .insert(language.to_string(), (client, spec.language_id));
        Ok(())
    }

    /// Start a language server from a config command override (`[lsp.<lang>]`).
    ///
    /// Spawns `argv` directly (after `${ROOT}` expansion) instead of resolving
    /// a [`ServerSpec`](crate::servers::ServerSpec) on PATH, so a Dockerized server can run without the
    /// native toolchain. The `languageId` for the `initialize` handshake is
    /// borrowed from the language's spec (every supported language has one);
    /// an override for an unknown cartog language is rejected — cartog still
    /// needs a recognized language to drive `didOpen`'s `languageId`.
    fn start_override(&mut self, language: &str, argv: Vec<String>) -> Result<()> {
        if argv.is_empty() {
            bail!("[lsp.{language}] command is empty");
        }
        if argv[0].trim().is_empty() {
            bail!("[lsp.{language}] command executable (first element) is blank");
        }
        let language_id = find_servers(language)
            .first()
            .map(|s| s.language_id)
            .with_context(|| {
                format!("[lsp.{language}] override requires a known cartog language")
            })?;

        let root = root_for_substitution(&self.root);
        let expanded: Vec<String> = argv.iter().map(|a| expand_root(a, &root)).collect();

        let child = Command::new(&expanded[0])
            .args(&expanded[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Log keyed by language, not argv[0]: several `docker`-based
            // overrides would otherwise all write to `docker.log`.
            .stderr(open_lsp_log(language))
            .current_dir(&self.root)
            .spawn()
            .with_context(|| format!("failed to spawn override for {language}: {}", expanded[0]))?;

        let mut client = LspClient::new(child)?;
        tracing::info!("LSP: waiting for {language} override server to load...");
        // Command overrides may run the server in a container/sandbox with a
        // separate PID namespace, where cartog's host PID does not exist — the
        // LSP `processId` liveness check would then make the server exit at
        // startup (observed: pyright/typescript-language-server die during
        // `didOpen`). Send `null` to disable parent-process monitoring. The
        // trade-off (the server won't self-exit if cartog is SIGKILLed) is
        // covered by `LspClient`'s Drop reaping the child on every normal exit.
        self.initialize(&mut client, None)?;

        self.clients
            .insert(language.to_string(), (client, language_id));
        Ok(())
    }

    /// Resolve many `textDocument/definition` positions in one file. Requests
    /// are pipelined in windows of `DEFINITION_BATCH_WINDOW` (sent together,
    /// then collected) so round-trips overlap without an unbounded number of
    /// in-flight requests flooding the server's stdin or this client's memory.
    /// Returns one result per input position, in order; a per-position `Err`
    /// (timeout, LSP error) is isolated to that position. Positions are
    /// `(line, character)`, LSP 0-based.
    ///
    /// Each successful outcome: `Ok(None)` = no parseable answer (truly
    /// unresolvable — typo, dyn dispatch, macro, or a non-`file://` URI like
    /// jdtls's `jdt://`); `Ok(Some(InRoot(..)))` = target inside the indexed
    /// root; `Ok(Some(External))` = target outside the root (stdlib, deps,
    /// node_modules), caller should mark `state=3`.
    /// `cancel`, when `Some` and it returns `true`, aborts between windows with
    /// an `Err` whose cause is `"cancelled"` (worst-case latency: one window).
    pub fn definitions_batch(
        &mut self,
        language: &str,
        file_path: &str,
        positions: &[(u32, u32)],
        cancel: Option<crate::LspCancel<'_>>,
    ) -> Result<Vec<Result<Option<DefinitionOutcome>>>> {
        // Clone root so the per-window parse can borrow it while `client` holds
        // a `&mut` of `self.clients`.
        let root = self.root.clone();
        let uri = path_to_uri(&root.join(file_path));
        let (client, _) = self
            .clients
            .get_mut(language)
            .with_context(|| format!("no running LSP client for {language}"))?;

        let mut out = Vec::with_capacity(positions.len());
        for window in positions.chunks(DEFINITION_BATCH_WINDOW) {
            if cancel.is_some_and(|c| c()) {
                bail!(cartog_core::CANCELLED_MSG);
            }
            let params: Vec<(&str, Value)> = window
                .iter()
                .map(|&(line, character)| {
                    (
                        "textDocument/definition",
                        serde_json::json!({
                            "textDocument": { "uri": uri },
                            "position": { "line": line, "character": character },
                        }),
                    )
                })
                .collect();
            // A write failure aborts the whole call (the pipe is broken); the
            // caller treats that as a likely server death.
            let replies = client.request_batch(&params)?;
            for reply in replies {
                out.push(reply.and_then(|result| parse_definition_response(&result, &root)));
            }
        }
        Ok(out)
    }

    /// Notify the server that a file is open (required before definition requests).
    pub fn open_file(&mut self, language: &str, file_path: &str, content: &str) -> Result<()> {
        let (client, language_id) = self
            .clients
            .get_mut(language)
            .with_context(|| format!("no running LSP client for {language}"))?;

        let uri = path_to_uri(&self.root.join(file_path));

        client.send_notification(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": content,
                },
            }),
        )
    }

    /// Notify the server that a file is closed (frees server-side resources).
    pub fn close_file(&mut self, language: &str, file_path: &str) -> Result<()> {
        let (client, _) = self
            .clients
            .get_mut(language)
            .with_context(|| format!("no running LSP client for {language}"))?;

        let uri = path_to_uri(&self.root.join(file_path));

        client.send_notification(
            "textDocument/didClose",
            serde_json::json!({
                "textDocument": { "uri": uri },
            }),
        )
    }

    /// Check if the client for a language is still alive.
    pub fn is_alive(&mut self, language: &str) -> bool {
        self.clients
            .get_mut(language)
            .is_some_and(|(c, _)| c.is_alive())
    }

    /// Gracefully shut down all servers via the `shutdown`/`exit` handshake.
    /// Reaping is left to [`LspClient`]'s `Drop` as each client leaves scope.
    pub fn shutdown_all(&mut self) {
        for (lang, (mut client, _)) in self.clients.drain() {
            if let Err(e) = client.send_request("shutdown", Value::Null) {
                tracing::debug!("shutdown failed for {lang}: {e:#}");
                continue; // Drop reaps the child
            }
            let _ = client.send_notification("exit", Value::Null);

            // Wait briefly for clean exit; Drop force-kills if still alive.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while matches!(client.child.try_wait(), Ok(None))
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    /// Run the LSP `initialize`/`initialized` handshake and wait for readiness.
    ///
    /// `process_id` is the parent PID the server should monitor (`Some` for a
    /// native server cartog spawns directly); pass `None` for a sandboxed/
    /// containerized override server, where cartog's host PID is absent from the
    /// server's PID namespace and a stale `processId` makes it exit at startup.
    fn initialize(&self, client: &mut LspClient, process_id: Option<u32>) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let _result =
            client.send_request("initialize", initialize_params(&root_uri, process_id))?;

        client.send_notification("initialized", serde_json::json!({}))?;

        // Two-strategy readiness detection:
        // 1. Progress-based: wait for $/progress begin→end lifecycle (rust-analyzer)
        // 2. Probe-based fallback: if no progress within 5s, poll with definition requests
        self.wait_until_ready(client)?;

        Ok(())
    }

    /// Wait for the server to be ready.
    ///
    /// **Strategy 1 — Progress notifications** (servers like rust-analyzer):
    /// Track `$/progress` begin/end scopes. Ready when all scopes close + 2s quiesce.
    ///
    /// **Strategy 2 — Skip** (servers like typescript-language-server):
    /// If no `$/progress` arrives within 5s, proceed immediately. These servers
    /// respond to definition requests while loading (returning null for unloaded files).
    fn wait_until_ready(&self, client: &mut LspClient) -> Result<()> {
        let start = std::time::Instant::now();
        let deadline = start + std::time::Duration::from_secs(ready_timeout_secs());

        // Phase 1: try progress-based detection
        if let Some(elapsed) = self.wait_via_progress(client, deadline)? {
            tracing::info!("LSP: ready ({elapsed:.1}s)");
            return Ok(());
        }

        // No progress support — proceed immediately (server handles requests while loading)
        if let Some(elapsed) = self.wait_no_progress() {
            tracing::info!("LSP: no progress support, proceeding after {elapsed:.0}s");
        }
        Ok(())
    }

    /// Wait for $/progress scopes to complete. Returns `Some(elapsed)` if ready,
    /// `None` if no progress was received within the first 5s (caller should fallback).
    fn wait_via_progress(
        &self,
        client: &mut LspClient,
        deadline: std::time::Instant,
    ) -> Result<Option<f32>> {
        let start = std::time::Instant::now();

        // Phase 1: wait up to PROGRESS_DETECT_SECS for any progress notification
        let detect_deadline = start + std::time::Duration::from_secs(PROGRESS_DETECT_SECS);
        let mut seen_any = false;

        client.recv_until(detect_deadline, |msg| {
            if msg.get("method").and_then(|m| m.as_str()) == Some("$/progress") {
                seen_any = true;
                return true; // got one — move to phase 2
            }
            false
        });

        if !seen_any {
            return Ok(None); // no progress support — caller should fallback
        }

        // Phase 2: track all progress scopes until completion
        let mut active_scopes: u32 = 1; // we already saw one begin
        let mut all_done_at: Option<std::time::Instant> = None;
        let quiesce = std::time::Duration::from_secs(2);
        let mut seen_titles = std::collections::HashSet::new();

        // Process the first notification we already received (it's in the buffer)
        // — actually recv_until consumed it via callback. We counted it as active_scopes=1.

        let done = client.recv_until(deadline, |msg| {
            let method = msg.get("method").and_then(|m| m.as_str());
            if method != Some("$/progress") {
                return all_done_at.is_some_and(|t| t.elapsed() >= quiesce);
            }

            let value = match msg.get("params").and_then(|p| p.get("value")) {
                Some(v) => v,
                None => return false,
            };

            match value.get("kind").and_then(|k| k.as_str()) {
                Some("begin") => {
                    let title = value
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("loading");
                    if seen_titles.insert(title.to_string()) {
                        tracing::info!("LSP: {title}...");
                    }
                    active_scopes += 1;
                    all_done_at = None;
                }
                Some("report") => {
                    if let Some(msg) = value.get("message").and_then(|m| m.as_str()) {
                        tracing::debug!("LSP: {msg}");
                    }
                }
                Some("end") => {
                    active_scopes = active_scopes.saturating_sub(1);
                    tracing::debug!("LSP: scope ended (active={active_scopes})");
                    if active_scopes == 0 {
                        all_done_at = Some(std::time::Instant::now());
                    }
                }
                _ => {}
            }
            all_done_at.is_some_and(|t| t.elapsed() >= quiesce)
        });

        let elapsed = start.elapsed().as_secs_f32();
        if done {
            Ok(Some(elapsed))
        } else {
            tracing::info!("LSP: still loading after {elapsed:.0}s, proceeding anyway");
            Ok(Some(elapsed))
        }
    }

    /// Fallback for servers without `$/progress` support (e.g., typescript-language-server).
    /// These servers respond to definition requests immediately even while loading,
    /// returning null for unloaded files. No point in probing — proceed directly.
    fn wait_no_progress(&self) -> Option<f32> {
        Some(PROGRESS_DETECT_SECS as f32)
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

/// Parsed definition location from an LSP response.
#[derive(Debug, PartialEq, Eq)]
pub struct DefinitionLocation {
    /// Relative file path within project root.
    pub file_path: String,
    /// 1-based line number.
    pub line: u32,
}

/// The shape of a successful LSP `textDocument/definition` answer.
///
/// Distinguishes "target inside the indexed root" (caller tries to map to a
/// symbol) from "target outside the indexed root" (stdlib, third-party deps —
/// caller marks the edge as `state=3` external without further lookup).
#[derive(Debug, PartialEq, Eq)]
pub enum DefinitionOutcome {
    /// Definition lives inside the indexed root.
    InRoot(DefinitionLocation),
    /// Definition lives outside the indexed root (stdlib, deps, node_modules).
    /// The URI is logged by `parse_definition_response` before returning; it
    /// is not stored here to avoid per-call allocations on a hot path.
    External,
}

/// Expand `${ROOT}` in a command-override argv element to the project root.
/// `root` is host-absolute, so a Dockerized server mounting `-v ${ROOT}:${ROOT}`
/// sees the repo at the same path cartog uses to build `file://` URIs.
fn expand_root(arg: &str, root: &str) -> String {
    arg.replace("${ROOT}", root)
}

/// Build the LSP `initialize` params. `process_id` becomes JSON `null` when
/// `None`, which disables the server's parent-process liveness check — required
/// for override servers in a separate PID namespace (see [`LspManager::initialize`]).
fn initialize_params(root_uri: &str, process_id: Option<u32>) -> Value {
    serde_json::json!({
        "processId": process_id,
        "rootUri": root_uri,
        "capabilities": {
            "window": { "workDoneProgress": true },
            "textDocument": { "definition": { "dynamicRegistration": false } }
        },
    })
}

/// Project-root string for `${ROOT}` substitution. Strips the Windows verbatim
/// `\\?\` prefix that `canonicalize` adds: external tools (Docker `-v` mounts)
/// reject it, and it never appears in the `file://` URIs cartog exchanges. The
/// verbatim-UNC form `\\?\UNC\server\share` is restored to a plain UNC path
/// `\\server\share` rather than left as the malformed `UNC\server\share`.
fn root_for_substitution(root: &Path) -> String {
    let s = root.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        // Restore plain UNC (`\\server\share`) only when there's a body after
        // `UNC\` — guard against a degenerate `\\?\UNC\` collapsing to a bare `\\`.
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) if !unc.is_empty() => format!(r"\\{unc}"),
            _ => rest.to_string(),
        },
        None => s.to_string(),
    }
}

fn path_to_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri)
        .ok()
        .and_then(|u| u.to_file_path().ok())
}

/// Parse a textDocument/definition response into a [`DefinitionOutcome`].
///
/// Handles both single `Location` and `Location[]` responses. Non-`file://`
/// URIs (e.g. jdtls's `jdt://`) collapse to `Ok(None)` — cartog cannot index
/// them and they should be treated as truly unresolvable, not external.
fn parse_definition_response(result: &Value, root: &Path) -> Result<Option<DefinitionOutcome>> {
    let location = if result.is_array() {
        result.get(0)
    } else if result.get("uri").is_some() {
        Some(result)
    } else {
        None
    };

    let Some(loc) = location else {
        return Ok(None);
    };

    let uri = loc
        .get("uri")
        .and_then(|u| u.as_str())
        .context("missing uri in Location")?;

    let raw_path = match uri_to_path(uri) {
        Some(p) => p,
        None => return Ok(None),
    };

    // Canonicalize both sides before the in-root check: language servers can
    // emit non-canonical URIs (e.g. `/var/folders/...` on macOS where root
    // canonical is `/private/var/...`), AND the caller may have passed a
    // symlinked root (e.g. `/tmp/proj` → `/private/tmp/proj`). A lexical
    // strip_prefix on an asymmetric pair (one canonicalized, one not) would
    // wrongly tag in-root edges as External and burn a sticky state=3
    // marker — the exact regression the canonicalize was meant to close.
    //
    // Either side can fail to canonicalize: root may be missing in tests, or
    // raw_path can point to a file not yet on disk (generated code, race
    // against an unsaved buffer). To keep the strip_prefix symmetric, we
    // fall back to comparing both RAW paths together — never mix canonical
    // root with raw target (or vice versa).
    let (canonical_root, abs_path) = match (
        std::fs::canonicalize(root),
        std::fs::canonicalize(&raw_path),
    ) {
        (Ok(r), Ok(p)) => (r, p),
        _ => (root.to_path_buf(), raw_path),
    };

    // Out-of-root targets (stdlib, deps, node_modules) become External.
    let rel_path = match abs_path.strip_prefix(&canonical_root) {
        Ok(rel) => rel.to_string_lossy().to_string(),
        Err(_) => {
            tracing::debug!("definition outside root: {uri}");
            return Ok(Some(DefinitionOutcome::External));
        }
    };

    let line = loc
        .get("range")
        .and_then(|r| r.get("start"))
        .and_then(|s| s.get("line"))
        .and_then(|l| l.as_u64())
        .unwrap_or(0) as u32
        + 1; // LSP 0-based → cartog 1-based

    Ok(Some(DefinitionOutcome::InRoot(DefinitionLocation {
        file_path: rel_path,
        line,
    })))
}

/// Test-only fakes and accessors shared by the `manager` and `resolve` tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::time::Duration;

    /// Insert a pre-built client so tests can drive `definitions_batch` without
    /// a real language server on PATH.
    impl LspManager {
        pub(crate) fn insert_client_for_test(&mut self, language: &str, client: LspClient) {
            self.clients
                .insert(language.to_string(), (client, "python"));
        }

        pub(crate) fn set_definition_timeout_for_test(
            &mut self,
            language: &str,
            timeout: Duration,
        ) {
            if let Some((client, _)) = self.clients.get_mut(language) {
                client.set_timeout(timeout);
            }
        }
    }

    /// Fake server: emits `frames` verbatim, then stays alive without replying.
    /// `exec` replaces sh so Drop's kill+reap leaves nothing orphaned.
    #[cfg(unix)]
    pub(crate) fn scripted_fake_server(frames: &str) -> LspClient {
        use std::process::{Command, Stdio};
        // POSIX single-quote escaping so `frames` (JSON, quotes, CR/LF) can't
        // break out of the printf argument.
        let quoted = format!("'{}'", frames.replace('\'', r"'\''"));
        let script = format!("printf '%s' {quoted}; exec sleep 600");
        let child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fake server");
        LspClient::new(child).expect("client over fake server")
    }

    /// Fake server that never replies: every request times out, process stays up.
    #[cfg(unix)]
    pub(crate) fn silent_fake_server() -> LspClient {
        scripted_fake_server("")
    }

    /// One `result: null` LSP response frame per id, for [`scripted_fake_server`].
    #[cfg(unix)]
    pub(crate) fn null_result_frames(ids: &[i64]) -> String {
        ids.iter()
            .map(|id| {
                let body = format!(r#"{{"jsonrpc":"2.0","id":{id},"result":null}}"#);
                format!("Content-Length: {}\r\n\r\n{body}", body.len())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::test_support::silent_fake_server;
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn definitions_batch_cancels_between_windows() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = LspManager::new(tmp.path());
        mgr.insert_client_for_test("python", silent_fake_server());
        // Short per-window timeout so window 0's unanswered requests resolve
        // fast (as timeout Errs) and the test doesn't wait the 10s default.
        mgr.set_definition_timeout_for_test("python", Duration::from_millis(300));

        // Two windows' worth of positions.
        let positions: Vec<(u32, u32)> = (0..DEFINITION_BATCH_WINDOW as u32 + 8)
            .map(|i| (i, 0))
            .collect();

        // Probe trips only after the first window's check passed.
        let calls = AtomicU32::new(0);
        let cancel = || calls.fetch_add(1, Ordering::SeqCst) >= 1;

        let err = mgr
            .definitions_batch("python", "a.py", &positions, Some(&cancel))
            .expect_err("cancel between windows must abort the batch");
        assert!(err.to_string().contains("cancelled"), "got: {err}");
    }

    #[test]
    fn test_path_to_uri() {
        assert_eq!(
            path_to_uri(Path::new("/home/user/project")),
            "file:///home/user/project"
        );
    }

    #[test]
    fn test_uri_to_path() {
        let p = uri_to_path("file:///home/user/project/src/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("/home/user/project/src/main.rs"));
    }

    #[test]
    fn test_uri_to_path_non_file() {
        assert!(uri_to_path("https://example.com").is_none());
    }

    #[test]
    fn expand_root_replaces_all_occurrences() {
        assert_eq!(
            expand_root("-v=${ROOT}:${ROOT}", "/home/me/proj"),
            "-v=/home/me/proj:/home/me/proj"
        );
        assert_eq!(expand_root("--stdio", "/home/me/proj"), "--stdio");
        assert_eq!(expand_root("${ROOT}", "/r"), "/r");
    }

    #[test]
    fn root_for_substitution_strips_windows_verbatim_prefix() {
        // canonicalize on Windows yields `\\?\C:\...`; Docker `-v` rejects the
        // verbatim prefix, so it must be stripped before substitution.
        assert_eq!(
            root_for_substitution(Path::new(r"\\?\C:\Users\me\proj")),
            r"C:\Users\me\proj"
        );
        // Verbatim-UNC is restored to a plain UNC path, not left as `UNC\...`.
        assert_eq!(
            root_for_substitution(Path::new(r"\\?\UNC\server\share\proj")),
            r"\\server\share\proj"
        );
        // Unix paths (no prefix) pass through unchanged.
        assert_eq!(
            root_for_substitution(Path::new("/home/me/proj")),
            "/home/me/proj"
        );
    }

    #[test]
    fn start_override_rejects_empty_argv() {
        let mut mgr = LspManager::with_overrides(
            Path::new("/tmp/proj"),
            HashMap::from([("dart".to_string(), Vec::new())]),
        );
        let err = mgr.start("dart").unwrap_err();
        assert!(err.to_string().contains("command is empty"), "{err}");
    }

    #[test]
    fn initialize_params_processid_null_for_override() {
        // Override (sandboxed/container) server: processId must be JSON null so the
        // server doesn't monitor cartog's host PID (absent in its PID namespace)
        // and exit at startup — the bug that killed pyright/typescript-language-server.
        let p = initialize_params("file:///proj", None);
        assert!(p["processId"].is_null(), "override processId must be null");
        // Native server: a real PID is sent so liveness monitoring works.
        let p = initialize_params("file:///proj", Some(4242));
        assert_eq!(p["processId"].as_u64(), Some(4242));
    }

    #[test]
    fn initialize_params_preserves_root_and_capabilities() {
        // Guard the extraction: rootUri and the capability hints the resolver
        // relies on must survive intact regardless of the processId arm.
        let p = initialize_params("file:///proj", None);
        assert_eq!(p["rootUri"], "file:///proj");
        assert_eq!(p["capabilities"]["window"]["workDoneProgress"], true);
        assert_eq!(
            p["capabilities"]["textDocument"]["definition"]["dynamicRegistration"],
            false
        );
    }

    #[test]
    fn start_override_rejects_blank_executable() {
        let mut mgr = LspManager::with_overrides(
            Path::new("/tmp/proj"),
            HashMap::from([("dart".to_string(), vec!["   ".to_string()])]),
        );
        let err = mgr.start("dart").unwrap_err();
        assert!(err.to_string().contains("blank"), "{err}");
    }

    #[test]
    fn start_override_rejects_unknown_language() {
        let mut mgr = LspManager::with_overrides(
            Path::new("/tmp/proj"),
            HashMap::from([("cobol".to_string(), vec!["cobol-lsp".to_string()])]),
        );
        let err = mgr.start("cobol").unwrap_err();
        assert!(err.to_string().contains("known cartog language"), "{err}");
    }

    fn assert_in_root(outcome: DefinitionOutcome, expected_path: &str, expected_line: u32) {
        match outcome {
            DefinitionOutcome::InRoot(loc) => {
                assert_eq!(loc.file_path, expected_path);
                assert_eq!(loc.line, expected_line);
            }
            DefinitionOutcome::External => panic!("expected InRoot, got External"),
        }
    }

    #[test]
    fn test_parse_definition_single_location() {
        let root = Path::new("/project");
        let result = serde_json::json!({
            "uri": "file:///project/src/auth.rs",
            "range": { "start": { "line": 10, "character": 4 }, "end": { "line": 10, "character": 20 } },
        });

        let outcome = parse_definition_response(&result, root).unwrap().unwrap();
        assert_in_root(outcome, "src/auth.rs", 11); // 0-based → 1-based
    }

    #[test]
    fn test_parse_definition_array() {
        let root = Path::new("/project");
        let result = serde_json::json!([
            {
                "uri": "file:///project/src/auth.rs",
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 10 } },
            }
        ]);

        let outcome = parse_definition_response(&result, root).unwrap().unwrap();
        assert_in_root(outcome, "src/auth.rs", 6);
    }

    #[test]
    fn test_parse_definition_null() {
        let root = Path::new("/project");
        let result = Value::Null;

        assert!(parse_definition_response(&result, root).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_outside_root_yields_external() {
        let root = Path::new("/project");
        let result = serde_json::json!({
            "uri": "file:///other/src/lib.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        });

        let outcome = parse_definition_response(&result, root).unwrap().unwrap();
        assert_eq!(outcome, DefinitionOutcome::External);
    }

    #[test]
    fn test_parse_definition_external_per_language_uris() {
        // Real-world out-of-root URI shapes emitted by each language server we
        // support. All must collapse to `External` so the resolver tags them
        // state=3. Stdlib/registry/node_modules paths use platform-typical
        // locations — the strip_prefix test is purely lexical so they need not
        // exist on disk.
        let root = Path::new("/Users/dev/project");
        let cases: &[(&str, &str)] = &[
            // pyright → cpython stdlib
            ("python", "file:///usr/lib/python3.11/json/decoder.py"),
            // rust-analyzer → cargo registry
            (
                "rust",
                "file:///Users/dev/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.0.0/src/lib.rs",
            ),
            // typescript-language-server → node_modules (still under HOME, just outside the project root)
            (
                "typescript",
                "file:///Users/dev/other-project/node_modules/lodash/index.js",
            ),
            // gopls → GOROOT
            (
                "go",
                "file:///opt/homebrew/Cellar/go/1.22.0/libexec/src/fmt/print.go",
            ),
            // intelephense (PHP) → vendor in a sibling repo
            ("php", "file:///Users/dev/vendor-cache/symfony/console/Application.php"),
            // ruby-lsp → bundle install path
            ("ruby", "file:///Users/dev/.rbenv/gems/3.2.0/gems/rails-7.0.0/lib/rails.rb"),
        ];

        for (lang, uri) in cases {
            let result = serde_json::json!({
                "uri": uri,
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
            });
            let outcome = parse_definition_response(&result, root)
                .unwrap()
                .unwrap_or_else(|| panic!("{lang}: parse returned None for {uri}"));
            assert_eq!(
                outcome,
                DefinitionOutcome::External,
                "{lang}: expected External for {uri}"
            );
        }
    }

    #[test]
    fn test_parse_definition_non_file_uri_yields_none() {
        // jdtls returns `jdt://` URIs for JDK and Maven artifacts. Cartog
        // cannot index them at all (no filesystem path), so they must collapse
        // to `Ok(None)` — truly unresolvable rather than external.
        let root = Path::new("/project");
        let result = serde_json::json!({
            "uri": "jdt://contents/java.base/java.lang/String.class",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        });

        assert!(parse_definition_response(&result, root).unwrap().is_none());
    }

    #[test]
    fn test_parse_definition_symmetric_fallback_on_missing_target() {
        // Regression: when root canonicalizes (`/tmp/proj` → `/private/tmp/proj`
        // on macOS) but raw_path does NOT (file not yet on disk: generated
        // code, race against an unsaved buffer), the asymmetric pair made
        // strip_prefix fail and burned an in-root edge as sticky External.
        // The fix uses RAW paths on both sides whenever either canonicalize
        // fails, so the lexical strip_prefix matches symmetrically.
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path(); // exists and canonicalizes
        let not_yet_on_disk = root.join("generated.rs");
        // Use the same path_to_uri helper the rest of the codebase uses so
        // the test stays correct on Windows (drive-letter / verbatim paths).
        let uri = path_to_uri(&not_yet_on_disk);
        let result = serde_json::json!({
            "uri": uri,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
        });
        let outcome = parse_definition_response(&result, root)
            .unwrap()
            .expect("expected Some outcome for in-root not-yet-existent target");
        // Symmetric raw-vs-raw strip_prefix must classify this as InRoot, not
        // External, so the resolver leaves it at state=0 for the next reindex.
        match outcome {
            DefinitionOutcome::InRoot(loc) => {
                assert_eq!(loc.file_path, "generated.rs");
            }
            DefinitionOutcome::External => {
                panic!("missing in-root target must not be marked External (sticky state=3)");
            }
        }
    }
}
