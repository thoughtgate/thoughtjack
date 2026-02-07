//! MCP request handler dispatch (TJ-SPEC-002 / TJ-SPEC-003).
//!
//! Routes incoming JSON-RPC requests to the appropriate handler based on
//! the MCP method name.  Each handler receives the current effective state
//! and returns an optional `JsonRpcResponse`.

pub mod initialize;
pub mod prompts;
pub mod resources;
pub mod tools;

use crate::config::schema::{ContentValue, GeneratorLimits};
use crate::error::ThoughtJackError;
use crate::generator::{PayloadGenerator, create_generator};
use crate::phase::EffectiveState;
use crate::transport::jsonrpc::{JsonRpcRequest, JsonRpcResponse};

/// Dispatches an MCP request to the appropriate handler.
///
/// Returns `Some(response)` for known methods, or `None` for unknown
/// methods (the caller decides how to handle unknowns based on
/// [`UnknownMethodHandling`](crate::config::schema::UnknownMethodHandling)).
///
/// # Errors
///
/// Returns an error if handler execution fails (e.g. generator error,
/// file I/O error).
///
/// Implements: TJ-SPEC-002 F-001
pub async fn handle_request(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    server_name: &str,
    server_version: &str,
    limits: &GeneratorLimits,
) -> Result<Option<JsonRpcResponse>, ThoughtJackError> {
    match request.method.as_str() {
        "initialize" => Ok(Some(initialize::handle(
            request,
            effective_state,
            server_name,
            server_version,
        ))),
        "ping" | "resources/subscribe" | "resources/unsubscribe" | "logging/setLevel" => Ok(Some(
            JsonRpcResponse::success(request.id.clone(), serde_json::json!({})),
        )),
        "completion/complete" => Ok(Some(JsonRpcResponse::success(
            request.id.clone(),
            serde_json::json!({
                "completion": { "values": [], "hasMore": false }
            }),
        ))),
        "tools/list" => Ok(Some(tools::handle_list(request, effective_state))),
        "tools/call" => tools::handle_call(request, effective_state, limits)
            .await
            .map(Some),
        "resources/list" => Ok(Some(resources::handle_list(request, effective_state))),
        "resources/read" => resources::handle_read(request, effective_state, limits)
            .await
            .map(Some),
        "prompts/list" => Ok(Some(prompts::handle_list(request, effective_state))),
        "prompts/get" => prompts::handle_get(request, effective_state, limits)
            .await
            .map(Some),
        _ => Ok(None),
    }
}

/// Resolves a [`ContentValue`] to a string.
///
/// - `Static(s)` — returns the string directly.
/// - `Generated { generator }` — creates the generator and materializes
///   the payload as a lossy UTF-8 string.
/// - `File { path }` — reads the file asynchronously.
///
/// # Errors
///
/// Returns an error if generator creation/execution fails or if the
/// file cannot be read.
///
/// Implements: TJ-SPEC-005 F-001
pub async fn resolve_content(
    value: &ContentValue,
    limits: &GeneratorLimits,
) -> Result<String, ThoughtJackError> {
    match value {
        ContentValue::Static(s) => Ok(s.clone()),
        ContentValue::Generated { generator } => {
            let generator_impl: Box<dyn PayloadGenerator> = create_generator(generator, limits)?;
            let payload = generator_impl.generate()?;
            Ok(String::from_utf8_lossy(&payload.into_bytes()).into_owned())
        }
        ContentValue::File { path } => {
            let path_str = path.to_string_lossy();
            if path.is_absolute() || path_str.contains("..") {
                return Err(ThoughtJackError::Io(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("file path not allowed: {}", path.display()),
                )));
            }
            let content = tokio::fs::read_to_string(path).await?;
            Ok(content)
        }
    }
}
