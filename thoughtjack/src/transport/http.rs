//! HTTP transport implementation (TJ-SPEC-002 F-003).
//!
//! Implements the [`Transport`] trait over HTTP using axum. Incoming JSON-RPC
//! requests arrive via `POST /message`, responses stream back as chunked HTTP
//! bodies, and server-initiated notifications/requests are broadcast via
//! Server-Sent Events on `GET /sse`.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use dashmap::DashMap;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::{ConnectionContext, JsonRpcMessage, Result, Transport, TransportType};
use crate::config::schema::DeliveryConfig;
use crate::error::TransportError;

/// Configuration for the HTTP transport.
///
/// Implements: TJ-SPEC-002 F-003
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Address to bind to, e.g. `"0.0.0.0:8080"`.
    pub bind_addr: String,
    /// Maximum allowed request body size in bytes.
    pub max_message_size: usize,
}

/// An incoming request received by the `POST /message` handler.
struct IncomingRequest {
    message: JsonRpcMessage,
    response_tx: mpsc::Sender<std::result::Result<Bytes, io::Error>>,
    connection_id: u64,
    remote_addr: SocketAddr,
    connected_at: Instant,
}

/// Per-connection state tracked while a request is in flight.
pub struct ConnectionState {
    /// Remote address of the client.
    pub remote_addr: SocketAddr,
    /// When this connection was established.
    pub connected_at: Instant,
    /// Running count of requests on this connection.
    pub request_count: AtomicU64,
}

/// Shared state between the axum handlers and `HttpTransport`.
struct HttpSharedState {
    incoming_tx: mpsc::Sender<IncomingRequest>,
    sse_tx: broadcast::Sender<String>,
    connections: Arc<DashMap<u64, ConnectionState>>,
    next_connection_id: AtomicU64,
    max_message_size: usize,
    cancel: CancellationToken,
}

/// RAII guard that removes a connection from the `DashMap` on drop.
///
/// Ensures connection tracking is cleaned up on all exit paths
/// (success, error, panic) in the HTTP handler pipeline.
///
/// Implements: TJ-SPEC-002 F-003
struct ConnectionGuard {
    connections: Arc<DashMap<u64, ConnectionState>>,
    connection_id: u64,
}

impl ConnectionGuard {
    const fn new(connections: Arc<DashMap<u64, ConnectionState>>, connection_id: u64) -> Self {
        Self {
            connections,
            connection_id,
        }
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.connections.remove(&self.connection_id);
    }
}

/// HTTP transport implementing the [`Transport`] trait via a channel bridge.
///
/// Axum handlers push requests into an internal channel; [`Transport::receive_message`]
/// reads from the channel. Responses flow back through per-request response channels
/// that drive chunked HTTP response bodies.
///
/// Implements: TJ-SPEC-002 F-003
pub struct HttpTransport {
    shared: Arc<HttpSharedState>,
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<IncomingRequest>>,
    current_response:
        tokio::sync::Mutex<Option<mpsc::Sender<std::result::Result<Bytes, io::Error>>>>,
    // std::sync::Mutex is intentional: held briefly for field access, never across .await points.
    // Per tokio docs, std::sync::Mutex is preferred when the critical section is short and synchronous.
    current_context: std::sync::Mutex<ConnectionContext>,
    /// RAII guard that cleans up connection tracking on drop.
    // std::sync::Mutex: same rationale as current_context — brief, synchronous access only.
    current_guard: std::sync::Mutex<Option<ConnectionGuard>>,
    _server_handle: JoinHandle<()>,
    // TODO(v0.2): add per-connection rate limiting
    // TODO(v0.2): add configurable request timeout — without one, slow clients
    // hold response channels indefinitely; ~32 concurrent slow requests can DoS
    // the transport by filling the incoming channel (P1 issue #11)
}

impl HttpTransport {
    /// Binds the HTTP transport to the configured address.
    ///
    /// Returns the transport and the actual bound address (useful when binding
    /// to port 0 in tests).
    ///
    /// # Errors
    ///
    /// Returns a [`TransportError`] if the TCP listener cannot bind.
    ///
    /// Implements: TJ-SPEC-002 F-003
    pub async fn bind(config: HttpConfig, cancel: CancellationToken) -> Result<(Self, SocketAddr)> {
        let (incoming_tx, incoming_rx) = mpsc::channel::<IncomingRequest>(32);
        let (sse_tx, _) = broadcast::channel::<String>(256);

        let listener = TcpListener::bind(&config.bind_addr)
            .await
            .map_err(|e| TransportError::ConnectionFailed(format!("bind failed: {e}")))?;

        let bound_addr = listener
            .local_addr()
            .map_err(|e| TransportError::ConnectionFailed(format!("local_addr failed: {e}")))?;

        let shared = Arc::new(HttpSharedState {
            incoming_tx,
            sse_tx,
            connections: Arc::new(DashMap::new()),
            next_connection_id: AtomicU64::new(1),
            max_message_size: config.max_message_size,
            cancel: cancel.clone(),
        });

        let router = build_router(Arc::clone(&shared));
        let service = router.into_make_service_with_connect_info::<SocketAddr>();

        let server_cancel = cancel.clone();
        let server_handle = tokio::spawn(async move {
            info!(%bound_addr, "HTTP transport started");
            axum::serve(listener, service)
                .with_graceful_shutdown(async move {
                    server_cancel.cancelled().await;
                })
                .await
                .ok();
            debug!("HTTP transport shut down");
        });

        let transport = Self {
            shared,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            current_response: tokio::sync::Mutex::new(None),
            current_context: std::sync::Mutex::new(ConnectionContext::stdio()),
            current_guard: std::sync::Mutex::new(None),
            _server_handle: server_handle,
        };

        Ok((transport, bound_addr))
    }

    /// Gracefully shuts down the HTTP transport.
    pub fn shutdown(&self) {
        self.shared.cancel.cancel();
    }
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("connections", &self.shared.connections.len())
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Transport for HttpTransport {
    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>> {
        let mut rx = self.incoming_rx.lock().await;
        let incoming = rx.recv().await;
        drop(rx);

        let Some(req) = incoming else {
            return Ok(None);
        };

        // Store the response sender for subsequent send_message/send_raw calls
        {
            let mut guard = self.current_response.lock().await;
            *guard = Some(req.response_tx);
        }

        // Track connection here (not in the handler) to avoid a race where the
        // handler inserts a connection and the ConnectionGuard from a previous
        // request removes the wrong entry.
        self.shared.connections.insert(
            req.connection_id,
            ConnectionState {
                remote_addr: req.remote_addr,
                connected_at: req.connected_at,
                request_count: AtomicU64::new(1),
            },
        );

        // Update connection context using the timestamp captured in the handler
        // Poisoned mutex means a thread panicked while holding the lock — data is
        // corrupt, so panicking here is the correct response.
        {
            let mut ctx = self.current_context.lock().expect("context mutex poisoned");
            *ctx = ConnectionContext {
                connection_id: req.connection_id,
                remote_addr: Some(req.remote_addr),
                is_exclusive: false,
                connected_at: req.connected_at,
            };
        }

        // Create RAII guard for connection cleanup (replaces any previous guard)
        {
            let mut guard = self.current_guard.lock().expect("guard mutex poisoned");
            *guard = Some(ConnectionGuard::new(
                Arc::clone(&self.shared.connections),
                req.connection_id,
            ));
        }

        Ok(Some(req.message))
    }

    async fn send_message(&self, message: &JsonRpcMessage) -> Result<()> {
        match message {
            JsonRpcMessage::Response(_) => {
                // Responses go to the per-request response channel (HTTP body)
                let tx = {
                    let guard = self.current_response.lock().await;
                    guard.as_ref().cloned()
                };
                let Some(tx) = tx else {
                    return Err(TransportError::ConnectionClosed(
                        "no active response channel (send_message called before receive_message)"
                            .into(),
                    ));
                };
                let serialized = serde_json::to_vec(message)?;
                tx.send(Ok(Bytes::from(serialized))).await.map_err(|_| {
                    TransportError::ConnectionClosed("response channel closed".into())
                })?;
            }
            JsonRpcMessage::Notification(_) | JsonRpcMessage::Request(_) => {
                // Server-initiated notifications/requests go to SSE broadcast
                let serialized = serde_json::to_string(message)?;
                // Ignore send errors — no subscribers is fine
                let _ = self.shared.sse_tx.send(serialized);
            }
        }
        Ok(())
    }

    async fn send_raw(&self, bytes: &[u8]) -> Result<()> {
        let tx = {
            let guard = self.current_response.lock().await;
            guard.as_ref().cloned()
        };
        let Some(tx) = tx else {
            return Err(TransportError::ConnectionClosed(
                "no active response channel (send_raw called before receive_message)".into(),
            ));
        };
        tx.send(Ok(Bytes::copy_from_slice(bytes)))
            .await
            .map_err(|_| TransportError::ConnectionClosed("response channel closed".into()))?;
        Ok(())
    }

    fn supports_behavior(&self, _behavior: &DeliveryConfig) -> bool {
        true
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Http
    }

    async fn finalize_response(&self) -> Result<()> {
        // Take the sender — dropping it closes the stream and completes
        // the HTTP chunked response.
        let sender = {
            let mut guard = self.current_response.lock().await;
            guard.take()
        };
        drop(sender);

        // Drop the RAII guard — removes connection from tracking
        let guard = {
            let mut g = self.current_guard.lock().expect("guard mutex poisoned");
            g.take()
        };
        drop(guard);

        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        self.current_context
            .lock()
            .expect("context mutex poisoned")
            .clone()
    }
}

// ============================================================================
// Axum Router
// ============================================================================

/// Builds the axum router with `POST /message` and `GET /sse` routes.
fn build_router(shared: Arc<HttpSharedState>) -> Router {
    // Override axum's default 2MB body limit with the configured max_message_size
    // (default 10MB). Without this, requests between 2MB and max_message_size
    // are rejected by axum before reaching the handler's size check.
    let body_limit = axum::extract::DefaultBodyLimit::max(shared.max_message_size);

    Router::new()
        .route("/message", post(handle_post_message))
        .route("/sse", get(handle_sse))
        .layer(body_limit)
        .with_state(shared)
}

/// `POST /message` handler.
///
/// Parses the request body as a JSON-RPC message, pushes it into the incoming
/// channel, and returns a streaming response body that the transport fills in
/// when `send_message` / `send_raw` is called.
async fn handle_post_message(
    State(shared): State<Arc<HttpSharedState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    body: axum::body::Bytes,
) -> Response {
    // EC-TRANS-006: empty body
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty request body").into_response();
    }

    // F-008: message size limit
    if body.len() > shared.max_message_size {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "message too large: {} bytes (limit: {})",
                body.len(),
                shared.max_message_size
            ),
        )
            .into_response();
    }

    // Parse JSON-RPC
    let message: JsonRpcMessage = match serde_json::from_slice(&body) {
        Ok(msg) => msg,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid JSON-RPC: {e}")).into_response();
        }
    };

    let connection_id = shared.next_connection_id.fetch_add(1, Ordering::SeqCst);
    let connected_at = Instant::now();

    // Connection tracking is deferred to receive_message to avoid a race where
    // the handler inserts and the ConnectionGuard from the previous request
    // removes the wrong entry.

    // Create response body channel
    let (response_tx, response_rx) = mpsc::channel::<std::result::Result<Bytes, io::Error>>(64);

    let incoming = IncomingRequest {
        message,
        response_tx,
        connection_id,
        remote_addr: addr,
        connected_at,
    };

    // Push into the transport channel
    if shared.incoming_tx.send(incoming).await.is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "server shutting down").into_response();
    }

    // Return streaming response body
    let stream = ReceiverStream::new(response_rx);
    let body = Body::from_stream(stream);

    Response::builder()
        .header("content-type", "application/json")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `GET /sse` handler.
///
/// Returns a Server-Sent Events stream that broadcasts all server-initiated
/// notifications and requests.
async fn handle_sse(
    State(shared): State<Arc<HttpSharedState>>,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<SseEvent, std::convert::Infallible>>>
{
    let rx = shared.sse_tx.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(
        |result: std::result::Result<String, _>| {
            result.ok().map(|data| Ok(SseEvent::default().data(data)))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ============================================================================
// Helpers
// ============================================================================

/// Parses a bind address string into a full `host:port` form.
///
/// Accepts:
/// - `:8080` → `0.0.0.0:8080`
/// - `8080` → `0.0.0.0:8080`
/// - `1.2.3.4:8080` → as-is
///
/// # Errors
///
/// Returns [`TransportError::ConnectionFailed`] if the result cannot be
/// parsed as a valid socket address.
///
/// Implements: TJ-SPEC-002 F-003
pub fn parse_bind_addr(input: &str) -> std::result::Result<String, TransportError> {
    let addr = if input.starts_with(':') {
        format!("0.0.0.0{input}")
    } else if input.parse::<u16>().is_ok() {
        format!("0.0.0.0:{input}")
    } else {
        input.to_string()
    };
    // Validate it can be parsed as a socket address
    addr.parse::<SocketAddr>().map_err(|e| {
        TransportError::ConnectionFailed(format!("invalid bind address \"{input}\": {e}"))
    })?;
    Ok(addr)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::DEFAULT_MAX_MESSAGE_SIZE;
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn test_shared_state() -> Arc<HttpSharedState> {
        let (incoming_tx, _incoming_rx) = mpsc::channel(32);
        let (sse_tx, _) = broadcast::channel(256);
        Arc::new(HttpSharedState {
            incoming_tx,
            sse_tx,
            connections: Arc::new(DashMap::new()),
            next_connection_id: AtomicU64::new(1),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            cancel: CancellationToken::new(),
        })
    }

    use axum::extract::connect_info::MockConnectInfo;

    /// Builds a test router with `ConnectInfo` support.
    fn test_router(shared: Arc<HttpSharedState>) -> Router {
        build_router(shared).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9999))))
    }

    // ------------------------------------------------------------------
    // parse_bind_addr
    // ------------------------------------------------------------------

    #[test]
    fn parse_bind_addr_colon_port() {
        assert_eq!(parse_bind_addr(":8080").unwrap(), "0.0.0.0:8080");
    }

    #[test]
    fn parse_bind_addr_port_only() {
        assert_eq!(parse_bind_addr("8080").unwrap(), "0.0.0.0:8080");
    }

    #[test]
    fn parse_bind_addr_full() {
        assert_eq!(parse_bind_addr("1.2.3.4:8080").unwrap(), "1.2.3.4:8080");
    }

    #[test]
    fn parse_bind_addr_localhost() {
        assert_eq!(parse_bind_addr("127.0.0.1:3000").unwrap(), "127.0.0.1:3000");
    }

    #[test]
    fn parse_bind_addr_invalid() {
        assert!(parse_bind_addr("not-an-address").is_err());
    }

    // ------------------------------------------------------------------
    // POST /message error cases
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn post_empty_body_returns_400() {
        let shared = test_shared_state();
        let app = test_router(shared);

        let req = Request::builder()
            .method("POST")
            .uri("/message")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_invalid_json_returns_400() {
        let shared = test_shared_state();
        let app = test_router(shared);

        let req = Request::builder()
            .method("POST")
            .uri("/message")
            .header("content-type", "application/json")
            .body(Body::from("not json"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_oversized_body_returns_413() {
        let (incoming_tx, _rx) = mpsc::channel(32);
        let (sse_tx, _) = broadcast::channel(256);
        let shared = Arc::new(HttpSharedState {
            incoming_tx,
            sse_tx,
            connections: Arc::new(DashMap::new()),
            next_connection_id: AtomicU64::new(1),
            max_message_size: 10, // tiny limit
            cancel: CancellationToken::new(),
        });
        let app = test_router(shared);

        let body = r#"{"jsonrpc":"2.0","method":"test","id":1}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/message")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn post_valid_message_returns_200() {
        let (incoming_tx, mut incoming_rx) = mpsc::channel(32);
        let (sse_tx, _) = broadcast::channel(256);
        let shared = Arc::new(HttpSharedState {
            incoming_tx,
            sse_tx,
            connections: Arc::new(DashMap::new()),
            next_connection_id: AtomicU64::new(1),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            cancel: CancellationToken::new(),
        });
        let app = test_router(shared);

        let body = r#"{"jsonrpc":"2.0","method":"initialize","params":{},"id":1}"#;
        let req = Request::builder()
            .method("POST")
            .uri("/message")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();

        // Spawn a consumer that sends a response via the channel
        tokio::spawn(async move {
            if let Some(incoming) = incoming_rx.recv().await {
                let response = Bytes::from(r#"{"jsonrpc":"2.0","result":{},"id":1}"#);
                incoming.response_tx.send(Ok(response)).await.ok();
                // Drop sender to close the stream
            }
        });

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ------------------------------------------------------------------
    // GET /sse
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn sse_endpoint_returns_200() {
        let shared = test_shared_state();
        let app = test_router(shared);

        let req = Request::builder()
            .method("GET")
            .uri("/sse")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    // ------------------------------------------------------------------
    // Connection tracking
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn connection_tracking() {
        let cancel = CancellationToken::new();
        let config = HttpConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (transport, _addr) = HttpTransport::bind(config, cancel.clone()).await.unwrap();

        // Initially no connections
        assert_eq!(transport.shared.connections.len(), 0);

        // After finalize, connection should be cleaned up
        transport.finalize_response().await.unwrap();
        assert_eq!(transport.shared.connections.len(), 0);

        transport.shutdown();
    }

    // ------------------------------------------------------------------
    // Transport trait basics
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn transport_type_is_http() {
        let cancel = CancellationToken::new();
        let config = HttpConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (transport, _addr) = HttpTransport::bind(config, cancel.clone()).await.unwrap();
        assert_eq!(transport.transport_type(), TransportType::Http);
        transport.shutdown();
    }

    #[tokio::test]
    async fn supports_all_behaviors() {
        let cancel = CancellationToken::new();
        let config = HttpConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (transport, _addr) = HttpTransport::bind(config, cancel.clone()).await.unwrap();
        assert!(transport.supports_behavior(&DeliveryConfig::Normal));
        assert!(transport.supports_behavior(&DeliveryConfig::SlowLoris {
            byte_delay_ms: Some(100),
            chunk_size: Some(1),
        }));
        transport.shutdown();
    }

    #[tokio::test]
    async fn debug_format() {
        let cancel = CancellationToken::new();
        let config = HttpConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (transport, _addr) = HttpTransport::bind(config, cancel.clone()).await.unwrap();
        let debug = format!("{transport:?}");
        assert!(debug.contains("HttpTransport"));
        transport.shutdown();
    }

    #[tokio::test]
    async fn default_connection_context_is_stdio() {
        let cancel = CancellationToken::new();
        let config = HttpConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (transport, _addr) = HttpTransport::bind(config, cancel.clone()).await.unwrap();
        let ctx = transport.connection_context();
        // Default context before any request is stdio-like (connection_id 0)
        assert_eq!(ctx.connection_id, 0);
        transport.shutdown();
    }
}
