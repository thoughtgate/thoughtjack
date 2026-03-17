use std::sync::Arc;
use std::time::Duration;

use oatf::primitives::parse_duration;
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::error::EngineError;
use crate::transport::{JsonRpcMessage, JsonRpcNotification, JsonRpcResponse, Transport};

use super::helpers::u64_to_usize;

/// Apply delivery behavior to a response.
///
/// Reads `state["behavior"]["delivery"]` and applies the configured
/// delivery mode: `normal`, `delayed`, `slow_stream`, or `unbounded`.
/// Parameters are read from `state["behavior"]["parameters"]` per §5.1.
///
/// For `slow_stream`, the byte-drip loop is spawned into a background
/// task and the handle is returned so the caller can continue receiving
/// messages while delivery is in progress.  All other modes send
/// synchronously and return `None`.
///
/// # Errors
///
/// Returns `EngineError::Driver` on transport failure.
pub(super) async fn apply_delivery(
    transport: &Arc<dyn Transport>,
    state: &Value,
    response_msg: &JsonRpcMessage,
) -> Result<Option<JoinHandle<Result<(), EngineError>>>, EngineError> {
    let delivery = state
        .get("behavior")
        .and_then(|b| b.get("delivery"))
        .and_then(Value::as_str)
        .unwrap_or("normal");

    let params = state.get("behavior").and_then(|b| b.get("parameters"));

    match delivery {
        "delayed" => {
            let delay_ms = params
                .and_then(|p| p.get("delay_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(1000);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            transport
                .send_message(response_msg)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
            Ok(None)
        }
        "slow_stream" => {
            let bytes = serde_json::to_vec(response_msg)
                .map_err(|e| EngineError::Driver(format!("serialize error: {e}")))?;
            let byte_delay_ms = params
                .and_then(|p| p.get("byte_delay_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(50);
            // Capture the per-request response channel before spawning so that
            // interleaved receive_message() calls (which overwrite the shared
            // current_response slot) don't redirect our bytes to a different client.
            let captured_writer = transport
                .capture_raw_writer()
                .await
                .map_err(|e| EngineError::Driver(format!("capture writer: {e}")))?;
            let transport = Arc::clone(transport);
            Ok(Some(tokio::spawn(async move {
                for byte in &bytes {
                    if let Some(ref writer) = captured_writer {
                        writer
                            .send_raw(&[*byte])
                            .await
                            .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
                    } else {
                        transport
                            .send_raw(&[*byte])
                            .await
                            .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
                    }
                    tokio::time::sleep(Duration::from_millis(byte_delay_ms)).await;
                }
                // Send newline terminator for NDJSON framing
                if let Some(ref writer) = captured_writer {
                    writer
                        .send_raw(b"\n")
                        .await
                        .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
                } else {
                    transport
                        .send_raw(b"\n")
                        .await
                        .map_err(|e| EngineError::Driver(format!("send_raw error: {e}")))?;
                }
                Ok(())
            })))
        }
        "unbounded" => {
            let max_line_length = u64_to_usize(
                params
                    .and_then(|p| p.get("max_line_length"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1_000_000),
            );
            let nesting_depth = u64_to_usize(
                params
                    .and_then(|p| p.get("nesting_depth"))
                    .and_then(Value::as_u64)
                    .unwrap_or(100),
            );
            let inflated = inflate_response(response_msg, max_line_length, nesting_depth);
            transport
                .send_message(&inflated)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
            Ok(None)
        }
        _ => {
            if delivery != "normal" {
                tracing::warn!(delivery, "unknown delivery behavior, using normal");
            }
            transport
                .send_message(response_msg)
                .await
                .map_err(|e| EngineError::Driver(format!("send error: {e}")))?;
            Ok(None)
        }
    }
}

/// Inflate a response message to create an oversized payload.
///
/// Pads text content items to reach `max_line_length` and wraps the
/// result in nested `{"wrapper": ...}` objects up to `nesting_depth`.
pub(super) fn inflate_response(
    msg: &JsonRpcMessage,
    max_line_length: usize,
    nesting_depth: usize,
) -> JsonRpcMessage {
    let JsonRpcMessage::Response(resp) = msg else {
        return msg.clone();
    };

    let Some(result) = &resp.result else {
        return msg.clone();
    };

    let mut inflated = result.clone();

    // Pad text fields in content items
    if let Some(items) = inflated.get_mut("content").and_then(Value::as_array_mut) {
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && text.len() < max_line_length
            {
                let padded = format!("{}{}", text, "X".repeat(max_line_length - text.len()));
                item.as_object_mut()
                    .map(|obj| obj.insert("text".to_string(), Value::String(padded)));
            }
        }
    }

    // Wrap in nested objects
    for _ in 0..nesting_depth {
        inflated = json!({ "wrapper": inflated });
    }

    JsonRpcMessage::Response(JsonRpcResponse::success(resp.id.clone(), inflated))
}

/// Apply side effects before delivering the response.
///
/// Reads `state["behavior"]["side_effects"]` as an array of
/// `{type, parameters}` objects per §5.1. Supports `notification_flood`,
/// `id_collision`, and `connection_reset`.
///
/// # Errors
///
/// Returns `EngineError::Driver` for `connection_reset` (intentional)
/// or on transport failure.
pub(super) async fn apply_side_effects(
    transport: &Arc<dyn Transport>,
    state: &Value,
    request_id: &Value,
) -> Result<(), EngineError> {
    let Some(effects) = state
        .get("behavior")
        .and_then(|b| b.get("side_effects"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };

    for effect in effects {
        let effect_type = effect.get("type").and_then(Value::as_str).unwrap_or("");
        let params = effect.get("parameters");

        match effect_type {
            "notification_flood" => {
                let method = params
                    .and_then(|p| p.get("method"))
                    .and_then(Value::as_str)
                    .unwrap_or("notifications/message");
                let rate = params
                    .and_then(|p| p.get("rate"))
                    .and_then(Value::as_u64)
                    .unwrap_or(10);
                let duration_str = params
                    .and_then(|p| p.get("duration"))
                    .and_then(Value::as_str)
                    .unwrap_or("1s");
                let duration =
                    parse_duration(duration_str).unwrap_or(std::time::Duration::from_secs(1));

                let start = tokio::time::Instant::now();
                let interval = if rate > 0 {
                    std::time::Duration::from_millis(1000 / rate)
                } else {
                    std::time::Duration::from_millis(100)
                };

                while start.elapsed() < duration {
                    let notif = JsonRpcNotification::new(
                        method,
                        Some(json!({ "level": "info", "data": "flood" })),
                    );
                    transport
                        .send_message(&JsonRpcMessage::Notification(notif))
                        .await
                        .map_err(|e| EngineError::Driver(format!("flood send error: {e}")))?;
                    tokio::time::sleep(interval).await;
                }
            }
            "id_collision" => {
                let count = params
                    .and_then(|p| p.get("count"))
                    .and_then(Value::as_u64)
                    .unwrap_or(1);
                for _ in 0..count {
                    let collision =
                        JsonRpcResponse::success(request_id.clone(), json!({"collision": true}));
                    transport
                        .send_message(&JsonRpcMessage::Response(collision))
                        .await
                        .map_err(|e| EngineError::Driver(format!("collision send error: {e}")))?;
                }
            }
            "connection_reset" => {
                return Err(EngineError::Driver(
                    "connection_reset side effect triggered".to_string(),
                ));
            }
            _ => {
                tracing::warn!(side_effect = effect_type, "unknown side effect, skipping");
            }
        }
    }

    Ok(())
}
