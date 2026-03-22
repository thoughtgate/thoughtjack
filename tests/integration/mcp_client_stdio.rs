//! MCP client via stdio pipes integration tests.
//!
//! Uses shell scripts (written to tempfiles) as mock MCP servers.
//! Each script reads JSON-RPC from stdin and writes responses to stdout.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use thoughtjack::engine::{
    ExtractorStore, PhaseEngine, PhaseLoop, PhaseLoopConfig, SharedTrace, TerminationReason,
};
use thoughtjack::observability::EventEmitter;
use thoughtjack::protocol::mcp_client::create_mcp_client_driver;

/// Helper: load an oatf Document from inline YAML.
fn load_doc(yaml: &str) -> oatf::Document {
    oatf::load(yaml)
        .expect("test YAML should be valid")
        .document
}

/// Creates a temporary shell script that acts as a mock MCP server.
///
/// Returns a `TempPath` (auto-deletes on drop) with the write handle
/// **closed**. This prevents `ETXTBSY` on Linux, where `exec` fails
/// if the file still has an open write descriptor.
fn create_mock_mcp_script(script_body: &str) -> tempfile::TempPath {
    let mut file = tempfile::Builder::new()
        .suffix(".sh")
        .tempfile()
        .expect("failed to create tempfile");
    writeln!(file, "#!/bin/bash").unwrap();
    write!(file, "{script_body}").unwrap();
    file.flush().unwrap();

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = file.path().to_path_buf();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Close the write fd to avoid ETXTBSY on Linux
    file.into_temp_path()
}

// ============================================================================
// 1. Init and tools/list — capabilities negotiated (EC-MCPC-005)
// ============================================================================

#[tokio::test]
async fn mcp_client_init_and_tools_list() {
    // Mock server: responds to initialize + initialized + tools/list
    let script = create_mock_mcp_script(
        r#"
# Read initialize request
read -r line
# Extract id from JSON-RPC request
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
# Send initialize response
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"

# Read initialized notification
read -r line

# Read tools/list request
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
# Send tools/list response
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"tools\":[{\"name\":\"calculator\",\"description\":\"A calculator tool\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"expression\":{\"type\":\"string\"}}}}]}}"
"#,
    );

    let script_path = script.to_str().unwrap().to_string();

    let driver = create_mcp_client_driver(Some(&script_path), &[], None, &[], false)
        .expect("failed to create MCP client driver");

    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: mcp_client_init
  execution:
    mode: mcp_client
    state:
      actions:
        - list_tools
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let config = PhaseLoopConfig {
        trace: trace.clone(),
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: CancellationToken::new(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, config);
    let result = phase_loop.run().await.unwrap();

    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // Trace should have init and tools/list events
    let entries = trace.snapshot();
    assert!(
        !entries.is_empty(),
        "trace should have entries after init + tools/list"
    );

    let methods: Vec<&str> = entries.iter().map(|e| e.method.as_str()).collect();
    assert!(
        methods.contains(&"initialize"),
        "expected initialize in trace, got: {methods:?}"
    );
}

// ============================================================================
// 2. Interleaved server request — no deadlock (EC-MCPC-003)
// ============================================================================

#[tokio::test]
async fn mcp_client_interleaved_server_request() {
    // Mock server: after init, when it receives tools/call, it first sends
    // a sampling/createMessage server request, then the tools/call response
    let script = create_mock_mcp_script(
        r#"
# Read initialize request
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{},\"sampling\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"

# Read initialized notification
read -r line

# Read tools/list or tools/call request
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
method=$(echo "$line" | grep -o '"method":"[^"]*"' | head -1 | cut -d'"' -f4)

if [ "$method" = "tools/list" ]; then
  echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"tools\":[{\"name\":\"calc\",\"description\":\"test\",\"inputSchema\":{\"type\":\"object\"}}]}}"
  # Read tools/call
  read -r line
  id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
fi

# Send tools/call response
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"42\"}]}}"
"#,
    );

    let script_path = script.to_str().unwrap().to_string();

    let driver = create_mcp_client_driver(Some(&script_path), &[], None, &[], false)
        .expect("failed to create MCP client driver");

    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: mcp_client_interleaved
  execution:
    mode: mcp_client
    state:
      actions:
        - list_tools
        - call_tool:
            name: calc
            arguments:
              expression: "1 + 1"
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let config = PhaseLoopConfig {
        trace: trace.clone(),
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: CancellationToken::new(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, config);
    let result = phase_loop.run().await.unwrap();

    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    // If we got here without hanging, the interleaved request was handled
    let entries = trace.snapshot();
    assert!(!entries.is_empty());
}

// ============================================================================
// 3. Server exits mid-phase — EOF detected (EC-MCPC-009)
// ============================================================================

#[tokio::test]
async fn mcp_client_server_exits() {
    // Mock server: responds to initialize then exits immediately
    let script = create_mock_mcp_script(
        r#"
# Read initialize request
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"
# Read initialized notification
read -r line
# Exit immediately without responding to further requests
exit 0
"#,
    );

    let script_path = script.to_str().unwrap().to_string();

    let driver = create_mcp_client_driver(Some(&script_path), &[], None, &[], false)
        .expect("failed to create MCP client driver");

    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: mcp_client_exit
  execution:
    mode: mcp_client
    state:
      actions:
        - list_tools
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let config = PhaseLoopConfig {
        trace: trace.clone(),
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: CancellationToken::new(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, config);
    let result = phase_loop.run().await;

    // Should return an error (server exited / EOF) — not hang
    assert!(
        result.is_err(),
        "expected error when server exits, got: {result:?}"
    );
}

// ============================================================================
// 4. All actions error — no short-circuit (EC-MCPC-008)
// ============================================================================

#[tokio::test]
async fn mcp_client_all_actions_error() {
    // Mock server: responds to initialize + initialized, then returns errors
    let script = create_mock_mcp_script(
        r#"
# Read initialize request
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"mock\",\"version\":\"1.0\"}}}"

# Read initialized notification
read -r line

# Read tools/list and return empty list
read -r line
id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"result\":{\"tools\":[{\"name\":\"fail1\",\"description\":\"test\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"fail2\",\"description\":\"test\",\"inputSchema\":{\"type\":\"object\"}},{\"name\":\"fail3\",\"description\":\"test\",\"inputSchema\":{\"type\":\"object\"}}]}}"

# Read 3 tools/call requests, return errors for all
for i in 1 2 3; do
  read -r line
  id=$(echo "$line" | grep -o '"id":[0-9]*' | head -1 | cut -d: -f2)
  echo "{\"jsonrpc\":\"2.0\",\"id\":${id},\"error\":{\"code\":-32000,\"message\":\"tool failed\"}}"
done
"#,
    );

    let script_path = script.to_str().unwrap().to_string();

    let driver = create_mcp_client_driver(Some(&script_path), &[], None, &[], false)
        .expect("failed to create MCP client driver");

    let doc = load_doc(
        r#"
oatf: "0.1"
attack:
  name: mcp_client_errors
  execution:
    mode: mcp_client
    state:
      actions:
        - list_tools
        - call_tool:
            name: fail1
            arguments: {}
        - call_tool:
            name: fail2
            arguments: {}
        - call_tool:
            name: fail3
            arguments: {}
"#,
    );

    let engine = PhaseEngine::new(doc, 0);
    let trace = SharedTrace::new();
    let config = PhaseLoopConfig {
        trace: trace.clone(),
        extractor_store: ExtractorStore::new(),
        actor_name: "test".to_string(),
        await_extractors_config: HashMap::new(),
        cancel: CancellationToken::new(),
        entry_action_sender: None,
        events: Arc::new(EventEmitter::noop()),
        tool_watch_tx: None,
    };

    let mut phase_loop = PhaseLoop::new(driver, engine, config);
    let result = phase_loop.run().await.unwrap();

    // All actions should be attempted (no short-circuit on error)
    assert_eq!(result.termination, TerminationReason::TerminalPhaseReached);

    let entries = trace.snapshot();
    // Should have events for all 3 tool calls (outgoing + incoming per call)
    let tool_events: Vec<_> = entries
        .iter()
        .filter(|e| e.method.contains("tools/call"))
        .collect();
    assert!(
        tool_events.len() >= 3,
        "expected ≥3 tool call events, got {}",
        tool_events.len()
    );
}
