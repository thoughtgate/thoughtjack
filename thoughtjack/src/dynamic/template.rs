//! Template interpolation engine (TJ-SPEC-009 F-001).
//!
//! Performs single-pass `${...}` variable substitution in strings.
//! Missing variables resolve to empty string. No recursive evaluation.

use regex::Regex;
use std::sync::LazyLock;

use super::context::TemplateContext;

/// Regex for matching `${...}` template variables.
static TEMPLATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([^}]+)\}").expect("valid regex"));

/// Sentinel marker for escaped `$${` sequences.
const ESCAPE_SENTINEL: &str = "\x00ESC_DOLLAR\x00";

/// Resolves all `${...}` template variables in a string.
///
/// - `$${` is treated as a literal `${` (escape syntax).
/// - Missing variables resolve to empty string.
/// - No recursive evaluation: handler/match output is NOT re-interpolated.
///
/// Implements: TJ-SPEC-009 F-001
#[must_use]
pub fn resolve_template(template: &str, ctx: &TemplateContext) -> String {
    // Step 1: Replace escaped $${  with sentinel
    let working = template.replace("$${", ESCAPE_SENTINEL);

    // Step 2: Replace ${...} with resolved values
    let result = TEMPLATE_RE
        .replace_all(&working, |caps: &regex::Captures| {
            let path = &caps[1];
            ctx.get_variable(path).unwrap_or_default()
        })
        .to_string();

    // Step 3: Restore sentinel → ${
    result.replace(ESCAPE_SENTINEL, "${")
}

/// Returns `true` if the string contains any `${...}` template variables.
#[must_use]
pub fn has_templates(s: &str) -> bool {
    TEMPLATE_RE.is_match(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic::context::{ItemType, TemplateContext};
    use serde_json::json;

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            args: json!({"query": "hello world", "path": "/etc/passwd", "count": 5, "items": [{"name": "a"}, {"name": "b"}]}),
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

    #[test]
    fn test_basic_interpolation() {
        let ctx = make_ctx();
        let result = resolve_template("Search: ${args.query}", &ctx);
        assert_eq!(result, "Search: hello world");
    }

    #[test]
    fn test_multiple_variables() {
        let ctx = make_ctx();
        let result = resolve_template("Tool: ${tool.name}, Phase: ${phase.name}", &ctx);
        assert_eq!(result, "Tool: web_search, Phase: exploit");
    }

    // EC-DYN-001: missing variable
    #[test]
    fn test_missing_variable_empty_string() {
        let ctx = make_ctx();
        let result = resolve_template("Query: ${args.missing}", &ctx);
        assert_eq!(result, "Query: ");
    }

    // Escaped $${ handling
    #[test]
    fn test_escaped_dollar() {
        let ctx = make_ctx();
        let result = resolve_template("Literal: $${not.resolved}", &ctx);
        assert_eq!(result, "Literal: ${not.resolved}");
    }

    #[test]
    fn test_no_templates() {
        let ctx = make_ctx();
        let result = resolve_template("Plain text", &ctx);
        assert_eq!(result, "Plain text");
    }

    #[test]
    fn test_nested_path() {
        let ctx = make_ctx();
        let result = resolve_template("Item: ${args.items[0].name}", &ctx);
        assert_eq!(result, "Item: a");
    }

    // EC-DYN-014: special characters passed through
    #[test]
    fn test_special_characters_passthrough() {
        let mut ctx = make_ctx();
        ctx.args = json!({"query": "<script>alert(1)</script>"});
        let result = resolve_template("${args.query}", &ctx);
        assert_eq!(result, "<script>alert(1)</script>");
    }

    // EC-DYN-017: env var with dollar sign — uses PATH which always exists
    // and contains no `${` sequences, so no reinterpretation can occur.
    #[test]
    fn test_env_var_no_reinterpretation() {
        let ctx = make_ctx();
        // PATH is always set and its value should be returned verbatim
        let result = resolve_template("${env.PATH}", &ctx);
        assert!(!result.is_empty(), "PATH env var should be non-empty");
        assert_eq!(result, std::env::var("PATH").unwrap_or_default());
    }

    #[test]
    fn test_has_templates() {
        assert!(has_templates("Hello ${name}"));
        assert!(!has_templates("Hello world"));
        assert!(!has_templates("Cost: $100"));
    }

    #[test]
    fn test_numeric_value() {
        let ctx = make_ctx();
        let result = resolve_template("Count: ${args.count}", &ctx);
        assert_eq!(result, "Count: 5");
    }

    // Function calls in templates
    #[test]
    fn test_function_in_template() {
        let ctx = make_ctx();
        let result = resolve_template("Upper: ${fn.upper(args.query)}", &ctx);
        assert_eq!(result, "Upper: HELLO WORLD");
    }

    #[test]
    fn test_function_len() {
        let ctx = make_ctx();
        let result = resolve_template("Len: ${fn.len(args.query)}", &ctx);
        assert_eq!(result, "Len: 11");
    }
}
