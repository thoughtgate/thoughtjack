use std::collections::HashMap;

use oatf::ResponseEntry;
use oatf::primitives::{interpolate_value, select_response};
use serde_json::{Value, json};

use crate::transport::JsonRpcResponse;
use crate::transport::jsonrpc::error_codes;

use super::generation::apply_generation;

/// Dispatch a response using the OATF `select_response()` SDK function.
///
/// Finds matching `ResponseEntry` from the item's `responses` array,
/// interpolates values, and validates synthesized output if applicable.
#[allow(clippy::cognitive_complexity)]
pub fn dispatch_response(
    request_id: &Value,
    item: &Value,
    extractors: &HashMap<String, String>,
    request_context: &Value,
    output_schema: Option<&Value>,
    raw_synthesize: bool,
) -> JsonRpcResponse {
    let Some(responses_value) = item.get("responses") else {
        // No responses defined — return empty content
        return JsonRpcResponse::success(request_id.clone(), json!({ "content": [] }));
    };

    let entries: Vec<ResponseEntry> = match serde_json::from_value(responses_value.clone()) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(error = %err, "failed to deserialize response entries");
            return JsonRpcResponse::error(
                request_id.clone(),
                error_codes::INTERNAL_ERROR,
                format!("response configuration error: {err}"),
            );
        }
    };

    let Some(entry) = select_response(&entries, request_context) else {
        // No matching response
        return JsonRpcResponse::success(request_id.clone(), json!({ "content": [] }));
    };

    // Check for synthesize block
    if entry.synthesize.is_some() && entry.extra.is_empty() {
        // No static content and synthesize required — GenerationProvider not available yet
        tracing::info!("synthesize block encountered but GenerationProvider not available");
        return JsonRpcResponse::error(
            request_id.clone(),
            error_codes::INTERNAL_ERROR,
            "synthesize not yet supported — GenerationProvider not available",
        );
    }

    // Build response from extra fields with interpolation
    let extra_value = serde_json::to_value(&entry.extra).unwrap_or(Value::Null);
    let (interpolated, diagnostics) =
        interpolate_value(&extra_value, extractors, Some(request_context), None);

    for diag in &diagnostics {
        tracing::debug!(diagnostic = ?diag, "interpolation diagnostic");
    }

    // Apply payload generation for content items with `generate` blocks
    let mut interpolated = interpolated;
    apply_generation(&mut interpolated);

    // Validate synthesized output if applicable
    if entry.synthesize.is_some()
        && !raw_synthesize
        && let Err(err) = crate::engine::generation::validate_synthesized_output(
            "mcp",
            &interpolated,
            output_schema,
        )
    {
        tracing::warn!(error = %err, "synthesized output validation failed");
        return JsonRpcResponse::error(
            request_id.clone(),
            error_codes::INTERNAL_ERROR,
            format!("synthesize validation: {err}"),
        );
    }

    JsonRpcResponse::success(request_id.clone(), interpolated)
}
