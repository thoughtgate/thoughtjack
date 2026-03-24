use std::collections::HashMap;

use serde_json::{Value, json};
use tokio::sync::watch;

use super::types::ToolDefinition;

/// Builds the merged tool list and routing table for context-mode.
///
/// When tool names collide across actors, **both** tools are disambiguated
/// by prefixing `{actor_name}__{tool_name}`. Tools without collisions retain
/// their original names. Disambiguated tools have the actor name prepended
/// to their description for LLM clarity.
///
/// This follows the pattern established by real agent frameworks (`LangGraph`,
/// `CrewAI`, `AutoGen`, A2A spec) where agents are presented as distinct entities
/// rather than merged into a flat tool namespace.
///
/// Implements: TJ-SPEC-022 F-023
#[must_use]
pub fn build_tool_roster(
    server_tool_watches: &[(String, watch::Receiver<Vec<ToolDefinition>>)],
) -> (Vec<ToolDefinition>, HashMap<String, String>) {
    // Pass 1: collect all (actor, tool) pairs and detect name collisions
    let mut pairs: Vec<(String, ToolDefinition)> = Vec::new();
    let mut name_actors: HashMap<String, Vec<String>> = HashMap::new();

    for (actor_name, watch_rx) in server_tool_watches {
        for tool in watch_rx.borrow().iter() {
            name_actors
                .entry(tool.name.clone())
                .or_default()
                .push(actor_name.clone());
            pairs.push((actor_name.clone(), tool.clone()));
        }
    }

    // Pass 2: build tool list with bidirectional disambiguation
    let mut all_tools = Vec::new();
    let mut router: HashMap<String, String> = HashMap::new();

    for (actor_name, tool) in pairs {
        let is_collision = name_actors[&tool.name].len() > 1;
        let effective_name = if is_collision {
            let disambiguated = format!("{}__{}", sanitize_tool_name(&actor_name), tool.name);
            tracing::info!(
                original = %tool.name,
                disambiguated = %disambiguated,
                actor = %actor_name,
                "tool name collision — disambiguating with actor prefix"
            );
            disambiguated
        } else {
            tool.name.clone()
        };

        let description = if is_collision {
            format!("[Server: {}] {}", actor_name, tool.description)
        } else {
            tool.description
        };

        router.insert(effective_name.clone(), actor_name);
        all_tools.push(ToolDefinition {
            name: effective_name,
            description,
            parameters: tool.parameters,
        });
    }

    (all_tools, router)
}

/// Extracts tool definitions from an actor's effective state.
///
/// For MCP actors: reads `state.tools[]` and maps `name`, `description`,
/// `inputSchema`. For A2A actors: reads `state.skills[]` with permissive schema.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_tool_definitions(state: &Value) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();

    // MCP tools
    if let Some(tool_array) = state.get("tools").and_then(Value::as_array) {
        for tool in tool_array {
            let name = sanitize_tool_name(tool.get("name").and_then(Value::as_str).unwrap_or(""));
            if name.is_empty() {
                continue;
            }
            let description = tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"}));
            tools.push(ToolDefinition {
                name,
                description,
                parameters,
            });
        }
    }

    // A2A skills — uses shared helpers from mcp_server/helpers.rs to avoid
    // duplicating the skill lookup logic across context.rs, handlers.rs,
    // and driver.rs. See helpers::a2a_skill_array() for the full lookup chain.
    if let Some(skill_array) = crate::engine::mcp_server::helpers::a2a_skill_array(state) {
        for skill in skill_array {
            let Some(raw_name) = crate::engine::mcp_server::helpers::a2a_skill_name(skill) else {
                continue;
            };
            let name = sanitize_tool_name(raw_name);
            if name.is_empty() {
                continue;
            }
            let description = skill
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            tools.push(ToolDefinition {
                name,
                description,
                parameters: json!({"type": "object", "properties": {}, "additionalProperties": true}),
            });
        }
    }

    tools
}

/// Mode-aware tool definition extraction for a single actor.
///
/// For `a2a_server` actors: creates one tool per agent (actor name as tool
/// name, Agent Card metadata + skill roster in description, single `message`
/// parameter). For all other modes: delegates to [`extract_tool_definitions`].
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_tool_definitions_for_actor(
    state: &Value,
    actor_name: &str,
    mode: &str,
) -> Vec<ToolDefinition> {
    if mode == "a2a_server" {
        extract_a2a_agent_tool(actor_name, state)
            .into_iter()
            .collect()
    } else {
        extract_tool_definitions(state)
    }
}

/// Builds a single tool definition representing an A2A agent.
///
/// Creates one tool per agent (not per skill). The tool name is the actor
/// name, the description combines Agent Card metadata with a skill roster,
/// and the tool accepts a single `message` parameter. This matches how
/// real A2A orchestrators (Google ADK, etc.) present agents to the LLM.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn extract_a2a_agent_tool(actor_name: &str, state: &Value) -> Option<ToolDefinition> {
    use std::fmt::Write;

    use crate::engine::mcp_server::helpers::{a2a_skill_array, a2a_skill_name};

    let card = state.get("agent_card").unwrap_or(state);

    // Need at least an agent_card or skills to produce a tool
    let has_card = card.get("name").is_some() || card.get("description").is_some();
    let has_skills = a2a_skill_array(state).is_some_and(|arr| !arr.is_empty());
    if !has_card && !has_skills {
        return None;
    }

    let mut desc = String::new();

    // Agent identity
    let agent_name = card["name"].as_str().unwrap_or(actor_name);
    let agent_desc = card["description"].as_str().unwrap_or("");
    let url = card["url"].as_str().unwrap_or("");
    let version = card["version"].as_str().unwrap_or("");

    desc.push_str(agent_desc);
    if !url.is_empty() || !version.is_empty() {
        let _ = write!(desc, "\n\nAgent: {agent_name}");
        if !version.is_empty() {
            let _ = write!(desc, " (v{version})");
        }
        if !url.is_empty() {
            let _ = write!(desc, "\nURL: {url}");
        }
    }

    // Capabilities
    if let Some(caps) = card.get("capabilities") {
        let streaming = caps["streaming"].as_bool().unwrap_or(false);
        let push = caps["pushNotifications"].as_bool().unwrap_or(false);
        let _ = write!(
            desc,
            "\nCapabilities: streaming={streaming}, pushNotifications={push}"
        );
    }

    // Authentication
    if let Some(auth) = card.get("authentication") {
        if let Some(schemes) = auth["schemes"].as_array() {
            let scheme_strs: Vec<&str> = schemes.iter().filter_map(Value::as_str).collect();
            if !scheme_strs.is_empty() {
                let _ = write!(desc, "\nAuthentication: {}", scheme_strs.join(", "));
            }
        }
        if let Some(creds) = auth["credentials"].as_array() {
            let cred_strs: Vec<&str> = creds.iter().filter_map(Value::as_str).collect();
            if !cred_strs.is_empty() {
                let _ = write!(desc, "\nCredentials: {}", cred_strs.join(", "));
            }
        }
    }

    // Webhook / push notification registration (R5)
    if let Some(webhook) = state.get("webhook_registration") {
        if let Some(wh_url) = webhook["url"].as_str() {
            let _ = write!(desc, "\nWebhook URL: {wh_url}");
        }
        if let Some(wh_creds) = webhook
            .pointer("/authentication/credentials")
            .and_then(Value::as_str)
        {
            let _ = write!(desc, "\nWebhook Credentials: {wh_creds}");
        }
    }

    // Skill roster
    if let Some(skills) = a2a_skill_array(state)
        && !skills.is_empty()
    {
        desc.push_str("\n\nSkills:");
        for skill in skills {
            let skill_id = a2a_skill_name(skill).unwrap_or("unknown");
            let skill_name = skill["name"].as_str().unwrap_or("");
            let skill_desc = skill["description"].as_str().unwrap_or("");

            let _ = write!(desc, "\n- {skill_id}");
            if !skill_name.is_empty() && skill_name != skill_id {
                let _ = write!(desc, " ({skill_name})");
            }
            if !skill_desc.is_empty() {
                let _ = write!(desc, ": {skill_desc}");
            }

            // Include examples to steer LLM message composition
            if let Some(examples) = skill["examples"].as_array() {
                let ex_strs: Vec<&str> = examples.iter().filter_map(Value::as_str).collect();
                if !ex_strs.is_empty() {
                    let quoted: Vec<String> = ex_strs.iter().map(|e| format!("\"{e}\"")).collect();
                    let _ = write!(desc, "\n  Examples: {}", quoted.join(", "));
                }
            }
        }
    }

    let tool_name = sanitize_tool_name(actor_name);
    if tool_name.is_empty() {
        return None;
    }

    Some(ToolDefinition {
        name: tool_name,
        description: desc,
        parameters: json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Task message to send to this agent"
                }
            },
            "required": ["message"]
        }),
    })
}

/// Sanitise a tool name for LLM API compatibility.
///
/// Ensures the name matches `^[a-zA-Z0-9_-]+$` (required by `OpenAI` and
/// other providers). Characters outside that set are replaced with `_`,
/// consecutive underscores are collapsed, and leading/trailing underscores
/// are trimmed.
///
/// Implements: TJ-SPEC-022 F-001
#[must_use]
pub fn sanitize_tool_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // Collapse consecutive underscores and trim leading/trailing
    let mut result = String::with_capacity(replaced.len());
    let mut prev_underscore = true; // treat start as underscore to trim leading
    for c in replaced.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
            result.push(c);
        }
    }
    // Trim trailing underscore
    if result.ends_with('_') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn test_extract_tool_definitions_mcp() {
        let state = json!({
            "tools": [
                {
                    "name": "search",
                    "description": "Search the web",
                    "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
                },
                {
                    "name": "read",
                    "description": "Read a file"
                }
            ]
        });
        let tools = extract_tool_definitions(&state);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[1].name, "read");
        assert_eq!(tools[1].parameters, json!({"type": "object"}));
    }

    #[test]
    fn test_extract_tool_definitions_a2a() {
        let state = json!({
            "skills": [
                {"name": "translate", "description": "Translate text"},
                {"id": "summarize", "description": "Summarize text"}
            ]
        });
        let tools = extract_tool_definitions(&state);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "translate");
        assert_eq!(tools[1].name, "summarize");
    }

    #[test]
    fn test_extract_tool_definitions_empty() {
        let tools = extract_tool_definitions(&json!({}));
        assert!(tools.is_empty());
    }
}
