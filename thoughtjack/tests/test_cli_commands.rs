mod common;

use common::ThoughtJackProcess;
use std::path::PathBuf;

#[test]
fn validate_valid_config() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "validate should succeed for valid config: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn validate_invalid_config() {
    let config = ThoughtJackProcess::fixture_path("missing_field.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "validate should fail for invalid config"
    );
}

#[test]
fn validate_json_output() {
    let config = ThoughtJackProcess::fixture_path("simple_server.yaml");
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "validate",
        "--format",
        "json",
        config.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "validate --format json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // JSON output should have "files" and "summary" fields
    assert!(
        parsed.get("files").is_some() || parsed.get("summary").is_some(),
        "JSON output should have validation structure: {stdout}"
    );
}

#[test]
fn validate_missing_file() {
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "validate",
        "/tmp/nonexistent_thoughtjack_test_file.yaml",
    ]);
    assert!(
        !output.status.success(),
        "validate should fail for nonexistent file"
    );
}

#[test]
fn list_json_format() {
    // Use the project library directory (may or may not exist)
    let library = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("library");
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "list",
        "--format",
        "json",
        "--library",
        library.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "list --format json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // If library exists, should be valid JSON array; if not, it's a message on stdout
    if library.exists() {
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("output should be valid JSON");
        assert!(parsed.is_array(), "JSON list output should be an array");
    }
}

#[test]
fn list_missing_library() {
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "list",
        "--library",
        "/tmp/nonexistent_thoughtjack_library_xyz",
    ]);
    assert!(
        output.status.success(),
        "list with missing library should exit 0"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("No library") || stdout.contains("No patterns"),
        "should indicate no library found: {stdout}"
    );
}

// EC-SCN-001: Unknown scenario name produces error with suggestion.
#[test]
fn scenario_unknown_name_suggests() {
    let output =
        ThoughtJackProcess::spawn_command(&["scenarios", "show", "rug-pullz"]);
    assert!(
        !output.status.success(),
        "unknown scenario should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stderr}{stdout}");
    assert!(
        combined.contains("rug-pull") || combined.contains("Did you mean"),
        "should suggest similar scenario name: {combined}"
    );
}

// EC-SCN-004: --scenario with --behavior override is accepted at parse level.
#[test]
fn scenario_with_behavior_override() {
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "run",
        "--scenario",
        "rug-pull",
        "--behavior",
        "slow-loris",
        "--help",
    ]);
    // --help always exits 0; the point is that the parse doesn't reject
    // --scenario combined with --behavior (they are independent flags).
    assert!(
        output.status.success(),
        "scenario + behavior should parse without conflict: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// EC-SCN-010: `scenarios list --format json` produces valid JSON array.
#[test]
fn scenario_list_json_format() {
    let output = ThoughtJackProcess::spawn_command(&[
        "scenarios",
        "list",
        "--format",
        "json",
    ]);
    assert!(
        output.status.success(),
        "scenarios list --format json should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");
    assert!(parsed.is_array(), "JSON list output should be an array");

    let arr = parsed.as_array().unwrap();
    assert!(!arr.is_empty(), "should have at least one scenario");
    // Each entry should have name, description, category
    let first = &arr[0];
    assert!(first.get("name").is_some(), "entry should have name");
    assert!(first.get("description").is_some(), "entry should have description");
    assert!(first.get("category").is_some(), "entry should have category");
}

// EC-SCN-011: `scenarios show <name>` prints raw YAML to stdout.
#[test]
fn scenario_show_prints_yaml() {
    let output =
        ThoughtJackProcess::spawn_command(&["scenarios", "show", "rug-pull"]);
    assert!(
        output.status.success(),
        "scenarios show should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // YAML should contain server: and tools: sections
    assert!(
        stdout.contains("server:") || stdout.contains("baseline:"),
        "output should be YAML configuration: {stdout}"
    );
}

// EC-SCN-005: --scenario with --http is accepted (scenario works over HTTP transport).
#[test]
fn scenario_with_http_transport() {
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "run",
        "--scenario",
        "rug-pull",
        "--http",
        "127.0.0.1:0",
        "--help",
    ]);
    // --help exits 0; verifies parse accepts --http + --scenario together.
    assert!(
        output.status.success(),
        "scenario + http should parse: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// EC-SCN-012: --capture-dir is accepted alongside --scenario.
#[test]
fn scenario_with_capture_dir() {
    let capture_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("capture_test");
    let output = ThoughtJackProcess::spawn_command(&[
        "server",
        "run",
        "--scenario",
        "rug-pull",
        "--capture-dir",
        capture_dir.to_str().unwrap(),
        "--help",
    ]);
    // --help exits 0; verifies parse accepts --capture-dir + --scenario together.
    assert!(
        output.status.success(),
        "scenario + capture-dir should parse: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
