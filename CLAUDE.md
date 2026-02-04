# CLAUDE.md - ThoughtJack

> Adversarial MCP Server for Security Testing

## Project Overview

ThoughtJack is a configurable adversarial MCP (Model Context Protocol) server designed to test AI agent security. It simulates malicious MCP servers that execute temporal attacks (rug pulls, sleeper agents), deliver malformed payloads, and test client resilience to protocol-level attacks.

**Purpose**: Offensive security testing tool — the counterpart to ThoughtGate (defensive proxy).

**Target users**: Security researchers testing MCP client implementations.

## Tech Stack

- **Language**: Rust 2024 edition (MSRV 1.85)
- **Async runtime**: Tokio
- **CLI**: Clap (derive mode)
- **Serialization**: Serde (JSON + YAML)
- **Logging**: tracing + tracing-subscriber
- **Metrics**: metrics + metrics-exporter-prometheus

When adding dependencies, always check crates.io for the latest stable version:
```bash
cargo search <crate-name> --limit 1
```

## Architecture

See `specs/ARCHITECTURE.md` for the full system architecture and component diagrams.

The codebase follows the TJ-SPEC specifications in the `/specs` folder:

| Module | Spec | Purpose |
|--------|------|---------|
| `config/` | TJ-SPEC-001, 006 | Configuration schema and loader |
| `transport/` | TJ-SPEC-002 | stdio and HTTP transport abstraction |
| `phase/` | TJ-SPEC-003 | Phase engine state machine |
| `behavior/` | TJ-SPEC-004 | Delivery behaviors and side effects |
| `generator/` | TJ-SPEC-005 | Payload generators (`$generate` directive) |
| `cli/` | TJ-SPEC-007 | Command-line interface |
| `observability/` | TJ-SPEC-008 | Logging, metrics, events |

Key architectural principles:
- **Lazy generator evaluation**: `$generate` creates factory objects at config load; bytes generated at response time
- **Atomic phase state**: Use `AtomicU64` + `DashMap` for lock-free concurrent access
- **Response before transition**: Response uses pre-transition state; entry actions fire after send

## Build Commands

```bash
# Check compilation
cargo check

# Build debug
cargo build

# Build release
cargo build --release

# Run tests
cargo test

# Run with coverage
cargo llvm-cov --html

# Lint
cargo clippy -- -D warnings

# Format
cargo fmt

# Format check (CI)
cargo fmt -- --check

# Run the server
cargo run -- server run --config <path>

# Validate a config
cargo run -- server validate <path>
```

## Commit Conventions

This project uses **Conventional Commits** (https://www.conventionalcommits.org/).

### Format

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

### Types

| Type | Description |
|------|-------------|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `style` | Code style/formatting (no logic change) |
| `refactor` | Code restructuring (no feature/fix) |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `build` | Build system or dependencies |
| `ci` | CI/CD configuration |
| `chore` | Maintenance tasks |
| `revert` | Reverting a previous commit |

### Scopes

Use these scopes matching the module structure:

| Scope | When to use |
|-------|-------------|
| `config` | Configuration schema or loader changes |
| `transport` | Transport trait or implementations |
| `phase` | Phase engine, triggers, state |
| `behavior` | Delivery behaviors or side effects |
| `generator` | Payload generators |
| `cli` | CLI commands, args, output |
| `observability` | Logging, metrics, events |
| `server` | Server runtime orchestration |
| `deps` | Dependency updates |

### Examples

```bash
# New feature
feat(phase): add content matching for trigger evaluation

# Bug fix
fix(transport): handle partial reads in stdio transport

# Breaking change (note the !)
feat(config)!: rename inputSchema to input_schema for consistency

# Multiple scopes or cross-cutting
feat(phase,behavior): integrate side effect triggers with phase transitions

# No scope for broad changes
chore: update all dependencies to latest versions

# With body and footer
fix(generator): prevent stack overflow in nested JSON generation

The iterative approach replaces recursive calls to avoid
stack exhaustion at depth > 10000.

Fixes #42
```

### Commit Message Guidelines

1. **Subject line**: Max 72 characters, imperative mood ("add" not "added")
2. **Body**: Wrap at 72 characters, explain *what* and *why* (not *how*)
3. **Footer**: Reference issues with `Fixes #N` or `Closes #N`
4. **Breaking changes**: Add `!` after type/scope OR add `BREAKING CHANGE:` footer

## Code Style

### Rust Conventions

- Follow Rust API guidelines: https://rust-lang.github.io/api-guidelines/
- Use `rustfmt` defaults (no custom config)
- All public items must have doc comments
- Prefer `thiserror` for error types
- Use `tracing` macros, not `println!`

### Error Handling

```rust
// Use thiserror for library errors
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to parse YAML at {path}: {message}")]
    ParseError { path: PathBuf, message: String },
}

// Propagate with ? operator, add context where helpful
let config = loader.load(&path)
    .map_err(|e| ConfigError::LoadFailed { path, source: e })?;
```

### Async Patterns

```rust
// Prefer async closures (Rust 2024)
let handle = tokio::spawn(async move || {
    // ...
});

// Use CancellationToken for cooperative shutdown
tokio::select! {
    _ = cancel.cancelled() => { /* cleanup */ }
    result = operation() => { /* handle */ }
}
```

### Testing

- Unit tests in same file: `#[cfg(test)] mod tests { ... }`
- Integration tests in `tests/` directory
- Use `tokio::test` for async tests
- Test edge cases documented in specs (EC-XXX-NNN)

## File Naming

| Type | Convention | Example |
|------|------------|---------|
| Modules | snake_case | `phase_engine.rs` |
| Types | PascalCase | `PhaseEngine` |
| Functions | snake_case | `evaluate_trigger` |
| Constants | SCREAMING_SNAKE | `MAX_NEST_DEPTH` |
| Test files | `test_*.rs` or `*_test.rs` | `test_phase_engine.rs` |
| Fixtures | descriptive | `tests/fixtures/rug_pull.yaml` |

## Directory Structure

```
thoughtjack/
├── Cargo.toml
├── CLAUDE.md              # This file
├── README.md
├── specs/                 # Specifications and architecture docs
│   ├── ARCHITECTURE.md
│   ├── TJ-SPEC-001_Configuration_Schema.md
│   ├── TJ-SPEC-002_Transport_Abstraction.md
│   ├── TJ-SPEC-003_Phase_Engine.md
│   ├── TJ-SPEC-004_Behavioral_Modes.md
│   ├── TJ-SPEC-005_Payload_Generation.md
│   ├── TJ-SPEC-006_Configuration_Loader.md
│   ├── TJ-SPEC-007_CLI_Interface.md
│   └── TJ-SPEC-008_Observability.md
├── src/
│   ├── main.rs            # Entry point
│   ├── lib.rs             # Library root
│   ├── error.rs           # Error types
│   ├── server.rs          # Server runtime
│   ├── config/
│   │   ├── mod.rs
│   │   ├── schema.rs      # Type definitions
│   │   ├── loader.rs      # YAML loading + directives
│   │   └── validation.rs  # Config validation
│   ├── transport/
│   │   ├── mod.rs
│   │   ├── stdio.rs
│   │   ├── http.rs
│   │   └── jsonrpc.rs
│   ├── phase/
│   │   ├── mod.rs
│   │   ├── state.rs
│   │   ├── engine.rs
│   │   ├── trigger.rs
│   │   └── actions.rs
│   ├── behavior/
│   │   ├── mod.rs
│   │   ├── delivery.rs
│   │   └── side_effects.rs
│   ├── generator/
│   │   ├── mod.rs
│   │   ├── nested_json.rs
│   │   ├── garbage.rs
│   │   └── ...
│   ├── cli/
│   │   ├── mod.rs
│   │   ├── args.rs
│   │   └── commands/
│   └── observability/
│       ├── mod.rs
│       ├── logging.rs
│       ├── metrics.rs
│       └── events.rs
├── library/               # Attack pattern library
│   ├── tools/
│   ├── servers/
│   └── resources/
└── tests/
    ├── integration/
    └── fixtures/
```

## Specification References

All specifications are stored in the `/specs` folder. When implementing features, read the relevant spec first:

| File | Description |
|------|-------------|
| `specs/ARCHITECTURE.md` | System architecture and component diagram |
| `specs/TJ-SPEC-001_Configuration_Schema.md` | YAML configuration format |
| `specs/TJ-SPEC-002_Transport_Abstraction.md` | stdio and HTTP transports |
| `specs/TJ-SPEC-003_Phase_Engine.md` | State machine for temporal attacks |
| `specs/TJ-SPEC-004_Behavioral_Modes.md` | Delivery behaviors and side effects |
| `specs/TJ-SPEC-005_Payload_Generation.md` | `$generate` directive |
| `specs/TJ-SPEC-006_Configuration_Loader.md` | YAML parsing and validation |
| `specs/TJ-SPEC-007_CLI_Interface.md` | Commands and flags |
| `specs/TJ-SPEC-008_Observability.md` | Logging, metrics, events |

**Always read the relevant spec before implementing.** Each spec contains:
- Functional requirements (F-NNN)
- Edge cases (EC-XXX-NNN) — these need tests
- Non-functional requirements (NFR-NNN)
- Definition of Done checklist
- Example code and configurations

## Security Considerations

ThoughtJack is an **offensive security tool**. When developing:

1. **Never run against production systems**
2. **Respect resource limits** — Generators have configurable max sizes
3. **Test in isolation** — Use containers or VMs
4. **No real exfiltration** — Tool simulates attacks, doesn't actually steal data

## Common Tasks

### Adding a new delivery behavior

1. Add variant to `DeliveryConfig` enum in `src/config/schema.rs`
2. Create struct implementing `DeliveryBehavior` trait in `src/behavior/delivery.rs`
3. Update factory function `create_delivery_behavior()`
4. Add tests for stdio and HTTP transports
5. Document in library examples

### Adding a new generator

1. Add variant to `GeneratorType` enum in `src/config/schema.rs`
2. Create struct implementing `PayloadGenerator` trait in `src/generator/`
3. Update factory function `create_generator()`
4. Add limit validation in constructor
5. Add determinism test (same seed = same output)

### Adding a new CLI command

1. Add variant to `Commands` enum in `src/cli/args.rs`
2. Create handler in `src/cli/commands/`
3. Add to dispatch in `src/main.rs`
4. Update shell completions