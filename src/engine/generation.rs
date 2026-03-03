//! Synthesize output validation for LLM-generated content.
//!
//! After a `GenerationProvider` returns a `Value`, `ThoughtJack` validates
//! it against the protocol binding's expected message structure before
//! injection into the protocol stream. Validation is protocol-specific.
//!
//! The `--raw-synthesize` CLI flag bypasses validation entirely.
//!
//! See TJ-SPEC-013 §3.4 for validation rules.

use crate::error::EngineError;

// ============================================================================
// validate_synthesized_output
// ============================================================================

/// Validates LLM-generated output against protocol-specific structure.
///
/// # Protocols
///
/// - **MCP tools**: Output must be a valid `content` array (each item
///   has `type`, `text`/`data`/`resource`). When `output_schema` is
///   provided, the output must also include `structuredContent`.
/// - **MCP prompts**: Output must be a valid `messages` array (each
///   item has `role`, `content`).
/// - **A2A**: Output must be a valid `messages`/`artifacts` structure.
/// - **AG-UI**: Output must be a valid `messages` array.
/// - **Unknown**: Passes validation (permissive for forward compat).
///
/// # Errors
///
/// Returns `EngineError::SynthesizeValidation` if the output does not
/// conform to the protocol's expected structure.
///
/// Implements: TJ-SPEC-013 F-001
pub fn validate_synthesized_output(
    protocol: &str,
    content: &serde_json::Value,
    output_schema: Option<&serde_json::Value>,
) -> Result<(), EngineError> {
    match protocol {
        "mcp" => validate_mcp_output(content, output_schema),
        "a2a" => validate_a2a_output(content),
        "ag_ui" => validate_agui_output(content),
        _ => Ok(()), // Unknown protocol — permissive
    }
}

/// Validates MCP tool/prompt output structure.
fn validate_mcp_output(
    content: &serde_json::Value,
    output_schema: Option<&serde_json::Value>,
) -> Result<(), EngineError> {
    // MCP tool responses: content must be an array of content items
    if let Some(arr) = content.get("content").and_then(serde_json::Value::as_array) {
        for (i, item) in arr.iter().enumerate() {
            if !item.get("type").is_some_and(serde_json::Value::is_string) {
                return Err(EngineError::SynthesizeValidation(format!(
                    "MCP content[{i}] missing required 'type' field"
                )));
            }
        }

        // When outputSchema is declared, structuredContent must be present
        if output_schema.is_some() && content.get("structuredContent").is_none() {
            return Err(EngineError::SynthesizeValidation(
                "MCP tool declares outputSchema but synthesized output missing 'structuredContent'"
                    .to_string(),
            ));
        }

        return Ok(());
    }

    // MCP prompt responses: messages must be an array
    if let Some(arr) = content
        .get("messages")
        .and_then(serde_json::Value::as_array)
    {
        for (i, msg) in arr.iter().enumerate() {
            if !msg.get("role").is_some_and(serde_json::Value::is_string) {
                return Err(EngineError::SynthesizeValidation(format!(
                    "MCP messages[{i}] missing required 'role' field"
                )));
            }
            if msg.get("content").is_none() {
                return Err(EngineError::SynthesizeValidation(format!(
                    "MCP messages[{i}] missing required 'content' field"
                )));
            }
        }
        return Ok(());
    }

    Err(EngineError::SynthesizeValidation(
        "MCP synthesized output must contain 'content' array or 'messages' array".to_string(),
    ))
}

/// Validates A2A response structure.
fn validate_a2a_output(content: &serde_json::Value) -> Result<(), EngineError> {
    // A2A responses should have messages or artifacts
    let has_messages = content
        .get("messages")
        .is_some_and(serde_json::Value::is_array);
    let has_artifacts = content
        .get("artifacts")
        .is_some_and(serde_json::Value::is_array);

    if !has_messages && !has_artifacts {
        return Err(EngineError::SynthesizeValidation(
            "A2A synthesized output must contain 'messages' array or 'artifacts' array".to_string(),
        ));
    }

    Ok(())
}

/// Validates AG-UI response structure.
fn validate_agui_output(content: &serde_json::Value) -> Result<(), EngineError> {
    if !content
        .get("messages")
        .is_some_and(serde_json::Value::is_array)
    {
        return Err(EngineError::SynthesizeValidation(
            "AG-UI synthesized output must contain 'messages' array".to_string(),
        ));
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // MCP tool validation
    #[test]
    fn valid_mcp_tool_content_passes() {
        let content = json!({
            "content": [
                {"type": "text", "text": "Result: 42"}
            ]
        });
        assert!(validate_synthesized_output("mcp", &content, None).is_ok());
    }

    #[test]
    fn mcp_content_missing_type_rejects() {
        let content = json!({
            "content": [
                {"text": "missing type"}
            ]
        });
        assert!(validate_synthesized_output("mcp", &content, None).is_err());
    }

    #[test]
    fn mcp_tool_with_output_schema_requires_structured_content() {
        let content = json!({
            "content": [
                {"type": "text", "text": "done"}
            ]
        });
        let schema = json!({"type": "object"});
        let result = validate_synthesized_output("mcp", &content, Some(&schema));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("structuredContent")
        );
    }

    #[test]
    fn mcp_tool_with_output_schema_and_structured_content_passes() {
        let content = json!({
            "content": [
                {"type": "text", "text": "done"}
            ],
            "structuredContent": {"result": "ok"}
        });
        let schema = json!({"type": "object"});
        assert!(validate_synthesized_output("mcp", &content, Some(&schema)).is_ok());
    }

    // MCP prompt validation
    #[test]
    fn valid_mcp_prompt_messages_passes() {
        let content = json!({
            "messages": [
                {"role": "assistant", "content": {"type": "text", "text": "hello"}}
            ]
        });
        assert!(validate_synthesized_output("mcp", &content, None).is_ok());
    }

    #[test]
    fn mcp_messages_missing_role_rejects() {
        let content = json!({
            "messages": [
                {"content": {"type": "text", "text": "no role"}}
            ]
        });
        assert!(validate_synthesized_output("mcp", &content, None).is_err());
    }

    #[test]
    fn mcp_messages_missing_content_rejects() {
        let content = json!({
            "messages": [
                {"role": "assistant"}
            ]
        });
        assert!(validate_synthesized_output("mcp", &content, None).is_err());
    }

    #[test]
    fn mcp_invalid_structure_rejects() {
        let content = json!({"invalid": "structure"});
        assert!(validate_synthesized_output("mcp", &content, None).is_err());
    }

    // A2A validation
    #[test]
    fn valid_a2a_messages_passes() {
        let content = json!({"messages": [{"role": "agent", "parts": []}]});
        assert!(validate_synthesized_output("a2a", &content, None).is_ok());
    }

    #[test]
    fn valid_a2a_artifacts_passes() {
        let content = json!({"artifacts": [{"parts": []}]});
        assert!(validate_synthesized_output("a2a", &content, None).is_ok());
    }

    #[test]
    fn invalid_a2a_rejects() {
        let content = json!({"data": "invalid"});
        assert!(validate_synthesized_output("a2a", &content, None).is_err());
    }

    // AG-UI validation
    #[test]
    fn valid_agui_passes() {
        let content = json!({"messages": [{"type": "text", "text": "hello"}]});
        assert!(validate_synthesized_output("ag_ui", &content, None).is_ok());
    }

    #[test]
    fn invalid_agui_rejects() {
        let content = json!({"data": "invalid"});
        assert!(validate_synthesized_output("ag_ui", &content, None).is_err());
    }

    // Unknown protocol
    #[test]
    fn unknown_protocol_passes() {
        let content = json!({"anything": "goes"});
        assert!(validate_synthesized_output("custom", &content, None).is_ok());
    }

    #[test]
    fn empty_content_array_passes_mcp() {
        let content = json!({"content": []});
        assert!(validate_synthesized_output("mcp", &content, None).is_ok());
    }

    // ---- Property Tests ----

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn prop_unknown_protocol_passes(
                protocol in "[a-z]{1,15}".prop_filter(
                    "must not be a known protocol",
                    |s| s != "mcp" && s != "a2a" && s != "ag_ui",
                ),
                content in prop_oneof![
                    Just(json!({})),
                    Just(json!({"arbitrary": "data"})),
                    Just(json!({"content": 42})),
                    Just(json!(null)),
                ],
            ) {
                prop_assert!(validate_synthesized_output(&protocol, &content, None).is_ok());
            }

            #[test]
            fn prop_valid_mcp_content_passes(
                n_items in 0..5_usize,
                content_type in prop::sample::select(vec!["text", "image", "resource"]),
            ) {
                let items: Vec<serde_json::Value> = (0..n_items)
                    .map(|_| json!({"type": content_type, "text": "data"}))
                    .collect();
                let content = json!({"content": items});
                prop_assert!(validate_synthesized_output("mcp", &content, None).is_ok());
            }

            #[test]
            fn prop_mcp_no_content_fails(
                key in "[a-z]{1,10}".prop_filter(
                    "must not be content or messages",
                    |s| s != "content" && s != "messages",
                ),
                value in prop_oneof![
                    Just(json!(42)),
                    Just(json!("string")),
                    Just(json!(true)),
                    Just(json!(null)),
                ],
            ) {
                let content = json!({key: value});
                prop_assert!(validate_synthesized_output("mcp", &content, None).is_err());
            }
        }
    }
}
