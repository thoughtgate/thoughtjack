# Contributing to ThoughtJack

Thank you for your interest in contributing to ThoughtJack! This document provides guidelines and instructions for contributing.

## Code of Conduct

Be respectful and constructive in all interactions. ThoughtJack is a security testing tool, so please:
- Never use it against systems you don't own or have authorization to test
- Report security vulnerabilities responsibly (see [SECURITY.md](docs/SECURITY.md))
- Keep discussions focused on security research and testing

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/thoughtjack.git`
3. Add upstream remote: `git remote add upstream https://github.com/thoughtgate/thoughtjack.git`
4. Create a feature branch: `git checkout -b feature/your-feature-name`

## Development Setup

### Prerequisites

- Rust 1.85+ (2024 edition)
- `cargo-fuzz` for fuzzing tests (optional)
- `cosign` for verifying release signatures (optional)

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run clippy
cargo clippy -- -D warnings

# Format code
cargo fmt
```

## Security Testing

### Running Fuzzing Locally

ThoughtJack uses cargo-fuzz (libFuzzer) for continuous security testing:

```bash
# Install nightly toolchain and cargo-fuzz
rustup toolchain install nightly
cargo install cargo-fuzz

# Update corpus with current scenarios
./fuzz/scripts/update_corpus.sh

# Run a fuzz target (5 minutes)
cd fuzz
cargo +nightly fuzz run fuzz_config_loader -- -max_total_time=300

# View coverage
cargo +nightly fuzz coverage fuzz_config_loader
```

### Reporting Fuzzing Crashes

If you find a crash while fuzzing:

1. **Minimize the crash**:
   ```bash
   cargo +nightly fuzz tmin fuzz_config_loader artifacts/fuzz_config_loader/crash-*
   ```

2. **Create a regression test**:
   ```bash
   # Add minimized crash to test fixtures
   cp artifacts/fuzz_config_loader/minimized-crash tests/fixtures/fuzz-crash-001.yaml

   # Write a test that reproduces it
   # (add to tests/integration/test_config_loader.rs)
   ```

3. **Report via GitHub Security Advisories**:
   - Go to https://github.com/thoughtgate/thoughtjack/security/advisories
   - Click "Report a vulnerability"
   - Include minimized crash input and stack trace

## Adding New Attack Scenarios

1. Create a YAML file in `scenarios/`:
   ```yaml
   metadata:
     name: "Your Attack Name"
     description: "Brief description of the attack"
     severity: "high"  # low, medium, high, critical
     category: "temporal"  # temporal, injection, dos, resource, protocol, multi-vector

   server:
     name: "your-attack-server"
     version: "1.0.0"

   # Define baseline and phases...
   ```

2. Add documentation to the YAML file (used by docs generator)

3. Test the scenario:
   ```bash
   # Validate config
   cargo run -- server validate scenarios/your-attack.yaml

   # Run it
   cargo run -- server run --config scenarios/your-attack.yaml

   # Generate diagram
   cargo run -- diagram scenarios/your-attack.yaml
   ```

4. Update the corpus:
   ```bash
   ./fuzz/scripts/update_corpus.sh
   ```

5. Add tests in `tests/integration/`

## Commit Guidelines

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

### Types

- `feat`: New feature or capability
- `fix`: Bug fix
- `docs`: Documentation only
- `style`: Code style/formatting (no logic change)
- `refactor`: Code restructuring (no feature/fix)
- `perf`: Performance improvement
- `test`: Adding or updating tests
- `build`: Build system or dependencies
- `ci`: CI/CD configuration
- `chore`: Maintenance tasks

### Scopes

Match the module structure:
- `config`, `transport`, `phase`, `behavior`, `generator`
- `cli`, `observability`, `server`
- `deps` for dependency updates

### Examples

```bash
feat(phase): add content matching for trigger evaluation
fix(transport): handle partial reads in stdio transport
docs(security): add fuzzing instructions to CONTRIBUTING.md
test(generator): add regression test for nested JSON depth limits
```

## Pull Request Process

1. **Update your branch** with latest upstream:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```

2. **Run all checks** before submitting:
   ```bash
   cargo fmt -- --check
   cargo clippy -- -D warnings
   cargo test
   cargo build --release
   ```

3. **Write clear PR description**:
   - What does this change?
   - Why is it needed?
   - How was it tested?
   - Link to related issues

4. **Wait for CI** - All checks must pass:
   - Build (Linux, macOS, Windows)
   - Tests
   - Clippy (pedantic + nursery)
   - CodeQL analysis
   - cargo-deny (dependency audit)

5. **Address review feedback** - Respond to all comments

6. **Squash commits** before merge (maintainers will do this)

## Code Style

- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `rustfmt` defaults (no custom config)
- All `pub` items need doc comments with `Implements: TJ-SPEC-NNN F-NNN`
- Prefer `thiserror` for error types
- Use `tracing` macros, not `println!`
- Never use `gen` as a variable name (reserved keyword in Rust 2024)

## Testing

- Unit tests in same file: `#[cfg(test)] mod tests { ... }`
- Integration tests in `tests/` directory
- Use `tokio::test` for async tests
- Test edge cases documented in specs
- Add fuzzing corpus entries for new parsers/generators

## Security Considerations

When adding new features:

1. **Set resource limits** - All generators must respect `GeneratorLimits`
2. **Prevent infinite loops** - Use iteration instead of recursion where possible
3. **Validate input** - Never trust config files or JSON-RPC messages
4. **Document attack vectors** - Explain what attack this simulates and why
5. **Test in isolation** - Never run against production systems

Example:
```rust
pub fn new(params: &HashMap<String, Value>, limits: &GeneratorLimits) -> Result<Self, GeneratorError> {
    let depth = require_usize(params, "depth")?;

    // Validate against limits
    if depth > limits.max_nest_depth {
        return Err(GeneratorError::LimitExceeded(
            format!("depth {depth} exceeds limit {}", limits.max_nest_depth)
        ));
    }

    // Check estimated size
    let estimated_size = estimate_size(depth);
    if estimated_size > limits.max_payload_bytes {
        return Err(GeneratorError::LimitExceeded(
            format!("estimated size {estimated_size} exceeds limit {}", limits.max_payload_bytes)
        ));
    }

    Ok(Self { depth, /* ... */ })
}
```

## Documentation

- Follow [Diataxis](https://diataxis.fr/) framework:
  - **Tutorials** - Step-by-step guides (learning-oriented)
  - **How-To Guides** - Task recipes (problem-oriented)
  - **Reference** - API and config docs (information-oriented)
  - **Explanation** - Architecture and design (understanding-oriented)

- Update docs when adding features:
  ```bash
  # Regenerate documentation
  cargo run -- docs generate

  # Validate docs
  cargo run -- docs validate
  ```

## Questions?

- Open a [discussion](https://github.com/thoughtgate/thoughtjack/discussions)
- Check existing [issues](https://github.com/thoughtgate/thoughtjack/issues)
- Read the [documentation](https://thoughtgate.github.io/thoughtjack/)

Thank you for contributing to ThoughtJack! 🎉
