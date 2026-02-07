//! Command subprocess handler (TJ-SPEC-009 F-004).
//!
//! Executes a subprocess with JSON on stdin and reads the response from
//! stdout. Requires `--allow-external-handlers`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::config::schema::ContentItem;
use crate::error::HandlerError;

use crate::dynamic::context::TemplateContext;
use crate::dynamic::template::resolve_template;

use super::{HandlerResponse, DEFAULT_TIMEOUT_MS, MAX_RESPONSE_SIZE, build_handler_body};

/// Executes a command handler subprocess.
///
/// Spawns the configured command, writes the request as JSON to stdin,
/// and reads the response from stdout. Environment variable values are
/// template-resolved.
///
/// # Errors
///
/// Returns `HandlerError::NotEnabled` if handlers are not allowed.
/// Returns `HandlerError::SpawnFailed` if the process cannot be spawned.
/// Returns `HandlerError::Timeout` if the process exceeds the timeout.
/// Returns `HandlerError::NonZeroExit` if the process exits with non-zero status.
/// Returns `HandlerError::InvalidResponse` if stdout cannot be parsed.
/// Returns `HandlerError::ToolError` if the handler returns an error response.
///
/// Implements: TJ-SPEC-009 F-004
#[allow(clippy::implicit_hasher)]
pub async fn execute_command_handler(
    cmd: &[String],
    timeout_ms: Option<u64>,
    env: Option<&HashMap<String, String>>,
    working_dir: Option<&PathBuf>,
    ctx: &TemplateContext,
    item_type_str: &str,
    allow_external_handlers: bool,
) -> Result<Vec<ContentItem>, HandlerError> {
    if !allow_external_handlers {
        return Err(HandlerError::NotEnabled);
    }

    if cmd.is_empty() {
        return Err(HandlerError::SpawnFailed(
            "empty command array".to_string(),
        ));
    }

    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let body = build_handler_body(item_type_str, ctx);

    let body_bytes = serde_json::to_vec(&body)
        .map_err(|e| HandlerError::InvalidResponse(e.to_string()))?;

    debug!(cmd = ?cmd, "executing command handler");

    let mut command = tokio::process::Command::new(&cmd[0]);
    command
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Template-resolve env var values
    if let Some(env_vars) = env {
        for (key, value) in env_vars {
            let resolved = resolve_template(value, ctx);
            command.env(key, resolved);
        }
    }

    if let Some(dir) = working_dir {
        command.current_dir(dir);
    }

    let mut child = command
        .spawn()
        .map_err(|e| HandlerError::SpawnFailed(e.to_string()))?;

    // Write body to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&body_bytes).await.map_err(|e| {
            HandlerError::SpawnFailed(format!("failed to write to stdin: {e}"))
        })?;
        drop(stdin);
    }

    // Wait for process with timeout
    let output = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| HandlerError::Timeout)?
        .map_err(|e| HandlerError::SpawnFailed(e.to_string()))?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(HandlerError::NonZeroExit {
            code: output.status.code(),
            stderr,
        });
    }

    // EC-DYN-007: exit 0 with stderr → log warning, use stdout
    if !stderr.is_empty() {
        warn!(
            cmd = ?cmd,
            stderr = %stderr,
            "command handler produced stderr output"
        );
    }

    if output.stdout.len() > MAX_RESPONSE_SIZE {
        return Err(HandlerError::InvalidResponse(format!(
            "stdout exceeds {MAX_RESPONSE_SIZE} byte limit"
        )));
    }

    let handler_response: HandlerResponse = serde_json::from_slice(&output.stdout)
        .map_err(|e| HandlerError::InvalidResponse(e.to_string()))?;

    match handler_response {
        HandlerResponse::Full { content } => Ok(content),
        HandlerResponse::Simple { text } => Ok(vec![ContentItem::Text {
            text: crate::config::schema::ContentValue::Static(text),
        }]),
        HandlerResponse::Error { error } => Err(HandlerError::ToolError(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic::context::{ItemType, TemplateContext};
    use serde_json::json;

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            args: json!({"x": 1}),
            item_name: "calc".to_string(),
            item_type: ItemType::Tool,
            call_count: 1,
            phase_name: "baseline".to_string(),
            phase_index: -1,
            request_id: None,
            request_method: "tools/call".to_string(),
            connection_id: 1,
            resource_name: None,
            resource_mime_type: None,
        }
    }

    // EC-DYN-022: handler not enabled
    #[tokio::test]
    async fn test_not_enabled() {
        let ctx = make_ctx();
        let result = execute_command_handler(
            &["echo".to_string()],
            None,
            None,
            None,
            &ctx,
            "tool",
            false,
        )
        .await;
        assert!(matches!(result.unwrap_err(), HandlerError::NotEnabled));
    }

    #[tokio::test]
    async fn test_empty_cmd() {
        let ctx = make_ctx();
        let result = execute_command_handler(
            &[],
            None,
            None,
            None,
            &ctx,
            "tool",
            true,
        )
        .await;
        assert!(matches!(result.unwrap_err(), HandlerError::SpawnFailed(_)));
    }

    // EC-DYN-008: non-zero exit
    #[tokio::test]
    async fn test_nonzero_exit() {
        let ctx = make_ctx();
        let result = execute_command_handler(
            &["false".to_string()],
            None,
            None,
            None,
            &ctx,
            "tool",
            true,
        )
        .await;
        assert!(matches!(
            result.unwrap_err(),
            HandlerError::NonZeroExit { .. }
        ));
    }
}
