use std::collections::VecDeque;
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
    /// Notifications buffered during `read_response` for later consumption by `recv_until`.
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

    /// Send a JSON-RPC request and wait for the matching response.
    pub fn send_request<P: Serialize>(&mut self, method: &str, params: P) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&msg)?;
        self.read_response(id)
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

    fn read_response(&mut self, expected_id: i64) -> Result<Value> {
        let deadline = Instant::now() + self.timeout;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timeout waiting for response to request {expected_id}");
            }

            let msg = match self.receiver.recv_timeout(remaining) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    bail!("timeout waiting for response to request {expected_id}");
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("LSP server disconnected");
                }
            };

            // Server-initiated request — auto-respond per LSP spec
            if is_server_request(&msg) {
                let _ = self.auto_respond(&msg);
                continue;
            }

            // Server notification — buffer for later consumption by recv_until
            if is_notification(&msg) {
                self.buffered_notifications.push_back(msg);
                continue;
            }

            // Response with matching ID — return it
            if let Some(id) = msg.get("id") {
                if id.as_i64() == Some(expected_id) {
                    if let Some(error) = msg.get("error") {
                        let message = error
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown LSP error");
                        bail!("LSP error: {message}");
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            // Response with wrong ID — discard (shouldn't happen in practice)
        }
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
    /// Reap the child LSP server. `std::process::Child` does NOT kill or wait
    /// on drop, so without this a client dropped before reaching
    /// `LspManager::shutdown_all` (e.g. an `initialize()` that timed out) would
    /// orphan a heavyweight server process. Graceful `shutdown`/`exit` is the
    /// manager's job; this is the unconditional safety net.
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

        // A long-lived child stands in for an LSP server whose initialize()
        // failed before the manager could insert it for graceful shutdown.
        // `sleep 600` would outlive the test if Drop did not kill it.
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

        // After Drop, the PID must be reaped (no zombie, not running). On Unix
        // a reaped child's PID is freed; `kill -0` returning Err confirms it.
        let still_alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(!still_alive, "Drop must kill+reap the child (pid {pid})");
    }
}
