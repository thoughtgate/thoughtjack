# CLAUDE.md - ThoughtJack

> Adversarial Agent Security Testing Tool

## Project Overview

ThoughtJack is a configurable adversarial testing tool for AI agent security. It simulates malicious servers and clients across multiple agent protocols (MCP, A2A, AG-UI), executing temporal attacks (rug pulls, sleeper agents), delivering malformed payloads, and testing agent resilience to protocol-level attacks.

Attack scenarios are authored as OATF (Open Agentic Testing Framework) documents — a declarative YAML format parsed by the `oatf-rs` SDK. ThoughtJack is the execution engine that brings these documents to life across protocols.

**Purpose**: Offensive security testing tool — the counterpart to ThoughtGate (defensive proxy).

**Target users**: Security researchers and security managers testing AI agent implementations.

## Tech Stack

- **Language**: Rust 2024 edition (MSRV 1.88)
- **Async runtime**: Tokio
- **CLI**: Clap (derive mode)
- **Serialization**: Serde (JSON + YAML)
- **Logging**: tracing + tracing-subscriber
- **Metrics**: metrics + metrics-exporter-prometheus
- **SDK**: `oatf-rs` — OATF document parsing, validation, interpolation, trigger evaluation, extractor capture
- **Concurrency primitives**: `tokio::sync::watch` (extractor publication), `CancellationToken` (cooperative shutdown), `DashMap` (shared extractor store)

**Monolithic crate**: ThoughtJack is a single crate. All protocol drivers, the engine, and CLI live in one `Cargo.toml`. A `[workspace]` in `Cargo.toml` includes the `fuzz/` crate for cargo-fuzz targets.

When adding dependencies, always check crates.io for the latest stable version:
```bash
cargo search <crate-name> --limit 1
```

## Architecture

See `specs/ARCHITECTURE.md` for the full system architecture and component diagrams.

The codebase follows the TJ-SPEC specifications in the `/specs` folder:

### v0.2 Modules (existing)

| Module | Spec | Purpose |
|--------|------|---------|
| `config/` | TJ-SPEC-001, 006 | Configuration schema and loader |
| `transport/` | TJ-SPEC-002 | stdio and HTTP transport abstraction |
| `phase/` | TJ-SPEC-003 | Phase engine state machine (v0.2 — being replaced by engine/) |
| `behavior/` | TJ-SPEC-004 | Delivery behaviors and side effects |
| `generator/` | TJ-SPEC-005 | Payload generators (`$generate` directive) |
| `cli/` | TJ-SPEC-007 | Command-line interface |
| `observability/` | TJ-SPEC-008 | Logging, metrics, events |
| `docgen/` | TJ-SPEC-011 | Documentation site generation |

### v0.5 Modules (new — OATF-based engine)

| Module | Spec | Purpose |
|--------|------|---------|
| `engine/` | TJ-SPEC-013 | Core engine: PhaseEngine, PhaseLoop, PhaseDriver trait, GenerationProvider |
| `engine/mcp_server.rs` | TJ-SPEC-013 §8.2 | MCP server PhaseDriver (the original mode, reimplemented on new engine) |
| `verdict/` | TJ-SPEC-014 | Grace period, indicator evaluation, verdict computation, output |
| `orchestration/` | TJ-SPEC-015 | Multi-actor: ExtractorStore, ActorRunner, Orchestrator, await_extractors |
| `protocol/agui.rs` | TJ-SPEC-016 | AG-UI client PhaseDriver |
| `protocol/a2a_server.rs` | TJ-SPEC-017 | A2A server PhaseDriver |
| `protocol/a2a_client.rs` | TJ-SPEC-017 | A2A client PhaseDriver |
| `protocol/mcp_client.rs` | TJ-SPEC-018 | MCP client PhaseDriver + server request handler |

### Key Architectural Principles

**v0.2 (still applies to existing modules):**
- **Lazy generator evaluation**: `$generate` creates factory objects at config load; bytes generated at response time
- **Atomic phase state**: Use `AtomicU64` + `DashMap` for lock-free concurrent access
- **Response before transition**: Response uses pre-transition state; entry actions fire after send

**v0.5 engine:**
- **PhaseLoop owns the event loop**: Trace append, extractor capture, trigger evaluation, phase advancement — all in PhaseLoop. Drivers only do protocol I/O.
- **Extractors via watch channel**: PhaseLoop publishes `HashMap<String, String>` on a `watch::Sender` after each event. Drivers receive `watch::Receiver`. Server-mode drivers borrow per-request; client-mode drivers clone once.
- **SDK delegates, ThoughtJack orchestrates**: The `oatf-rs` SDK handles document parsing, template interpolation, trigger evaluation, and extractor capture. ThoughtJack handles protocol transport, concurrency, and attack execution.
- **Synthesize output validation is permissive**: Validation is on by default but `--raw-synthesize` bypasses it entirely. This is an adversarial testing tool — intentionally malformed responses are a valid test case.

### v0.5 Key Types and Interfaces

These are the core abstractions that all v0.5 code builds on. **Read TJ-SPEC-013 §8 for full details.**

```rust
// The trait all protocol drivers implement. One impl per protocol mode.
trait PhaseDriver: Send {
    async fn drive_phase(
        &mut self,
        phase_index: usize,
        state: &serde_json::Value,
        extractors: watch::Receiver<HashMap<String, String>>,  // NOT &HashMap
        event_tx: mpsc::UnboundedSender<ProtocolEvent>,
        cancel: CancellationToken,
    ) -> Result<DriveResult, Error>;

    async fn on_phase_advanced(&mut self, from: usize, to: usize) -> Result<(), Error> {
        Ok(())
    }
}

// PhaseLoop<D: PhaseDriver> — generic over driver. Owns:
// - PhaseEngine (state machine)
// - watch::Sender<HashMap<String, String>> (extractor publication)
// - SharedTrace (append-only trace buffer)
// - ExtractorStore (cross-actor shared state)
// Runs the tokio::select! between driver execution and event consumption.

// ProtocolEvent — emitted by drivers, consumed by PhaseLoop
struct ProtocolEvent {
    direction: Direction,  // Incoming (from agent) or Outgoing (to agent)
    method: String,        // e.g. "tools/call", "message/send", "RUN_FINISHED"
    content: serde_json::Value,
}

// Orchestrator spawns ActorRunner tasks (one per actor in the OATF document).
// Each ActorRunner creates a PhaseLoop<SpecificDriver> based on the actor's mode.
// Results collected, merged trace passed to verdict pipeline.
```

**Driver implementation pattern** (follow this for every new driver):
1. Implement `PhaseDriver` trait
2. In `drive_phase()`: do protocol I/O, emit events on `event_tx`, respect `cancel`
3. Server-mode: `extractors.borrow().clone()` per request (fresh values)
4. Client-mode: `extractors.borrow().clone()` once at start (single request)
5. Response dispatch uses `oatf::select_response()` for ordered matching
6. Template interpolation uses `oatf::interpolate_template()` / `oatf::interpolate_value()`
7. Synthesize uses `GenerationProvider` + validation (unless `--raw-synthesize`)

### SDK Functions (oatf-rs)

These SDK calls appear throughout the engine code. If `oatf-rs` isn't available yet, stub them in `src/oatf_stubs.rs`:

| Function | Purpose | Used in |
|----------|---------|---------|
| `evaluate_trigger()` | Check if event matches phase trigger, track count | PhaseEngine |
| `interpolate_template()` | Replace `{{extractor_name}}` in strings | All drivers |
| `interpolate_value()` | Replace `{{...}}` in JSON values | All drivers |
| `select_response()` | Ordered-match response selection | All drivers |
| `evaluate_extractor()` | Capture values from protocol events | PhaseLoop |
| `resolve_event_qualifier()` | Extract qualifier from event content | PhaseLoop |
| `parse_event_qualifier()` | Split "tools/call[calculator]" → base + qualifier | PhaseLoop |
| `extract_protocol()` | Derive "mcp"/"a2a"/"ag_ui" from actor.mode | PhaseLoop |
| `compute_effective_state()` | Merge phase state chain | PhaseEngine |
| `CelEvaluator` | CEL expression evaluation for indicators | Verdict pipeline |

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

# Lint (must match CI — include --tests)
cargo clippy --tests -- -D warnings

# Format
cargo fmt

# Format check (CI)
cargo fmt -- --check

# Run a scenario (v0.5)
cargo run -- run --scenario <path.yaml>

# Run the server (v0.2 mode)
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
| `phase` | Phase engine, triggers, state (v0.2) |
| `engine` | v0.5 core engine: PhaseEngine, PhaseLoop, PhaseDriver |
| `verdict` | Verdict pipeline, indicator evaluation, output |
| `orchestration` | Multi-actor orchestration, ExtractorStore |
| `protocol` | Protocol drivers (MCP server/client, A2A, AG-UI) |
| `behavior` | Delivery behaviors or side effects |
| `generator` | Payload generators |
| `cli` | CLI commands, args, output |
| `observability` | Logging, metrics, events |
| `server` | Server runtime orchestration |
| `deps` | Dependency updates |

### Examples

```bash
# New feature
feat(engine): implement PhaseLoop with watch channel extractor publication

# Protocol driver
feat(protocol): add AG-UI client PhaseDriver

# Verdict pipeline
feat(verdict): implement CEL-based indicator evaluation

# Bug fix
fix(transport): handle partial reads in stdio transport

# Breaking change (note the !)
feat(config)!: rename inputSchema to input_schema for consistency

# Multiple scopes or cross-cutting
feat(engine,protocol): integrate A2A server driver with PhaseLoop

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
- **Clippy nursery lints are enabled** (`Cargo.toml` `[lints.clippy]` sets `nursery = "warn"`, CI promotes to errors). `cognitive_complexity` threshold is set to 50 in `clippy.toml`.

### Requirement Traceability

All public items must include an `Implements:` line in their doc comment linking to the spec requirement they satisfy. This aligns with the ThoughtGate sister project convention.

**Format:**
```rust
/// Brief description of the item.
///
/// Detailed explanation if needed.
///
/// Implements: TJ-SPEC-013 F-001
```

**Rules:**
- Place `Implements:` as the **last** doc-comment line (after `# Errors`, `# Panics`, etc.)
- Use spec ID + requirement ID: `TJ-SPEC-NNN F-NNN`
- Multiple requirements comma-separated: `TJ-SPEC-008 F-009, EC-OBS-021`
- Edge cases use `EC-XXX-NNN`, non-functional use `NFR-NNN`
- Annotate: `pub struct`, `pub enum`, `pub trait`, `pub fn`, `pub const`, `pub type`
- Skip: enum variants, struct fields, re-exports in `mod.rs`
- Module-level `//!` doc comments reference the spec but do not need `Implements:`

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

// watch channel for extractor publication (v0.5 pattern)
let (tx, _) = tokio::sync::watch::channel(HashMap::new());
// Publisher (PhaseLoop): tx.send(new_map) — cheap atomic swap
// Consumer (driver): rx.borrow().clone() — atomic load + clone
```

### Testing

- Unit tests in same file: `#[cfg(test)] mod tests { ... }`
- Integration tests in `tests/` directory
- Use `tokio::test` for async tests
- Test edge cases documented in specs (EC-XXX-NNN)
- Integration tests for drivers: use mock transports, verify JSON-RPC message sequences

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
├── Cargo.toml             # Single crate (no workspace)
├── build.rs               # Build-time metadata
├── CLAUDE.md              # This file
├── README.md
├── specs/                 # Specifications and architecture docs
│   ├── ARCHITECTURE.md
│   ├── TJ-SPEC-001_Configuration_Schema.md
│   ├── ...
│   ├── TJ-SPEC-008_Observability.md
│   ├── TJ-SPEC-013_OATF_Integration.md
│   ├── TJ-SPEC-014_Verdict_Evaluation_Output.md
│   ├── TJ-SPEC-015_Multi_Actor_Orchestration.md
│   ├── TJ-SPEC-016_AGUI_Protocol_Support.md
│   ├── TJ-SPEC-017_A2A_Protocol_Support.md
│   └── TJ-SPEC-018_MCP_Client_Mode.md
├── src/
│   ├── main.rs            # Entry point
│   ├── lib.rs             # Library root
│   ├── error.rs           # Error types
│   ├── server.rs          # Server runtime (v0.2)
│   ├── config/            # TJ-SPEC-001, 006
│   │   ├── mod.rs
│   │   ├── schema.rs
│   │   ├── loader.rs
│   │   └── validation.rs
│   ├── transport/         # TJ-SPEC-002
│   │   ├── mod.rs
│   │   ├── stdio.rs
│   │   ├── http.rs
│   │   └── jsonrpc.rs
│   ├── engine/            # TJ-SPEC-013 — v0.5 core engine
│   │   ├── mod.rs
│   │   ├── phase.rs       # PhaseEngine
│   │   ├── loop.rs        # PhaseLoop (or phase_loop.rs)
│   │   ├── driver.rs      # PhaseDriver trait + DriveResult + ProtocolEvent
│   │   ├── generation.rs  # GenerationProvider + synthesize validation
│   │   ├── types.rs       # Shared types (Direction, PhaseAction, etc.)
│   │   └── mcp_server.rs  # MCP server PhaseDriver (013 §8.2)
│   ├── verdict/           # TJ-SPEC-014
│   │   ├── mod.rs
│   │   ├── evaluation.rs  # Indicator evaluation pipeline
│   │   ├── grace.rs       # Grace period
│   │   ├── semantic.rs    # SemanticEvaluator (LLM-as-judge)
│   │   └── output.rs      # JSON + human summary output
│   ├── orchestration/     # TJ-SPEC-015
│   │   ├── mod.rs
│   │   ├── store.rs       # ExtractorStore
│   │   ├── runner.rs      # ActorRunner
│   │   ├── orchestrator.rs
│   │   └── trace.rs       # SharedTrace
│   ├── protocol/          # TJ-SPEC-016, 017, 018
│   │   ├── mod.rs
│   │   ├── agui.rs        # AG-UI client driver
│   │   ├── a2a_server.rs  # A2A server driver
│   │   ├── a2a_client.rs  # A2A client driver
│   │   └── mcp_client.rs  # MCP client driver + server request handler
│   ├── phase/             # TJ-SPEC-003 (v0.2 — kept for backward compat)
│   │   └── ...
│   ├── behavior/          # TJ-SPEC-004
│   │   └── ...
│   ├── generator/         # TJ-SPEC-005
│   │   └── ...
│   ├── docgen/            # TJ-SPEC-011
│   │   └── ...
│   ├── cli/               # TJ-SPEC-007
│   │   └── ...
│   └── observability/     # TJ-SPEC-008
│       └── ...
├── scenarios/             # Built-in attack scenarios (TJ-SPEC-010)
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

### Foundation (v0.2)

| File | Description |
|------|-------------|
| `specs/ARCHITECTURE.md` | System architecture and component diagram |
| `specs/TJ-SPEC-001_Configuration_Schema.md` | YAML configuration format |
| `specs/TJ-SPEC-002_Transport_Abstraction.md` | stdio and HTTP transports |
| `specs/TJ-SPEC-003_Phase_Engine.md` | State machine for temporal attacks (v0.2) |
| `specs/TJ-SPEC-004_Behavioral_Modes.md` | Delivery behaviors and side effects |
| `specs/TJ-SPEC-005_Payload_Generation.md` | `$generate` directive |
| `specs/TJ-SPEC-006_Configuration_Loader.md` | YAML parsing and validation |
| `specs/TJ-SPEC-007_CLI_Interface.md` | Commands and flags |
| `specs/TJ-SPEC-008_Observability.md` | Logging, metrics, events |

### OATF Engine (v0.5)

| File | Description |
|------|-------------|
| `specs/TJ-SPEC-013_OATF_Integration.md` | Core engine: PhaseEngine, PhaseLoop, PhaseDriver, MCP server driver, SDK integration |
| `specs/TJ-SPEC-014_Verdict_Evaluation_Output.md` | Grace period, indicator evaluation, verdict computation, output formats |
| `specs/TJ-SPEC-015_Multi_Actor_Orchestration.md` | ExtractorStore, ActorRunner, Orchestrator, await_extractors, merged trace |
| `specs/TJ-SPEC-016_AGUI_Protocol_Support.md` | AG-UI client PhaseDriver, SSE streaming, event mapping |
| `specs/TJ-SPEC-017_A2A_Protocol_Support.md` | A2A server + client PhaseDrivers, Agent Card, task dispatch |
| `specs/TJ-SPEC-018_MCP_Client_Mode.md` | MCP client PhaseDriver, split transport, server request handler |

**Always read the relevant spec before implementing.** Each spec contains:
- Functional requirements (F-NNN)
- Edge cases (EC-XXX-NNN) — these need tests
- Non-functional requirements (NFR-NNN)
- Definition of Done checklist
- Pseudocode with explicit SDK call sites and type signatures

## Security Considerations

ThoughtJack is an **offensive security tool**. When developing:

1. **Never run against production systems**
2. **Respect resource limits** — Generators have configurable max sizes
3. **Test in isolation** — Use containers or VMs
4. **No real exfiltration** — Tool simulates attacks, doesn't actually steal data
5. **`--raw-synthesize` is intentional** — Bypassing output validation is a feature, not a bug. It enables testing how agents handle malformed protocol messages.

## Common Tasks

### Adding a new protocol driver

1. Create `src/protocol/<name>.rs`
2. Implement `PhaseDriver` trait (see TJ-SPEC-013 §8.4 for the trait)
3. Follow the driver pattern: protocol I/O in `drive_phase()`, events on `event_tx`
4. Server-mode: `extractors.borrow().clone()` per request
5. Client-mode: `extractors.borrow().clone()` once at start
6. Use `oatf::select_response()` for response dispatch
7. Add to `ActorRunner` match in `src/orchestration/runner.rs`
8. Write integration tests with mock transport
9. Add commit scope to this file if it's a new protocol

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
