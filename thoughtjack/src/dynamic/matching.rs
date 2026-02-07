//! Conditional match evaluation (TJ-SPEC-009 F-002).
//!
//! Compiles config-level match conditions into evaluatable types and
//! resolves match blocks against a [`TemplateContext`].

use tracing::warn;

use crate::config::schema::MatchConditionConfig;
use crate::error::ThoughtJackError;

use super::context::TemplateContext;

/// A compiled match condition ready for evaluation.
///
/// Implements: TJ-SPEC-009 F-002
#[derive(Debug)]
pub enum MatchCondition {
    /// Glob pattern match
    Glob(glob::Pattern),
    /// Regex match
    Regex(regex::Regex),
    /// Substring match
    Contains(String),
    /// Prefix match
    Prefix(String),
    /// Suffix match
    Suffix(String),
    /// Field existence check
    Exists(bool),
    /// Numeric greater-than comparison
    GreaterThan(f64),
    /// Numeric less-than comparison
    LessThan(f64),
    /// Any of the listed conditions must match (OR)
    AnyOf(Vec<Self>),
}

impl MatchCondition {
    /// Compiles a config-level match condition into an evaluatable form.
    ///
    /// Globs and regexes are compiled at load time for performance.
    /// Regex patterns are size-limited to prevent compile-time denial of service.
    ///
    /// # Errors
    ///
    /// Returns an error if a glob or regex pattern is invalid.
    ///
    /// Implements: TJ-SPEC-009 F-002
    pub fn compile(config: &MatchConditionConfig) -> Result<Self, ThoughtJackError> {
        match config {
            MatchConditionConfig::Operator {
                contains,
                prefix,
                suffix,
                exists,
                gt,
                lt,
                any_of,
            } => {
                // any_of in operator form
                if let Some(patterns) = any_of {
                    let conditions: Result<Vec<_>, _> =
                        patterns.iter().map(|p| compile_single_string(p)).collect();
                    return Ok(Self::AnyOf(conditions?));
                }
                if let Some(val) = exists {
                    return Ok(Self::Exists(*val));
                }
                if let Some(val) = gt {
                    return Ok(Self::GreaterThan(*val));
                }
                if let Some(val) = lt {
                    return Ok(Self::LessThan(*val));
                }
                if let Some(val) = contains {
                    return Ok(Self::Contains(val.clone()));
                }
                if let Some(val) = prefix {
                    return Ok(Self::Prefix(val.clone()));
                }
                if let Some(val) = suffix {
                    return Ok(Self::Suffix(val.clone()));
                }
                // Empty operator — treat as always-match
                Ok(Self::Exists(true))
            }
            MatchConditionConfig::GlobList(patterns) => {
                let conditions: Result<Vec<_>, _> =
                    patterns.iter().map(|p| compile_single_string(p)).collect();
                Ok(Self::AnyOf(conditions?))
            }
            MatchConditionConfig::Single(pattern) => compile_single_string(pattern),
        }
    }

    /// Tests whether this condition matches the given value.
    ///
    /// `value` is `None` when the field does not exist.
    ///
    /// Implements: TJ-SPEC-009 F-002
    #[must_use]
    pub fn matches(&self, value: Option<&str>) -> bool {
        match self {
            Self::Glob(pattern) => value.is_some_and(|v| pattern.matches(v)),
            Self::Regex(re) => value.is_some_and(|v| {
                // EC-DYN-011: regex timeout handled via size limit at compile time.
                // For catastrophic backtracking, Rust's regex crate is safe by default
                // (no exponential backtracking). The size_limit prevents pathological
                // compilation. At runtime, regex::Regex guarantees linear-time matching.
                re.is_match(v)
            }),
            Self::Contains(s) => value.is_some_and(|v| v.contains(s.as_str())),
            Self::Prefix(s) => value.is_some_and(|v| v.starts_with(s.as_str())),
            Self::Suffix(s) => value.is_some_and(|v| v.ends_with(s.as_str())),
            Self::Exists(expected) => {
                let exists = value.is_some();
                exists == *expected
            }
            Self::GreaterThan(threshold) => value
                .and_then(|v| v.parse::<f64>().ok())
                .is_some_and(|n| n > *threshold),
            Self::LessThan(threshold) => value
                .and_then(|v| v.parse::<f64>().ok())
                .is_some_and(|n| n < *threshold),
            Self::AnyOf(conditions) => conditions.iter().any(|c| c.matches(value)),
        }
    }
}

/// Compiles a single string pattern (bare string) into a `MatchCondition`.
///
/// If the string starts with `regex:`, it's compiled as a regex.
/// Otherwise, it's compiled as a glob pattern.
fn compile_single_string(pattern: &str) -> Result<MatchCondition, ThoughtJackError> {
    if let Some(regex_str) = pattern.strip_prefix("regex:") {
        let re = regex::RegexBuilder::new(regex_str)
            .size_limit(1 << 20) // 1MB compile limit
            .build()
            .map_err(|e| {
                ThoughtJackError::Config(crate::error::ConfigError::InvalidValue {
                    field: "match condition regex".to_string(),
                    value: regex_str.to_string(),
                    expected: format!("valid regex: {e}"),
                })
            })?;
        Ok(MatchCondition::Regex(re))
    } else {
        let pat = glob::Pattern::new(pattern).map_err(|e| {
            ThoughtJackError::Config(crate::error::ConfigError::InvalidValue {
                field: "match condition glob".to_string(),
                value: pattern.to_string(),
                expected: format!("valid glob pattern: {e}"),
            })
        })?;
        Ok(MatchCondition::Glob(pat))
    }
}

/// A compiled when clause with named conditions.
///
/// All conditions in a when clause must match (AND).
///
/// Implements: TJ-SPEC-009 F-002
pub struct WhenClause {
    /// Field path → compiled condition pairs
    pub conditions: Vec<(String, MatchCondition)>,
}

impl WhenClause {
    /// Tests whether all conditions in this clause match the given context.
    ///
    /// Implements: TJ-SPEC-009 F-002
    #[must_use]
    pub fn matches(&self, ctx: &TemplateContext) -> bool {
        self.conditions.iter().all(|(path, condition)| {
            let value = ctx.get_variable(path);
            condition.matches(value.as_deref())
        })
    }
}

/// A compiled match branch (when or default).
///
/// Implements: TJ-SPEC-009 F-002
pub enum CompiledBranch {
    /// Conditional branch
    When {
        /// The when clause to evaluate
        clause: WhenClause,
        /// Index into the original config's match branches
        index: usize,
    },
    /// Default fallback branch
    Default {
        /// Index into the original config's match branches
        index: usize,
    },
}

/// A compiled match block with ordered branches.
///
/// Implements: TJ-SPEC-009 F-002
pub struct MatchBlock {
    /// Ordered branches (first match wins)
    pub branches: Vec<CompiledBranch>,
}

impl MatchBlock {
    /// Compiles a list of match branch configs into a match block.
    ///
    /// # Errors
    ///
    /// Returns an error if any condition fails to compile.
    ///
    /// Implements: TJ-SPEC-009 F-002
    pub fn compile(
        configs: &[crate::config::schema::MatchBranchConfig],
    ) -> Result<Self, ThoughtJackError> {
        let mut branches = Vec::with_capacity(configs.len());

        for (idx, config) in configs.iter().enumerate() {
            match config {
                crate::config::schema::MatchBranchConfig::When { when, .. } => {
                    let mut conditions = Vec::with_capacity(when.len());
                    for (path, cond_config) in when {
                        let compiled = MatchCondition::compile(cond_config)?;
                        conditions.push((path.clone(), compiled));
                    }
                    branches.push(CompiledBranch::When {
                        clause: WhenClause { conditions },
                        index: idx,
                    });
                }
                crate::config::schema::MatchBranchConfig::Default { .. } => {
                    if idx < configs.len() - 1 {
                        warn!(
                            "default branch at index {idx} is not the last branch; \
                             subsequent branches are unreachable"
                        );
                    }
                    branches.push(CompiledBranch::Default { index: idx });
                }
            }
        }

        Ok(Self { branches })
    }

    /// Evaluates the match block against the given context.
    ///
    /// Returns the index of the first matching branch, or `None` if no match.
    ///
    /// Implements: TJ-SPEC-009 F-002
    #[must_use]
    pub fn evaluate(&self, ctx: &TemplateContext) -> Option<usize> {
        for branch in &self.branches {
            match branch {
                CompiledBranch::When { clause, index } if clause.matches(ctx) => {
                    return Some(*index);
                }
                CompiledBranch::Default { index } => {
                    return Some(*index);
                }
                CompiledBranch::When { .. } => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::MatchConditionConfig;
    use crate::dynamic::context::{ItemType, TemplateContext};
    use serde_json::json;

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            args: json!({"path": "/etc/passwd", "query": "secret passwords", "count": "5"}),
            item_name: "read_file".to_string(),
            item_type: ItemType::Tool,
            call_count: 1,
            phase_name: "baseline".to_string(),
            phase_index: -1,
            request_id: Some(json!(1)),
            request_method: "tools/call".to_string(),
            connection_id: 1,
            resource_name: None,
            resource_mime_type: None,
        }
    }

    #[test]
    fn test_glob_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Single("*.env".into())).unwrap();
        assert!(cond.matches(Some("config.env")));
        assert!(!cond.matches(Some("config.txt")));
    }

    #[test]
    fn test_regex_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Single(
            "regex:(?i).*(password|secret).*".into(),
        ))
        .unwrap();
        assert!(cond.matches(Some("secret passwords")));
        assert!(cond.matches(Some("MY PASSWORD")));
        assert!(!cond.matches(Some("normal query")));
    }

    #[test]
    fn test_contains_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: Some(".env".into()),
            prefix: None,
            suffix: None,
            exists: None,
            gt: None,
            lt: None,
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("config.env")));
        assert!(cond.matches(Some("/path/.env.local")));
        assert!(!cond.matches(Some("config.txt")));
    }

    #[test]
    fn test_prefix_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: None,
            prefix: Some("/etc/".into()),
            suffix: None,
            exists: None,
            gt: None,
            lt: None,
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("/etc/passwd")));
        assert!(!cond.matches(Some("/var/log")));
    }

    #[test]
    fn test_suffix_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: None,
            prefix: None,
            suffix: Some(".pem".into()),
            exists: None,
            gt: None,
            lt: None,
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("server.pem")));
        assert!(!cond.matches(Some("server.key")));
    }

    #[test]
    fn test_exists_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: None,
            prefix: None,
            suffix: None,
            exists: Some(true),
            gt: None,
            lt: None,
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("anything")));
        assert!(!cond.matches(None));
    }

    #[test]
    fn test_gt_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: None,
            prefix: None,
            suffix: None,
            exists: None,
            gt: Some(3.0),
            lt: None,
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("5")));
        assert!(!cond.matches(Some("2")));
        assert!(!cond.matches(Some("not a number")));
    }

    #[test]
    fn test_lt_match() {
        let cond = MatchCondition::compile(&MatchConditionConfig::Operator {
            contains: None,
            prefix: None,
            suffix: None,
            exists: None,
            gt: None,
            lt: Some(10.0),
            any_of: None,
        })
        .unwrap();
        assert!(cond.matches(Some("5")));
        assert!(!cond.matches(Some("15")));
    }

    #[test]
    fn test_any_of_glob_list() {
        let cond = MatchCondition::compile(&MatchConditionConfig::GlobList(vec![
            "*.pem".into(),
            "*.key".into(),
            "id_rsa*".into(),
        ]))
        .unwrap();
        assert!(cond.matches(Some("server.pem")));
        assert!(cond.matches(Some("private.key")));
        assert!(cond.matches(Some("id_rsa.pub")));
        assert!(!cond.matches(Some("readme.txt")));
    }

    #[test]
    fn test_when_clause_and() {
        let clause = WhenClause {
            conditions: vec![
                ("args.path".into(), MatchCondition::Prefix("/etc/".into())),
                (
                    "args.query".into(),
                    MatchCondition::Contains("secret".into()),
                ),
            ],
        };
        let ctx = make_ctx();
        assert!(clause.matches(&ctx));
    }

    #[test]
    fn test_when_clause_and_fails() {
        let clause = WhenClause {
            conditions: vec![
                ("args.path".into(), MatchCondition::Prefix("/var/".into())),
                (
                    "args.query".into(),
                    MatchCondition::Contains("secret".into()),
                ),
            ],
        };
        let ctx = make_ctx();
        assert!(!clause.matches(&ctx));
    }

    // EC-DYN-010: empty match block
    #[test]
    fn test_empty_match_block() {
        let block = MatchBlock { branches: vec![] };
        let ctx = make_ctx();
        assert_eq!(block.evaluate(&ctx), None);
    }

    // EC-DYN-009: no match, no default
    #[test]
    fn test_no_match_no_default() {
        let block = MatchBlock {
            branches: vec![CompiledBranch::When {
                clause: WhenClause {
                    conditions: vec![(
                        "args.path".into(),
                        MatchCondition::Prefix("/nonexistent/".into()),
                    )],
                },
                index: 0,
            }],
        };
        let ctx = make_ctx();
        assert_eq!(block.evaluate(&ctx), None);
    }

    #[test]
    fn test_first_match_wins() {
        let block = MatchBlock {
            branches: vec![
                CompiledBranch::When {
                    clause: WhenClause {
                        conditions: vec![(
                            "args.path".into(),
                            MatchCondition::Prefix("/etc/".into()),
                        )],
                    },
                    index: 0,
                },
                CompiledBranch::When {
                    clause: WhenClause {
                        conditions: vec![(
                            "args.query".into(),
                            MatchCondition::Contains("secret".into()),
                        )],
                    },
                    index: 1,
                },
            ],
        };
        let ctx = make_ctx();
        // Both match, but first wins
        assert_eq!(block.evaluate(&ctx), Some(0));
    }

    #[test]
    fn test_default_branch() {
        let block = MatchBlock {
            branches: vec![
                CompiledBranch::When {
                    clause: WhenClause {
                        conditions: vec![(
                            "args.path".into(),
                            MatchCondition::Prefix("/nonexistent/".into()),
                        )],
                    },
                    index: 0,
                },
                CompiledBranch::Default { index: 1 },
            ],
        };
        let ctx = make_ctx();
        assert_eq!(block.evaluate(&ctx), Some(1));
    }
}
