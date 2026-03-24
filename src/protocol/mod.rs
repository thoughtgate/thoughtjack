//! Protocol-specific `PhaseDriver` implementations.
//!
//! Each protocol mode gets its own module:
//! - AG-UI client driver (`agui`) — TJ-SPEC-016
//! - A2A server driver (`a2a_server`) — TJ-SPEC-017
//! - A2A client driver (`a2a_client`) — TJ-SPEC-017
//! - MCP client driver (`mcp_client`) — TJ-SPEC-018
//!
//! The MCP server driver lives in `engine::mcp_server` (TJ-SPEC-013 §8.2).

pub mod a2a_client;
pub mod a2a_server;
pub mod agui;
pub mod context_agui;
pub mod mcp_client;
