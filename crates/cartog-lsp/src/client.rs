use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// Minimal synchronous LSP client over stdio pipes.
///
/// Uses a background reader thread to avoid blocking on IO when waiting
/// for progress notifications during server initialization.
///
/// Handles server-initiated requests (e.g., `window/workDoneProgress/create`)
/// by auto-responding with `null` result, as required by the LSP spec.
pub struct LspClient {
    pub child: Child,
    stdin: ChildStdin,
    receiver: mpsc::Receiver<Value>,
    _reader_handle: JoinHandle<()>,
    next_id: i64,
    timeout: Duration,
    /// Notifications buffered during a synchronous `send_request` for later
    /// consumption by `recv_until` (used only during the initialize handshake).
    buffered_notifications: VecDeque<Value>,
}

impl LspClient {
    pub fn new(mut child: Child) -> Result<Self> {
        let stdin = child.stdin.take().context("no stdin on child process")?;
        let stdout = child.stdout.take().context("no stdout on child process")?;

        let (tx, rx) = mpsc::channel();
        let handle = std::thread::spawn(move || reader_thread(stdout, tx));

        Ok(Self {
            child,
            stdin,
            receiver: rx,
            _reader_handle: handle,
            next_id: 1,
            timeout: DEFAULT_TIMEOUT,
            buffered_notifications: VecDeque::new(),
        })
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

    fn write_message(&mut self, msg: &Value) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(body.as_bytes())?;
        self.stdin.flush()?;
        Ok(())
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
                entry.unwrap_or_else(|| {
                    Err(anyhow::anyhow!(
                        "timeout waiting for response to request {}",
                        ids[i]
                    ))
                })
            })
            .collect()
    }

    /// Respond to a server-initiated request with `result: null`.
    fn auto_respond(&mut self, request: &Value) -> Result<()> {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        self.write_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null,
        }))
    }
}

impl Drop for LspClient {
    /// Reap the child: `std::process::Child` does not kill/wait on drop, so a
    /// client dropped before `shutdown_all` (e.g. failed init) would orphan it.
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
}
