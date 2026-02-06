//! Shared integration-test harness for spawning a `ThoughtJack` server as a
//! child process and communicating over stdio JSON-RPC.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

/// Default timeout for reading a single message from the server.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// Extended timeout for slow-delivery tests.
pub const SLOW_TIMEOUT: Duration = Duration::from_secs(30);

/// A running `ThoughtJack` server process with helpers for JSON-RPC I/O.
///
/// The child process is killed on drop via `kill_on_drop(true)`.
#[allow(clippy::missing_panics_doc)]
pub struct ThoughtJackProcess {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    next_id: i64,
    pending_notifications: Vec<Value>,
}

impl ThoughtJackProcess {
    /// Spawns a new server with the given YAML config.
    #[allow(clippy::missing_panics_doc)]
    pub fn spawn(config_path: &Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_thoughtjack");
        let mut child = Command::new(bin)
            .args([
                "server",
                "run",
                "--config",
                config_path.to_str().expect("non-UTF-8 config path"),
                "--quiet",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("failed to spawn thoughtjack");

        let stdin = child.stdin.take().expect("stdin not captured");
        let stdout = child.stdout.take().expect("stdout not captured");

        Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 1,
            pending_notifications: Vec::new(),
        }
    }

    /// Reads one NDJSON message from the server's stdout.
    ///
    /// Panics on EOF, I/O error, or if no message arrives within `timeout`.
    #[allow(clippy::missing_panics_doc)]
    pub async fn read_message(&mut self, timeout: Duration) -> Value {
        let mut line = String::new();
        let result = tokio::time::timeout(timeout, async {
            loop {
                line.clear();
                let n = self
                    .reader
                    .read_line(&mut line)
                    .await
                    .expect("read_line I/O error");
                assert!(n > 0, "unexpected EOF from server");
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    return serde_json::from_str::<Value>(trimmed)
                        .unwrap_or_else(|e| panic!("invalid JSON from server: {e}\nline: {line}"));
                }
            }
        })
        .await;
        result.expect("timed out waiting for message from server")
    }

    /// Sends a JSON-RPC request and returns the matching response.
    ///
    /// Any notifications received while waiting are buffered in
    /// `pending_notifications`.
    #[allow(clippy::missing_panics_doc)]
    pub async fn send_request(&mut self, method: &str, params: Option<Value>) -> Value {
        self.send_request_timeout(method, params, DEFAULT_TIMEOUT)
            .await
    }

    /// Like [`send_request`](Self::send_request) but with a custom timeout.
    #[allow(clippy::missing_panics_doc)]
    pub async fn send_request_timeout(
        &mut self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let mut buf = serde_json::to_string(&request).expect("failed to serialize request");
        buf.push('\n');
        self.stdin
            .write_all(buf.as_bytes())
            .await
            .expect("failed to write to stdin");
        self.stdin.flush().await.expect("failed to flush stdin");

        // Read messages until we find the response matching our id
        loop {
            let msg = self.read_message(timeout).await;

            // Check if this is the response to our request
            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                return msg;
            }

            // Otherwise it's a notification — buffer it
            self.pending_notifications.push(msg);
        }
    }

    /// Sends the MCP `initialize` handshake and returns the response.
    #[allow(clippy::missing_panics_doc)]
    pub async fn send_initialize(&mut self) -> Value {
        self.send_request(
            "initialize",
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "integration-test", "version": "0.0.1" }
            })),
        )
        .await
    }

    /// Waits for a notification with the given method.
    ///
    /// Checks the internal buffer first, then reads new messages.
    #[allow(clippy::missing_panics_doc)]
    pub async fn expect_notification(&mut self, expected_method: &str) -> Value {
        // Check buffered notifications first
        if let Some(idx) = self
            .pending_notifications
            .iter()
            .position(|n| n.get("method").and_then(Value::as_str) == Some(expected_method))
        {
            return self.pending_notifications.remove(idx);
        }

        // Read new messages until we find the notification
        loop {
            let msg = self.read_message(DEFAULT_TIMEOUT).await;
            if msg.get("method").and_then(Value::as_str) == Some(expected_method) {
                return msg;
            }
            self.pending_notifications.push(msg);
        }
    }

    /// Shuts down the server by closing stdin and waiting for exit.
    #[allow(clippy::missing_panics_doc)]
    pub async fn shutdown(self) {
        let Self {
            mut child,
            stdin,
            reader: _,
            ..
        } = self;

        // Drop stdin to signal EOF
        drop(stdin);

        // Wait up to 5 seconds for clean exit
        let wait_result = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;

        if wait_result.is_err() {
            // Timed out — kill the process
            child.kill().await.expect("failed to kill child");
        }
    }

    /// Returns the path to a test fixture.
    #[must_use]
    pub fn fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }
}
