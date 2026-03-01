//! MCP server-mode `PhaseDriver` implementation.
//!
//! `McpServerDriver` listens for JSON-RPC requests from an AI agent
//! client, dispatches responses based on the current OATF phase's
//! effective state, applies behavioral modifiers (delayed, `slow_stream`,
//! `notification_flood`, etc.), supports elicitation interleaving, and
//! emits protocol events for the `PhaseLoop` to process.
//!
//! `McpTransportEntryActionSender` implements `EntryActionSender` to
//! deliver phase-transition notifications and elicitations over the
//! transport.
//!
//! See TJ-SPEC-013 §8.2 for the MCP server driver specification.

mod behavior;
mod driver;
mod generation;
mod handlers;
mod helpers;
mod response;

#[cfg(test)]
mod tests;

pub use driver::{McpServerDriver, McpTransportEntryActionSender};
