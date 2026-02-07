mod common;

use common::ThoughtJackProcess;

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
