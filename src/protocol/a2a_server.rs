//! A2A server-mode `PhaseDriver` implementation.
//!
//! `A2aServerDriver` runs a custom axum HTTP server implementing the A2A
//! protocol: Agent Card discovery (`GET /.well-known/agent.json`), JSON-RPC
//! dispatch (`POST /`), and SSE streaming for `message/stream`. Each incoming
//! request emits `ProtocolEvent`s for the `PhaseLoop` to process.
//!
//! This is a server-mode driver: it waits for client connections. Extractors
//! are borrowed fresh per request via `watch::Receiver::borrow().clone()`.
//!
//! See TJ-SPEC-017 for the full A2A protocol support specification.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use oatf::ResponseEntry;
use oatf::primitives::{interpolate_value, select_response};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::engine::driver::PhaseDriver;
use crate::engine::types::{Direction, DriveResult, ProtocolEvent};
use crate::error::EngineError;

// ============================================================================
// Constants
// ============================================================================

/// A2A error code: Task not found.
const TASK_NOT_FOUND: i64 = -32000;

/// A2A error code: Task not cancelable (already terminal).
const TASK_NOT_CANCELABLE: i64 = -32001;

/// A2A error code: Push notifications not supported.
const PUSH_NOT_SUPPORTED: i64 = -32002;

/// A2A error code: Unsupported operation.
const UNSUPPORTED_OPERATION: i64 = -32003;

/// JSON-RPC error code: Parse error.
const PARSE_ERROR: i64 = -32700;

/// JSON-RPC error code: Invalid request.
const INVALID_REQUEST: i64 = -32600;

/// JSON-RPC error code: Method not found.
const METHOD_NOT_FOUND: i64 = -32601;

/// JSON-RPC error code: Invalid params.
const INVALID_PARAMS: i64 = -32602;

/// Terminal task states per A2A protocol.
const TERMINAL_STATES: &[&str] = &["completed", "canceled", "failed", "rejected"];

/// Default inter-event delay for SSE streaming (ms).
const SSE_EVENT_DELAY_MS: u64 = 200;

/// Maximum accepted JSON-RPC body size for A2A server requests.
const MAX_JSONRPC_BODY_SIZE: usize = crate::transport::DEFAULT_MAX_MESSAGE_SIZE;

// ============================================================================
// TaskStore
// ============================================================================

/// A stored A2A task.
struct StoredTask {
    /// Task ID (server-generated UUID).
    id: String,
    /// Context ID grouping related tasks.
    context_id: String,
    /// Current task status string.
    status: String,
    /// Accumulated conversation history.
    history: Vec<Value>,
    /// Accumulated artifacts.
    artifacts: Vec<Value>,
    /// When the task was created.
    #[allow(dead_code)]
    created_at: Instant,
}

/// Per-actor task store for A2A server mode.
///
/// Tracks tasks by ID and groups them by context ID.
///
/// Implements: TJ-SPEC-017 F-005
struct TaskStore {
    /// Tasks keyed by task ID.
    tasks: HashMap<String, StoredTask>,
    /// Context ID → task IDs.
    contexts: HashMap<String, Vec<String>>,
}

impl TaskStore {
    /// Creates an empty task store.
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            contexts: HashMap::new(),
        }
    }

    /// Creates a new task and returns its ID and context ID.
    fn create_task(&mut self, context_id: Option<&str>) -> (String, String) {
        let task_id = uuid::Uuid::new_v4().to_string();
        let ctx_id = context_id.map_or_else(|| uuid::Uuid::new_v4().to_string(), String::from);

        let task = StoredTask {
            id: task_id.clone(),
            context_id: ctx_id.clone(),
            status: "submitted".to_string(),
            history: Vec::new(),
            artifacts: Vec::new(),
            created_at: Instant::now(),
        };

        self.tasks.insert(task_id.clone(), task);
        self.contexts
            .entry(ctx_id.clone())
            .or_default()
            .push(task_id.clone());

        (task_id, ctx_id)
    }

    /// Gets a task by ID.
    fn get_task(&self, id: &str) -> Option<&StoredTask> {
        self.tasks.get(id)
    }

    /// Gets a mutable reference to a task by ID.
    fn get_task_mut(&mut self, id: &str) -> Option<&mut StoredTask> {
        self.tasks.get_mut(id)
    }

    /// Cancels a task. Returns an error tuple `(code, message)` if not cancelable.
    fn cancel_task(&mut self, id: &str) -> Result<(), (i64, String)> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| (TASK_NOT_FOUND, format!("Task not found: {id}")))?;

        if is_terminal(&task.status) {
            return Err((
                TASK_NOT_CANCELABLE,
                format!("Task not cancelable: already in '{}' state", task.status),
            ));
        }

        task.status = "canceled".to_string();
        Ok(())
    }
}

/// Returns `true` if the given task status is terminal.
fn is_terminal(status: &str) -> bool {
    TERMINAL_STATES.contains(&status)
}

// ============================================================================
// A2aSharedState
// ============================================================================

/// Shared state between the axum handlers and the driver.
///
/// Updated by `drive_phase()` at the start of each phase; read by
/// axum handlers on each request.
///
/// Implements: TJ-SPEC-017 F-001
struct A2aSharedState {
    /// Current Agent Card (from `state.agent_card`).
    agent_card: RwLock<Value>,
    /// Per-actor task store.
    task_store: RwLock<TaskStore>,
    /// Event channel for emitting `ProtocolEvent`s from handlers.
    event_tx: RwLock<Option<mpsc::UnboundedSender<ProtocolEvent>>>,
    /// Extractor watch channel for fresh values per request.
    extractors: RwLock<Option<watch::Receiver<HashMap<String, String>>>>,
    /// Current phase effective state.
    state: RwLock<Value>,
    /// Whether handlers should accept requests for the current phase.
    ///
    /// Toggled to `false` during phase transitions to avoid serving
    /// stale state between `drive_phase()` calls.
    accepting_requests: AtomicBool,
    /// Bypass synthesize output validation.
    raw_synthesize: bool,
}

// ============================================================================
// Axum Handlers
// ============================================================================

/// `GET /.well-known/agent.json` — serve the Agent Card.
///
/// Implements: TJ-SPEC-017 F-002
async fn handle_agent_card(State(shared): State<Arc<A2aSharedState>>) -> Response {
    if !shared.accepting_requests.load(Ordering::Acquire) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "phase transition in progress",
        )
            .into_response();
    }

    let card = shared.agent_card.read().await.clone();

    // Emit events
    if let Some(tx) = shared.event_tx.read().await.as_ref() {
        let _ = tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: "agent_card/get".to_string(),
            content: json!({}),
        });
        let _ = tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "agent_card/get".to_string(),
            content: card.clone(),
        });
    }

    axum::Json(card).into_response()
}

/// `POST /` — JSON-RPC dispatch.
///
/// Implements: TJ-SPEC-017 F-001
async fn handle_jsonrpc(
    State(shared): State<Arc<A2aSharedState>>,
    body: axum::body::Bytes,
) -> Response {
    if !shared.accepting_requests.load(Ordering::Acquire) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "phase transition in progress",
        )
            .into_response();
    }

    // Parse JSON body
    let request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("A2A JSON parse error: {e}");
            return axum::Json(jsonrpc_error(&Value::Null, PARSE_ERROR, "Parse error"))
                .into_response();
        }
    };

    let (id, method, params) = match validate_jsonrpc_request(&request) {
        Ok(validated) => validated,
        Err(error) => return axum::Json(error).into_response(),
    };

    // Emit incoming event
    if let Some(tx) = shared.event_tx.read().await.as_ref() {
        let _ = tx.send(ProtocolEvent {
            direction: Direction::Incoming,
            method: method.clone(),
            content: params.clone(),
        });
    }

    // Route by method
    match method.as_str() {
        "message/send" => handle_message_send(&shared, &id, &params).await,
        "message/stream" => handle_message_stream(&shared, &id, &params).await,
        "tasks/get" => handle_tasks_get(&shared, &id, &params).await,
        "tasks/cancel" => handle_tasks_cancel(&shared, &id, &params).await,
        "tasks/resubscribe" => handle_tasks_resubscribe(&shared, &id, &params).await,
        "tasks/pushNotificationConfig/set"
        | "tasks/pushNotificationConfig/get"
        | "tasks/pushNotificationConfig/list"
        | "tasks/pushNotificationConfig/delete" => {
            handle_push_notification(&shared, &id, &method).await
        }
        "agent/authenticatedExtendedCard" => {
            let card = shared.agent_card.read().await.clone();
            let result = jsonrpc_success(&id, &card);
            emit_outgoing(&shared, &method, &card).await;
            axum::Json(result).into_response()
        }
        _ => {
            let error = jsonrpc_error(
                &id,
                METHOD_NOT_FOUND,
                &format!("Method not found: {method}"),
            );
            emit_outgoing(&shared, &method, &error).await;
            axum::Json(error).into_response()
        }
    }
}

#[allow(clippy::result_large_err)]
fn validate_local_peer(addr: SocketAddr) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "non-local peer rejected").into_response())
    }
}

#[allow(clippy::result_large_err)]
fn validate_local_origin(headers: &HeaderMap) -> Result<(), Response> {
    let header_value = headers
        .get("origin")
        .or_else(|| headers.get("host"))
        .and_then(|v| v.to_str().ok());

    let Some(header_value) = header_value else {
        return Err((StatusCode::FORBIDDEN, "missing Origin or Host header").into_response());
    };

    let Some(hostname) = extract_hostname_for_origin_check(header_value) else {
        return Err((StatusCode::FORBIDDEN, "dns rebinding rejected").into_response());
    };

    if !matches!(
        hostname.as_str(),
        "localhost" | "127.0.0.1" | "[::1]" | "::1" | "0.0.0.0"
    ) {
        return Err((StatusCode::FORBIDDEN, "dns rebinding rejected").into_response());
    }

    Ok(())
}

/// Router-level local-only guard for all A2A endpoints.
async fn require_local_only(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(resp) = validate_local_peer(addr) {
        return resp;
    }
    if let Err(resp) = validate_local_origin(request.headers()) {
        return resp;
    }
    next.run(request).await
}

fn extract_hostname_for_origin_check(header_value: &str) -> Option<String> {
    let authority = if header_value.contains("://") {
        header_value
            .parse::<Uri>()
            .ok()?
            .authority()?
            .as_str()
            .to_string()
    } else {
        header_value.to_string()
    };

    if authority == "::1" {
        return Some("::1".to_string());
    }

    if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped.find(']')?;
        return Some(format!("[{}]", &stripped[..end]).to_ascii_lowercase());
    }

    Some(
        authority
            .split(':')
            .next()
            .unwrap_or(authority.as_str())
            .to_ascii_lowercase(),
    )
}

/// Handle `message/send` — synchronous task response.
///
/// Implements: TJ-SPEC-017 F-003
async fn handle_message_send(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> Response {
    // Validate required params.message field
    if params.get("message").is_none() || params["message"].is_null() {
        let error = jsonrpc_error(
            request_id,
            INVALID_PARAMS,
            "Invalid params: missing required 'message' field",
        );
        emit_outgoing(
            shared,
            "message/send",
            error.get("error").unwrap_or(&Value::Null),
        )
        .await;
        return axum::Json(error).into_response();
    }

    let (result, method) = dispatch_task_response(shared, request_id, params).await;
    emit_outgoing(
        shared,
        &method,
        result.get("result").unwrap_or(&Value::Null),
    )
    .await;
    axum::Json(result).into_response()
}

/// Handle `message/stream` — SSE streaming task response.
///
/// Implements: TJ-SPEC-017 F-004
#[allow(clippy::too_many_lines)]
async fn handle_message_stream(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> Response {
    // Validate required params.message field
    if params.get("message").is_none() || params["message"].is_null() {
        let error = jsonrpc_error(
            request_id,
            INVALID_PARAMS,
            "Invalid params: missing required 'message' field",
        );
        emit_outgoing(
            shared,
            "message/stream",
            error.get("error").unwrap_or(&Value::Null),
        )
        .await;
        return axum::Json(error).into_response();
    }

    let state = shared.state.read().await.clone();
    let request_message = params.get("message").cloned().unwrap_or(Value::Null);

    // Get fresh extractors
    let current_extractors = get_extractors(shared).await;

    // Select response entry
    let (status, history_msgs, artifacts) = resolve_task_content(
        &state,
        &request_message,
        &current_extractors,
        shared.raw_synthesize,
    );

    // Create task in store
    let context_id_hint = request_message.get("contextId").and_then(Value::as_str);
    let (task_id, context_id) = shared.task_store.write().await.create_task(context_id_hint);

    let req_id = request_id.clone();

    // Emit SSE events for trace
    if let Some(tx) = shared.event_tx.read().await.as_ref() {
        let _ = tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: "message/stream".to_string(),
            content: json!({
                "taskId": task_id,
                "contextId": context_id,
                "status": status,
                "artifacts_count": artifacts.len(),
            }),
        });
    }

    let delay_ms = SSE_EVENT_DELAY_MS;
    let history_for_stream = Arc::new(history_msgs);
    let artifacts_for_stream = Arc::new(artifacts);
    let final_status = status.clone();
    let shared_for_stream = Arc::clone(shared);
    let task_id_for_stream = task_id.clone();
    let context_id_for_stream = context_id.clone();
    let req_id_for_stream = req_id.clone();
    let total_steps = artifacts_for_stream.len() + 3;

    let sse_stream = futures_util::stream::unfold(
        (
            shared_for_stream,
            task_id_for_stream,
            context_id_for_stream,
            req_id_for_stream,
            final_status,
            history_for_stream,
            artifacts_for_stream,
            0usize,
            delay_ms,
            total_steps,
        ),
        |(shared, task_id, context_id, req_id, final_status, history_msgs, artifacts, step, delay, total_steps)| async move {
            if step >= total_steps {
                return None;
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;

            let event = match step {
                0 => {
                    if let Some(task) = shared.task_store.write().await.get_task_mut(&task_id) {
                        task.status = "submitted".to_string();
                    }
                    let initial_task = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "kind": "task",
                            "id": task_id,
                            "contextId": context_id,
                            "status": { "state": "submitted" },
                            "history": history_msgs.as_ref(),
                        }
                    });
                    SseEvent::default().data(initial_task.to_string())
                }
                1 => {
                    if let Some(task) = shared.task_store.write().await.get_task_mut(&task_id) {
                        task.status = "working".to_string();
                    }
                    let working = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "kind": "status-update",
                            "taskId": task_id,
                            "contextId": context_id,
                            "status": { "state": "working" },
                            "final": false,
                        }
                    });
                    SseEvent::default().data(working.to_string())
                }
                final_step if final_step == total_steps - 1 => {
                    if let Some(task) = shared.task_store.write().await.get_task_mut(&task_id) {
                        task.status = final_status.clone();
                        task.history = history_msgs.as_ref().clone();
                        task.artifacts = artifacts.as_ref().clone();
                    }
                    let final_status = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "kind": "status-update",
                            "taskId": task_id,
                            "contextId": context_id,
                            "status": { "state": final_status },
                            "final": true,
                        }
                    });
                    SseEvent::default().data(final_status.to_string())
                }
                artifact_step => {
                    let artifact_index = artifact_step - 2;
                    let artifact = artifacts[artifact_index].clone();
                    if let Some(task) = shared.task_store.write().await.get_task_mut(&task_id) {
                        task.artifacts.push(artifact.clone());
                    }
                    let artifact_event = json!({
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "result": {
                            "kind": "artifact-update",
                            "taskId": task_id,
                            "contextId": context_id,
                            "artifact": artifact,
                        }
                    });
                    SseEvent::default().data(artifact_event.to_string())
                }
            };

            Some((
                Ok::<_, std::convert::Infallible>(event),
                (shared, task_id, context_id, req_id, final_status, history_msgs, artifacts, step + 1, delay, total_steps),
            ))
        },
    );

    Sse::new(sse_stream).into_response()
}

fn validate_jsonrpc_request(request: &Value) -> Result<(Value, String, Value), Value> {
    let Some(obj) = request.as_object() else {
        return Err(jsonrpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request: expected JSON-RPC object",
        ));
    };

    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(jsonrpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request: expected jsonrpc='2.0'",
        ));
    }

    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        return Err(jsonrpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request: missing 'method'",
        ));
    };

    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    if !matches!(id, Value::Null | Value::String(_) | Value::Number(_)) {
        return Err(jsonrpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request: 'id' must be string, number, or null",
        ));
    }

    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    if !params.is_null() && !params.is_object() && !params.is_array() {
        return Err(jsonrpc_error(
            &Value::Null,
            INVALID_REQUEST,
            "Invalid request: 'params' must be an object or array",
        ));
    }

    Ok((id, method.to_string(), params))
}

fn required_task_id(params: &Value) -> Result<&str, &'static str> {
    match params.get("id") {
        Some(Value::String(id)) if !id.trim().is_empty() => Ok(id.as_str()),
        Some(Value::String(_)) => Err("Invalid params: 'id' must be a non-empty string"),
        Some(_) => Err("Invalid params: 'id' must be a string"),
        None => Err("Invalid params: missing required 'id' field"),
    }
}

async fn invalid_params_response(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    method: &str,
    message: &str,
) -> Response {
    let result = jsonrpc_error(request_id, INVALID_PARAMS, message);
    emit_outgoing(shared, method, result.get("error").unwrap_or(&Value::Null)).await;
    axum::Json(result).into_response()
}

/// Handle `tasks/get` — return task status.
///
/// Implements: TJ-SPEC-017 F-005
async fn handle_tasks_get(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> Response {
    let task_id = match required_task_id(params) {
        Ok(id) => id,
        Err(msg) => return invalid_params_response(shared, request_id, "tasks/get", msg).await,
    };

    let result = {
        let store = shared.task_store.read().await;
        store.get_task(task_id).map_or_else(
            || {
                jsonrpc_error(
                    request_id,
                    TASK_NOT_FOUND,
                    &format!("Task not found: {task_id}"),
                )
            },
            |task| {
                let task_result = json!({
                    "kind": "task",
                    "id": task.id,
                    "contextId": task.context_id,
                    "status": { "state": task.status },
                    "history": task.history,
                    "artifacts": task.artifacts,
                });
                jsonrpc_success(request_id, &task_result)
            },
        )
    };

    emit_outgoing(
        shared,
        "tasks/get",
        result
            .get("result")
            .or_else(|| result.get("error"))
            .unwrap_or(&Value::Null),
    )
    .await;
    axum::Json(result).into_response()
}

/// Handle `tasks/cancel` — cancel a task.
///
/// Implements: TJ-SPEC-017 F-005
async fn handle_tasks_cancel(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> Response {
    let task_id = match required_task_id(params) {
        Ok(id) => id,
        Err(msg) => return invalid_params_response(shared, request_id, "tasks/cancel", msg).await,
    };

    let result = {
        let mut store = shared.task_store.write().await;
        match store.cancel_task(task_id) {
            Ok(()) => {
                let task = store.get_task(task_id).unwrap();
                let task_result = json!({
                    "kind": "task",
                    "id": task.id,
                    "contextId": task.context_id,
                    "status": { "state": task.status },
                });
                drop(store);
                jsonrpc_success(request_id, &task_result)
            }
            Err((code, msg)) => {
                drop(store);
                jsonrpc_error(request_id, code, &msg)
            }
        }
    };

    emit_outgoing(
        shared,
        "tasks/cancel",
        result
            .get("result")
            .or_else(|| result.get("error"))
            .unwrap_or(&Value::Null),
    )
    .await;
    axum::Json(result).into_response()
}

/// Handle `tasks/resubscribe` — resubscribe to task updates.
///
/// Returns error if the task is not found or already in a terminal state
/// (completed, canceled, failed, rejected).
///
/// Implements: TJ-SPEC-017 F-005
async fn handle_tasks_resubscribe(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> Response {
    let task_id = match required_task_id(params) {
        Ok(id) => id,
        Err(msg) => {
            return invalid_params_response(shared, request_id, "tasks/resubscribe", msg).await;
        }
    };

    let result = {
        let store = shared.task_store.read().await;
        store.get_task(task_id).map_or_else(
            || {
                jsonrpc_error(
                    request_id,
                    TASK_NOT_FOUND,
                    &format!("Task not found: {task_id}"),
                )
            },
            |task| {
                if is_terminal(&task.status) {
                    return jsonrpc_error(
                        request_id,
                        UNSUPPORTED_OPERATION,
                        &format!(
                            "Cannot resubscribe to task in terminal state: '{}'",
                            task.status
                        ),
                    );
                }
                let task_result = json!({
                    "kind": "task",
                    "id": task.id,
                    "contextId": task.context_id,
                    "status": { "state": task.status },
                    "history": task.history,
                    "artifacts": task.artifacts,
                });
                jsonrpc_success(request_id, &task_result)
            },
        )
    };

    emit_outgoing(
        shared,
        "tasks/resubscribe",
        result
            .get("result")
            .or_else(|| result.get("error"))
            .unwrap_or(&Value::Null),
    )
    .await;
    axum::Json(result).into_response()
}

/// Handle push notification config methods — acknowledged but no-op.
///
/// Implements: TJ-SPEC-017 EC-A2A-011
async fn handle_push_notification(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    method: &str,
) -> Response {
    let result = jsonrpc_error(
        request_id,
        PUSH_NOT_SUPPORTED,
        "Push notification not supported",
    );

    emit_outgoing(shared, method, result.get("error").unwrap_or(&Value::Null)).await;
    axum::Json(result).into_response()
}

// ============================================================================
// Response Dispatch Helpers
// ============================================================================

/// Dispatches a task response using `select_response()` and `interpolate_value()`.
///
/// Returns the complete JSON-RPC response and the method string for tracing.
///
/// Implements: TJ-SPEC-017 F-003
async fn dispatch_task_response(
    shared: &Arc<A2aSharedState>,
    request_id: &Value,
    params: &Value,
) -> (Value, String) {
    let state = shared.state.read().await.clone();
    let request_message = params.get("message").cloned().unwrap_or(Value::Null);

    // Get fresh extractors
    let current_extractors = get_extractors(shared).await;

    // Resolve response content
    let (status, history_msgs, artifacts) = resolve_task_content(
        &state,
        &request_message,
        &current_extractors,
        shared.raw_synthesize,
    );

    // Create task in store
    let context_id_hint = request_message.get("contextId").and_then(Value::as_str);
    let (task_id, context_id) = shared.task_store.write().await.create_task(context_id_hint);

    // Store task data
    {
        let mut store = shared.task_store.write().await;
        if let Some(task) = store.get_task_mut(&task_id) {
            task.status.clone_from(&status);
            task.history.clone_from(&history_msgs);
            task.artifacts.clone_from(&artifacts);
        }
    }

    // Check response_type
    let response_type = resolve_response_type(&state, &request_message, &current_extractors);

    let result = if response_type == "message" {
        // Direct Message response
        let agent_msg = history_msgs
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("agent"))
            .cloned()
            .unwrap_or_else(|| {
                json!({
                    "kind": "message",
                    "role": "agent",
                    "parts": [{"kind": "text", "text": ""}],
                    "messageId": uuid::Uuid::new_v4().to_string(),
                    "contextId": context_id,
                })
            });

        let mut msg = agent_msg;
        if msg.get("contextId").is_none() {
            msg["contextId"] = Value::String(context_id.clone());
        }
        if msg.get("kind").is_none() {
            msg["kind"] = Value::String("message".to_string());
        }

        jsonrpc_success(request_id, &msg)
    } else {
        // Task response (default)
        let task_result = json!({
            "kind": "task",
            "id": task_id,
            "contextId": context_id,
            "status": { "state": status },
            "history": history_msgs,
            "artifacts": artifacts,
        });
        jsonrpc_success(request_id, &task_result)
    };

    (result, "message/send".to_string())
}

/// Resolves task content from phase state using `select_response()`.
///
/// Returns `(status, history_messages, artifacts)`.
// Complexity: task content resolution with response matching, interpolation, and artifact assembly
#[allow(clippy::cognitive_complexity)]
fn resolve_task_content(
    state: &Value,
    request_message: &Value,
    extractors: &HashMap<String, String>,
    raw_synthesize: bool,
) -> (String, Vec<Value>, Vec<Value>) {
    let task_responses = state.get("task_responses");

    let Some(responses_value) = task_responses else {
        return ("completed".to_string(), Vec::new(), Vec::new());
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = %err, "failed to deserialize task_responses entries");
            return ("completed".to_string(), Vec::new(), Vec::new());
        }
    };

    let Some(entry) = select_response(&entries, request_message) else {
        return ("completed".to_string(), Vec::new(), Vec::new());
    };

    // Check for synthesize block
    if entry.synthesize.is_some() && entry.extra.is_empty() {
        tracing::info!("synthesize block encountered but GenerationProvider not available");
        return ("failed".to_string(), Vec::new(), Vec::new());
    }

    // Build response from extra fields with interpolation
    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, diagnostics) =
        interpolate_value(&extra_value, extractors, Some(request_message), None);

    for diag in &diagnostics {
        tracing::debug!(diagnostic = ?diag, "interpolation diagnostic");
    }

    // Validate if synthesize present
    if entry.synthesize.is_some()
        && !raw_synthesize
        && let Err(err) =
            crate::engine::generation::validate_synthesized_output("a2a", &interpolated, None)
    {
        tracing::warn!(error = %err, "synthesized output validation failed");
        return ("failed".to_string(), Vec::new(), Vec::new());
    }

    // Extract status
    let status = interpolated
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();

    // Build history messages
    let mut history: Vec<Value> = Vec::new();

    // Add the original user message to history
    if !request_message.is_null() {
        let mut user_msg = request_message.clone();
        if user_msg.get("kind").is_none() {
            user_msg["kind"] = Value::String("message".to_string());
        }
        history.push(user_msg);
    }

    // Add agent response messages from entry
    if let Some(msgs) = interpolated.get("messages").and_then(Value::as_array) {
        for msg in msgs {
            let mut agent_msg = msg.clone();
            agent_msg["kind"] = Value::String("message".to_string());
            if agent_msg.get("messageId").is_none() {
                agent_msg["messageId"] = Value::String(uuid::Uuid::new_v4().to_string());
            }
            history.push(agent_msg);
        }
    }

    // Extract artifacts
    let artifacts = interpolated
        .get("artifacts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut art| {
            if art.get("artifactId").is_none() {
                art["artifactId"] = Value::String(uuid::Uuid::new_v4().to_string());
            }
            art
        })
        .collect();

    (status, history, artifacts)
}

/// Resolves the `response_type` from matching entry.
fn resolve_response_type(
    state: &Value,
    request_message: &Value,
    extractors: &HashMap<String, String>,
) -> String {
    let Some(responses_value) = state.get("task_responses") else {
        return "task".to_string();
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(entries) => entries,
        Err(_) => return "task".to_string(),
    };

    let Some(entry) = select_response(&entries, request_message) else {
        return "task".to_string();
    };

    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, _) =
        interpolate_value(&extra_value, extractors, Some(request_message), None);

    interpolated
        .get("response_type")
        .and_then(Value::as_str)
        .unwrap_or("task")
        .to_string()
}

/// Gets current extractors from the shared state.
async fn get_extractors(shared: &Arc<A2aSharedState>) -> HashMap<String, String> {
    shared
        .extractors
        .read()
        .await
        .as_ref()
        .map(|rx| rx.borrow().clone())
        .unwrap_or_default()
}

/// Emits an outgoing `ProtocolEvent`.
async fn emit_outgoing(shared: &Arc<A2aSharedState>, method: &str, content: &Value) {
    if let Some(tx) = shared.event_tx.read().await.as_ref() {
        let _ = tx.send(ProtocolEvent {
            direction: Direction::Outgoing,
            method: method.to_string(),
            content: content.clone(),
        });
    }
}

// ============================================================================
// JSON-RPC Helpers
// ============================================================================

/// Builds a JSON-RPC 2.0 success response.
fn jsonrpc_success(id: &Value, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Builds a JSON-RPC 2.0 error response.
fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

// ============================================================================
// Axum Router
// ============================================================================

/// Builds the axum router for the A2A server.
///
/// Implements: TJ-SPEC-017 F-001
fn build_router(shared: Arc<A2aSharedState>) -> Router {
    let body_limit = axum::extract::DefaultBodyLimit::max(MAX_JSONRPC_BODY_SIZE);
    Router::new()
        .route("/.well-known/agent.json", get(handle_agent_card))
        .route("/", post(handle_jsonrpc))
        .layer(body_limit)
        .route_layer(middleware::from_fn(require_local_only))
        .with_state(shared)
}

// ============================================================================
// A2aServerDriver
// ============================================================================

/// A2A server-mode protocol driver.
///
/// Runs a custom axum HTTP server implementing the A2A protocol.
/// Agent Card and task response dispatch are driven by the current
/// phase's effective state. The server persists across phase transitions.
///
/// Implements: TJ-SPEC-017 F-001
pub struct A2aServerDriver {
    /// Bind address for the HTTP server.
    bind_addr: String,
    /// Bypass synthesize output validation.
    // Reserved for GenerationProvider integration (v0.6+)
    #[allow(dead_code)]
    raw_synthesize: bool,
    /// Shared state between driver and axum handlers.
    shared: Arc<A2aSharedState>,
    /// Server task handle.
    server_handle: Option<JoinHandle<()>>,
    /// Actual bound address (resolved after bind).
    bound_addr: Option<SocketAddr>,
    /// Optional readiness sender used by the orchestrator gate.
    ready_tx: Option<oneshot::Sender<()>>,
    /// Optional sender used by runner observability to emit `ActorReady` on bind.
    bound_addr_tx: Option<oneshot::Sender<SocketAddr>>,
    /// Cancel token for the HTTP server's lifetime (not per-phase).
    ///
    /// This is separate from the per-phase cancel token passed to
    /// `drive_phase()`. The HTTP server must persist across phase
    /// transitions and only shut down when the driver is dropped.
    server_cancel: CancellationToken,
}

#[async_trait]
impl PhaseDriver for A2aServerDriver {
    async fn drive_phase(
        &mut self,
        _phase_index: usize,
        state: &Value,
        extractors: watch::Receiver<HashMap<String, String>>,
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, EngineError> {
        self.shared
            .accepting_requests
            .store(false, Ordering::Release);

        // Update shared state with current phase
        let agent_card_raw = state.get("agent_card").cloned().unwrap_or(json!({}));
        let current_extractors = extractors.borrow().clone();
        let (agent_card, _) = interpolate_value(&agent_card_raw, &current_extractors, None, None);
        *self.shared.agent_card.write().await = agent_card;
        *self.shared.state.write().await = state.clone();
        *self.shared.event_tx.write().await = Some(event_tx);
        *self.shared.extractors.write().await = Some(extractors);

        // Start server on first call
        if self.server_handle.is_none() {
            let listener = TcpListener::bind(&self.bind_addr)
                .await
                .map_err(|e| EngineError::Driver(format!("A2A server bind failed: {e}")))?;

            let addr = listener
                .local_addr()
                .map_err(|e| EngineError::Driver(format!("failed to get local addr: {e}")))?;
            self.bound_addr = Some(addr);
            if let Some(tx) = self.bound_addr_tx.take() {
                let _ = tx.send(addr);
            }
            if let Some(tx) = self.ready_tx.take() {
                let _ = tx.send(());
            }

            tracing::info!(%addr, "A2A server listening");

            let router = build_router(Arc::clone(&self.shared));
            let server_cancel = self.server_cancel.clone();

            self.server_handle = Some(tokio::spawn(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(server_cancel.cancelled_owned())
                .await
                .ok();
            }));
        }

        self.shared
            .accepting_requests
            .store(true, Ordering::Release);

        // Server-mode: wait for cancellation
        cancel.cancelled().await;
        self.shared
            .accepting_requests
            .store(false, Ordering::Release);
        Ok(DriveResult::Complete)
    }

    async fn on_phase_advanced(&mut self, _from: usize, _to: usize) -> Result<(), EngineError> {
        // Agent card and state are updated at the start of the next drive_phase() call
        Ok(())
    }
}

// ============================================================================
// Public Constructor
// ============================================================================

/// Creates an `A2aServerDriver` for the given bind address and configuration.
///
/// Called by the orchestration runner when an actor's mode is `"a2a_server"`.
///
/// Implements: TJ-SPEC-017 F-001
#[must_use]
pub fn create_a2a_server_driver(bind_addr: &str, raw_synthesize: bool) -> A2aServerDriver {
    let shared = Arc::new(A2aSharedState {
        agent_card: RwLock::new(json!({})),
        task_store: RwLock::new(TaskStore::new()),
        event_tx: RwLock::new(None),
        extractors: RwLock::new(None),
        state: RwLock::new(json!({})),
        accepting_requests: AtomicBool::new(false),
        raw_synthesize,
    });

    A2aServerDriver {
        bind_addr: bind_addr.to_string(),
        raw_synthesize,
        shared,
        server_handle: None,
        bound_addr: None,
        ready_tx: None,
        bound_addr_tx: None,
        server_cancel: CancellationToken::new(),
    }
}

impl A2aServerDriver {
    /// Sets the readiness sender consumed after a successful bind.
    pub fn set_ready_sender(&mut self, tx: oneshot::Sender<()>) {
        self.ready_tx = Some(tx);
    }

    /// Sets the bound-address sender consumed after a successful bind.
    pub fn set_bound_addr_sender(&mut self, tx: oneshot::Sender<SocketAddr>) {
        self.bound_addr_tx = Some(tx);
    }
}

impl Drop for A2aServerDriver {
    fn drop(&mut self) {
        self.server_cancel.cancel();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::connect_info::MockConnectInfo;

    fn test_router(shared: Arc<A2aSharedState>) -> Router {
        build_router(shared).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 9999))))
    }

    fn local_request_builder(method: &str, uri: &str) -> axum::http::request::Builder {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost:3000")
    }

    // ---- TaskStore Tests ----

    #[test]
    fn create_and_get_task() {
        let mut store = TaskStore::new();
        let (task_id, ctx_id) = store.create_task(None);

        assert!(!task_id.is_empty());
        assert!(!ctx_id.is_empty());

        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.id, task_id);
        assert_eq!(task.context_id, ctx_id);
        assert_eq!(task.status, "submitted");
        assert!(task.history.is_empty());
        assert!(task.artifacts.is_empty());
    }

    #[test]
    fn create_task_with_context_id() {
        let mut store = TaskStore::new();
        let (task_id, ctx_id) = store.create_task(Some("my-ctx"));

        assert_eq!(ctx_id, "my-ctx");
        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.context_id, "my-ctx");
    }

    #[test]
    fn cancel_active_task() {
        let mut store = TaskStore::new();
        let (task_id, _) = store.create_task(None);

        assert!(store.cancel_task(&task_id).is_ok());
        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.status, "canceled");
    }

    #[test]
    fn cancel_completed_task_errors() {
        let mut store = TaskStore::new();
        let (task_id, _) = store.create_task(None);
        store.get_task_mut(&task_id).unwrap().status = "completed".to_string();

        let result = store.cancel_task(&task_id);
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, TASK_NOT_CANCELABLE);
    }

    #[test]
    fn cancel_nonexistent_task_errors() {
        let mut store = TaskStore::new();
        let result = store.cancel_task("nonexistent");
        assert!(result.is_err());
        let (code, _) = result.unwrap_err();
        assert_eq!(code, TASK_NOT_FOUND);
    }

    #[test]
    fn get_nonexistent_task() {
        let store = TaskStore::new();
        assert!(store.get_task("nonexistent").is_none());
    }

    #[test]
    fn context_id_tracking() {
        let mut store = TaskStore::new();
        let (task1, ctx1) = store.create_task(Some("ctx-shared"));
        let (task2, ctx2) = store.create_task(Some("ctx-shared"));

        assert_eq!(ctx1, "ctx-shared");
        assert_eq!(ctx2, "ctx-shared");

        let tasks_in_ctx = store.contexts.get("ctx-shared").unwrap();
        assert!(tasks_in_ctx.contains(&task1));
        assert!(tasks_in_ctx.contains(&task2));
        assert_eq!(tasks_in_ctx.len(), 2);
    }

    #[test]
    fn terminal_state_detection() {
        assert!(is_terminal("completed"));
        assert!(is_terminal("canceled"));
        assert!(is_terminal("failed"));
        assert!(is_terminal("rejected"));
        assert!(!is_terminal("submitted"));
        assert!(!is_terminal("working"));
        assert!(!is_terminal("input-required"));
        assert!(!is_terminal("auth-required"));
        assert!(!is_terminal("unknown"));
    }

    // ---- JSON-RPC Helper Tests ----

    #[test]
    fn jsonrpc_success_format() {
        let result = jsonrpc_success(&json!("req-1"), &json!({"kind": "task"}));
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], "req-1");
        assert_eq!(result["result"]["kind"], "task");
    }

    #[test]
    fn jsonrpc_error_format() {
        let result = jsonrpc_error(&json!("req-1"), METHOD_NOT_FOUND, "Method not found");
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], "req-1");
        assert_eq!(result["error"]["code"], -32601);
        assert_eq!(result["error"]["message"], "Method not found");
    }

    // ---- Response Dispatch Tests ----

    #[test]
    fn resolve_task_content_with_matching_response() {
        let state = json!({
            "task_responses": [
                {
                    "status": "completed",
                    "messages": [
                        {
                            "role": "agent",
                            "parts": [{"kind": "text", "text": "Done!"}]
                        }
                    ]
                }
            ]
        });
        let request_message = json!({
            "role": "user",
            "parts": [{"kind": "text", "text": "Do something"}]
        });

        let (status, history, artifacts) =
            resolve_task_content(&state, &request_message, &HashMap::new(), false);

        assert_eq!(status, "completed");
        // History should contain user message + agent response
        assert!(history.len() >= 2);
        assert!(artifacts.is_empty());
    }

    #[test]
    fn resolve_task_content_no_responses() {
        let state = json!({});
        let request_message = json!({"role": "user"});

        let (status, history, artifacts) =
            resolve_task_content(&state, &request_message, &HashMap::new(), false);

        assert_eq!(status, "completed");
        assert!(history.is_empty());
        assert!(artifacts.is_empty());
    }

    #[test]
    fn resolve_task_content_with_artifacts() {
        let state = json!({
            "task_responses": [
                {
                    "status": "completed",
                    "messages": [
                        {"role": "agent", "parts": [{"kind": "text", "text": "Here's the data"}]}
                    ],
                    "artifacts": [
                        {"parts": [{"kind": "text", "text": "artifact content"}]}
                    ]
                }
            ]
        });
        let request_message = json!({"role": "user", "parts": [{"kind": "text", "text": "test"}]});

        let (status, _history, artifacts) =
            resolve_task_content(&state, &request_message, &HashMap::new(), false);

        assert_eq!(status, "completed");
        assert_eq!(artifacts.len(), 1);
        // Each artifact should have an artifactId
        assert!(artifacts[0].get("artifactId").is_some());
    }

    #[test]
    fn resolve_response_type_defaults_to_task() {
        let state = json!({
            "task_responses": [
                {"status": "completed", "messages": []}
            ]
        });
        let rt = resolve_response_type(&state, &json!({}), &HashMap::new());
        assert_eq!(rt, "task");
    }

    #[test]
    fn resolve_response_type_message() {
        let state = json!({
            "task_responses": [
                {
                    "status": "completed",
                    "response_type": "message",
                    "messages": [{"role": "agent", "parts": []}]
                }
            ]
        });
        let rt = resolve_response_type(&state, &json!({}), &HashMap::new());
        assert_eq!(rt, "message");
    }

    #[test]
    fn response_dispatch_with_interpolation() {
        let state = json!({
            "task_responses": [
                {
                    "status": "completed",
                    "messages": [
                        {
                            "role": "agent",
                            "parts": [{"kind": "text", "text": "Hello {{name}}"}]
                        }
                    ]
                }
            ]
        });
        let mut extractors = HashMap::new();
        extractors.insert("name".to_string(), "World".to_string());

        let (_, history, _) =
            resolve_task_content(&state, &json!({"role": "user"}), &extractors, false);

        // Check that interpolation occurred in agent messages
        let agent_msg = history
            .iter()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("agent"));
        assert!(agent_msg.is_some());
        let text = agent_msg.unwrap()["parts"][0]["text"]
            .as_str()
            .unwrap_or("");
        assert_eq!(text, "Hello World");
    }

    // ---- Router Tests (requires tower::ServiceExt) ----

    #[tokio::test]
    async fn agent_card_endpoint() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({
                "name": "Test Agent",
                "skills": [{"id": "test", "name": "Test Skill"}]
            })),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let request = local_request_builder("GET", "/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let card: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(card["name"], "Test Agent");
    }

    #[tokio::test]
    async fn rejects_non_loopback_peer() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router =
            build_router(shared).layer(MockConnectInfo(SocketAddr::from(([10, 0, 0, 2], 9999))));

        let request = local_request_builder("GET", "/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_non_local_origin() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let request = local_request_builder("POST", "/")
            .header("origin", "https://evil.example")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"jsonrpc":"2.0","id":"1","method":"tasks/get","params":{}}"#,
            ))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_requests_during_phase_transition() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(false),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let request = local_request_builder("GET", "/.well-known/agent.json")
            .body(Body::empty())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn oversized_body_returns_413() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let oversized = vec![b'a'; MAX_JSONRPC_BODY_SIZE + 1];
        let request = local_request_builder("POST", "/")
            .header("content-type", "application/json")
            .body(Body::from(oversized))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn unknown_method_returns_error() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "custom/extension",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from("not valid json"))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], PARSE_ERROR);
    }

    #[tokio::test]
    async fn missing_method_returns_invalid_request() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({"jsonrpc": "2.0", "id": "1", "params": {}});

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_REQUEST);
    }

    #[tokio::test]
    async fn message_send_returns_task() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({
                "task_responses": [
                    {
                        "status": "completed",
                        "messages": [
                            {"role": "agent", "parts": [{"kind": "text", "text": "Done"}]}
                        ]
                    }
                ]
            })),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "message/send",
            "params": {
                "message": {
                    "role": "user",
                    "parts": [{"kind": "text", "text": "Hello"}],
                    "messageId": "msg-1",
                    "kind": "message"
                }
            }
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["result"]["kind"], "task");
        assert!(resp["result"]["id"].is_string());
        assert!(resp["result"]["contextId"].is_string());
        assert_eq!(resp["result"]["status"]["state"], "completed");
    }

    #[tokio::test]
    async fn push_notification_returns_not_supported() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/pushNotificationConfig/set",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], PUSH_NOT_SUPPORTED);
    }

    #[tokio::test]
    async fn message_send_missing_message_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "message/send",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn message_stream_missing_message_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "message/stream",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tasks_get_missing_id_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/get",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tasks_get_non_string_id_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/get",
            "params": {"id": 42}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tasks_cancel_missing_id_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/cancel",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tasks_resubscribe_missing_id_returns_invalid_params() {
        use axum::body::Body;
        use tower::ServiceExt;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(TaskStore::new()),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);
        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/resubscribe",
            "params": {}
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn resubscribe_terminal_task_returns_unsupported() {
        use axum::body::Body;
        use tower::ServiceExt;

        let task_store = {
            let mut store = TaskStore::new();
            let (task_id, _) = store.create_task(None);
            store.get_task_mut(&task_id).unwrap().status = "completed".to_string();
            (store, task_id)
        };
        let (store, task_id) = task_store;

        let shared = Arc::new(A2aSharedState {
            agent_card: RwLock::new(json!({})),
            task_store: RwLock::new(store),
            event_tx: RwLock::new(None),
            extractors: RwLock::new(None),
            state: RwLock::new(json!({})),
            accepting_requests: AtomicBool::new(true),
            raw_synthesize: false,
        });

        let router = test_router(shared);

        let body = json!({
            "jsonrpc": "2.0",
            "id": "req-1",
            "method": "tasks/resubscribe",
            "params": { "id": task_id }
        });

        let request = local_request_builder("POST", "/")
            .header("Content-Type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        let resp_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let resp: Value = serde_json::from_slice(&resp_body).unwrap();

        assert_eq!(resp["error"]["code"], UNSUPPORTED_OPERATION);
    }

    #[test]
    fn create_driver() {
        let driver = create_a2a_server_driver("127.0.0.1:9090", false);
        assert_eq!(driver.bind_addr, "127.0.0.1:9090");
        assert!(!driver.raw_synthesize);
        assert!(driver.server_handle.is_none());
        assert!(driver.bound_addr.is_none());
    }
}
