//! Protocol-specific `PhaseDriver` implementations.
//!
//! Each protocol mode gets its own module. The AG-UI client driver
//! is the first client-mode driver, complementing the existing
//! MCP server driver in `engine::mcp_server`.

pub mod agui;
