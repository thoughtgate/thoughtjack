//! Dynamic response system (TJ-SPEC-009).
//!
//! Provides template interpolation, conditional matching, response sequences,
//! and external handler delegation for dynamic MCP responses.

pub mod context;
pub mod functions;
pub mod handlers;
pub mod matching;
pub mod sequence;
pub mod template;

use crate::config::schema::{
    ContentItem, ContentValue, ExhaustedBehavior, HandlerConfig, MatchBranchConfig, PromptMessage,
    ResourceContentConfig, SequenceEntryConfig,
};
use crate::error::ThoughtJackError;

use context::TemplateContext;
use handlers::command::execute_command_handler;
use handlers::http::execute_http_handler;
use matching::MatchBlock;
use sequence::{SequenceExhausted, resolve_sequence_index};
use template::resolve_template;

/// Resolved tool/resource content from the dynamic pipeline.
///
/// Implements: TJ-SPEC-009 F-001
pub struct ResolvedContent {
    /// The resolved content items
    pub content: Vec<ContentItem>,
}

/// Resolved prompt content from the dynamic pipeline.
///
/// Implements: TJ-SPEC-009 F-001
pub struct ResolvedPrompt {
    /// The resolved prompt messages
    pub messages: Vec<PromptMessage>,
}

/// Resolves dynamic tool response content.
///
/// Pipeline: match → sequence → handler → template interpolation.
///
/// # Errors
///
/// Returns an error if handler execution fails, sequence is exhausted
/// with error behavior, or any internal resolution error occurs.
///
/// Implements: TJ-SPEC-009 F-001
#[allow(clippy::too_many_arguments)]
pub async fn resolve_tool_content(
    content: &[ContentItem],
    match_block: Option<&[MatchBranchConfig]>,
    sequence: Option<&[SequenceEntryConfig]>,
    on_exhausted: ExhaustedBehavior,
    handler: Option<&HandlerConfig>,
    ctx: &TemplateContext,
    allow_external_handlers: bool,
    http_client: &reqwest::Client,
) -> Result<ResolvedContent, ThoughtJackError> {
    // Step 1: Match evaluation — select branch if match block present
    let (effective_content, effective_sequence, effective_on_exhausted, effective_handler) =
        resolve_match_branch(match_block, content, sequence, on_exhausted, handler, ctx)?;

    // Step 2: Sequence resolution
    let final_content = if let Some(seq) = effective_sequence {
        resolve_sequence_content(seq, effective_on_exhausted, ctx)?
    } else {
        effective_content.to_vec()
    };

    // Step 3: Handler delegation
    if let Some(handler_config) = effective_handler {
        let handler_content = execute_handler(
            handler_config,
            ctx,
            "tool",
            allow_external_handlers,
            http_client,
        )
        .await?;
        return Ok(ResolvedContent {
            content: handler_content,
        });
    }

    // Step 4: Template interpolation on final content
    let interpolated = interpolate_content_items(&final_content, ctx);

    Ok(ResolvedContent {
        content: interpolated,
    })
}

/// Resolves dynamic prompt response messages.
///
/// Pipeline: match → sequence → handler → template interpolation.
///
/// # Errors
///
/// Returns an error if handler execution fails, sequence is exhausted
/// with error behavior, or any internal resolution error occurs.
///
/// Implements: TJ-SPEC-009 F-001
#[allow(clippy::too_many_arguments)]
pub async fn resolve_prompt_content(
    messages: &[PromptMessage],
    match_block: Option<&[MatchBranchConfig]>,
    sequence: Option<&[SequenceEntryConfig]>,
    on_exhausted: ExhaustedBehavior,
    handler: Option<&HandlerConfig>,
    ctx: &TemplateContext,
    allow_external_handlers: bool,
    http_client: &reqwest::Client,
) -> Result<ResolvedPrompt, ThoughtJackError> {
    // Step 1: Match evaluation
    if let Some(branches) = match_block {
        if !branches.is_empty() {
            let compiled = MatchBlock::compile(branches)?;
            if let Some(idx) = compiled.evaluate(ctx) {
                let branch = &branches[idx];
                let branch_messages = match branch {
                    MatchBranchConfig::When {
                        messages: msgs,
                        sequence: seq,
                        on_exhausted: oe,
                        handler: h,
                        ..
                    }
                    | MatchBranchConfig::Default {
                        messages: msgs,
                        sequence: seq,
                        on_exhausted: oe,
                        handler: h,
                        ..
                    } => {
                        // Handler in branch
                        if let Some(handler_config) = h {
                            let handler_content = execute_handler(
                                handler_config,
                                ctx,
                                "prompt",
                                allow_external_handlers,
                                http_client,
                            )
                            .await?;
                            // Convert handler content to prompt messages
                            let converted = content_to_messages(&handler_content);
                            return Ok(ResolvedPrompt {
                                messages: converted,
                            });
                        }
                        // Sequence in branch
                        if let Some(seq_entries) = seq {
                            let oe_val = oe.unwrap_or_default();
                            resolve_sequence_messages(seq_entries, oe_val, ctx)?
                        } else {
                            msgs.clone()
                        }
                    }
                };
                let interpolated = interpolate_messages(&branch_messages, ctx);
                return Ok(ResolvedPrompt {
                    messages: interpolated,
                });
            }
        }
    }

    // Step 2: Sequence resolution (top-level)
    if let Some(seq) = sequence {
        let resolved_msgs = resolve_sequence_messages(seq, on_exhausted, ctx)?;
        let interpolated = interpolate_messages(&resolved_msgs, ctx);
        return Ok(ResolvedPrompt {
            messages: interpolated,
        });
    }

    // Step 3: Handler delegation (top-level)
    if let Some(handler_config) = handler {
        let handler_content = execute_handler(
            handler_config,
            ctx,
            "prompt",
            allow_external_handlers,
            http_client,
        )
        .await?;
        let converted = content_to_messages(&handler_content);
        return Ok(ResolvedPrompt {
            messages: converted,
        });
    }

    // Step 4: Template interpolation on static messages
    let interpolated = interpolate_messages(messages, ctx);
    Ok(ResolvedPrompt {
        messages: interpolated,
    })
}

/// Resolves resource-specific content entries from match branches.
///
/// If a matching branch has `contents` (resource-specific `{uri, text, mimeType}`
/// entries), returns them with template interpolation applied. Returns `None` if
/// no match block, no matching branch, or the matched branch uses handler/sequence
/// (which are handled by the tool content pipeline).
///
/// # Errors
///
/// Returns an error if match block compilation fails (e.g., invalid regex).
///
/// Implements: TJ-SPEC-009 F-002
pub fn resolve_resource_match_contents(
    match_block: &[MatchBranchConfig],
    ctx: &TemplateContext,
) -> Result<Option<Vec<ResourceContentConfig>>, ThoughtJackError> {
    if match_block.is_empty() {
        return Ok(None);
    }

    let compiled = MatchBlock::compile(match_block)?;
    let Some(idx) = compiled.evaluate(ctx) else {
        return Ok(None);
    };

    let branch = &match_block[idx];
    match branch {
        MatchBranchConfig::When {
            contents,
            handler,
            sequence,
            ..
        }
        | MatchBranchConfig::Default {
            contents,
            handler,
            sequence,
            ..
        } => {
            // Handler and sequence take priority — fall through to tool pipeline
            if handler.is_some() || sequence.is_some() {
                return Ok(None);
            }
            match contents {
                Some(entries) if !entries.is_empty() => {
                    Ok(Some(interpolate_resource_contents(entries, ctx)))
                }
                _ => Ok(None),
            }
        }
    }
}

/// Applies template interpolation to resource content entries.
fn interpolate_resource_contents(
    entries: &[ResourceContentConfig],
    ctx: &TemplateContext,
) -> Vec<ResourceContentConfig> {
    entries
        .iter()
        .map(|entry| ResourceContentConfig {
            uri: resolve_template(&entry.uri, ctx),
            text: entry.text.as_ref().map(|t| resolve_template(t, ctx)),
            mime_type: entry.mime_type.clone(),
        })
        .collect()
}

/// Effective config after match evaluation.
type MatchResult<'a> = (
    &'a [ContentItem],
    Option<&'a [SequenceEntryConfig]>,
    ExhaustedBehavior,
    Option<&'a HandlerConfig>,
);

/// Resolves the matching branch and returns effective content/sequence/handler.
fn resolve_match_branch<'a>(
    match_block: Option<&'a [MatchBranchConfig]>,
    default_content: &'a [ContentItem],
    default_sequence: Option<&'a [SequenceEntryConfig]>,
    default_on_exhausted: ExhaustedBehavior,
    default_handler: Option<&'a HandlerConfig>,
    ctx: &TemplateContext,
) -> Result<MatchResult<'a>, ThoughtJackError> {
    if let Some(branches) = match_block {
        if !branches.is_empty() {
            let compiled = MatchBlock::compile(branches)?;
            if let Some(idx) = compiled.evaluate(ctx) {
                let branch = &branches[idx];
                return Ok(match branch {
                    MatchBranchConfig::When {
                        content,
                        sequence,
                        on_exhausted,
                        handler,
                        ..
                    }
                    | MatchBranchConfig::Default {
                        content,
                        sequence,
                        on_exhausted,
                        handler,
                        ..
                    } => (
                        content.as_slice(),
                        sequence.as_deref(),
                        on_exhausted.unwrap_or_default(),
                        handler.as_ref(),
                    ),
                });
            }
        }
    }

    Ok((
        default_content,
        default_sequence,
        default_on_exhausted,
        default_handler,
    ))
}

/// Resolves a sequence entry to content items.
fn resolve_sequence_content(
    sequence: &[SequenceEntryConfig],
    on_exhausted: ExhaustedBehavior,
    ctx: &TemplateContext,
) -> Result<Vec<ContentItem>, ThoughtJackError> {
    let index = resolve_sequence_index(sequence.len(), ctx.call_count, on_exhausted).map_err(
        |SequenceExhausted| {
            crate::error::HandlerError::InvalidResponse("response sequence exhausted".to_string())
        },
    )?;
    Ok(sequence[index].content.clone())
}

/// Resolves a sequence entry to prompt messages.
fn resolve_sequence_messages(
    sequence: &[SequenceEntryConfig],
    on_exhausted: ExhaustedBehavior,
    ctx: &TemplateContext,
) -> Result<Vec<PromptMessage>, ThoughtJackError> {
    let index = resolve_sequence_index(sequence.len(), ctx.call_count, on_exhausted).map_err(
        |SequenceExhausted| {
            crate::error::HandlerError::InvalidResponse("response sequence exhausted".to_string())
        },
    )?;
    Ok(sequence[index].messages.clone())
}

/// Executes an external handler (HTTP or command).
async fn execute_handler(
    config: &HandlerConfig,
    ctx: &TemplateContext,
    item_type_str: &str,
    allow_external_handlers: bool,
    http_client: &reqwest::Client,
) -> Result<Vec<ContentItem>, ThoughtJackError> {
    let items = match config {
        HandlerConfig::Http {
            url,
            timeout_ms,
            headers,
        } => {
            execute_http_handler(
                http_client,
                url,
                *timeout_ms,
                headers.as_ref(),
                ctx,
                item_type_str,
                allow_external_handlers,
            )
            .await?
        }
        HandlerConfig::Command {
            cmd,
            timeout_ms,
            env,
            working_dir,
        } => {
            execute_command_handler(
                cmd,
                *timeout_ms,
                env.as_ref(),
                working_dir.as_ref(),
                ctx,
                item_type_str,
                allow_external_handlers,
            )
            .await?
        }
    };

    Ok(items)
}

/// Applies template interpolation to content items.
///
/// Only interpolates static text values — generated and file values
/// are left untouched.
fn interpolate_content_items(items: &[ContentItem], ctx: &TemplateContext) -> Vec<ContentItem> {
    items
        .iter()
        .map(|item| match item {
            ContentItem::Text {
                text: ContentValue::Static(s),
            } => ContentItem::Text {
                text: ContentValue::Static(resolve_template(s, ctx)),
            },
            other => other.clone(),
        })
        .collect()
}

/// Applies template interpolation to prompt messages.
fn interpolate_messages(messages: &[PromptMessage], ctx: &TemplateContext) -> Vec<PromptMessage> {
    messages
        .iter()
        .map(|msg| {
            let content = match &msg.content {
                ContentValue::Static(s) => ContentValue::Static(resolve_template(s, ctx)),
                other => other.clone(),
            };
            PromptMessage {
                role: msg.role,
                content,
            }
        })
        .collect()
}

/// Converts handler content items to prompt messages (user role).
fn content_to_messages(content: &[ContentItem]) -> Vec<PromptMessage> {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::Text { text } => Some(PromptMessage {
                role: crate::config::schema::Role::User,
                content: text.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ContentValue;
    use crate::dynamic::context::ItemType;
    use serde_json::json;

    fn make_ctx() -> TemplateContext {
        TemplateContext {
            args: json!({"query": "hello world"}),
            item_name: "test_tool".to_string(),
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
    fn test_interpolate_content_items() {
        let ctx = make_ctx();
        let items = vec![ContentItem::Text {
            text: ContentValue::Static("Query: ${args.query}".to_string()),
        }];
        let result = interpolate_content_items(&items, &ctx);
        match &result[0] {
            ContentItem::Text {
                text: ContentValue::Static(s),
            } => assert_eq!(s, "Query: hello world"),
            _ => panic!("expected static text"),
        }
    }

    #[test]
    fn test_interpolate_messages() {
        let ctx = make_ctx();
        let messages = vec![PromptMessage {
            role: crate::config::schema::Role::User,
            content: ContentValue::Static("Search for: ${args.query}".to_string()),
        }];
        let result = interpolate_messages(&messages, &ctx);
        match &result[0].content {
            ContentValue::Static(s) => assert_eq!(s, "Search for: hello world"),
            _ => panic!("expected static content"),
        }
    }

    #[test]
    fn test_content_to_messages() {
        let content = vec![ContentItem::Text {
            text: ContentValue::Static("hello".to_string()),
        }];
        let messages = content_to_messages(&content);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, crate::config::schema::Role::User);
    }

    #[tokio::test]
    async fn test_resolve_tool_content_static() {
        let ctx = make_ctx();
        let content = vec![ContentItem::Text {
            text: ContentValue::Static("Query: ${args.query}".to_string()),
        }];
        let client = handlers::http::create_http_client();
        let result = resolve_tool_content(
            &content,
            None,
            None,
            ExhaustedBehavior::Last,
            None,
            &ctx,
            false,
            &client,
        )
        .await
        .unwrap();
        match &result.content[0] {
            ContentItem::Text {
                text: ContentValue::Static(s),
            } => assert_eq!(s, "Query: hello world"),
            _ => panic!("expected static text"),
        }
    }

    #[tokio::test]
    async fn test_resolve_tool_content_sequence() {
        let mut ctx = make_ctx();
        let content = vec![];
        let sequence = vec![
            SequenceEntryConfig {
                content: vec![ContentItem::Text {
                    text: ContentValue::Static("first".to_string()),
                }],
                messages: vec![],
            },
            SequenceEntryConfig {
                content: vec![ContentItem::Text {
                    text: ContentValue::Static("second".to_string()),
                }],
                messages: vec![],
            },
        ];
        let client = handlers::http::create_http_client();

        // First call
        ctx.call_count = 1;
        let result = resolve_tool_content(
            &content,
            None,
            Some(&sequence),
            ExhaustedBehavior::Last,
            None,
            &ctx,
            false,
            &client,
        )
        .await
        .unwrap();
        match &result.content[0] {
            ContentItem::Text {
                text: ContentValue::Static(s),
            } => assert_eq!(s, "first"),
            _ => panic!("expected static text"),
        }

        // Second call
        ctx.call_count = 2;
        let result = resolve_tool_content(
            &content,
            None,
            Some(&sequence),
            ExhaustedBehavior::Last,
            None,
            &ctx,
            false,
            &client,
        )
        .await
        .unwrap();
        match &result.content[0] {
            ContentItem::Text {
                text: ContentValue::Static(s),
            } => assert_eq!(s, "second"),
            _ => panic!("expected static text"),
        }
    }

    #[test]
    fn test_resolve_resource_match_contents_with_entries() {
        let mut ctx = make_ctx();
        ctx.item_type = ItemType::Resource;
        ctx.item_name = "file:///test".to_string();
        ctx.resource_name = Some("test".to_string());

        let branches = vec![MatchBranchConfig::Default {
            default: json!(true),
            content: vec![],
            sequence: None,
            on_exhausted: None,
            handler: None,
            messages: vec![],
            contents: Some(vec![ResourceContentConfig {
                uri: "${resource.uri}".to_string(),
                text: Some("injected content".to_string()),
                mime_type: Some("text/plain".to_string()),
            }]),
        }];

        let result = resolve_resource_match_contents(&branches, &ctx)
            .unwrap()
            .expect("expected Some");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].uri, "file:///test");
        assert_eq!(result[0].text.as_deref(), Some("injected content"));
        assert_eq!(result[0].mime_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn test_resolve_resource_match_contents_none_when_handler_present() {
        let ctx = make_ctx();
        let branches = vec![MatchBranchConfig::Default {
            default: json!(true),
            content: vec![],
            sequence: None,
            on_exhausted: None,
            handler: Some(crate::config::schema::HandlerConfig::Http {
                url: "http://localhost".to_string(),
                timeout_ms: None,
                headers: None,
            }),
            messages: vec![],
            contents: Some(vec![ResourceContentConfig {
                uri: "test".to_string(),
                text: Some("text".to_string()),
                mime_type: None,
            }]),
        }];

        let result = resolve_resource_match_contents(&branches, &ctx).unwrap();
        assert!(
            result.is_none(),
            "handler should take priority over contents"
        );
    }

    #[test]
    fn test_resolve_resource_match_contents_empty_block() {
        let ctx = make_ctx();
        let branches: Vec<MatchBranchConfig> = vec![];
        let result = resolve_resource_match_contents(&branches, &ctx).unwrap();
        assert!(result.is_none());
    }
}
