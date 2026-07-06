use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Framed bytes for the writer thread. The writer acks each on a **persistent**
/// per-client channel (not one allocated per call) — writes are strictly serial
/// (one owner, blocks on the ack before the next), so a single channel suffices
/// and keeps this off the per-edge allocation path.
type WriteJob = Vec<u8>;

/// Minimal synchronous LSP client over stdio pipes.
///
/// A background reader thread avoids blocking on IO while waiting for progress
/// notifications during init; a background writer thread owns `ChildStdin` so a
/// server that stops draining its stdin surfaces as a bounded ack timeout on the
/// owner, not a wedged `write_all` that hangs the index (see `WRITE_TIMEOUT_PREFIX`).
///
/// Handles server-initiated requests (e.g., `window/workDoneProgress/create`)
/// by auto-responding with `null` result, as required by the LSP spec.
///
/// No Loom model: owner↔writer talk over one FIFO `mpsc` + per-call ack channel,
/// no shared atomic, so there is no memory-ordering interleaving to explore.
pub struct LspClient {
    pub child: Child,
    writer_tx: mpsc::Sender<WriteJob>,
    /// Persistent ack channel (see [`WriteJob`]); the writer sends one result per
    /// job here and `write_message` waits on it.
    write_ack: mpsc::Receiver<io::Result<()>>,
    _writer_handle: JoinHandle<()>,
    receiver: mpsc::Receiver<Value>,
    _reader_handle: JoinHandle<()>,
    next_id: i64,
    timeout: Duration,
    /// Deadline for one framed write to be acked. A server that stops reading
    /// fills the ~64KB pipe buffer and parks the writer's `write_all`; this
    /// bounds how long the owner waits before bailing (writer is reaped on `Drop`).
    /// Set generously (10s): a healthy server draining a huge `didOpen` (>64KB
    /// minified file) must not trip it, or that language falls back to heuristics.
    write_timeout: Duration,
    /// Latched once a write ack times out. The writer thread stays parked on the
    /// wedged pipe (FIFO), so every later write would also queue behind it and
    /// burn a full `write_timeout`; once wedged, writes fast-fail instead. The
    /// only cure is dropping the client (kill → `BrokenPipe`), so callers must
    /// discard a wedged client rather than reuse it.
    write_wedged: AtomicBool,
    /// Notifications buffered during a synchronous `send_request` for later
    /// consumption by `recv_until` (used only during the initialize handshake).
    buffered_notifications: VecDeque<Value>,
}

impl LspClient {
    pub fn new(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().context("no stdin on child process")?;
        let stdout = child.stdout.take().context("no stdout on child process")?;

        let (tx, rx) = mpsc::channel();
        let reader_handle = std::thread::spawn(move || reader_thread(stdout, tx));

        let (writer_tx, write_rx) = mpsc::channel();
        let (ack_tx, write_ack) = mpsc::channel();
        let writer_handle = std::thread::spawn(move || writer_thread(stdin, write_rx, ack_tx));

        Ok(Self {
            child,
            writer_tx,
            write_ack,
            _writer_handle: writer_handle,
            receiver: rx,
            _reader_handle: reader_handle,
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            write_timeout: DEFAULT_TIMEOUT,
            write_wedged: AtomicBool::new(false),
            buffered_notifications: VecDeque::new(),
        })
    }

    /// True once a write timed out: the writer thread is parked on a pipe the
    /// server stopped reading, so this client can no longer be written to and
    /// should be discarded (see [`LspManager::drop_client`](crate::manager::LspManager)).
    pub fn is_write_wedged(&self) -> bool {
        self.write_wedged.load(Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and wait for the matching response. Used for the
    /// one-at-a-time handshake traffic (`initialize`, `shutdown`); pipelined
    /// definition queries go through [`Self::request_batch`].
    pub fn send_request<P: Serialize>(&mut self, method: &str, params: P) -> Result<Value> {
        let id = self.write_request(method, params)?;
        self.collect_responses(&[id], Instant::now() + self.timeout)
            .pop()
            .expect("collect_responses returns one result per id")
    }

    /// Write a request frame and return its id, without waiting for the reply.
    fn write_request<P: Serialize>(&mut self, method: &str, params: P) -> Result<i64> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&msg)?;
        Ok(id)
    }

    /// Send a batch of `(method, params)` requests pipelined — all are written
    /// before any response is read, so their round-trips overlap — then collect
    /// the replies in input order. Returns one `Result<Value>` per request.
    ///
    /// A single shared deadline (`self.timeout`) bounds the whole batch, so a
    /// stalled or out-of-order server cannot make the cost scale with the batch
    /// size: once it elapses, every still-missing request resolves to a timeout
    /// `Err`. Notifications are drained and discarded (no unbounded buffering),
    /// and responses for ids not in this batch are ignored — nothing leaks past
    /// the call. The caller is responsible for keeping the batch small enough to
    /// bound in-flight memory and stdin backpressure (see `definitions_batch`).
    pub fn request_batch<P: Serialize>(
        &mut self,
        requests: &[(&str, P)],
    ) -> Result<Vec<Result<Value>>> {
        let mut ids = Vec::with_capacity(requests.len());
        for (method, params) in requests {
            ids.push(self.write_request(method, params)?);
        }
        Ok(self.collect_responses(&ids, Instant::now() + self.timeout))
    }

    /// Shorten the per-batch timeout (tests only) so stall paths don't wait 10s.
    #[cfg(test)]
    pub(crate) fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Shorten the per-write ack timeout (tests only) so a server that stops
    /// reading stdin surfaces a write timeout fast, not after the 10s default.
    #[cfg(test)]
    pub(crate) fn set_write_timeout(&mut self, timeout: Duration) {
        self.write_timeout = timeout;
    }

    /// Send a JSON-RPC notification (no response expected).
    pub fn send_notification<P: Serialize>(&mut self, method: &str, params: P) -> Result<()> {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&msg)
    }

    /// Check if the child process is still alive.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Receive messages until deadline, passing each to a callback.
    /// Returns when the callback returns `true` (done) or deadline is reached.
    /// Drains buffered notifications first (from prior `read_response` calls).
    pub fn recv_until(
        &mut self,
        deadline: Instant,
        mut on_message: impl FnMut(&Value) -> bool,
    ) -> bool {
        // Drain notifications buffered during read_response
        while let Some(msg) = self.buffered_notifications.pop_front() {
            if on_message(&msg) {
                return true;
            }
        }

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }

            match self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(500)))
            {
                Ok(msg) => {
                    // Auto-respond to server-initiated requests
                    if is_server_request(&msg) {
                        let _ = self.auto_respond(&msg);
                        continue;
                    }
                    if on_message(&msg) {
                        return true;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Frame `msg` into one `Vec` (header + body = one atomic unit), hand it to
    /// the writer thread, and wait for the ack on the persistent channel. A stuck
    /// stdin shows up as an ack `Timeout` (bounded by `write_timeout`) so the
    /// owner can bail.
    ///
    /// Once a prior write wedged, fast-fail: the writer is still parked on the
    /// dead pipe (FIFO), so a fresh job would only queue behind it and burn
    /// another full `write_timeout`. Returns the same [`WRITE_TIMEOUT_PREFIX`]
    /// error so callers classify it identically. This latch also means no ack
    /// from a wedged write can ever leak into a later call: once wedged we never
    /// send again, so the reused ack channel stays in lockstep with the sends.
    fn write_message(&mut self, msg: &Value) -> Result<()> {
        if self.write_wedged.load(Ordering::Relaxed) {
            bail!("{WRITE_TIMEOUT_PREFIX} (client already wedged)");
        }

        let body = serde_json::to_string(msg)?;
        let mut bytes = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        bytes.extend_from_slice(body.as_bytes());

        self.writer_tx
            .send(bytes)
            .map_err(|_| anyhow::anyhow!("LSP writer thread gone"))?;

        match self.write_ack.recv_timeout(self.write_timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e).context("writing LSP message to server stdin"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.write_wedged.store(true, Ordering::Relaxed);
                bail!("{WRITE_TIMEOUT_PREFIX} after {:?}", self.write_timeout)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("LSP writer thread gone"),
        }
    }

    /// Read replies for `ids` (already sent) under one shared `deadline`,
    /// returning one `Result<Value>` per id in input order.
    ///
    /// Out-of-order replies are matched by id, so the server may answer in any
    /// order. Once `deadline` elapses, every still-unfilled slot is a timeout
    /// `Err` — a single bound for the whole batch, not per id, so a stalled
    /// server costs one `timeout`, never `n × timeout`. A disconnect fails all
    /// remaining slots at once. Notifications are buffered for `recv_until` only
    /// in the single-id (handshake) case; in batch mode they are discarded so a
    /// chatty server can't grow `buffered_notifications` without bound. Replies
    /// for ids not in this batch are dropped, so nothing leaks past the call.
    fn collect_responses(&mut self, ids: &[i64], deadline: Instant) -> Vec<Result<Value>> {
        let buffer_notifications = ids.len() == 1; // handshake path feeds recv_until
        let mut slot: HashMap<i64, usize> =
            ids.iter().enumerate().map(|(i, &id)| (id, i)).collect();
        let mut out: Vec<Option<Result<Value>>> = (0..ids.len()).map(|_| None).collect();
        let mut filled = 0usize;

        while filled < ids.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break; // deadline hit — leftover slots become timeouts below
            }

            let msg = match self.receiver.recv_timeout(remaining) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // Server gone: fail every outstanding slot now, no waiting.
                    for entry in out.iter_mut() {
                        if entry.is_none() {
                            *entry = Some(Err(anyhow::anyhow!("LSP server disconnected")));
                        }
                    }
                    return out.into_iter().map(Option::unwrap).collect();
                }
            };

            if is_server_request(&msg) {
                let _ = self.auto_respond(&msg);
                continue;
            }
            if is_notification(&msg) {
                if buffer_notifications {
                    self.buffered_notifications.push_back(msg);
                }
                // batch mode: drop it — nothing drains buffered_notifications
                // during resolution, so buffering would grow without bound.
                continue;
            }

            // A response. Match it to a slot in this batch; ignore any other id
            // (a stale/duplicate/late reply for a request we no longer await).
            let Some(id) = msg.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let Some(i) = slot.remove(&id) else {
                continue;
            };
            let outcome = match msg.get("error") {
                Some(error) => Err(anyhow::anyhow!(
                    "LSP error: {}",
                    error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown LSP error")
                )),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            out[i] = Some(outcome);
            filled += 1;
        }

        // Any slot still empty timed out (or the deadline elapsed before send).
        out.into_iter()
            .enumerate()
            .map(|(i, entry)| {
                entry.unwrap_or_else(|| Err(anyhow::anyhow!("{REQUEST_TIMEOUT_PREFIX} {}", ids[i])))
            })
            .collect()
    }

    /// Respond to a server-initiated request with `result: null`.
    fn auto_respond(&mut self, request: &Value) -> Result<()> {
        self.write_message(&build_auto_response(request))
    }
}

/// Batch-deadline timeout message; matched by [`is_request_timeout`] (same
/// message-based pattern as `cartog_core::is_cancelled`).
const REQUEST_TIMEOUT_PREFIX: &str = "timeout waiting for response to request";

/// True when `err` is a batch-deadline timeout (no reply at all — unlike an
/// LSP error response, which proves the server is responsive).
pub(crate) fn is_request_timeout(err: &anyhow::Error) -> bool {
    err.root_cause()
        .to_string()
        .starts_with(REQUEST_TIMEOUT_PREFIX)
}

/// Write-ack timeout message; matched by [`is_write_timeout`]. Kept distinct from
/// [`REQUEST_TIMEOUT_PREFIX`] so the read-side all-timeout-window counter (a
/// mute-but-reading server) is unaffected by this write-side stall.
const WRITE_TIMEOUT_PREFIX: &str = "timeout writing to LSP server stdin";

/// True when `err` is a write-ack timeout: the server stopped draining its stdin
/// (alive but stuck), so the drain should stop early rather than re-block.
pub(crate) fn is_write_timeout(err: &anyhow::Error) -> bool {
    err.root_cause()
        .to_string()
        .starts_with(WRITE_TIMEOUT_PREFIX)
}

/// Build the auto-response to a server-initiated request: echo its `id` with a
/// null result, as the LSP spec requires.
fn build_auto_response(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": null,
    })
}

impl Drop for LspClient {
    /// Reap the child: `std::process::Child` does not kill/wait on drop, so a
    /// client dropped before `shutdown_all` (e.g. failed init) would orphan it.
    /// The body runs before fields drop, so `kill()` closes the server's stdin
    /// read-end while `_writer_handle` still holds the write end — a writer parked
    /// on a non-reading server then gets `BrokenPipe` and exits (never joined).
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A server-initiated request has both `id` and `method`.
fn is_server_request(msg: &Value) -> bool {
    msg.get("id").is_some() && msg.get("method").is_some()
}

/// A notification has `method` but no `id`.
fn is_notification(msg: &Value) -> bool {
    msg.get("method").is_some() && msg.get("id").is_none()
}

/// Owns `ChildStdin` and performs every write, one framed job at a time, acking
/// each result on the persistent `ack` channel. Exits when `writer_tx` drops (on
/// `LspClient::drop`) or the owner is gone (ack send fails). A server that stops
/// reading parks the `write_all` here; `Drop` kills the child, so the pipe breaks
/// and the parked write returns `BrokenPipe` and this thread ends.
fn writer_thread(
    mut stdin: ChildStdin,
    rx: mpsc::Receiver<WriteJob>,
    ack: mpsc::Sender<io::Result<()>>,
) {
    for bytes in rx {
        let result = stdin.write_all(&bytes).and_then(|()| stdin.flush());
        // Owner gone (client dropped): nothing left to serve — stop.
        if ack.send(result).is_err() {
            break;
        }
    }
}

/// Background thread that reads LSP messages and sends them to the channel.
fn reader_thread(stdout: ChildStdout, tx: mpsc::Sender<Value>) {
    let mut reader = BufReader::new(stdout);

    loop {
        let msg = match read_message(&mut reader) {
            Ok(msg) => msg,
            Err(_) => break,
        };
        if tx.send(msg).is_err() {
            break;
        }
    }
}

fn read_message(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let content_length = read_headers(reader)?;
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid JSON in LSP message")
}

fn read_headers(reader: &mut BufReader<ChildStdout>) -> Result<usize> {
    let mut content_length = None;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            bail!("LSP server closed stdout (EOF)");
        }

        if line == "\r\n" || line == "\n" {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }

    content_length.context("missing Content-Length header in LSP message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_server_request() {
        let req =
            serde_json::json!({"id": 1, "method": "window/workDoneProgress/create", "params": {}});
        assert!(is_server_request(&req));
    }

    #[test]
    fn test_is_notification() {
        let notif = serde_json::json!({"method": "$/progress", "params": {}});
        assert!(is_notification(&notif));
        assert!(!is_server_request(&notif));
    }

    #[test]
    fn test_response_is_neither() {
        let resp = serde_json::json!({"id": 1, "result": null});
        assert!(!is_server_request(&resp));
        assert!(!is_notification(&resp));
    }

    #[test]
    fn is_write_timeout_matches_only_the_write_prefix() {
        let write = anyhow::anyhow!("{WRITE_TIMEOUT_PREFIX} after 300ms");
        assert!(is_write_timeout(&write));
        assert!(!is_request_timeout(&write));

        let request = anyhow::anyhow!("{REQUEST_TIMEOUT_PREFIX} 7");
        assert!(!is_write_timeout(&request));

        let lsp_error = anyhow::anyhow!("LSP error: boom");
        assert!(!is_write_timeout(&lsp_error));
    }

    #[test]
    fn build_auto_response_echoes_id_with_null_result() {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 999,
            "method": "window/workDoneProgress/create",
            "params": {}
        });
        let resp = build_auto_response(&req);
        assert_eq!(resp["id"], 999);
        assert_eq!(resp["result"], Value::Null);
        assert_eq!(resp["jsonrpc"], "2.0");
        assert!(resp.get("method").is_none()); // a response, not a re-issued request
    }

    #[test]
    fn build_auto_response_preserves_string_id() {
        // LSP ids may be strings; clone keeps the type, not just the value.
        let req = serde_json::json!({ "id": "abc", "method": "m" });
        assert_eq!(build_auto_response(&req)["id"], "abc");
    }

    #[test]
    fn test_jsonrpc_request_format() {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "textDocument/definition",
            "params": { "textDocument": { "uri": "file:///test.rs" } },
        });
        let body = serde_json::to_string(&msg).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        let full = format!("{header}{body}");

        assert!(full.starts_with("Content-Length: "));
        assert!(full.contains("\r\n\r\n"));
        let parts: Vec<&str> = full.splitn(2, "\r\n\r\n").collect();
        let parsed: Value = serde_json::from_str(parts[1]).unwrap();
        assert_eq!(parsed["method"], "textDocument/definition");
    }

    #[test]
    fn test_jsonrpc_response_parsing() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "uri": "file:///foo.rs", "range": {} },
        });

        assert!(response.get("error").is_none());
        let result = response.get("result").unwrap();
        assert_eq!(result["uri"], "file:///foo.rs");
    }

    #[test]
    fn test_jsonrpc_error_response() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32600, "message": "Invalid Request" },
        });

        let error = response.get("error").unwrap();
        let message = error.get("message").and_then(|m| m.as_str()).unwrap();
        assert_eq!(message, "Invalid Request");
    }

    #[cfg(unix)]
    #[test]
    fn drop_reaps_child_so_an_un_shutdown_client_does_not_leak() {
        use std::process::{Command, Stdio};

        // `sleep 600` stands in for an LSP server; it outlives the test if Drop doesn't kill it.
        let mut child = Command::new("sleep")
            .arg("600")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(
            matches!(child.try_wait(), Ok(None)),
            "child should be alive before client owns it"
        );

        let client = LspClient::new(child).expect("client over live child");
        drop(client);

        // `kill -0` fails once the child is reaped.
        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!still_alive, "Drop must kill+reap the child (pid {pid})");
    }

    /// Frame a JSON body the way an LSP server would, for the fake-server tests.
    #[cfg(unix)]
    fn framed(body: &str) -> String {
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    /// Spawn a fake LSP server that writes `frames` to stdout then idles.
    /// `exec sleep` REPLACES the shell (no forked child), so `LspClient::Drop`
    /// kill+reap of the single process leaves nothing orphaned. stdin stays
    /// open (the client owns the write end), so the reader sees no EOF until
    /// the process is killed on Drop.
    #[cfg(unix)]
    fn fake_server(frames: &str) -> LspClient {
        use std::process::{Command, Stdio};
        let script = format!("printf '%s' '{frames}'; exec sleep 600");
        let child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fake server");
        LspClient::new(child).expect("client over fake server")
    }

    #[cfg(unix)]
    #[test]
    fn request_batch_matches_out_of_order_replies_by_id() {
        // Server answers the SECOND request (id 2) before the first (id 1).
        // request_batch must still return them in input order.
        let r2 = framed(r#"{"jsonrpc":"2.0","id":2,"result":{"v":2}}"#);
        let r1 = framed(r#"{"jsonrpc":"2.0","id":1,"result":{"v":1}}"#);
        let mut client = fake_server(&format!("{r2}{r1}"));

        let replies = client
            .request_batch(&[("m", serde_json::json!({})), ("m", serde_json::json!({}))])
            .expect("batch sends");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].as_ref().unwrap()["v"], 1);
        assert_eq!(replies[1].as_ref().unwrap()["v"], 2);
    }

    #[cfg(unix)]
    #[test]
    fn request_batch_isolates_per_request_error() {
        // id 1 errors, id 2 succeeds — the error must not sink id 2.
        let e1 = framed(r#"{"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"boom"}}"#);
        let r2 = framed(r#"{"jsonrpc":"2.0","id":2,"result":{"v":2}}"#);
        let mut client = fake_server(&format!("{e1}{r2}"));

        let replies = client
            .request_batch(&[("m", serde_json::json!({})), ("m", serde_json::json!({}))])
            .expect("batch sends");
        assert!(replies[0]
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("boom"));
        assert_eq!(replies[1].as_ref().unwrap()["v"], 2);
    }

    #[cfg(unix)]
    #[test]
    fn request_batch_shares_one_deadline_for_missing_replies() {
        // Server answers only id 2; ids 1 and 3 never come. A single shared
        // deadline must cap the whole batch at ~one timeout (not 3×), and the
        // missing slots return timeout Errs in input order.
        let r2 = framed(r#"{"jsonrpc":"2.0","id":2,"result":{"v":2}}"#);
        let mut client = fake_server(&r2);
        // Generous enough to clear process-spawn latency, tight enough that
        // 3 × timeout would blow the assertion below.
        let timeout = Duration::from_millis(1500);
        client.set_timeout(timeout);

        let started = Instant::now();
        let replies = client
            .request_batch(&[
                ("m", serde_json::json!({})),
                ("m", serde_json::json!({})),
                ("m", serde_json::json!({})),
            ])
            .expect("batch sends");
        let elapsed = started.elapsed();

        assert_eq!(replies.len(), 3);
        assert!(replies[0].is_err(), "id 1 missing → timeout");
        assert_eq!(replies[1].as_ref().unwrap()["v"], 2);
        assert!(replies[2].is_err(), "id 3 missing → timeout");
        // One shared deadline: the whole batch waits ~timeout, never 3×.
        assert!(
            elapsed < timeout * 2,
            "batch took {elapsed:?}; deadline should be shared, not per-id"
        );
    }

    /// Spawn a fake server that writes `frames`, then CLOSES STDOUT but stays
    /// alive (`exec sleep ... 1>&-`). The reader thread sees EOF → the channel
    /// disconnects, while stdin stays open so the client's sends still succeed.
    /// (Exiting the process outright would race the client's writes against a
    /// closed stdin and surface a broken pipe instead of a clean disconnect.)
    #[cfg(unix)]
    fn fake_server_eof_stdout(frames: &str) -> LspClient {
        use std::process::{Command, Stdio};
        let script = format!("printf '%s' '{frames}'; exec sleep 600 1>&-");
        let child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn fake server");
        LspClient::new(child).expect("client over fake server")
    }

    #[cfg(unix)]
    #[test]
    fn request_batch_fails_remaining_slots_fast_on_disconnect() {
        // Server answers id 1 then exits, so ids 2 and 3 disconnect rather than
        // wait out the deadline. With a long timeout, a fast return proves the
        // Disconnected arm short-circuits every outstanding slot.
        let r1 = framed(r#"{"jsonrpc":"2.0","id":1,"result":{"v":1}}"#);
        let mut client = fake_server_eof_stdout(&r1);
        client.set_timeout(Duration::from_secs(30)); // would dominate if we waited

        let started = Instant::now();
        let replies = client
            .request_batch(&[
                ("m", serde_json::json!({})),
                ("m", serde_json::json!({})),
                ("m", serde_json::json!({})),
            ])
            .expect("batch sends");
        let elapsed = started.elapsed();

        assert_eq!(replies.len(), 3);
        assert_eq!(replies[0].as_ref().unwrap()["v"], 1);
        assert!(replies[1].is_err() && replies[2].is_err());
        assert!(
            replies[1]
                .as_ref()
                .unwrap_err()
                .to_string()
                .contains("disconnect"),
            "got: {:?}",
            replies[1]
        );
        // Disconnect must short-circuit, not burn the 30s deadline.
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn request_batch_drains_notifications_and_server_requests() {
        // A notification and a server-initiated request are interleaved with the
        // real replies. Both must be handled (auto-responded / discarded) without
        // disturbing reply matching, so the batch still completes.
        let note = framed(r#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#);
        let srv_req = framed(
            r#"{"jsonrpc":"2.0","id":999,"method":"window/workDoneProgress/create","params":{}}"#,
        );
        let r1 = framed(r#"{"jsonrpc":"2.0","id":1,"result":{"v":1}}"#);
        let r2 = framed(r#"{"jsonrpc":"2.0","id":2,"result":{"v":2}}"#);
        let mut client = fake_server(&format!("{note}{srv_req}{r1}{r2}"));

        let replies = client
            .request_batch(&[("m", serde_json::json!({})), ("m", serde_json::json!({}))])
            .expect("batch sends");
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0].as_ref().unwrap()["v"], 1);
        assert_eq!(replies[1].as_ref().unwrap()["v"], 2);
    }

    /// A didOpen frame larger than the ~64KB pipe buffer, so a server that never
    /// drains its stdin wedges the write. `silent_fake_server` (`exec sleep`)
    /// never reads stdin, so this reproduces the hang on current `main` and the
    /// bounded write-ack timeout after the fix.
    #[cfg(unix)]
    fn big_didopen(text_len: usize) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": { "textDocument": { "uri": "file:///x.py", "text": "x".repeat(text_len) } },
        })
    }

    #[cfg(unix)]
    #[test]
    fn write_message_times_out_when_child_never_reads_stdin() {
        use crate::manager::test_support::silent_fake_server;

        let mut client = silent_fake_server();
        client.set_write_timeout(Duration::from_millis(300));

        let started = Instant::now();
        let err = client
            .write_message(&big_didopen(200_000))
            .expect_err("a non-reading server must make the write time out");
        let elapsed = started.elapsed();

        assert!(is_write_timeout(&err), "got: {err}");
        // Classification, not exact timing (cf. the wall-clock deadline flake):
        // a loose bound just proves it returned instead of hanging forever.
        assert!(elapsed < client.write_timeout * 4, "took {elapsed:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_timeout_does_not_hang_drop() {
        use crate::manager::test_support::silent_fake_server;

        let mut client = silent_fake_server();
        client.set_write_timeout(Duration::from_millis(300));
        let pid = client.child.id();
        let _ = client.write_message(&big_didopen(200_000));

        drop(client); // kill unblocks the parked writer; must return promptly

        let still_alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!still_alive, "Drop must kill+reap the child (pid {pid})");
    }

    #[cfg(unix)]
    #[test]
    fn second_write_fast_fails_after_a_wedge_instead_of_blocking_again() {
        // The writer thread is a serial FIFO parked on the wedged pipe, so a
        // naive second write would queue behind it and burn another full
        // write_timeout. Once wedged, writes must fast-fail (near-instant).
        use crate::manager::test_support::silent_fake_server;

        let mut client = silent_fake_server();
        client.set_write_timeout(Duration::from_millis(300));

        let first = client
            .write_message(&big_didopen(200_000))
            .expect_err("first write wedges");
        assert!(is_write_timeout(&first), "got: {first}");
        assert!(client.is_write_wedged());

        let started = Instant::now();
        let second = client
            .write_message(&big_didopen(10))
            .expect_err("second write must fast-fail, not block again");
        let elapsed = started.elapsed();
        assert!(is_write_timeout(&second), "got: {second}");
        // Well under one write_timeout: it did not queue behind the parked job.
        assert!(
            elapsed < Duration::from_millis(100),
            "second write should be near-instant, took {elapsed:?}"
        );
    }
}
