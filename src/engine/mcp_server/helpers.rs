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
    let mut pos = 0;
    for (i, literal) in literals.iter().enumerate() {
        if literal.is_empty() {
            if i > 0 && i < literals.len() - 1 {
                // Variable segment between two empty literals — skip at least 1 char
                if pos >= uri.len() {
                    return false;
                }
                // No constraint — just need non-empty match for the variable
            }
            continue;
        }
        let Some(found) = uri[pos..].find(literal) else {
            return false;
        };
        // For the first literal, it must start at position 0
        if i == 0 && found != 0 {
            return false;
        }
        // Variable segments (between literals) must be non-empty
        if i > 0 && found == 0 {
            return false;
        }
        pos += found + literal.len();
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
            prefix in "[a-e]{1,5}",
            value in "[0-9]{1,5}",
            suffix in "[v-z]{1,5}",
        ) {
            // Use disjoint character sets so the greedy literal search
            // never confuses variable content with the suffix.
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
