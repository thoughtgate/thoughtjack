use serde_json::Value;

/// Find an item by `name` field in a state array.
pub fn find_by_name(state: &Value, collection: &str, name: &str) -> Option<Value> {
    state
        .get(collection)
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        .cloned()
}

/// Find an item by an arbitrary field in a state array.
pub fn find_by_field(state: &Value, collection: &str, field: &str, value: &str) -> Option<Value> {
    state
        .get(collection)
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get(field).and_then(Value::as_str) == Some(value))
        .cloned()
}

/// Find a resource template whose `uriTemplate` matches the given URI.
///
/// Uses RFC 6570 Level 1 matching: literal segments must match exactly,
/// `{var}` segments consume non-empty substrings between literals.
pub fn find_matching_template(state: &Value, uri: &str) -> Option<Value> {
    state
        .get("resource_templates")
        .and_then(Value::as_array)?
        .iter()
        .find(|t| {
            t.get("uriTemplate")
                .and_then(Value::as_str)
                .is_some_and(|tmpl| matches_uri_template(tmpl, uri))
        })
        .cloned()
}

/// Check if a URI matches an RFC 6570 Level 1 template.
///
/// Splits the template on `{...}` markers and checks that literal segments
/// appear in order with non-empty variable segments between them.
pub fn matches_uri_template(template: &str, uri: &str) -> bool {
    let mut literals = Vec::new();
    let mut rest = template;

    // Split template into literal segments around {var} markers
    while let Some(start) = rest.find('{') {
        literals.push(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            return false; // Malformed template
        };
        rest = &rest[start + end + 1..];
    }
    literals.push(rest); // Trailing literal (may be empty)

    // Match literals against the URI in order
    let last_nonempty = literals.iter().rposition(|l| !l.is_empty());
    let mut pos = 0;
    for (i, literal) in literals.iter().enumerate() {
        if literal.is_empty() {
            if i > 0 && i < literals.len() - 1 {
                // Variable segment between two empty literals — skip at least 1 char
                if pos >= uri.len() {
                    return false;
                }
                // Advance by one full character (may be multi-byte UTF-8)
                pos += uri[pos..].chars().next().map_or(1, char::len_utf8);
            }
            continue;
        }
        // After a variable (i > 0), skip at least 1 char so the variable is non-empty.
        // Use the full character width to avoid landing mid-UTF-8-sequence.
        let skip = if i > 0 {
            uri.get(pos..)
                .and_then(|s| s.chars().next())
                .map_or(1, char::len_utf8)
        } else {
            0
        };
        if pos + skip > uri.len() {
            return false;
        }
        let search = &uri[pos + skip..];
        // Use rfind for the last non-empty literal so preceding variables
        // get maximum room (greedy find would match too early when the
        // variable value contains characters that look like the suffix).
        let found = if Some(i) == last_nonempty && i > 0 {
            search.rfind(literal)
        } else {
            search.find(literal)
        };
        let Some(found) = found else {
            return false;
        };
        // For the first literal, it must start at position 0
        if i == 0 && found != 0 {
            return false;
        }
        pos += skip + found + literal.len();
    }

    // If the last literal is non-empty, we've already matched to the end of it.
    // If there's a trailing variable (last literal is empty), it must consume the rest.
    let last_literal = literals.last().unwrap_or(&"");
    if last_literal.is_empty() {
        // Trailing variable — must have at least 1 char remaining
        // (unless there's no variable at all, i.e., template is all literal)
        if literals.len() > 1 {
            return pos < uri.len();
        }
    }

    pos == uri.len()
}

// ============================================================================
// A2A skill helpers
//
// Shared helpers for A2A skill lookup and conversion, used by both
// traffic-mode and context-mode when A2A actors are served by
// McpServerDriver.  Single source of truth so the logic isn't
// duplicated across driver.rs, handlers.rs, and the context module.
// ============================================================================

/// Find an A2A skill by canonical name across both `state.skills[]` and
/// `state.agent_card.skills[]`.
///
/// Matches on `id` first, then falls back to `name` — consistent with
/// [`crate::engine::a2a::skill_name`] which exposes skills using the same
/// priority order.  Without the `name` fallback, skills defined with only
/// a `name` field would be visible to the LLM but uncallable.
///
/// Returns the raw skill JSON value if found.
pub fn find_a2a_skill(state: &Value, skill_id: &str) -> Option<Value> {
    // Try id match first (both top-level and agent_card locations)
    find_by_field(state, "skills", "id", skill_id)
        .or_else(|| {
            state
                .get("agent_card")
                .and_then(|ac| ac.get("skills"))
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .find(|s| s.get("id").and_then(Value::as_str) == Some(skill_id))
                })
                .cloned()
        })
        // Fall back to name match (mirrors a2a::skill_name() priority)
        .or_else(|| find_by_field(state, "skills", "name", skill_id))
        .or_else(|| {
            state
                .get("agent_card")
                .and_then(|ac| ac.get("skills"))
                .and_then(Value::as_array)
                .and_then(|arr| {
                    arr.iter()
                        .find(|s| s.get("name").and_then(Value::as_str) == Some(skill_id))
                })
                .cloned()
        })
}

/// Return the A2A skills array from state, checking both `state.skills`
/// and `state.agent_card.skills`.
///
/// Delegates to [`crate::engine::a2a::skill_array`].
pub fn a2a_skill_array(state: &Value) -> Option<&Vec<Value>> {
    crate::engine::a2a::skill_array(state)
}

/// Resolve the canonical tool name for an A2A skill.
///
/// Delegates to [`crate::engine::a2a::skill_name`].
pub fn a2a_skill_name(skill: &Value) -> Option<&str> {
    crate::engine::a2a::skill_name(skill)
}

/// Builds response content from A2A state fields (`task.message.parts`,
/// `artifacts`).
///
/// When A2A skills have no `responses[]` array, this function extracts
/// response content from the broader state object where A2A scenarios
/// typically store task messages and artifact data.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn build_a2a_response_content(state: &Value) -> Option<A2aResponseContent> {
    let mut parts = Vec::new();

    // Try task message parts
    if let Some(task_parts) = state
        .pointer("/task/message/parts")
        .and_then(Value::as_array)
    {
        for part in task_parts {
            if part.get("type").and_then(Value::as_str) == Some("text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                parts.push(text.to_string());
            }
        }
    }

    // Try artifacts (both task.artifacts and top-level state.artifacts)
    let artifact_sources = [state.pointer("/task/artifacts"), state.get("artifacts")];
    for source in artifact_sources.into_iter().flatten() {
        if let Some(artifacts) = source.as_array() {
            for artifact in artifacts {
                if let Some(content) = artifact.get("content").and_then(Value::as_str) {
                    parts.push(content.to_string());
                }
            }
        }
    }

    if parts.is_empty() {
        return None;
    }

    let task_status = state
        .pointer("/task/status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_string();

    // For input-required, prepend a signal so the LLM understands the
    // agent is asking a question and expects a follow-up response.
    let mut text = String::new();
    if task_status == "input-required" {
        text.push_str("[Agent requires additional input]\n");
    }
    text.push_str(&parts.join("\n"));

    Some(A2aResponseContent {
        text,
        status: task_status,
    })
}

/// Structured response from an A2A actor, including task status.
///
/// Returned by [`build_a2a_response_content`]. The `status` field
/// enables the drive loop to detect `input-required` states.
pub struct A2aResponseContent {
    /// Response text (message parts + artifacts concatenated).
    pub text: String,
    /// A2A task status (`"completed"`, `"input-required"`, etc.).
    pub status: String,
}

/// Strip internal fields from a state object for wire format.
pub fn strip_internal_fields(value: &Value, fields: &[&str]) -> Value {
    let Some(obj) = value.as_object() else {
        return value.clone();
    };
    let mut cleaned = obj.clone();
    for field in fields {
        cleaned.remove(*field);
    }
    Value::Object(cleaned)
}

/// Saturating conversion from `u64` to `usize` for parameter parsing.
pub fn u64_to_usize(v: u64) -> usize {
    usize::try_from(v).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Regression test for fuzz crash-122a961bf40b7b79ae0394f3747fe1bad54a25f1.
    /// Template with variables matched against URI starting with multi-byte
    /// UTF-8 character (Ƃ = U+0182, 2 bytes). The `skip` logic advanced by
    /// one byte instead of one character, landing mid-UTF-8-sequence and
    /// panicking on the next string slice.
    #[test]
    fn regression_fuzz_multibyte_uri_no_panic() {
        // Decoded from the fuzzer crash artifact
        let template = "{Z}4{}}4{}\x01";
        let uri = "\u{0182}4{}2=e";
        // Must not panic — result doesn't matter
        let _ = matches_uri_template(template, uri);
    }

    /// Multi-byte characters in both template and URI should not panic.
    #[test]
    fn multibyte_chars_in_template_and_uri() {
        let _ = matches_uri_template("{v}é{w}", "Ƃéx");
        let _ = matches_uri_template("préfixe{v}suffixe", "préfixeXsuffixe");
        let _ = matches_uri_template("{v}", "日本語");
        let _ = matches_uri_template("{a}{b}{c}", "αβγ");
    }

    // ── find_by_name / find_by_field ───────────────────────────────

    #[test]
    fn find_by_name_found() {
        let state = serde_json::json!({
            "tools": [
                {"name": "search", "desc": "web search"},
                {"name": "calc", "desc": "calculator"}
            ]
        });
        let result = find_by_name(&state, "tools", "calc").unwrap();
        assert_eq!(result["desc"], "calculator");
    }

    #[test]
    fn find_by_name_not_found() {
        let state = serde_json::json!({"tools": [{"name": "search"}]});
        assert!(find_by_name(&state, "tools", "missing").is_none());
    }

    #[test]
    fn find_by_name_missing_collection() {
        let state = serde_json::json!({});
        assert!(find_by_name(&state, "tools", "any").is_none());
    }

    #[test]
    fn find_by_field_custom_field() {
        let state = serde_json::json!({
            "skills": [
                {"id": "s1", "name": "translate"},
                {"id": "s2", "name": "summarize"}
            ]
        });
        let result = find_by_field(&state, "skills", "id", "s2").unwrap();
        assert_eq!(result["name"], "summarize");
    }

    // ── find_a2a_skill ──────────────────────────────────────────────

    #[test]
    fn find_a2a_skill_by_id_top_level() {
        let state = serde_json::json!({
            "skills": [{"id": "translate", "name": "Translate Text"}]
        });
        let skill = find_a2a_skill(&state, "translate").unwrap();
        assert_eq!(skill["name"], "Translate Text");
    }

    #[test]
    fn find_a2a_skill_by_id_in_agent_card() {
        let state = serde_json::json!({
            "agent_card": {
                "skills": [{"id": "analyze", "name": "Data Analysis"}]
            }
        });
        let skill = find_a2a_skill(&state, "analyze").unwrap();
        assert_eq!(skill["name"], "Data Analysis");
    }

    #[test]
    fn find_a2a_skill_falls_back_to_name() {
        let state = serde_json::json!({
            "skills": [{"name": "process-data"}]
        });
        // No "id" field — should match on "name"
        let skill = find_a2a_skill(&state, "process-data").unwrap();
        assert_eq!(skill["name"], "process-data");
    }

    #[test]
    fn find_a2a_skill_name_fallback_in_agent_card() {
        let state = serde_json::json!({
            "agent_card": {
                "skills": [{"name": "deep-search"}]
            }
        });
        let skill = find_a2a_skill(&state, "deep-search").unwrap();
        assert_eq!(skill["name"], "deep-search");
    }

    #[test]
    fn find_a2a_skill_not_found() {
        let state = serde_json::json!({
            "skills": [{"id": "a", "name": "b"}]
        });
        assert!(find_a2a_skill(&state, "nonexistent").is_none());
    }

    #[test]
    fn find_a2a_skill_empty_state() {
        assert!(find_a2a_skill(&serde_json::json!({}), "any").is_none());
    }

    // ── build_a2a_response_content ──────────────────────────────────

    #[test]
    fn build_a2a_response_from_task_parts() {
        let state = serde_json::json!({
            "task": {
                "status": "completed",
                "message": {
                    "parts": [
                        {"type": "text", "text": "Analysis complete."},
                        {"type": "text", "text": "Revenue is $1M."}
                    ]
                }
            }
        });
        let resp = build_a2a_response_content(&state).unwrap();
        assert_eq!(resp.status, "completed");
        assert!(resp.text.contains("Analysis complete."));
        assert!(resp.text.contains("Revenue is $1M."));
    }

    #[test]
    fn build_a2a_response_from_artifacts() {
        let state = serde_json::json!({
            "task": {
                "status": "completed",
                "artifacts": [{"content": "CSV data here"}]
            }
        });
        let resp = build_a2a_response_content(&state).unwrap();
        assert!(resp.text.contains("CSV data here"));
    }

    #[test]
    fn build_a2a_response_input_required() {
        let state = serde_json::json!({
            "task": {
                "status": "input-required",
                "message": {
                    "parts": [{"type": "text", "text": "What file?"}]
                }
            }
        });
        let resp = build_a2a_response_content(&state).unwrap();
        assert_eq!(resp.status, "input-required");
        assert!(resp.text.starts_with("[Agent requires additional input]"));
    }

    #[test]
    fn build_a2a_response_empty_returns_none() {
        let state = serde_json::json!({"task": {"status": "completed"}});
        assert!(build_a2a_response_content(&state).is_none());
    }

    #[test]
    fn build_a2a_response_top_level_artifacts() {
        let state = serde_json::json!({
            "artifacts": [{"content": "top-level artifact"}]
        });
        let resp = build_a2a_response_content(&state).unwrap();
        assert!(resp.text.contains("top-level artifact"));
    }

    // ── strip_internal_fields ───────────────────────────────────────

    #[test]
    fn strip_internal_fields_removes_specified() {
        let value = serde_json::json!({"name": "calc", "responses": [], "_internal": true});
        let stripped = strip_internal_fields(&value, &["responses", "_internal"]);
        assert!(stripped.get("name").is_some());
        assert!(stripped.get("responses").is_none());
        assert!(stripped.get("_internal").is_none());
    }

    #[test]
    fn strip_internal_fields_non_object_passthrough() {
        let value = serde_json::json!("just a string");
        let stripped = strip_internal_fields(&value, &["anything"]);
        assert_eq!(stripped, serde_json::json!("just a string"));
    }

    // ── u64_to_usize ────────────────────────────────────────────────

    #[test]
    fn u64_to_usize_normal() {
        assert_eq!(u64_to_usize(42), 42);
        assert_eq!(u64_to_usize(0), 0);
    }

    // ── find_matching_template ───────────────────────────────────────

    #[test]
    fn find_matching_template_found() {
        let state = serde_json::json!({
            "resource_templates": [
                {"uriTemplate": "file:///{path}", "name": "file"}
            ]
        });
        let result = find_matching_template(&state, "file:///etc/passwd").unwrap();
        assert_eq!(result["name"], "file");
    }

    #[test]
    fn find_matching_template_no_match() {
        let state = serde_json::json!({
            "resource_templates": [
                {"uriTemplate": "file:///{path}", "name": "file"}
            ]
        });
        assert!(find_matching_template(&state, "http://example.com").is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn prop_literal_self_match(literal in "[a-zA-Z0-9/_.-]{1,40}") {
            // A template with no {var} matches only itself
            prop_assert!(matches_uri_template(&literal, &literal),
                "literal template '{}' should match itself", literal);
        }

        #[test]
        fn prop_variable_substitution(
            prefix in "[a-z]{1,5}",
            value in "[a-z0-9]{1,5}",
            suffix in "[a-z]{1,5}",
        ) {
            // Character sets overlap — the matcher must correctly skip
            // past the variable segment even when it contains suffix chars.
            let template = format!("{prefix}{{var}}{suffix}");
            let uri = format!("{prefix}{value}{suffix}");
            prop_assert!(matches_uri_template(&template, &uri));
        }

        #[test]
        fn prop_empty_variable_rejected(
            prefix in "[a-z]{1,5}",
            suffix in "[a-z]{1,5}",
        ) {
            let template = format!("{prefix}{{x}}{suffix}");
            // URI with no chars between prefix and suffix — variable is empty
            let uri = format!("{prefix}{suffix}");
            prop_assert!(!matches_uri_template(&template, &uri),
                "template '{}' should NOT match '{}' (empty variable)", template, uri);
        }

        #[test]
        fn prop_no_panic_on_arbitrary(
            template in ".*",
            uri in ".*",
        ) {
            // Should never panic, regardless of input
            let _ = matches_uri_template(&template, &uri);
        }
    }
}
