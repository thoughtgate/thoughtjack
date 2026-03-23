//! Context-mode transport: LLM API-backed conversation with channel handles.
//!
//! Actors interact via channel-based handles (`AgUiHandle`, `ServerHandle`) that
//! implement the `Transport` trait, keeping `PhaseDriver` code transport-agnostic.
//!
//! See TJ-SPEC-022 for the full specification.

mod driver;
mod extraction;
mod handles;
mod tool_roster;
pub mod types;

// Re-export all public items for backward compatibility
pub use driver::ContextTransport;
pub use extraction::{
    extract_response_id, extract_result_content, extract_run_agent_input_context,
    extract_run_agent_input_messages, extract_user_message, format_server_request_as_user_message,
};
pub use handles::{AgUiHandle, ServerActorEntry, ServerHandle, ServerRequest};
pub use tool_roster::{
    build_tool_roster, extract_a2a_agent_tool, extract_tool_definitions,
    extract_tool_definitions_for_actor, sanitize_tool_name,
};
pub use types::{
    ChatMessage, LlmProvider, LlmResponse, ProviderError, TextResponse, ToolCall, ToolDefinition,
};
