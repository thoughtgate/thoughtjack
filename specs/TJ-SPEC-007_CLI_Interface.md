# TJ-SPEC-007: CLI Interface

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-007` |
| **Title** | CLI Interface |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **High** |
| **Version** | v2.0.0 |
| **Tags** | `#cli` `#commands` `#flags` `#output` `#exit-codes` |
| **Supersedes** | TJ-SPEC-007 v1.0.0 |
| **Source** | TJ-SPEC-013 §12 (canonical definition) |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's command-line interface for v0.5. The CLI executes OATF documents across any supported protocol mode (MCP server/client, A2A server/client, AG-UI client), validates documents, and manages the built-in scenario library.

### 1.1 Motivation

The v0.5 CLI replaces the v0.2 protocol-specific subcommand tree (`thoughtjack server`, `thoughtjack agent`) with a single `thoughtjack run` command. The OATF document declares actor modes; the CLI provides runtime configuration (transport, endpoints, session control). This means one command handles any document regardless of protocol.

### 1.2 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Document-driven** | OATF document declares what to do; CLI says how and where |
| **Transport inference** | No explicit transport flags — inferred from endpoint flags |
| **Flags over positional args** | Clearer, order-independent |
| **Environment variable fallback** | CI/CD integration without command-line changes |
| **Structured output option** | JSON verdict for tooling, human summary for operators |
| **Fail fast with clear errors** | Don't start execution with invalid document |

### 1.3 Command Hierarchy

```
thoughtjack
├── run                  # Execute an OATF document (any mode, single or multi-actor)
├── scenarios            # Built-in scenario library
│   ├── list             # List available scenarios (filterable)
│   ├── show <name>      # Print scenario YAML 
│   └── run <name>       # Execute a built-in scenario (same flags as run)
├── validate <path>      # Validate OATF document (--normalize)
└── version              # Version info
```

### 1.4 Scope Boundaries

**In scope:**
- Command and subcommand structure
- Flag definitions and validation
- Transport inference rules
- Environment variable mapping (including authentication)
- Output model (human summary + JSON verdict)
- Exit codes
- Help text

**Out of scope:**
- Server runtime behavior (TJ-SPEC-013)
- OATF document format (oatf-rs SDK)
- Verdict computation logic (TJ-SPEC-014)
- Observability configuration (TJ-SPEC-008)

---

## 2. Functional Requirements

### F-001: Root Command

The system SHALL provide a root `thoughtjack` command with global options.

**Acceptance Criteria:**
- Shows help when run without subcommand
- Supports `--version` flag
- Supports `--help` flag
- Global flags available to all subcommands

**Usage:**
```bash
thoughtjack [OPTIONS] <COMMAND>

Commands:
  run          Execute an OATF document
  scenarios    List, inspect, and run built-in attack scenarios
  validate     Validate an OATF document
  version      Show version information
  help         Print help for a command

Options:
  -v, --verbose...     Increase logging verbosity (-v, -vv, -vvv)
  -q, --quiet          Suppress non-essential output
      --color <WHEN>   Colorize output [default: auto] [possible values: auto, always, never]
  -h, --help           Print help
  -V, --version        Print version
```

**Implementation:**
```rust
#[derive(Parser)]
#[command(name = "thoughtjack")]
#[command(author, version, about = "Adversarial agent security testing tool")]
#[command(propagate_version = true)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Increase verbosity (-v info, -vv debug, -vvv trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Suppress all non-error output
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Color output control
    #[arg(long, default_value = "auto", global = true, env = "THOUGHTJACK_COLOR")]
    pub color: ColorChoice,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Execute an OATF document
    Run(RunArgs),

    /// List, inspect, and run built-in attack scenarios
    Scenarios(ScenariosCommand),

    /// Validate an OATF document
    Validate(ValidateArgs),

    /// Display version and build information
    Version(VersionArgs),
}
```

### F-002: Run Command

The system SHALL provide `thoughtjack run` to execute any OATF document.

**Acceptance Criteria:**
- Loads and validates OATF document
- Infers transport from flags (see §3 Transport Inference)
- Supports all protocol modes via mode-specific flags
- Produces human summary on stderr, optional JSON verdict to file/stdout
- Returns exit code per verdict result (see §5 Exit Codes)

**Usage:**
```bash
thoughtjack run --config <path>

# Transport — MCP server
  --mcp-server <addr:port>          # Set → HTTP transport. Unset → stdio.

# Transport — MCP client
  --mcp-client-command <cmd>        # Set → stdio (spawn agent process)
  --mcp-client-args <args>          # Arguments for spawned process
  --mcp-client-endpoint <url>       # Set → HTTP

# Transport — AG-UI client
  --agui-client-endpoint <url>      # Agent endpoint

# Transport — A2A
  --a2a-server <addr:port>          # A2A server listen address
  --a2a-client-endpoint <url>       # A2A client target

# Session control
  --grace-period <duration>         # Override document grace period
  --max-session <duration>          # Safety timeout [default: 5m]

# Output
  --output <path>                   # Write JSON verdict to file (use - for stdout)
  --header <key:value>              # Global HTTP headers for client transports (repeatable)
  --no-semantic                     # Skip semantic indicators
  --raw-synthesize                  # Bypass synthesize output validation (inject LLM output as-is)
```

**Implementation:**
```rust
#[derive(Args)]
pub struct RunArgs {
    /// OATF document to execute
    #[arg(short, long, env = "THOUGHTJACK_CONFIG")]
    pub config: PathBuf,

    // --- MCP server transport ---
    /// MCP server HTTP listen address (omit for stdio)
    #[arg(long, value_name = "ADDR:PORT")]
    pub mcp_server: Option<String>,

    // --- MCP client transport ---
    /// Spawn agent via command (stdio transport)
    #[arg(long, value_name = "CMD")]
    pub mcp_client_command: Option<String>,

    /// Arguments for spawned agent process
    #[arg(long, value_name = "ARGS", requires = "mcp_client_command")]
    pub mcp_client_args: Option<String>,

    /// Connect to agent HTTP endpoint
    #[arg(long, value_name = "URL", conflicts_with = "mcp_client_command")]
    pub mcp_client_endpoint: Option<String>,

    // --- AG-UI client transport ---
    /// AG-UI agent endpoint
    #[arg(long, value_name = "URL")]
    pub agui_client_endpoint: Option<String>,

    // --- A2A transport ---
    /// A2A server listen address [default: 127.0.0.1:9090]
    #[arg(long, value_name = "ADDR:PORT")]
    pub a2a_server: Option<String>,

    /// A2A client target endpoint
    #[arg(long, value_name = "URL")]
    pub a2a_client_endpoint: Option<String>,

    // --- Session control ---
    /// Override document grace period
    #[arg(long, value_name = "DURATION")]
    pub grace_period: Option<humantime::Duration>,

    /// Safety timeout for entire session [default: 5m]
    #[arg(long, value_name = "DURATION", default_value = "5m")]
    pub max_session: humantime::Duration,

    // --- Output ---
    /// Write JSON verdict to file (use - for stdout)
    #[arg(long, value_name = "PATH")]
    pub output: Option<String>,

    /// HTTP headers for client transports (repeatable)
    #[arg(long = "header", value_name = "KEY:VALUE")]
    pub headers: Vec<String>,

    /// Skip semantic indicators
    #[arg(long)]
    pub no_semantic: bool,

    /// Bypass synthesize output validation
    #[arg(long)]
    pub raw_synthesize: bool,
}
```

**Examples:**
```bash
# MCP server via stdio (Claude Code integration)
thoughtjack run --config rug-pull.yaml

# MCP server via HTTP
thoughtjack run --config rug-pull.yaml --mcp-server 0.0.0.0:8080

# Probe an agent's MCP server via stdio
thoughtjack run --config probe.yaml --mcp-client-command "python agent.py"

# AG-UI client attack
thoughtjack run --config agui-attack.yaml --agui-client-endpoint http://localhost:8000/agent

# Cross-protocol multi-actor (MCP server stdio + A2A server)
thoughtjack run --config cross.yaml --a2a-server 0.0.0.0:9090

# CI: write JSON verdict, fail on exploit
thoughtjack run --config attack.yaml --output verdict.json
```

### F-003: Scenarios Command

The system SHALL provide `thoughtjack scenarios` to manage built-in attack scenarios.

**Acceptance Criteria:**
- `list` shows available scenarios with optional filtering
- `show` prints scenario YAML 
- `run` executes a built-in scenario with all `run` flags

**Usage:**
```bash
# List all built-in scenarios
thoughtjack scenarios list

# Filter by category or tag
thoughtjack scenarios list --category capability_poisoning
thoughtjack scenarios list --tag rug-pull

# Show scenario YAML
thoughtjack scenarios show rug-pull

# Execute built-in scenario (accepts all run flags)
thoughtjack scenarios run rug-pull
thoughtjack scenarios run rug-pull --mcp-server 0.0.0.0:8080 --output verdict.json
```

**Implementation:**
```rust
#[derive(Subcommand)]
pub enum ScenariosCommand {
    /// List available built-in scenarios
    List(ScenariosListArgs),
    /// Display scenario details
    Show(ScenariosShowArgs),
    /// Execute a built-in scenario
    Run(ScenariosRunArgs),
}

#[derive(Args)]
pub struct ScenariosListArgs {
    /// Filter by category
    #[arg(long)]
    pub category: Option<String>,
    /// Filter by tag
    #[arg(long)]
    pub tag: Option<String>,
}

#[derive(Args)]
pub struct ScenariosShowArgs {
    /// Scenario name
    pub name: String,
}

#[derive(Args)]
pub struct ScenariosRunArgs {
    /// Scenario name
    pub name: String,
    /// All run flags are inherited
    #[command(flatten)]
    pub run: RunArgs,
}
```

### F-004: Validate Command

The system SHALL provide `thoughtjack validate` for OATF document validation.

**Acceptance Criteria:**
- Validates document against OATF schema via SDK
- `--normalize` prints the pre-processed document (after `await_extractors` stripping)

**Usage:**
```bash
# Validate OATF document
thoughtjack validate attack.yaml

# Validate and print pre-processed OATF
thoughtjack validate attack.yaml --normalize

```

**Implementation:**
```rust
#[derive(Args)]
pub struct ValidateArgs {
    /// OATF document to validate
    pub path: PathBuf,

    /// Print pre-processed document
    #[arg(long)]
    pub normalize: bool,
}
```

### F-005: Version Command

The system SHALL provide version and build information.

**Acceptance Criteria:**
- Shows version, commit hash, build date, Rust version
- Machine-parseable output

**Usage:**
```bash
thoughtjack version
# thoughtjack 0.5.0 (abc1234 2025-02-25) rustc 1.85.0
```

---

## 3. Transport Inference

Transport is inferred from which flags are present. No explicit transport flag is needed.

| Actor Mode | Flag Present | Transport |
|------------|-------------|-----------|
| `mcp_server` | `--mcp-server` | HTTP on specified address |
| `mcp_server` | (none) | stdio (process stdin/stdout) |
| `mcp_client` | `--mcp-client-command` | stdio (spawn process) |
| `mcp_client` | `--mcp-client-endpoint` | HTTP |
| `ag_ui_client` | `--agui-client-endpoint` | HTTP (always) |
| `a2a_server` | `--a2a-server` or (none) | HTTP (default: `127.0.0.1:9090`) |
| `a2a_client` | `--a2a-client-endpoint` | HTTP (always) |

If a required flag is missing for an actor defined in the document, ThoughtJack exits with an error:
```
error: Actor 'probe' (mcp_client) requires --mcp-client-command or --mcp-client-endpoint
```

---

## 4. Authentication

Credentials are configured via environment variables, avoiding exposure in process lists.

| Environment Variable | Applies To | Description |
|---------------------|------------|-------------|
| `THOUGHTJACK_MCP_CLIENT_AUTHORIZATION` | `mcp_client` actors | `Authorization` header value |
| `THOUGHTJACK_A2A_CLIENT_AUTHORIZATION` | `a2a_client` actors | `Authorization` header value |
| `THOUGHTJACK_AGUI_AUTHORIZATION` | `ag_ui_client` actors | `Authorization` header value |
| `THOUGHTJACK_{MODE}_HEADER_{NAME}` | Specified mode | Arbitrary header (underscores → hyphens) |

Mode-specific env vars take precedence over `--header` for the same header name. The `--header` flag provides cross-cutting headers for all HTTP client transports.

**Examples:**
```bash
# Per-mode auth
export THOUGHTJACK_MCP_CLIENT_AUTHORIZATION="Bearer sk-agent-key"
export THOUGHTJACK_A2A_CLIENT_AUTHORIZATION="Bearer sk-other-key"
thoughtjack run --config cross-protocol.yaml

# Arbitrary headers
export THOUGHTJACK_AGUI_HEADER_X_API_KEY="key-123"
thoughtjack run --config agui-attack.yaml --agui-client-endpoint http://localhost:8000/agent
```

---

## 5. Exit Codes

Exit codes compose naturally with CI systems. Verdict exit codes take priority in the order listed.

| Code | Meaning | CI Interpretation |
|------|---------|-------------------|
| 0 | `not_exploited` | Pass — agent resisted the attack |
| 1 | `exploited` | Fail — agent has vulnerabilities |
| 2 | `error` | Unstable — evaluation incomplete (includes all-indicators-skipped) |
| 3 | `partial` | Warning — partial compliance detected |
| 10 | Runtime error | Infrastructure failure (config invalid, transport failure, etc.) |
| 64 | Usage error | Invalid arguments or flags |
| 130 | Interrupted | SIGINT (Ctrl+C) |
| 143 | Terminated | SIGTERM |

**CI safety guarantee:** A document whose indicators are all `semantic` produces exit code 2 (not 0) when no LLM key is configured. CI pipelines gating on exit code 0 correctly block rather than silently passing unevaluated security tests.

---

## 6. Output Model

- Human summary always printed to stderr during and after execution.
- `--output <path>`: writes structured JSON verdict to the specified file.
- `--output -`: writes JSON verdict to stdout.
- No `--output` flag: no structured output, human summary only.

The JSON verdict structure is defined in TJ-SPEC-014 §5.2.

---

## 7. Edge Cases

### EC-CLI-001: Missing Config Flag

**Trigger:** `thoughtjack run` without `--config` and `THOUGHTJACK_CONFIG` not set.

**Expected:** Exit code 64, error: `"error: --config <PATH> is required"`.

### EC-CLI-002: Missing Transport Flag for Actor Mode

**Trigger:** Document contains `mcp_client` actor, neither `--mcp-client-command` nor `--mcp-client-endpoint` provided.

**Expected:** Exit code 64, error listing the required flags for each unresolved actor.

### EC-CLI-003: Invalid OATF Document

**Trigger:** Document fails SDK validation.

**Expected:** Exit code 10, validation errors printed to stderr.

### EC-CLI-004: Conflicting Transport Flags

**Trigger:** Both `--mcp-client-command` and `--mcp-client-endpoint` specified.

**Expected:** Clap rejects (conflicts_with). Exit code 64.

### EC-CLI-005: Output to Stdout With Human Summary

**Trigger:** `--output -` specified.

**Expected:** JSON verdict to stdout, human summary to stderr. No interleaving.

### EC-CLI-006: SIGINT During Execution

**Trigger:** Ctrl+C during `thoughtjack run`.

**Expected:** Graceful shutdown via CancellationToken. Print partial verdict if available. Exit code 130.

### EC-CLI-007: SIGTERM During Execution

**Trigger:** SIGTERM sent to process.

**Expected:** Same as SIGINT but exit code 143.

### EC-CLI-008: Max Session Timeout

**Trigger:** Execution exceeds `--max-session` duration.

**Expected:** Cancel all actors, produce verdict from available trace. Exit code based on verdict (not error).

### EC-CLI-009: Grace Period Override

**Trigger:** `--grace-period 30s` with document specifying `grace_period: 10s`.

**Expected:** CLI override takes precedence. Grace period runs for 30 seconds.

### EC-CLI-010: Raw Synthesize Warning

**Trigger:** `--raw-synthesize` flag set.

**Expected:** Warning logged at startup: `"Synthesize output validation disabled (--raw-synthesize)"`. Execution proceeds.

---

## 8. Non-Functional Requirements

### NFR-001: Startup Time

The binary SHALL start (parse args, load document, begin execution) in under 200ms for single-actor documents.

### NFR-002: Binary Size

The stripped release binary SHALL be under 30 MB.

### NFR-003: Platform Support

The CLI SHALL work on Linux (x86_64, aarch64), macOS (x86_64, aarch64), and Windows (x86_64).

### NFR-004: Signal Handling

The CLI SHALL handle SIGINT and SIGTERM for graceful shutdown on Unix. On Windows, Ctrl+C handling via `ctrlc` crate.

---

## 9. Implementation

### 9.1 Module Structure

```
src/cli/
├── mod.rs           # Re-exports
├── args.rs          # Cli, Commands, RunArgs, etc.
└── commands/
    ├── run.rs       # Run command handler
    ├── scenarios.rs # Scenarios subcommands
    ├── validate.rs  # Validate command
    └── version.rs   # Version command
```

### 9.2 Error Formatting

```rust
impl ThoughtJackError {
    pub fn format(&self, color: bool) -> String {
        let prefix = if color {
            "error:".red().bold().to_string()
        } else {
            "error:".to_string()
        };

        match self {
            ThoughtJackError::Config(e) => format!("{prefix} Configuration error\n\n{e}"),
            ThoughtJackError::Validation(errors) => format!("{prefix} Document validation failed\n\n{errors}"),
            ThoughtJackError::TransportMissing { actor, mode, options } =>
                format!("{prefix} Actor '{actor}' ({mode}) requires {options}"),
            ThoughtJackError::Io(e) => format!("{prefix} {e}"),
            ThoughtJackError::Usage(msg) => format!("{prefix} {msg}\n\nFor more information, try '--help'"),
        }
    }
}
```

### 9.3 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Positional args for options | Hard to remember order | Use named flags |
| Exit code 1 for everything | Can't distinguish errors | Use specific exit codes (§5) |
| Panic on invalid input | Bad UX | Return Result, format error |
| Hardcoded colors | Breaks pipes and CI | Check TTY, respect `NO_COLOR` |
| Blocking on stdin check | Hangs if no input | Use non-blocking check or timeout |
| Global mutable state | Testing difficulties | Pass config through functions |

### 9.4 Testing Strategy

**Unit Tests:**
- Argument parsing for all commands
- Transport inference logic
- Exit code mapping
- Error formatting
- Environment variable handling

**Integration Tests:**
- Full command execution with mock transport
- Signal handling
- Output format validation (JSON verdict structure)
- `--output -` vs `--output file` behavior

**Snapshot Tests:**
- Help text for all commands

---

## 10. Definition of Done

- [ ] Root command with global options (verbose, quiet, color)
- [ ] `run` command with all transport flags
- [ ] Transport inference from flags
- [ ] `scenarios list`, `show`, `run` commands
- [ ] `validate` command with `--normalize`
- [ ] `version` command with build info
- [ ] Exit codes match §5
- [ ] Environment variable authentication (§4)
- [ ] Human summary to stderr
- [ ] JSON verdict to `--output` file or stdout
- [ ] Signal handling (SIGINT, SIGTERM)
- [ ] All 10 edge cases (EC-CLI-001 through EC-CLI-010) have tests
- [ ] Startup time < 200ms (NFR-001)
- [ ] Binary size < 30 MB stripped (NFR-002)
- [ ] Works on Linux, macOS, Windows (NFR-003)
- [ ] Help text is clear and complete
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 11. References

- [TJ-SPEC-013: OATF Integration](./TJ-SPEC-013_OATF_Integration.md) — §12 is the canonical CLI definition
- [TJ-SPEC-014: Verdict Evaluation Output](./TJ-SPEC-014_Verdict_Evaluation_Output.md) — Exit codes, JSON verdict structure
- [TJ-SPEC-002: Transport Abstraction](./TJ-SPEC-002_Transport_Abstraction.md) — Transport implementations
- [TJ-SPEC-008: Observability](./TJ-SPEC-008_Observability.md) — Logging configuration
- [Clap Derive Tutorial](https://docs.rs/clap/latest/clap/_derive/)
- [CLI Guidelines](https://clig.dev/)

---

## Appendix A: Environment Variable Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTJACK_CONFIG` | — | OATF document path |
| `THOUGHTJACK_COLOR` | `auto` | Color output mode |
| `THOUGHTJACK_LOG_LEVEL` | `warn` | Logging level (overrides -v) |
| `THOUGHTJACK_MCP_CLIENT_AUTHORIZATION` | — | MCP client Authorization header |
| `THOUGHTJACK_A2A_CLIENT_AUTHORIZATION` | — | A2A client Authorization header |
| `THOUGHTJACK_AGUI_AUTHORIZATION` | — | AG-UI Authorization header |
| `THOUGHTJACK_{MODE}_HEADER_{NAME}` | — | Arbitrary per-mode HTTP header |
| `NO_COLOR` | — | Disable color output (standard) |

## Appendix B: Dropped v0.2 CLI Features

| Removed | Reason |
|---------|--------|
| `thoughtjack server run` | Replaced by `thoughtjack run` |
| `thoughtjack server validate` | Replaced by `thoughtjack validate` |
| `thoughtjack server list` | Replaced by `thoughtjack scenarios list` |
| `thoughtjack diagram` | Removed. Diagram generation moved to OATF CLI toolchain. |
| `thoughtjack docs` | Build tooling, not user workflow |
| `thoughtjack completions` | Deferred |
| `--scenario <name>` on run | Moved to `thoughtjack scenarios run <name>` |
| `--tool <path>` | Not applicable in OATF model |
| `--behavior`, `--spoof-client`, `--profile` | Not applicable in OATF model |
| `--state-scope`, `--unknown-methods` | Hardcoded for v1 |
| `--capture-dir`, `--capture-redact` | Replaced by `--output` |
| `--allow-external-handlers` | Not applicable in OATF model |
| `--seed` | Per-generator in YAML |
| `--report`, `--export-trace` | Replaced by `--output` / deferred |
