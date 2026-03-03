//! Shared mock server infrastructure for integration tests.
//!
//! Provides `MockServer` — an axum-based HTTP server bound to an
//! ephemeral port — and SSE formatting helpers.

#![allow(dead_code)]

use std::net::SocketAddr;

use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// A test HTTP server wrapping an axum `Router` on an ephemeral port.
pub struct MockServer {
    addr: SocketAddr,
    handle: JoinHandle<()>,
}

impl MockServer {
    /// Starts an axum server on `127.0.0.1:0` (OS-assigned port).
    pub async fn start(router: Router) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        Self { addr, handle }
    }

    /// Returns the base URL, e.g. `http://127.0.0.1:12345`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }

    /// Returns the bound socket address.
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Formats an SSE event with an explicit `event:` type line.
pub fn sse_event(event_type: &str, data: &serde_json::Value) -> String {
    format!("event: {event_type}\ndata: {data}\n\n")
}

/// Formats a data-only SSE event (canonical AG-UI style, no `event:` line).
/// The event type is carried inside the JSON `type` field.
pub fn sse_data_line(data: &serde_json::Value) -> String {
    format!("data: {data}\n\n")
}

/// Discovers a free ephemeral port by binding and releasing.
///
/// There is a small race window between releasing and the caller binding,
/// but it is negligible in test environments.
pub async fn find_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}
