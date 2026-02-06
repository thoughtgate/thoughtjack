//! JSON-RPC 2.0 message types for MCP transport (TJ-SPEC-002).
//!
//! Provides serialization and deserialization of JSON-RPC 2.0 messages
//! used by the Model Context Protocol. Uses `serde_json::Value` for
//! params, result, error data, and IDs to support arbitrary JSON payloads
//! required by adversarial testing.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC protocol version string.
///
/// Implements: TJ-SPEC-002 F-001
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard JSON-RPC 2.0 error codes.
///
/// Implements: TJ-SPEC-002 F-001
pub mod error_codes {
    /// Invalid JSON was received by the server.
    pub const PARSE_ERROR: i64 = -32700;

    /// The JSON sent is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;

    /// The method does not exist / is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;

    /// Invalid method parameter(s).
    pub const INVALID_PARAMS: i64 = -32602;

    /// Internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A JSON-RPC 2.0 message.
///
/// Can be a request (has `method` and `id`), a notification (has `method` but
/// no `id`), or a response (has `result` or `error` and `id`).
///
/// Uses custom deserialization to reliably distinguish between variants by
/// inspecting which JSON keys are present, rather than relying on
/// `#[serde(untagged)]` which cannot reliably distinguish request from response.
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value does not implement Eq
pub enum JsonRpcMessage {
    /// A request expecting a response.
    Request(JsonRpcRequest),
    /// A response to a previous request.
    Response(JsonRpcResponse),
    /// A notification (no response expected).
    Notification(JsonRpcNotification),
}

impl JsonRpcMessage {
    /// Returns the message ID, if present.
    ///
    /// Requests and responses have IDs; notifications do not.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub const fn id(&self) -> Option<&Value> {
        match self {
            Self::Request(r) => Some(&r.id),
            Self::Response(r) => Some(&r.id),
            Self::Notification(_) => None,
        }
    }

    /// Returns the method name, if present.
    ///
    /// Requests and notifications have methods; responses do not.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn method(&self) -> Option<&str> {
        match self {
            Self::Request(r) => Some(&r.method),
            Self::Notification(n) => Some(&n.method),
            Self::Response(_) => None,
        }
    }
}

impl Serialize for JsonRpcMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Request(r) => r.serialize(serializer),
            Self::Response(r) => r.serialize(serializer),
            Self::Notification(n) => n.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let obj = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("JSON-RPC message must be an object"))?;

        let has_method = obj.contains_key("method");
        let has_id = obj.contains_key("id");
        let has_result = obj.contains_key("result");
        let has_error = obj.contains_key("error");

        if has_result || has_error {
            // Response: has result and/or error (and typically id)
            let response: JsonRpcResponse = serde_json::from_value(value)
                .map_err(|e| serde::de::Error::custom(format!("invalid response: {e}")))?;
            Ok(Self::Response(response))
        } else if has_method && has_id {
            // Request: has method and id
            let request: JsonRpcRequest = serde_json::from_value(value)
                .map_err(|e| serde::de::Error::custom(format!("invalid request: {e}")))?;
            Ok(Self::Request(request))
        } else if has_method {
            // Notification: has method but no id
            let notification: JsonRpcNotification = serde_json::from_value(value)
                .map_err(|e| serde::de::Error::custom(format!("invalid notification: {e}")))?;
            Ok(Self::Notification(notification))
        } else {
            Err(serde::de::Error::custom(
                "JSON-RPC message must have 'method' (request/notification) or 'result'/'error' (response)",
            ))
        }
    }
}

/// A JSON-RPC 2.0 request.
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value fields
pub struct JsonRpcRequest {
    /// Protocol version (must be "2.0").
    pub jsonrpc: String,

    /// Method name to invoke.
    pub method: String,

    /// Method parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,

    /// Request identifier.
    pub id: Value,
}

/// A JSON-RPC 2.0 response.
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value fields
pub struct JsonRpcResponse {
    /// Protocol version (must be "2.0").
    pub jsonrpc: String,

    /// Result value (present on success).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error value (present on failure).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,

    /// Request identifier this response corresponds to.
    pub id: Value,
}

impl JsonRpcResponse {
    /// Creates a successful response.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Creates an error response.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }
}

/// A JSON-RPC 2.0 notification (request with no `id`).
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value fields
pub struct JsonRpcNotification {
    /// Protocol version (must be "2.0").
    pub jsonrpc: String,

    /// Method name.
    pub method: String,

    /// Method parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    /// Creates a new notification.
    ///
    /// Implements: TJ-SPEC-002 F-001
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 error object.
///
/// Implements: TJ-SPEC-002 F-001
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::derive_partial_eq_without_eq)] // serde_json::Value fields
pub struct JsonRpcError {
    /// Error code.
    pub code: i64,

    /// Human-readable error message.
    pub message: String,

    /// Additional error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ========================================================================
    // Round-trip serialization tests
    // ========================================================================

    #[test]
    fn test_request_round_trip() {
        let request = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "tools/call".to_string(),
            params: Some(json!({"name": "calculator"})),
            id: json!(1),
        });

        let serialized = serde_json::to_string(&request).unwrap();
        let deserialized: JsonRpcMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(request, deserialized);
    }

    #[test]
    fn test_response_success_round_trip() {
        let response = JsonRpcMessage::Response(JsonRpcResponse::success(
            json!(1),
            json!({"content": [{"type": "text", "text": "42"}]}),
        ));

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: JsonRpcMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(response, deserialized);
    }

    #[test]
    fn test_response_error_round_trip() {
        let response = JsonRpcMessage::Response(JsonRpcResponse::error(
            json!(1),
            error_codes::METHOD_NOT_FOUND,
            "Method not found",
        ));

        let serialized = serde_json::to_string(&response).unwrap();
        let deserialized: JsonRpcMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(response, deserialized);
    }

    #[test]
    fn test_notification_round_trip() {
        let notification =
            JsonRpcMessage::Notification(JsonRpcNotification::new("notifications/progress", None));

        let serialized = serde_json::to_string(&notification).unwrap();
        let deserialized: JsonRpcMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(notification, deserialized);
    }

    #[test]
    fn test_notification_with_params_round_trip() {
        let notification = JsonRpcMessage::Notification(JsonRpcNotification::new(
            "notifications/tools/list_changed",
            Some(json!({})),
        ));

        let serialized = serde_json::to_string(&notification).unwrap();
        let deserialized: JsonRpcMessage = serde_json::from_str(&serialized).unwrap();
        assert_eq!(notification, deserialized);
    }

    // ========================================================================
    // Deserialization distinction tests
    // ========================================================================

    #[test]
    fn test_deserialize_request() {
        let json = r#"{"jsonrpc":"2.0","method":"tools/list","id":1}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, JsonRpcMessage::Request(_)));
    }

    #[test]
    fn test_deserialize_request_with_params() {
        let json = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"calc"},"id":"abc"}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Request(r) => {
                assert_eq!(r.method, "tools/call");
                assert_eq!(r.id, json!("abc"));
                assert!(r.params.is_some());
            }
            _ => panic!("Expected Request"),
        }
    }

    #[test]
    fn test_deserialize_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, JsonRpcMessage::Notification(_)));
    }

    #[test]
    fn test_deserialize_response_with_result() {
        let json = r#"{"jsonrpc":"2.0","result":42,"id":1}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(r) => {
                assert_eq!(r.result, Some(json!(42)));
                assert!(r.error.is_none());
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_deserialize_response_with_error() {
        let json =
            r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":1}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(r) => {
                assert!(r.result.is_none());
                let err = r.error.unwrap();
                assert_eq!(err.code, error_codes::METHOD_NOT_FOUND);
                assert_eq!(err.message, "Method not found");
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_deserialize_response_with_null_result() {
        // "result": null is detected as a response by the custom deserializer
        // (because the "result" key is present), but serde's Option<Value>
        // deserializes null as None.
        let json = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(r) => {
                assert!(r.result.is_none());
                assert!(r.error.is_none());
                assert_eq!(r.id, json!(1));
            }
            _ => panic!("Expected Response"),
        }
    }

    // ========================================================================
    // ID type tests
    // ========================================================================

    #[test]
    fn test_numeric_id() {
        let json = r#"{"jsonrpc":"2.0","method":"test","id":42}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id(), Some(&json!(42)));
    }

    #[test]
    fn test_string_id() {
        let json = r#"{"jsonrpc":"2.0","method":"test","id":"request-1"}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id(), Some(&json!("request-1")));
    }

    #[test]
    fn test_null_id() {
        let json = r#"{"jsonrpc":"2.0","result":null,"id":null}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.id(), Some(&Value::Null));
    }

    // ========================================================================
    // Accessor tests
    // ========================================================================

    #[test]
    fn test_request_accessors() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "tools/call".to_string(),
            params: None,
            id: json!(1),
        });
        assert_eq!(msg.method(), Some("tools/call"));
        assert_eq!(msg.id(), Some(&json!(1)));
    }

    #[test]
    fn test_notification_accessors() {
        let msg = JsonRpcMessage::Notification(JsonRpcNotification::new("test/method", None));
        assert_eq!(msg.method(), Some("test/method"));
        assert_eq!(msg.id(), None);
    }

    #[test]
    fn test_response_accessors() {
        let msg = JsonRpcMessage::Response(JsonRpcResponse::success(json!(5), json!("ok")));
        assert_eq!(msg.method(), None);
        assert_eq!(msg.id(), Some(&json!(5)));
    }

    // ========================================================================
    // Constructor tests
    // ========================================================================

    #[test]
    fn test_response_success_constructor() {
        let resp = JsonRpcResponse::success(json!(1), json!({"data": "test"}));
        assert_eq!(resp.jsonrpc, JSONRPC_VERSION);
        assert_eq!(resp.result, Some(json!({"data": "test"})));
        assert!(resp.error.is_none());
        assert_eq!(resp.id, json!(1));
    }

    #[test]
    fn test_response_error_constructor() {
        let resp = JsonRpcResponse::error(json!(2), error_codes::INTERNAL_ERROR, "oops");
        assert_eq!(resp.jsonrpc, JSONRPC_VERSION);
        assert!(resp.result.is_none());
        let err = resp.error.unwrap();
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, "oops");
        assert!(err.data.is_none());
        assert_eq!(resp.id, json!(2));
    }

    #[test]
    fn test_notification_constructor() {
        let notif = JsonRpcNotification::new("test", Some(json!({"key": "val"})));
        assert_eq!(notif.jsonrpc, JSONRPC_VERSION);
        assert_eq!(notif.method, "test");
        assert_eq!(notif.params, Some(json!({"key": "val"})));
    }

    #[test]
    fn test_notification_constructor_no_params() {
        let notif = JsonRpcNotification::new("ping", None);
        assert_eq!(notif.method, "ping");
        assert!(notif.params.is_none());
    }

    // ========================================================================
    // Error handling tests
    // ========================================================================

    #[test]
    fn test_invalid_json() {
        let result = serde_json::from_str::<JsonRpcMessage>("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_non_object_json() {
        let result = serde_json::from_str::<JsonRpcMessage>("[1, 2, 3]");
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_object() {
        let result = serde_json::from_str::<JsonRpcMessage>("{}");
        assert!(result.is_err());
    }

    // ========================================================================
    // Error code constants
    // ========================================================================

    #[test]
    fn test_error_codes() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    // ========================================================================
    // JsonRpcError with data
    // ========================================================================

    #[test]
    fn test_error_with_data() {
        let json = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error","data":{"detail":"unexpected token"}},"id":null}"#;
        let msg: JsonRpcMessage = serde_json::from_str(json).unwrap();
        match msg {
            JsonRpcMessage::Response(r) => {
                let err = r.error.unwrap();
                assert_eq!(err.code, error_codes::PARSE_ERROR);
                assert_eq!(err.data, Some(json!({"detail": "unexpected token"})));
            }
            _ => panic!("Expected Response"),
        }
    }

    // ========================================================================
    // Serialization format tests
    // ========================================================================

    #[test]
    fn test_request_serialization_format() {
        let msg = JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: "initialize".to_string(),
            params: Some(json!({})),
            id: json!(0),
        });
        let serialized = serde_json::to_string(&msg).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["method"], "initialize");
        assert_eq!(parsed["id"], 0);
    }

    #[test]
    fn test_notification_omits_params_when_none() {
        let notif = JsonRpcNotification::new("test", None);
        let serialized = serde_json::to_string(&notif).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert!(parsed.get("params").is_none());
    }

    #[test]
    fn test_response_omits_error_when_none() {
        let resp = JsonRpcResponse::success(json!(1), json!("ok"));
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert!(parsed.get("error").is_none());
        assert!(parsed.get("result").is_some());
    }

    #[test]
    fn test_response_omits_result_when_none() {
        let resp = JsonRpcResponse::error(json!(1), -32600, "bad");
        let serialized = serde_json::to_string(&resp).unwrap();
        let parsed: Value = serde_json::from_str(&serialized).unwrap();
        assert!(parsed.get("result").is_none());
        assert!(parsed.get("error").is_some());
    }
}
