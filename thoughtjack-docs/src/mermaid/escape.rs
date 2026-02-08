//! Mermaid label escaping and state ID slugification (TJ-SPEC-011 F-002).
//!
//! Mermaid's parser is fragile with special characters common in attack
//! scenarios — regex patterns, tool names with punctuation, URI strings, etc.

/// Slugify a phase name into a valid Mermaid `stateDiagram-v2` identifier.
///
/// Mermaid requires bare identifiers for state IDs — no spaces, dashes, or
/// special characters. Phase names in YAML are free-form strings, so this
/// function converts them to `[a-z0-9_]+` identifiers.
///
/// # Rules
/// 1. Lowercase the entire string
/// 2. Replace spaces and dashes with underscores
/// 3. Strip any character not in `[a-z0-9_]`
/// 4. Collapse consecutive underscores
/// 5. Trim leading/trailing underscores
/// 6. If result is empty, use `phase_{index}`
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn slugify_phase_name(name: &str, index: usize) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c == ' ' || c == '-' {
                '_'
            } else if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                // Strip non-alnum
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect();

    // Collapse consecutive underscores
    while slug.contains("__") {
        slug = slug.replace("__", "_");
    }

    // Trim leading/trailing underscores
    slug = slug.trim_matches('_').to_string();

    if slug.is_empty() {
        format!("phase_{index}")
    } else {
        slug
    }
}

/// Wrap a label string in double quotes for safe Mermaid rendering.
///
/// Escapes internal double quotes by replacing `"` with `#quot;`
/// (Mermaid's HTML entity escape syntax).
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn quote_label(label: &str) -> String {
    let escaped = label.replace('"', "#quot;");
    format!("\"{escaped}\"")
}

/// Escape special characters in Mermaid text content.
///
/// Replaces characters that break Mermaid parsing (`#`, `&`, `<`, `>`)
/// with their Mermaid-safe HTML entity equivalents.
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn escape_mermaid_chars(text: &str) -> String {
    text.replace('#', "#35;")
        .replace('&', "#amp;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
}

/// Truncate a string to `max_len` characters, appending `...` if truncated.
///
/// Implements: TJ-SPEC-011 F-002
#[must_use]
pub fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slugify_simple() {
        assert_eq!(slugify_phase_name("trust_building", 0), "trust_building");
    }

    #[test]
    fn test_slugify_spaces() {
        assert_eq!(slugify_phase_name("trust building", 0), "trust_building");
    }

    #[test]
    fn test_slugify_dashes() {
        assert_eq!(slugify_phase_name("trust-building", 0), "trust_building");
    }

    #[test]
    fn test_slugify_mixed_case() {
        assert_eq!(slugify_phase_name("Trust Building", 0), "trust_building");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(
            slugify_phase_name("exploit (phase #1)", 0),
            "exploit_phase_1"
        );
    }

    #[test]
    fn test_slugify_consecutive_underscores() {
        assert_eq!(slugify_phase_name("a--b  c", 0), "a_b_c");
    }

    #[test]
    fn test_slugify_leading_trailing() {
        assert_eq!(slugify_phase_name("-leading-", 0), "leading");
    }

    #[test]
    fn test_slugify_empty_fallback() {
        assert_eq!(slugify_phase_name("", 0), "phase_0");
        assert_eq!(slugify_phase_name("!!!", 3), "phase_3");
    }

    #[test]
    fn test_slugify_numbers() {
        assert_eq!(slugify_phase_name("phase 42", 0), "phase_42");
    }

    #[test]
    fn test_quote_label_simple() {
        assert_eq!(quote_label("hello"), r#""hello""#);
    }

    #[test]
    fn test_quote_label_with_quotes() {
        assert_eq!(quote_label(r#"say "hi""#), r#""say #quot;hi#quot;""#);
    }

    #[test]
    fn test_escape_mermaid_chars() {
        assert_eq!(
            escape_mermaid_chars("a < b & c > d"),
            "a #lt; b #amp; c #gt; d"
        );
    }

    #[test]
    fn test_escape_hash() {
        assert_eq!(escape_mermaid_chars("phase #1"), "phase #35;1");
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("short", 40), "short");
    }

    #[test]
    fn test_truncate_long() {
        let long = "a".repeat(50);
        let result = truncate(&long, 40);
        assert!(result.len() <= 40);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_exact_boundary() {
        assert_eq!(truncate("exact", 5), "exact");
    }
}
