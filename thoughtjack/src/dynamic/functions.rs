//! Built-in template functions (TJ-SPEC-009 F-008).
//!
//! Functions are invoked via `${fn.name(args)}` syntax in templates.
//! All functions are pure (no side effects) and return `None` on error.

use base64::Engine;

/// Evaluates a built-in function by name with the given arguments.
///
/// Returns `None` for unknown functions or evaluation errors.
///
/// Implements: TJ-SPEC-009 F-008
#[must_use]
pub fn evaluate_function(name: &str, args: &[String]) -> Option<String> {
    match name {
        "upper" => args.first().map(|s| s.to_uppercase()),
        "lower" => args.first().map(|s| s.to_lowercase()),
        "base64" => args
            .first()
            .map(|s| base64::engine::general_purpose::STANDARD.encode(s.as_bytes())),
        "json" => args
            .first()
            .map(|s| serde_json::to_string(s).unwrap_or_default()),
        "len" => args.first().map(|s| s.len().to_string()),
        "default" => {
            let first = args.first().filter(|s| !s.is_empty());
            Some(first.or_else(|| args.get(1)).cloned().unwrap_or_default())
        }
        "truncate" => {
            let s = args.first()?;
            let len: usize = args.get(1)?.parse().ok()?;
            Some(s.chars().take(len).collect())
        }
        "timestamp" => Some(chrono::Utc::now().timestamp().to_string()),
        "uuid" => Some(uuid::Uuid::new_v4().to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upper() {
        assert_eq!(
            evaluate_function("upper", &["hello".into()]),
            Some("HELLO".into())
        );
    }

    #[test]
    fn test_lower() {
        assert_eq!(
            evaluate_function("lower", &["HELLO".into()]),
            Some("hello".into())
        );
    }

    #[test]
    fn test_base64() {
        assert_eq!(
            evaluate_function("base64", &["hello".into()]),
            Some("aGVsbG8=".into())
        );
    }

    #[test]
    fn test_json() {
        let result = evaluate_function("json", &["hello world".into()]).unwrap();
        assert_eq!(result, "\"hello world\"");
    }

    #[test]
    fn test_len() {
        assert_eq!(
            evaluate_function("len", &["hello".into()]),
            Some("5".into())
        );
    }

    #[test]
    fn test_default_with_value() {
        assert_eq!(
            evaluate_function("default", &["hello".into(), "fallback".into()]),
            Some("hello".into())
        );
    }

    #[test]
    fn test_default_with_empty() {
        assert_eq!(
            evaluate_function("default", &[String::new(), "fallback".into()]),
            Some("fallback".into())
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(
            evaluate_function("truncate", &["hello world".into(), "5".into()]),
            Some("hello".into())
        );
    }

    #[test]
    fn test_timestamp() {
        let result = evaluate_function("timestamp", &[]).unwrap();
        let ts: i64 = result.parse().unwrap();
        assert!(ts > 1_000_000_000);
    }

    #[test]
    fn test_uuid() {
        let result = evaluate_function("uuid", &[]).unwrap();
        assert_eq!(result.len(), 36); // UUID v4 format
        assert!(result.contains('-'));
    }

    #[test]
    fn test_unknown_function() {
        assert_eq!(evaluate_function("nonexistent", &[]), None);
    }

    #[test]
    fn test_truncate_missing_args() {
        assert_eq!(evaluate_function("truncate", &["hello".into()]), None);
    }

    #[test]
    fn test_upper_no_args() {
        assert_eq!(evaluate_function("upper", &[]), None);
    }
}
