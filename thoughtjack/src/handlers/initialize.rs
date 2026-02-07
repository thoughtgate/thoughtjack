//! MCP `initialize` handler.
//!
//! Returns the server's capabilities and identification per the MCP
//! protocol specification.

use serde_json::json;

use crate::phase::EffectiveState;
use crate::transport::jsonrpc::{JsonRpcRequest, JsonRpcResponse};

/// MCP protocol version we advertise.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Handles an `initialize` request.
///
/// Builds the response from the effective state's capabilities and the
/// server metadata.
///
/// Implements: TJ-SPEC-002 F-001
#[must_use]
pub fn handle(
    request: &JsonRpcRequest,
    effective_state: &EffectiveState,
    server_name: &str,
    server_version: &str,
) -> JsonRpcResponse {
    let capabilities = build_capabilities(effective_state);

    JsonRpcResponse::success(
        request.id.clone(),
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": capabilities,
            "serverInfo": {
                "name": server_name,
                "version": server_version,
            }
        }),
    )
}

/// Builds the capabilities object from the effective state.
fn build_capabilities(effective_state: &EffectiveState) -> serde_json::Value {
    let mut caps = serde_json::Map::new();

    // If explicit capabilities are configured, use them
    if let Some(ref configured) = effective_state.capabilities {
        if let Some(ref tools_cap) = configured.tools {
            let mut t = serde_json::Map::new();
            if let Some(lc) = tools_cap.list_changed {
                t.insert("listChanged".to_string(), json!(lc));
            }
            caps.insert("tools".to_string(), serde_json::Value::Object(t));
        }
        if let Some(ref res_cap) = configured.resources {
            let mut r = serde_json::Map::new();
            if let Some(s) = res_cap.subscribe {
                r.insert("subscribe".to_string(), json!(s));
            }
            if let Some(lc) = res_cap.list_changed {
                r.insert("listChanged".to_string(), json!(lc));
            }
            caps.insert("resources".to_string(), serde_json::Value::Object(r));
        }
        if let Some(ref prompts_cap) = configured.prompts {
            let mut p = serde_json::Map::new();
            if let Some(lc) = prompts_cap.list_changed {
                p.insert("listChanged".to_string(), json!(lc));
            }
            caps.insert("prompts".to_string(), serde_json::Value::Object(p));
        }
    } else {
        // Infer capabilities from what's available
        if !effective_state.tools.is_empty() {
            caps.insert("tools".to_string(), json!({}));
        }
        if !effective_state.resources.is_empty() {
            caps.insert("resources".to_string(), json!({}));
        }
        if !effective_state.prompts.is_empty() {
            caps.insert("prompts".to_string(), json!({}));
        }
    }

    serde_json::Value::Object(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        Capabilities, ContentItem, ContentValue, ResponseConfig, ToolDefinition, ToolPattern,
        ToolsCapability,
    };
    use crate::phase::EffectiveState;
    use crate::transport::jsonrpc::JSONRPC_VERSION;
    use indexmap::IndexMap;

    fn make_request() -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialize".to_string(),
            params: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "1.0" }
            })),
            id: json!(0),
        }
    }

    fn empty_state() -> EffectiveState {
        EffectiveState {
            tools: IndexMap::new(),
            resources: IndexMap::new(),
            prompts: IndexMap::new(),
            capabilities: None,
            behavior: None,
        }
    }

    #[test]
    fn response_has_protocol_version() {
        let req = make_request();
        let resp = handle(&req, &empty_state(), "test-server", "1.0.0");
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn response_has_server_info() {
        let req = make_request();
        let resp = handle(&req, &empty_state(), "test-server", "1.0.0");
        let result = resp.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "test-server");
        assert_eq!(result["serverInfo"]["version"], "1.0.0");
    }

    #[test]
    fn infers_tools_capability_when_tools_present() {
        let mut state = empty_state();
        let mut tools = IndexMap::new();
        tools.insert(
            "calc".to_string(),
            ToolPattern {
                tool: ToolDefinition {
                    name: "calc".to_string(),
                    description: "Calculator".to_string(),
                    input_schema: json!({"type": "object"}),
                },
                response: ResponseConfig {
                    content: vec![ContentItem::Text {
                        text: ContentValue::Static("42".to_string()),
                    }],
                    is_error: None,
                },
                behavior: None,
            },
        );
        state.tools = tools;

        let req = make_request();
        let resp = handle(&req, &state, "test-server", "1.0.0");
        let result = resp.result.unwrap();
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn uses_explicit_capabilities() {
        let mut state = empty_state();
        state.capabilities = Some(Capabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(true),
            }),
            resources: None,
            prompts: None,
        });

        let req = make_request();
        let resp = handle(&req, &state, "test-server", "1.0.0");
        let result = resp.result.unwrap();
        assert_eq!(result["capabilities"]["tools"]["listChanged"], true);
    }

    #[test]
    fn empty_capabilities_for_empty_state() {
        let req = make_request();
        let resp = handle(&req, &empty_state(), "test-server", "1.0.0");
        let result = resp.result.unwrap();
        let caps = result["capabilities"].as_object().unwrap();
        assert!(caps.is_empty());
    }
}
