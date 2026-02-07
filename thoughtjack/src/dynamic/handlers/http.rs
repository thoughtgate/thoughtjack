//! HTTP external handler (TJ-SPEC-009 F-003).
//!
//! Sends a POST request to an external service and parses the response
//! as a [`HandlerResponse`]. Requires `--allow-external-handlers`.

use std::collections::HashMap;
use std::time::Duration;

use reqwest::redirect;
use tracing::debug;

use crate::config::schema::ContentItem;
use crate::error::HandlerError;

use crate::dynamic::context::TemplateContext;
use crate::dynamic::template::resolve_template;

use super::{DEFAULT_TIMEOUT_MS, HandlerResponse, MAX_RESPONSE_SIZE, build_handler_body};

/// Creates a shared HTTP client for handler requests.
///
/// No redirect following for security (prevents SSRF via open redirects).
///
/// # Panics
///
/// Panics if the HTTP client cannot be built (should never happen).
///
/// Implements: TJ-SPEC-009 F-003
#[must_use]
pub fn create_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(redirect::Policy::none())
        .build()
        .expect("failed to build HTTP client")
}

/// Executes an HTTP handler request.
///
/// Sends a POST to the configured URL with a JSON body describing the
/// request context. Header values are template-resolved.
///
/// # Errors
///
/// Returns `HandlerError::NotEnabled` if handlers are not allowed.
/// Returns `HandlerError::Network` on connection failures.
/// Returns `HandlerError::HttpStatus` on non-2xx responses.
/// Returns `HandlerError::Timeout` if the request exceeds the timeout.
/// Returns `HandlerError::InvalidResponse` if the response cannot be parsed.
/// Returns `HandlerError::ToolError` if the handler returns an error response.
///
/// Implements: TJ-SPEC-009 F-003
#[allow(clippy::implicit_hasher)]
pub async fn execute_http_handler(
    client: &reqwest::Client,
    url: &str,
    timeout_ms: Option<u64>,
    headers: Option<&HashMap<String, String>>,
    ctx: &TemplateContext,
    item_type_str: &str,
    allow_external_handlers: bool,
) -> Result<Vec<ContentItem>, HandlerError> {
    if !allow_external_handlers {
        return Err(HandlerError::NotEnabled);
    }

    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));

    let body = build_handler_body(item_type_str, ctx);

    debug!(url = url, "executing HTTP handler");

    let mut req = client.post(url).json(&body);

    // Template-resolve header values
    if let Some(hdrs) = headers {
        for (key, value) in hdrs {
            let resolved = resolve_template(value, ctx);
            req = req.header(key, resolved);
        }
    }

    let response = tokio::time::timeout(timeout, req.send())
        .await
        .map_err(|_| HandlerError::Timeout)?
        .map_err(|e| HandlerError::Network(e.to_string()))?;

    let status = response.status();
    if !status.is_success() {
        return Err(HandlerError::HttpStatus(status.as_u16()));
    }

    // Read body with size limit
    let bytes = tokio::time::timeout(timeout, response.bytes())
        .await
        .map_err(|_| HandlerError::Timeout)?
        .map_err(|e| HandlerError::Network(e.to_string()))?;

    if bytes.len() > MAX_RESPONSE_SIZE {
        return Err(HandlerError::InvalidResponse(format!(
            "response body exceeds {MAX_RESPONSE_SIZE} byte limit"
        )));
    }

    let handler_response: HandlerResponse =
        serde_json::from_slice(&bytes).map_err(|e| HandlerError::InvalidResponse(e.to_string()))?;

    parse_handler_response(handler_response)
}

/// Parses a `HandlerResponse` into content items.
///
/// Implements: TJ-SPEC-009 F-003
fn parse_handler_response(response: HandlerResponse) -> Result<Vec<ContentItem>, HandlerError> {
    match response {
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
    use crate::config::schema::ContentValue;
    use serde_json::json;

    #[test]
    fn test_parse_full_response() {
        let resp = HandlerResponse::Full {
            content: vec![ContentItem::Text {
                text: ContentValue::Static("hello".to_string()),
            }],
        };
        let items = parse_handler_response(resp).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn test_parse_simple_response() {
        let resp = HandlerResponse::Simple {
            text: "hello".to_string(),
        };
        let items = parse_handler_response(resp).unwrap();
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn test_parse_error_response() {
        let resp = HandlerResponse::Error {
            error: json!({"message": "not found"}),
        };
        let result = parse_handler_response(resp);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), HandlerError::ToolError(_)));
    }

    // EC-DYN-021: handler not enabled
    #[tokio::test]
    async fn test_not_enabled() {
        let client = create_http_client();
        let ctx = crate::dynamic::context::TemplateContext {
            args: json!({}),
            item_name: "test".to_string(),
            item_type: crate::dynamic::context::ItemType::Tool,
            call_count: 1,
            phase_name: "baseline".to_string(),
            phase_index: -1,
            request_id: None,
            request_method: "tools/call".to_string(),
            connection_id: 1,
            resource_name: None,
            resource_mime_type: None,
        };
        let result = execute_http_handler(
            &client,
            "http://localhost:9999",
            None,
            None,
            &ctx,
            "tool",
            false,
        )
        .await;
        assert!(matches!(result.unwrap_err(), HandlerError::NotEnabled));
    }
}
