//! Shared mock server infrastructure for integration tests.
//!
//! Provides `MockServer` — an axum-based HTTP server bound to an
//! ephemeral port — and SSE formatting helpers.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Lease object for a reserved local TCP port.
///
/// Holding this lease keeps a listener bound so other tests/processes cannot
/// claim the port. Call [`release`](Self::release) right before starting the
/// server that should bind this port.
pub struct ReservedPort {
    listener: Option<TcpListener>,
    port: u16,
}

impl ReservedPort {
    /// Returns the leased port number.
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Releases the lease and returns the reserved port number.
    pub fn release(self) -> u16 {
        let port = self.port;
        drop(self);
        port
    }
}

fn fallback_port() -> u16 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0));
    let seed = now.subsec_nanos() ^ std::process::id();
    20_000 + (seed % 30_000) as u16
}

/// Reserves an ephemeral local port and returns a lease handle.
///
/// Retries transient bind failures to reduce test flakiness on busy runners.
pub async fn reserve_local_port() -> ReservedPort {
    const MAX_ATTEMPTS: usize = 10;
    const RETRY_DELAY_MS: u64 = 20;

    for attempt in 1..=MAX_ATTEMPTS {
        match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => {
                let port = listener
                    .local_addr()
                    .expect("leased listener should have a local addr")
                    .port();
                return ReservedPort {
                    listener: Some(listener),
                    port,
                };
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                ) && attempt < MAX_ATTEMPTS =>
            {
                tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                // Some CI/sandbox environments intermittently reject temporary
                // bind probes. Fall back to an unleased high port and let the
                // real server bind operation decide.
                return ReservedPort {
                    listener: None,
                    port: fallback_port(),
                };
            }
            Err(err) => {
                panic!("failed to reserve local test port after {attempt} attempt(s): {err}")
            }
        }
    }

    ReservedPort {
        listener: None,
        port: fallback_port(),
    }
}

/// Discovers a free ephemeral port by binding and releasing.
///
/// There is a small race window between releasing and the caller binding,
/// but it is negligible in test environments.
pub async fn find_free_port() -> u16 {
    reserve_local_port().await.release()
}
