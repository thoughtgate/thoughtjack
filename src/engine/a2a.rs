//! Shared A2A protocol helpers.
//!
//! Functions for extracting skills and metadata from OATF A2A state,
//! used by both the engine (`mcp_server/helpers.rs`) and the transport
//! (`context/tool_roster.rs`) modules.  Keeping them here avoids a
//! transport → engine coupling violation.

use serde_json::Value;

/// Returns the A2A skills array from state, checking both `state.skills`
/// and `state.agent_card.skills`.
///
/// Implements: TJ-SPEC-017 F-001
#[must_use]
pub fn skill_array(state: &Value) -> Option<&Vec<Value>> {
    state.get("skills").and_then(Value::as_array).or_else(|| {
        state
            .get("agent_card")
            .and_then(|ac| ac.get("skills"))
            .and_then(Value::as_array)
    })
}

/// Resolves the canonical tool name for an A2A skill.
///
/// Prefers `id` (machine-readable, e.g. `"analyze-data"`) over `name`
/// (human-readable, e.g. `"Data Analysis"`) because LLM API providers
/// restrict tool function names to `[a-zA-Z0-9_-]+`.
///
/// Implements: TJ-SPEC-017 F-001
#[must_use]
pub fn skill_name(skill: &Value) -> Option<&str> {
    skill
        .get("id")
        .or_else(|| skill.get("name"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skill_array_from_top_level() {
        let state = json!({"skills": [{"id": "a"}, {"id": "b"}]});
        assert_eq!(skill_array(&state).unwrap().len(), 2);
    }

    #[test]
    fn skill_array_from_agent_card() {
        let state = json!({"agent_card": {"skills": [{"id": "x"}]}});
        assert_eq!(skill_array(&state).unwrap().len(), 1);
    }

    #[test]
    fn skill_array_prefers_top_level() {
        let state = json!({
            "skills": [{"id": "top"}],
            "agent_card": {"skills": [{"id": "card"}]}
        });
        let arr = skill_array(&state).unwrap();
        assert_eq!(arr[0]["id"], "top");
    }

    #[test]
    fn skill_name_prefers_id() {
        let skill = json!({"id": "analyze-data", "name": "Data Analysis"});
        assert_eq!(skill_name(&skill).unwrap(), "analyze-data");
    }

    #[test]
    fn skill_name_falls_back_to_name() {
        let skill = json!({"name": "Data Analysis"});
        assert_eq!(skill_name(&skill).unwrap(), "Data Analysis");
    }

    #[test]
    fn skill_name_returns_none_for_empty() {
        let skill = json!({});
        assert!(skill_name(&skill).is_none());
    }
}
