//! Template context for dynamic response resolution (TJ-SPEC-009).
//!
//! The [`TemplateContext`] struct carries all request-time data needed for
//! template interpolation, match evaluation, and handler invocation.

use serde_json::Value;

use super::functions::evaluate_function;

/// The type of MCP item being handled.
///
/// Implements: TJ-SPEC-009 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemType {
    /// A tool call (`tools/call`)
    Tool,
    /// A resource read (`resources/read`)
    Resource,
    /// A prompt get (`prompts/get`)
    Prompt,
}

/// Request-time context for template interpolation and match evaluation.
///
/// Carries all data needed by the dynamic response pipeline: request
/// arguments, item metadata, phase state, and connection info.
///
/// Implements: TJ-SPEC-009 F-001
pub struct TemplateContext {
    /// Request arguments (from `params.arguments` for tools/prompts)
    pub args: Value,
    /// Name of the item being handled (tool name, resource URI, prompt name)
    pub item_name: String,
    /// Type of item being handled
    pub item_type: ItemType,
    /// Number of times this item has been called (1-indexed, current call)
    pub call_count: u64,
    /// Current phase name (or "baseline")
    pub phase_name: String,
    /// Current phase index (-1 for baseline)
    pub phase_index: i64,
    /// JSON-RPC request ID (None for notifications)
    pub request_id: Option<Value>,
    /// JSON-RPC method name
    pub request_method: String,
    /// Connection identifier
    pub connection_id: u64,
    /// Resource display name (for `ItemType::Resource`)
    pub resource_name: Option<String>,
    /// Resource MIME type (for `ItemType::Resource`)
    pub resource_mime_type: Option<String>,
}

impl TemplateContext {
    /// Resolves a variable path to a string value.
    ///
    /// Routes by namespace prefix (`args.*`, `tool.*`, `resource.*`,
    /// `prompt.*`, `phase.*`, `request.*`, `connection.*`, `env.*`,
    /// `fn.*`). Missing variables return `None`.
    ///
    /// Implements: TJ-SPEC-009 F-001
    #[must_use]
    pub fn get_variable(&self, path: &str) -> Option<String> {
        match path {
            // Full args object
            "args" => Some(self.args.to_string()),

            // Tool namespace
            "tool.name" if self.item_type == ItemType::Tool => Some(self.item_name.clone()),
            "tool.call_count" if self.item_type == ItemType::Tool => {
                Some(self.call_count.to_string())
            }

            // Resource namespace
            "resource.uri" if self.item_type == ItemType::Resource => Some(self.item_name.clone()),
            "resource.name" if self.item_type == ItemType::Resource => self.resource_name.clone(),
            "resource.mimeType" if self.item_type == ItemType::Resource => {
                self.resource_mime_type.clone()
            }
            "resource.call_count" if self.item_type == ItemType::Resource => {
                Some(self.call_count.to_string())
            }

            // Prompt namespace
            "prompt.name" if self.item_type == ItemType::Prompt => Some(self.item_name.clone()),
            "prompt.call_count" if self.item_type == ItemType::Prompt => {
                Some(self.call_count.to_string())
            }

            // Phase namespace
            "phase.name" => Some(self.phase_name.clone()),
            "phase.index" => Some(self.phase_index.to_string()),

            // Request namespace
            "request.id" => self.request_id.as_ref().map(ToString::to_string),
            "request.method" => Some(self.request_method.clone()),

            // Connection namespace
            "connection.id" => Some(self.connection_id.to_string()),

            // Args sub-path
            p if p.starts_with("args.") => {
                let key = &p[5..];
                resolve_json_path(&self.args, key)
            }

            // Environment variables
            p if p.starts_with("env.") => {
                let var_name = &p[4..];
                Some(std::env::var(var_name).unwrap_or_default())
            }

            // Built-in functions
            p if p.starts_with("fn.") => {
                let func_expr = &p[3..];
                resolve_function_call(func_expr, self)
            }

            _ => None,
        }
    }
}

/// Resolves a JSON path like `user.profile.name` or `items[0].id`.
///
/// Returns `None` for missing paths. Null JSON values return `"null"`.
///
/// Implements: TJ-SPEC-009 F-001
fn resolve_json_path(value: &Value, path: &str) -> Option<String> {
    let mut current = value;

    // Parse segments: handles `field`, `[N]`, `field[N]`
    let segment_re = regex::Regex::new(r"(\w+)|\[(-?\d+)\]").ok()?;

    for cap in segment_re.captures_iter(path) {
        if let Some(key) = cap.get(1) {
            current = current.get(key.as_str())?;
        } else if let Some(idx) = cap.get(2) {
            let index: i64 = idx.as_str().parse().ok()?;
            let arr = current.as_array()?;
            let actual_index = if index < 0 {
                let len = i64::try_from(arr.len()).ok()?;
                if -index > len {
                    return None;
                }
                usize::try_from(len + index).ok()?
            } else {
                usize::try_from(index).ok()?
            };
            current = arr.get(actual_index)?;
        }
    }

    match current {
        Value::String(s) => Some(s.clone()),
        Value::Null => Some("null".to_string()),
        other => Some(other.to_string()),
    }
}

/// Parses and evaluates a function call expression like `upper(args.query)`.
fn resolve_function_call(expr: &str, ctx: &TemplateContext) -> Option<String> {
    // Parse: name(arg1, arg2, ...)
    let paren_start = expr.find('(')?;
    let paren_end = expr.rfind(')')?;
    if paren_end <= paren_start {
        return None;
    }

    let name = &expr[..paren_start];
    let args_str = &expr[paren_start + 1..paren_end];

    // Split args by comma, resolve each as a variable or literal
    let args: Vec<String> = if args_str.trim().is_empty() {
        vec![]
    } else {
        args_str
            .split(',')
            .map(|a| {
                let trimmed = a.trim();
                // Try resolving as a variable first
                ctx.get_variable(trimmed)
                    .unwrap_or_else(|| trimmed.trim_matches('"').to_string())
            })
            .collect()
    };

    evaluate_function(name, &args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_tool_context() -> TemplateContext {
        TemplateContext {
            args: json!({"query": "test search", "path": "/etc/passwd", "items": [{"id": 1}, {"id": 2}]}),
            item_name: "web_search".to_string(),
            item_type: ItemType::Tool,
            call_count: 3,
            phase_name: "exploit".to_string(),
            phase_index: 2,
            request_id: Some(json!("req-123")),
            request_method: "tools/call".to_string(),
            connection_id: 42,
            resource_name: None,
            resource_mime_type: None,
        }
    }

    fn make_resource_context() -> TemplateContext {
        TemplateContext {
            args: json!({}),
            item_name: "file:///etc/passwd".to_string(),
            item_type: ItemType::Resource,
            call_count: 1,
            phase_name: "baseline".to_string(),
            phase_index: -1,
            request_id: Some(json!(5)),
            request_method: "resources/read".to_string(),
            connection_id: 1,
            resource_name: Some("Password file".to_string()),
            resource_mime_type: Some("text/plain".to_string()),
        }
    }

    #[test]
    fn test_tool_variables() {
        let ctx = make_tool_context();
        assert_eq!(
            ctx.get_variable("tool.name"),
            Some("web_search".to_string())
        );
        assert_eq!(ctx.get_variable("tool.call_count"), Some("3".to_string()));
    }

    #[test]
    fn test_args_resolution() {
        let ctx = make_tool_context();
        assert_eq!(
            ctx.get_variable("args.query"),
            Some("test search".to_string())
        );
        assert_eq!(
            ctx.get_variable("args.path"),
            Some("/etc/passwd".to_string())
        );
        assert_eq!(ctx.get_variable("args.items[0].id"), Some("1".to_string()));
        assert_eq!(ctx.get_variable("args.items[1].id"), Some("2".to_string()));
    }

    #[test]
    fn test_full_args_object() {
        let ctx = make_tool_context();
        let result = ctx.get_variable("args").unwrap();
        assert!(result.contains("query"));
    }

    #[test]
    fn test_phase_variables() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("phase.name"), Some("exploit".to_string()));
        assert_eq!(ctx.get_variable("phase.index"), Some("2".to_string()));
    }

    #[test]
    fn test_request_variables() {
        let ctx = make_tool_context();
        assert_eq!(
            ctx.get_variable("request.id"),
            Some("\"req-123\"".to_string())
        );
        assert_eq!(
            ctx.get_variable("request.method"),
            Some("tools/call".to_string())
        );
    }

    #[test]
    fn test_connection_variable() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("connection.id"), Some("42".to_string()));
    }

    #[test]
    fn test_resource_variables() {
        let ctx = make_resource_context();
        assert_eq!(
            ctx.get_variable("resource.uri"),
            Some("file:///etc/passwd".to_string())
        );
        assert_eq!(
            ctx.get_variable("resource.name"),
            Some("Password file".to_string())
        );
        assert_eq!(
            ctx.get_variable("resource.mimeType"),
            Some("text/plain".to_string())
        );
        assert_eq!(
            ctx.get_variable("resource.call_count"),
            Some("1".to_string())
        );
    }

    #[test]
    fn test_missing_variable() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("args.missing"), None);
        assert_eq!(ctx.get_variable("unknown.path"), None);
    }

    // EC-DYN-002: deeply nested access
    #[test]
    fn test_deeply_nested_missing() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("args.user.profile.settings.theme"), None);
    }

    // EC-DYN-019: non-string argument comparison
    #[test]
    fn test_numeric_value_to_string() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("args.items[0].id"), Some("1".to_string()));
    }

    // EC-DYN-028: missing resource name
    #[test]
    fn test_missing_resource_name() {
        let mut ctx = make_resource_context();
        ctx.resource_name = None;
        assert_eq!(ctx.get_variable("resource.name"), None);
    }

    // Negative indexing
    #[test]
    fn test_negative_array_index() {
        let ctx = make_tool_context();
        assert_eq!(ctx.get_variable("args.items[-1].id"), Some("2".to_string()));
    }

    // Null value handling
    #[test]
    fn test_null_value() {
        let mut ctx = make_tool_context();
        ctx.args = json!({"key": null});
        assert_eq!(ctx.get_variable("args.key"), Some("null".to_string()));
    }

    // Tool namespace not available for Resource type
    #[test]
    fn test_wrong_namespace() {
        let ctx = make_resource_context();
        assert_eq!(ctx.get_variable("tool.name"), None);
    }
}
