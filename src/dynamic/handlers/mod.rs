//! External handler types and shared interface (TJ-SPEC-009 F-003, F-004).
//!
//! Defines the `HandlerResponse` type returned by HTTP and command handlers,
//! and the common execution interface.

pub mod command;
pub mod http;

use serde::Deserialize;
use serde_json::Value;

use crate::config::schema::ContentItem;
use crate::dynamic::context::TemplateContext;

/// Default handler timeout in milliseconds (30 seconds).
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Maximum response body size (10 MB).
pub const MAX_RESPONSE_SIZE: usize = 10 * 1024 * 1024;

/// Response returned by an external handler.
///
/// Deserialized from the handler's JSON output. Uses `untagged` so
/// the handler can return any of the three forms.
///
/// Implements: TJ-SPEC-009 F-003, F-004
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum HandlerResponse {
    /// Full MCP content array
    Full {
        /// Content items returned by the handler
        content: Vec<ContentItem>,
    },

    /// Simple text response
    Simple {
        /// Text content
        text: String,
    },

    /// Error response to pass through as tool error
    Error {
        /// Error details
        error: Value,
    },
}

/// JSON body sent to external handlers (both HTTP and command stdin).
///
/// The context format matches TJ-SPEC-009's handler protocol.
///
/// Implements: TJ-SPEC-009 F-003, F-004
#[must_use]
pub fn build_handler_body(item_type_str: &str, ctx: &TemplateContext) -> Value {
    let mut body = serde_json::json!({
        item_type_str: &ctx.item_name,
        "arguments": &ctx.args,
        "context": {
            "phase": &ctx.phase_name,
            "phase_index": ctx.phase_index,
            "tool_call_count": ctx.call_count,
            "connection_id": ctx.connection_id,
        }
    });

    if let Some(id) = &ctx.request_id {
        body["context"]["request_id"] = id.clone();
    }

    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_handler_response_full() {
        let json_str = r#"{"content": [{"type": "text", "text": "hello"}]}"#;
        let resp: HandlerResponse = serde_json::from_str(json_str).unwrap();
        assert!(matches!(resp, HandlerResponse::Full { .. }));
    }

    #[test]
    fn test_handler_response_simple() {
        let json_str = r#"{"text": "hello"}"#;
        let resp: HandlerResponse = serde_json::from_str(json_str).unwrap();
        assert!(matches!(resp, HandlerResponse::Simple { text } if text == "hello"));
    }

    #[test]
    fn test_handler_response_error() {
        let json_str = r#"{"error": {"message": "not found"}}"#;
        let resp: HandlerResponse = serde_json::from_str(json_str).unwrap();
        assert!(matches!(resp, HandlerResponse::Error { .. }));
    }

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            args: json!({"x": 1}),
            item_name: "calc".to_string(),
            item_type: crate::dynamic::context::ItemType::Tool,
            call_count: 3,
            phase_name: "baseline".to_string(),
            phase_index: -1,
            request_id: Some(json!("req-1")),
            request_method: "tools/call".to_string(),
            connection_id: 42,
            resource_name: None,
            resource_mime_type: None,
        }
    }

    #[test]
    fn test_build_handler_body() {
        let ctx = make_ctx();
        let body = build_handler_body("tool", &ctx);
        assert_eq!(body["tool"], "calc");
        assert_eq!(body["arguments"]["x"], 1);
        assert_eq!(body["context"]["phase"], "baseline");
        assert_eq!(body["context"]["phase_index"], -1);
        assert_eq!(body["context"]["tool_call_count"], 3);
        assert_eq!(body["context"]["connection_id"], 42);
        assert_eq!(body["context"]["request_id"], "req-1");
    }

    #[test]
    fn test_build_handler_body_no_request_id() {
        let mut ctx = make_ctx();
        ctx.request_id = None;
        let body = build_handler_body("tool", &ctx);
        assert!(body["context"].get("request_id").is_none());
    }
}
