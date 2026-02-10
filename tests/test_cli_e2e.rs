mod common;

use common::ThoughtJackProcess;

// ============================================================================
// version command
// ============================================================================

#[test]
fn version_human() {
    let output = ThoughtJackProcess::spawn_command(&["version"]);
    assert!(
        output.status.success(),
        "version should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lower = stdout.to_lowercase();
    assert!(
        lower.contains("thoughtjack"),
        "version output should contain 'thoughtjack': {stdout}"
    );
    // Check for semver-like pattern (digits.digits.digits)
    assert!(
        stdout.contains('.'),
        "version output should contain a version number: {stdout}"
    );
}

#[test]
fn version_json() {
    let output = ThoughtJackProcess::spawn_command(&["version", "--format", "json"]);
    assert!(
        output.status.success(),
        "version --format json should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("version JSON should be valid");
    assert!(
        parsed.get("name").is_some(),
        "JSON should have 'name' key: {stdout}"
    );
    assert!(
        parsed.get("version").is_some(),
        "JSON should have 'version' key: {stdout}"
    );
}

// ============================================================================
// completions command
// ============================================================================

#[test]
fn completions_bash() {
    let output = ThoughtJackProcess::spawn_command(&["completions", "bash"]);
    assert!(
        output.status.success(),
        "completions bash should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "completions bash should produce output");
    assert!(
        stdout.contains("thoughtjack"),
        "bash completions should reference thoughtjack: {stdout}"
    );
}

#[test]
fn completions_zsh() {
    let output = ThoughtJackProcess::spawn_command(&["completions", "zsh"]);
    assert!(
        output.status.success(),
        "completions zsh should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "completions zsh should produce output");
}

#[test]
fn completions_fish() {
    let output = ThoughtJackProcess::spawn_command(&["completions", "fish"]);
    assert!(
        output.status.success(),
        "completions fish should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.is_empty(), "completions fish should produce output");
}

// ============================================================================
// diagram command
// ============================================================================

#[test]
fn diagram_phased() {
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("rug-pull.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["diagram", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "diagram should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stateDiagram") || stdout.contains("graph") || stdout.contains("sequenceDiagram"),
        "diagram output should contain mermaid syntax: {stdout}"
    );
}

#[test]
fn diagram_simple() {
    let scenarios_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let path = scenarios_dir.join("prompt-injection.yaml");
    let output =
        ThoughtJackProcess::spawn_command(&["diagram", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "diagram (simple scenario) should exit 0: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.is_empty(),
        "diagram output should be non-empty"
    );
}

#[test]
fn diagram_missing_file() {
    let output =
        ThoughtJackProcess::spawn_command(&["diagram", "/tmp/nonexistent_thoughtjack_test.yaml"]);
    assert!(
        !output.status.success(),
        "diagram on missing file should fail"
    );
}

// ============================================================================
// docs command
// ============================================================================

#[test]
fn docs_validate() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scenarios = manifest_dir.join("scenarios");
    let registry = scenarios.join("registry.yaml");
    let output = ThoughtJackProcess::spawn_command(&[
        "docs",
        "validate",
        "--scenarios",
        scenarios.to_str().unwrap(),
        "--registry",
        registry.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "docs validate should exit 0: {}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn docs_generate() {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scenarios = manifest_dir.join("scenarios");
    let registry = scenarios.join("registry.yaml");
    let tmpdir = tempfile::tempdir().expect("failed to create temp dir");
    let output_dir = tmpdir.path().join("docs-output");

    let output = ThoughtJackProcess::spawn_command(&[
        "docs",
        "generate",
        "--scenarios",
        scenarios.to_str().unwrap(),
        "--registry",
        registry.to_str().unwrap(),
        "--output",
        output_dir.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "docs generate should exit 0: {}{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );

    // Verify output directory was created and contains files
    assert!(output_dir.exists(), "output directory should be created");
    let entries: Vec<_> = std::fs::read_dir(&output_dir)
        .expect("should read output dir")
        .collect();
    assert!(
        !entries.is_empty(),
        "output directory should contain generated files"
    );
}
