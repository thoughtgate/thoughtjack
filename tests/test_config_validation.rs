mod common;

use common::ThoughtJackProcess;

/// Empty YAML file should be rejected with a clear error.
#[test]
fn empty_file_rejected() {
    let config = ThoughtJackProcess::fixture_path("empty.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "empty file should fail validation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("empty") || stderr.contains("Empty"),
        "error should mention 'empty': {stderr}"
    );
}

/// Binary content should be rejected (not a valid YAML file).
#[test]
fn binary_content_rejected() {
    // Write a temporary file with binary content
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let bin_path = dir.path().join("binary.yaml");
    std::fs::write(&bin_path, b"\x00\x01\x02\x03\xff\xfe\xfd\xfc").unwrap();

    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", bin_path.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "binary content should fail validation"
    );
}

/// YAML syntax errors should be caught with a clear parse error message.
#[test]
fn yaml_syntax_error_rejected() {
    let config = ThoughtJackProcess::fixture_path("bad_yaml.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "invalid YAML syntax should fail validation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("parse") || stderr.contains("error") || stderr.contains("unexpected"),
        "error should describe the parse failure: {stderr}"
    );
}

/// Duplicate tool names produce a warning but pass validation (per SPEC-001 F-015).
#[test]
fn duplicate_tool_names_warning() {
    let config = ThoughtJackProcess::fixture_path("duplicate_tools.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "duplicate tool names should pass validation (warning only): {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Duplicate tool name"),
        "should warn about duplicate tool name: {stderr}"
    );
}

/// Generator with depth exceeding limits should be rejected at load time.
#[test]
fn oversized_generator_rejected() {
    let config = ThoughtJackProcess::fixture_path("oversized_generator.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["server", "validate", config.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "oversized generator should fail validation"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("limit") || stderr.contains("exceeds"),
        "error should mention exceeding a limit: {stderr}"
    );
}
