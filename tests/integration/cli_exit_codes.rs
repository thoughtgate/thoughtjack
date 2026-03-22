//! CLI subprocess exit-code regression tests.
//!
//! Runs the built `thoughtjack` binary end-to-end and asserts command-path
//! exit codes.

use std::io::Write;
use std::process::{Command, Output};
use std::thread::sleep;
use std::time::Duration;

use tempfile::NamedTempFile;

fn thoughtjack_bin() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_thoughtjack") {
        return std::path::PathBuf::from(path);
    }

    if let Some(path) = option_env!("CARGO_BIN_EXE_thoughtjack") {
        return std::path::PathBuf::from(path);
    }

    let mut path = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
        || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
        |dir| {
            let dir = std::path::PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(dir)
            }
        },
    );
    path.push("debug");
    path.push("thoughtjack");
    #[cfg(windows)]
    path.set_extension("exe");
    path
}

fn run_thoughtjack(args: &[&str]) -> Output {
    Command::new(thoughtjack_bin())
        .args(args)
        .output()
        .expect("failed to execute thoughtjack binary")
}

fn run_thoughtjack_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(thoughtjack_bin());
    cmd.args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    cmd.output().expect("failed to execute thoughtjack binary")
}

fn assert_exit_code(output: &Output, expected: i32, context: &str) {
    let actual = output
        .status
        .code()
        .unwrap_or_else(|| panic!("process terminated by signal in {context}"));
    assert_eq!(
        actual,
        expected,
        "{context}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_temp_yaml(content: &str) -> NamedTempFile {
    let mut file = NamedTempFile::new().expect("create temp yaml");
    file.write_all(content.as_bytes()).expect("write temp yaml");
    file
}

fn write_long_running_server_yaml() -> NamedTempFile {
    write_temp_yaml(
        r#"
oatf: "0.1"
attack:
  name: long-running-server
  execution:
    mode: mcp_server
    phases:
      - name: serve
        state:
          tools:
            - name: hold
              description: "wait"
              inputSchema:
                type: object
        trigger:
          event: tools/call
          count: 999
      - name: terminal
"#,
    )
}

fn write_terminal_yaml() -> NamedTempFile {
    write_temp_yaml(
        r#"
oatf: "0.1"
attack:
  name: terminal
  execution:
    mode: mcp_server
    phases:
      - name: terminal
        state:
          tools: []
"#,
    )
}

fn write_semantic_only_yaml() -> NamedTempFile {
    write_temp_yaml(
        r#"
oatf: "0.1"
attack:
  name: semantic-only
  indicators:
    - id: semantic_only
      protocol: mcp
      surface: tool_description
      target: description
      semantic:
        target: description
        intent: data exfiltration
        threshold: 0.7
  execution:
    mode: mcp_server
    phases:
      - name: terminal
        state:
          tools: []
"#,
    )
}

#[test]
fn cli_command_paths_have_expected_exit_codes() {
    let no_args = run_thoughtjack(&[]);
    assert_exit_code(&no_args, 64, "no subcommand should be usage error");

    let unknown = run_thoughtjack(&["frob"]);
    assert_exit_code(&unknown, 64, "unknown subcommand should be usage error");

    let version = run_thoughtjack(&["version", "--format", "json"]);
    assert_exit_code(&version, 0, "version command should succeed");
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("\"version\""),
        "version JSON output missing version field"
    );

    let validate = run_thoughtjack(&["validate", "tests/fixtures/smoke_test.yaml"]);
    assert_exit_code(&validate, 0, "validate command should succeed");

    let run_no_config = run_thoughtjack(&["run"]);
    assert_exit_code(
        &run_no_config,
        64,
        "run without --config should be usage error",
    );

    let mcp_client_only = write_temp_yaml(
        r#"
oatf: "0.1"
attack:
  name: mcp-client-only
  execution:
    mode: mcp_client
    phases:
      - name: probe
        state:
          actions:
            - list_tools
"#,
    );
    let run_missing_transport = run_thoughtjack(&[
        "run",
        "--config",
        &mcp_client_only.path().to_string_lossy(),
        "--quiet",
        "--max-session",
        "300ms",
    ]);
    assert_exit_code(
        &run_missing_transport,
        64,
        "missing mcp_client transport flags should be usage error",
    );

    let conflict = run_thoughtjack(&[
        "run",
        "--config",
        "tests/fixtures/smoke_test.yaml",
        "--mcp-client-command",
        "echo hi",
        "--mcp-client-endpoint",
        "http://localhost:9999",
    ]);
    assert_exit_code(&conflict, 64, "conflicting flags should be usage error");

    let scenarios_config = run_thoughtjack(&[
        "scenarios",
        "run",
        "oatf-002",
        "--config",
        "tests/fixtures/smoke_test.yaml",
    ]);
    assert_exit_code(
        &scenarios_config,
        64,
        "scenarios run should reject --config at parse time",
    );

    let scenarios_run = run_thoughtjack(&[
        "scenarios",
        "run",
        "oatf-002",
        "--mcp-server",
        "127.0.0.1:0",
        "--max-session",
        "300ms",
        "--quiet",
    ]);
    assert_exit_code(
        &scenarios_run,
        0,
        "scenarios run with --max-session should produce verdict (not runtime error)",
    );
}

#[test]
fn scenarios_run_ignores_thoughtjack_config_env() {
    let output = run_thoughtjack_with_env(
        &[
            "scenarios",
            "run",
            "oatf-002",
            "--mcp-server",
            "127.0.0.1:0",
            "--max-session",
            "1s",
            "--quiet",
        ],
        &[("THOUGHTJACK_CONFIG", "tests/fixtures/smoke_test.yaml")],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let code = output
        .status
        .code()
        .unwrap_or_else(|| panic!("process terminated by signal:\n{stderr}"));
    assert_ne!(
        code, 64,
        "scenarios run should not treat THOUGHTJACK_CONFIG env as an explicit --config override.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("--config is not supported with `scenarios run`"),
        "unexpected --config rejection from environment variable.\nstderr:\n{stderr}"
    );
}

#[test]
fn run_help_mentions_thoughtjack_config_env() {
    let output = run_thoughtjack(&["run", "--help"]);
    assert_exit_code(&output, 0, "run --help should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("THOUGHTJACK_CONFIG"),
        "run help should mention THOUGHTJACK_CONFIG.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains("--config <PATH>"),
        "run help should show config as a named argument.\nstdout:\n{stdout}"
    );
}

#[test]
fn run_accepts_thoughtjack_config_env() {
    let file = write_terminal_yaml();
    let config_path = file.path().to_string_lossy().into_owned();
    let output =
        run_thoughtjack_with_env(&["run", "--quiet"], &[("THOUGHTJACK_CONFIG", &config_path)]);

    assert_exit_code(
        &output,
        0,
        "run should accept THOUGHTJACK_CONFIG without an explicit --config flag",
    );
}

#[test]
fn quiet_verdict_exit_does_not_write_stderr() {
    let file = write_semantic_only_yaml();
    let config_path = file.path().to_string_lossy().into_owned();
    let output = run_thoughtjack(&["run", "--config", &config_path, "--quiet", "--output", "-"]);

    assert_exit_code(
        &output,
        2,
        "semantic-only verdict should exit with evaluation error code",
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("\"result\": \"error\""),
        "stdout should contain the JSON verdict.\nstdout:\n{stdout}"
    );
    assert!(
        stderr.trim().is_empty(),
        "quiet verdict exit should not write to stderr.\nstderr:\n{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn run_sigint_exits_130() {
    let file = write_long_running_server_yaml();
    let config_path = file.path().to_string_lossy().into_owned();
    let mut child = Command::new(thoughtjack_bin())
        .args([
            "run",
            "--config",
            &config_path,
            "--mcp-server",
            "127.0.0.1:0",
            "--max-session",
            "30s",
            "--quiet",
        ])
        .spawn()
        .expect("spawn thoughtjack");

    sleep(Duration::from_secs(1));

    let pid = child.id().to_string();
    let status = Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .expect("send SIGINT");
    assert!(status.success(), "failed to signal process {pid}");

    let status = child.wait().expect("wait for child");
    assert_eq!(
        status.code(),
        Some(130),
        "first SIGINT should produce exit 130, got {status:?}"
    );
}

#[cfg(unix)]
#[test]
fn run_sigterm_exits_143() {
    let file = write_long_running_server_yaml();
    let config_path = file.path().to_string_lossy().into_owned();
    let mut child = Command::new(thoughtjack_bin())
        .args([
            "run",
            "--config",
            &config_path,
            "--mcp-server",
            "127.0.0.1:0",
            "--max-session",
            "30s",
            "--quiet",
        ])
        .spawn()
        .expect("spawn thoughtjack");

    sleep(Duration::from_secs(1));

    let pid = child.id().to_string();
    let status = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("send SIGTERM");
    assert!(status.success(), "failed to signal process {pid}");

    let status = child.wait().expect("wait for child");
    assert_eq!(
        status.code(),
        Some(143),
        "first SIGTERM should produce exit 143, got {status:?}"
    );
}

#[tokio::test]
async fn run_multi_actor_client_completes_server_cancelled_exits_success() {
    let yaml = r#"
oatf: "0.1"
attack:
  name: mixed_completion
  execution:
    actors:
      - name: fast_actor
        mode: mcp_server
        phases:
          - name: terminal
            state:
              tools: []
      - name: slow_actor
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: hold
                  description: "wait"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
"#;
    let file = write_temp_yaml(yaml);
    let config_path = file.path().to_string_lossy().into_owned();
    let output = run_thoughtjack(&[
        "run",
        "--config",
        &config_path,
        "--max-session",
        "300ms",
        "--quiet",
    ]);

    assert_exit_code(
        &output,
        0,
        "run should succeed when at least one actor reaches a completed state",
    );
}

#[test]
fn run_multi_actor_all_timeout_or_cancel_exits_runtime_error() {
    let yaml = r#"
oatf: "0.1"
attack:
  name: all_cancelled
  execution:
    actors:
      - name: server_a
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
      - name: server_b
        mode: mcp_server
        phases:
          - name: serve
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
            trigger:
              event: tools/call
              count: 999
          - name: terminal
"#;
    let file = write_temp_yaml(yaml);
    let config_path = file.path().to_string_lossy().into_owned();

    let output = run_thoughtjack(&[
        "run",
        "--config",
        &config_path,
        "--mcp-server",
        "127.0.0.1:0",
        "--max-session",
        "300ms",
        "--quiet",
    ]);

    assert_exit_code(
        &output,
        10,
        "run should fail when every actor terminates due cancellation/timeout",
    );
}

// ---- --export-trace tests ----

#[test]
fn export_trace_writes_jsonl_file() {
    let yaml_file = write_terminal_yaml();
    let config_path = yaml_file.path().to_string_lossy().into_owned();

    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().join("trace.jsonl");
    let trace_path_str = trace_path.to_string_lossy().into_owned();

    let output = run_thoughtjack(&[
        "run",
        "--config",
        &config_path,
        "--mcp-server",
        "127.0.0.1:0",
        "--max-session",
        "500ms",
        "--quiet",
        "--export-trace",
        &trace_path_str,
    ]);

    assert_exit_code(&output, 0, "export-trace with terminal yaml");
    assert!(trace_path.exists(), "trace file should be created");

    // File should exist (may be empty if no protocol messages exchanged)
    let content = std::fs::read_to_string(&trace_path).unwrap();
    // Each non-empty line should be valid JSON
    for line in content.lines() {
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("each trace line should be valid JSON");
        assert!(parsed["seq"].is_number(), "trace entry should have seq");
        assert!(
            parsed["method"].is_string(),
            "trace entry should have method"
        );
        assert!(
            !parsed["content"].is_null(),
            "trace entry should have content"
        );
    }
}

#[test]
fn export_trace_creates_parent_dirs() {
    let yaml_file = write_terminal_yaml();
    let config_path = yaml_file.path().to_string_lossy().into_owned();

    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir
        .path()
        .join("nested")
        .join("dir")
        .join("trace.jsonl");
    let trace_path_str = trace_path.to_string_lossy().into_owned();

    let output = run_thoughtjack(&[
        "run",
        "--config",
        &config_path,
        "--mcp-server",
        "127.0.0.1:0",
        "--max-session",
        "500ms",
        "--quiet",
        "--export-trace",
        &trace_path_str,
    ]);

    assert_exit_code(&output, 0, "export-trace with nested dirs");
    assert!(
        trace_path.exists(),
        "trace file should be created in nested directory"
    );
}
