# TJ-SPEC-007: CLI Interface

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-007` |
| **Title** | CLI Interface |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **High** |
| **Version** | v1.0.0 |
| **Tags** | `#cli` `#commands` `#flags` `#subcommands` `#output` `#exit-codes` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's command-line interface — the primary way users interact with the tool to run adversarial MCP servers, validate configurations, and manage the attack pattern library.

### 1.1 Motivation

ThoughtJack needs a CLI that:

| Requirement | Rationale |
|-------------|-----------|
| **Subcommand structure** | Future extensibility (`thoughtjack server`, `thoughtjack agent`) |
| **Consistent with ecosystem** | Familiar to users of Rust CLI tools (cargo, ripgrep) |
| **CI/CD friendly** | Machine-readable output, deterministic exit codes |
| **Developer ergonomic** | Good defaults, helpful error messages, shell completion |

### 1.2 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Explicit subcommands** | `thoughtjack server` not `thoughtjack --server` |
| **Flags over positional args** | Clearer, order-independent |
| **Environment variable fallback** | CI/CD integration without command-line changes |
| **Structured output option** | JSON for tooling, human-readable by default |
| **Fail fast with clear errors** | Don't start server with invalid config |

### 1.3 Command Hierarchy

```
thoughtjack
├── server              # Run adversarial MCP server
│   ├── run             # Run server (default subcommand)
│   ├── validate        # Validate configuration
│   └── list            # List available patterns
│
├── scenarios           # List and inspect built-in attack scenarios
│   ├── list            # List available scenarios
│   └── show            # Display scenario YAML configuration
│
├── diagram             # Generate Mermaid diagrams from config files
│
├── docs                # Documentation generation
│   ├── generate        # Generate documentation pages from scenarios
│   └── validate        # Validate scenario metadata and registry
│
├── agent               # (Future) Run adversarial A2A agent
│   └── ...
│
├── completions         # Generate shell completions
│
└── version             # Show version information
```

### 1.4 Scope Boundaries

**In scope:**
- Command and subcommand structure
- Flag definitions and validation
- Environment variable mapping
- Output formats (human, JSON)
- Exit codes
- Shell completions
- Help text

**Out of scope:**
- Server runtime behavior (TJ-SPEC-002, TJ-SPEC-003)
- Configuration loading details (TJ-SPEC-006)
- Logging implementation (TJ-SPEC-008)

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
  server       Run an adversarial MCP server
  scenarios    List and inspect built-in attack scenarios
  diagram      Generate a Mermaid diagram from a scenario file
  docs         Generate documentation site from scenarios
  agent        Run an adversarial A2A agent (coming soon)
  completions  Generate shell completions
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
#[command(author, version, about)]
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
    /// Start or manage the adversarial MCP server
    Server(Box<ServerCommand>),

    /// List and inspect built-in attack scenarios
    Scenarios(ScenariosCommand),

    /// Generate a Mermaid diagram from a scenario file
    Diagram(DiagramArgs),

    /// Generate documentation site from scenarios
    Docs(DocsCommand),

    /// Run as an MCP client agent (coming soon)
    #[command(hide = true)]
    Agent(AgentCommand),

    /// Generate shell completion scripts
    Completions(CompletionsArgs),

    /// Display version and build information
    Version(VersionArgs),
}
```

### F-002: Server Run Command

The system SHALL provide `thoughtjack server run` to start an adversarial server.

**Acceptance Criteria:**
- Loads configuration from file
- Supports stdio transport (default) and HTTP transport
- Supports single-tool mode for quick testing
- Supports client name spoofing
- Supports behavior override
- Supports request/response capture for debugging
- External handlers require explicit `--allow-external-handlers` flag

**Usage:**
```bash
thoughtjack server run [OPTIONS]

Options:
  -c, --config <PATH>        Server configuration file [env: THOUGHTJACK_CONFIG]
  -t, --tool <PATH>          Single tool pattern (creates minimal server)
  -s, --scenario <NAME>      Built-in scenario name (see `scenarios list`)
      --http <ADDR>          Enable HTTP transport [default: stdio]
      --spoof-client <NAME>  Override client name in initialize [env: THOUGHTJACK_SPOOF_CLIENT]
      --behavior <MODE>      Override delivery behavior [env: THOUGHTJACK_BEHAVIOR]
      --state-scope <SCOPE>  Phase state scope for HTTP [default: per-connection]
                             [env: THOUGHTJACK_STATE_SCOPE] [values: per-connection, global]
      --profile <PROFILE>    Server profile [default: default]
                             [possible values: default, aggressive, stealth]
      --library <PATH>       Library root directory [default: ./library] [env: THOUGHTJACK_LIBRARY]
      --metrics-port <PORT>  Start Prometheus metrics exporter on port [env: THOUGHTJACK_METRICS_PORT]
      --capture-dir <PATH>   Capture request/response pairs [env: THOUGHTJACK_CAPTURE_DIR]
      --capture-redact       Redact sensitive fields in captures
      --allow-external-handlers  Enable HTTP/command handlers (SECURITY RISK)
                             [env: THOUGHTJACK_ALLOW_EXTERNAL_HANDLERS]
  -h, --help                 Print help
```

**Behavior Override Precedence:**

When `--behavior` is specified, it acts as a **global override** that supersedes all configuration-defined behaviors:

```
CLI --behavior (if set)
    ↓ (fallback if not set)
Tool-specific behavior (for tools/call matching tool name)
    ↓ (fallback)
Resource-specific behavior (for resources/read matching URI)
    ↓ (fallback)
Prompt-specific behavior (for prompts/get matching name)
    ↓ (fallback)
Phase behavior (if set)
    ↓ (fallback)
Server/baseline behavior (if set)
    ↓ (fallback)
Default (normal delivery, no side effects)
```

This allows quick ad-hoc testing without editing YAML:

```bash
# Force slow_loris on ALL responses regardless of config
thoughtjack server run --config attack.yaml --behavior slow_loris

# Force response_delay on all traffic
thoughtjack server run --config attack.yaml --behavior response_delay
```

**Note:** `--behavior` only overrides `delivery` mode. It does not affect `side_effects` defined in configuration.

**Examples:
```bash
  # Run server with configuration file
  thoughtjack server run --config attacks/rug_pull.yaml

  # Run single tool for quick testing
  thoughtjack server run --tool library/tools/calculator/injection.yaml

  # Run built-in scenario
  thoughtjack server run --scenario rug-pull

  # Run with HTTP transport
  thoughtjack server run --config attack.yaml --http :8080

  # Spoof client name (for scanner evasion testing)
  thoughtjack server run --config attack.yaml --spoof-client "mcp-scan"

  # Start Prometheus metrics exporter on port 9090
  thoughtjack server run --config attack.yaml --metrics-port 9090

  # Capture all requests/responses for debugging
  thoughtjack server run --config attack.yaml --capture-dir ./captures

  # Capture with sensitive field redaction
  thoughtjack server run --config attack.yaml --capture-dir ./captures --capture-redact

  # Enable external handlers (required for HTTP/command handlers in config)
  thoughtjack server run --config llm-injection.yaml --allow-external-handlers
```

**Implementation:**
```rust
#[derive(Args)]
pub struct ServerRunArgs {
    /// Server configuration file
    #[arg(short, long, env = "THOUGHTJACK_CONFIG", group = "source")]
    pub config: Option<PathBuf>,
    
    /// Single tool pattern (creates minimal server)
    #[arg(short, long, group = "source")]
    pub tool: Option<PathBuf>,

    /// Built-in scenario name (see `scenarios list` for options)
    #[arg(short = 's', long, group = "source")]
    pub scenario: Option<String>,

    /// Enable HTTP transport on specified address
    #[arg(long, value_name = "ADDR")]
    pub http: Option<String>,
    
    /// Override client name in initialize response
    #[arg(long, env = "THOUGHTJACK_SPOOF_CLIENT")]
    pub spoof_client: Option<String>,
    
    /// Override delivery behavior
    #[arg(long, env = "THOUGHTJACK_BEHAVIOR")]
    pub behavior: Option<DeliveryMode>,
    
    /// Phase state scope for HTTP transport
    #[arg(long, default_value = "per-connection", env = "THOUGHTJACK_STATE_SCOPE")]
    pub state_scope: StateScope,
    
    /// Server profile
    #[arg(long, default_value = "default")]
    pub profile: ServerProfile,
    
    /// Library root directory
    #[arg(long, default_value = "./library", env = "THOUGHTJACK_LIBRARY")]
    pub library: PathBuf,

    /// Start Prometheus metrics exporter on specified port
    #[arg(long, env = "THOUGHTJACK_METRICS_PORT")]
    pub metrics_port: Option<u16>,

    /// Directory to write request/response capture files
    #[arg(long, env = "THOUGHTJACK_CAPTURE_DIR")]
    pub capture_dir: Option<PathBuf>,
    
    /// Redact sensitive fields in capture files
    #[arg(long, requires = "capture_dir")]
    pub capture_redact: bool,
    
    /// Enable external handlers (HTTP/command) - SECURITY RISK
    /// Required when config contains `handler: { type: http }` or `handler: { type: command }`
    #[arg(long, env = "THOUGHTJACK_ALLOW_EXTERNAL_HANDLERS")]
    pub allow_external_handlers: bool,
}

#[derive(Clone, ValueEnum, Default)]
pub enum StateScope {
    /// Each HTTP connection gets independent phase state (default)
    #[default]
    PerConnection,
    /// All connections share phase state
    Global,
}

#[derive(Clone, ValueEnum)]
pub enum DeliveryMode {
    Normal,
    SlowLoris,
    UnboundedLine,
    NestedJson,
    ResponseDelay,
}

#[derive(Clone, ValueEnum, Default)]
pub enum ServerProfile {
    #[default]
    Default,
    Aggressive,
    Stealth,
}
```

### F-003: Server Validate Command

The system SHALL provide `thoughtjack server validate` to check configurations.

**Acceptance Criteria:**
- Validates configuration without starting server
- Reports all errors and warnings
- Supports JSON output for tooling
- Exit code 0 for valid, non-zero for invalid
- Can validate multiple files at once

**Usage:**
```bash
thoughtjack server validate [OPTIONS] <FILES>...

Arguments:
  <FILES>...  Configuration files to validate

Options:
      --format <FORMAT>  Output format [default: human] [possible values: human, json]
      --strict           Treat warnings as errors
      --library <PATH>   Library root directory [default: ./library]
  -h, --help             Print help

Examples:
  # Validate single file
  thoughtjack server validate attacks/rug_pull.yaml

  # Validate multiple files
  thoughtjack server validate attacks/*.yaml

  # JSON output for CI
  thoughtjack server validate --format json attacks/rug_pull.yaml

  # Strict mode (warnings are errors)
  thoughtjack server validate --strict attacks/rug_pull.yaml
```

**Output (human):**
```
Validating attacks/rug_pull.yaml...

✓ attacks/rug_pull.yaml is valid

  Warnings:
    - phases[0].unknown_field: Unknown field ignored (did you mean 'on_enter'?)

Summary: 1 file validated, 0 errors, 1 warning
```

**Output (JSON):**
```json
{
  "files": [
    {
      "path": "attacks/rug_pull.yaml",
      "valid": true,
      "errors": [],
      "warnings": [
        {
          "path": "phases[0].unknown_field",
          "message": "Unknown field ignored",
          "suggestion": "did you mean 'on_enter'?"
        }
      ]
    }
  ],
  "summary": {
    "total": 1,
    "valid": 1,
    "invalid": 0,
    "errors": 0,
    "warnings": 1
  }
}
```

**Implementation:**
```rust
#[derive(Args)]
pub struct ServerValidateArgs {
    /// Configuration files to validate
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    
    /// Output format
    #[arg(long, default_value = "human")]
    pub format: OutputFormat,
    
    /// Treat warnings as errors
    #[arg(long)]
    pub strict: bool,
    
    /// Library root directory
    #[arg(long, default_value = "./library", env = "THOUGHTJACK_LIBRARY")]
    pub library: PathBuf,
}

#[derive(Clone, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}
```

### F-004: Server List Command

The system SHALL provide `thoughtjack server list` to explore the pattern library.

**Acceptance Criteria:**
- Lists available servers, tools, resources, prompts, behaviors
- Supports filtering by category and tag
- Supports JSON output
- Shows brief descriptions

**Usage:**
```bash
thoughtjack server list [OPTIONS] [CATEGORY]

Arguments:
  [CATEGORY]  Category to list [possible values: servers, tools, resources, prompts, behaviors, all]

Options:
      --tag <TAG>        Filter by tag
      --format <FORMAT>  Output format [default: human] [possible values: human, json]
      --library <PATH>   Library root directory [default: ./library]
  -h, --help             Print help

Examples:
  # List all servers
  thoughtjack server list servers

  # List tools tagged with 'injection'
  thoughtjack server list tools --tag injection

  # List everything
  thoughtjack server list all

  # JSON output
  thoughtjack server list tools --format json
```

**Output (human):**
```
Tools (15 available)

  calculator/
    benign.yaml          Simple calculator (4 operations)
    injection.yaml       Calculator with prompt injection payload
    typosquat.yaml       Calculator with typosquatted name

  file_reader/
    benign.yaml          Read file contents
    path_traversal.yaml  Read with ../ in suggested paths

  web_search/
    benign.yaml          Web search tool
    exfiltration.yaml    Web search with data exfiltration

Use 'thoughtjack server list tools --tag <tag>' to filter
```

**Implementation:**
```rust
#[derive(Args)]
pub struct ServerListArgs {
    /// Category to list
    #[arg(default_value = "all")]
    pub category: ListCategory,
    
    /// Filter by tag
    #[arg(long)]
    pub tag: Option<String>,
    
    /// Output format
    #[arg(long, default_value = "human")]
    pub format: OutputFormat,
    
    /// Library root directory
    #[arg(long, default_value = "./library", env = "THOUGHTJACK_LIBRARY")]
    pub library: PathBuf,
}

#[derive(Clone, ValueEnum)]
pub enum ListCategory {
    Servers,
    Tools,
    Resources,
    Prompts,
    Behaviors,
    All,
}
```

### F-004a: Scenarios Command

The system SHALL provide `thoughtjack scenarios` to list and inspect built-in attack scenarios.

**Acceptance Criteria:**
- Lists all built-in scenarios with descriptions
- Shows full YAML configuration for a specific scenario
- Supports filtering by category and tag
- Supports JSON output

**Usage:**
```bash
thoughtjack scenarios list [OPTIONS]

Options:
      --category <CATEGORY>  Filter by category [possible values: injection, temporal, resource, protocol]
      --tag <TAG>            Filter by tag
      --format <FORMAT>      Output format [default: human] [possible values: human, json]
  -h, --help                 Print help

Examples:
  # List all scenarios
  thoughtjack scenarios list

  # List scenarios in the injection category
  thoughtjack scenarios list --category injection

  # Show full YAML for a specific scenario
  thoughtjack scenarios show rug-pull
```

**Output (human):**
```
Built-in Attack Scenarios

Temporal Attacks (3 scenarios)
  rug-pull             Classic rug-pull: helpful → malicious after trust building
  sleeper-agent        Dormant malicious behavior until specific trigger
  time-bomb            Malicious activation after time delay

Injection Attacks (2 scenarios)
  prompt-injection     Tool responses contain prompt injection payloads
  resource-injection   Resource URIs include path traversal attempts

Use 'scenarios show <name>' to view full configuration
```

**Implementation:**
```rust
#[derive(Args, Debug)]
pub struct ScenariosCommand {
    #[command(subcommand)]
    pub subcommand: ScenariosSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum ScenariosSubcommand {
    /// List available built-in scenarios
    List(ScenariosListArgs),

    /// Display the YAML configuration for a built-in scenario
    Show(ScenariosShowArgs),
}

#[derive(Args, Debug)]
pub struct ScenariosListArgs {
    /// Filter by category
    #[arg(long)]
    pub category: Option<ScenarioCategory>,

    /// Filter by tag
    #[arg(long)]
    pub tag: Option<String>,

    /// Output format
    #[arg(long, default_value = "human")]
    pub format: OutputFormat,
}

#[derive(Args, Debug)]
pub struct ScenariosShowArgs {
    /// Scenario name to display
    pub name: String,
}
```

See TJ-SPEC-010 for complete scenario specification.

### F-004b: Diagram Command

The system SHALL provide `thoughtjack diagram` to generate Mermaid diagrams from scenario configuration files.

**Acceptance Criteria:**
- Accepts a scenario YAML file as positional argument
- Supports diagram type selection (auto, state, sequence, flowchart)
- Defaults to auto-detecting the diagram type from scenario structure
- Supports writing output to a file via `--output`
- Outputs to stdout by default

**Usage:**
```bash
thoughtjack diagram [OPTIONS] <SCENARIO>

Arguments:
  <SCENARIO>  Path to scenario YAML file

Options:
      --diagram-type <TYPE>  Diagram type override [default: auto]
                             [possible values: auto, state, sequence, flowchart]
  -o, --output <PATH>       Write output to file instead of stdout
  -h, --help                Print help

Examples:
  # Generate diagram to stdout
  thoughtjack diagram attacks/rug_pull.yaml

  # Generate a state diagram and write to file
  thoughtjack diagram attacks/rug_pull.yaml --diagram-type state -o diagram.mmd

  # Generate a sequence diagram
  thoughtjack diagram attacks/injection.yaml --diagram-type sequence
```

**Implementation:**
```rust
#[derive(Args, Debug)]
pub struct DiagramArgs {
    /// Path to scenario YAML file
    pub scenario: PathBuf,

    /// Diagram type override (default: auto-detect)
    #[arg(long, default_value = "auto")]
    pub diagram_type: DiagramTypeChoice,

    /// Write output to file instead of stdout
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Clone, ValueEnum, Default)]
pub enum DiagramTypeChoice {
    #[default]
    Auto,
    State,
    Sequence,
    Flowchart,
}
```

See TJ-SPEC-011 for complete documentation site specification.

### F-004c: Docs Command

The system SHALL provide `thoughtjack docs` to generate and validate documentation from scenarios.

**Acceptance Criteria:**
- `thoughtjack docs generate` generates documentation pages from scenarios
- `thoughtjack docs validate` validates scenario metadata and registry
- Supports configurable scenarios directory, output directory, and registry file
- Supports strict mode (warnings become errors)

**Usage:**
```bash
thoughtjack docs <SUBCOMMAND>

Subcommands:
  generate   Generate documentation pages from scenarios
  validate   Validate scenario metadata and registry

thoughtjack docs generate [OPTIONS]

Options:
      --scenarios <PATH>   Scenarios directory [default: ./scenarios] [env: TJ_SCENARIOS_DIR]
      --output <PATH>      Output directory [default: ./docs-site] [env: TJ_DOCS_OUTPUT_DIR]
      --registry <PATH>    Registry file path [default: ./scenarios/registry.yaml] [env: TJ_REGISTRY_PATH]
      --strict             Promote warnings to errors [env: TJ_DOCS_STRICT]
  -h, --help               Print help

thoughtjack docs validate [OPTIONS]

Options:
      --scenarios <PATH>   Scenarios directory [default: ./scenarios] [env: TJ_SCENARIOS_DIR]
      --registry <PATH>    Registry file path [default: ./scenarios/registry.yaml] [env: TJ_REGISTRY_PATH]
      --strict             Promote warnings to errors [env: TJ_DOCS_STRICT]
  -h, --help               Print help

Examples:
  # Generate documentation site
  thoughtjack docs generate

  # Generate with custom paths
  thoughtjack docs generate --scenarios ./my-scenarios --output ./site

  # Validate scenario metadata
  thoughtjack docs validate

  # Strict validation (warnings become errors)
  thoughtjack docs validate --strict
```

**Implementation:**
```rust
#[derive(Args, Debug)]
pub struct DocsCommand {
    #[command(subcommand)]
    pub subcommand: DocsSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum DocsSubcommand {
    /// Generate documentation pages from scenarios
    Generate(DocsGenerateArgs),

    /// Validate scenario metadata and registry
    Validate(DocsValidateArgs),
}

#[derive(Args, Debug)]
pub struct DocsGenerateArgs {
    /// Scenarios directory
    #[arg(long, default_value = "./scenarios", env = "TJ_SCENARIOS_DIR")]
    pub scenarios: PathBuf,

    /// Output directory
    #[arg(long, default_value = "./docs-site", env = "TJ_DOCS_OUTPUT_DIR")]
    pub output: PathBuf,

    /// Registry file path
    #[arg(long, default_value = "./scenarios/registry.yaml", env = "TJ_REGISTRY_PATH")]
    pub registry: PathBuf,

    /// Promote warnings to errors
    #[arg(long, env = "TJ_DOCS_STRICT")]
    pub strict: bool,
}

#[derive(Args, Debug)]
pub struct DocsValidateArgs {
    /// Scenarios directory
    #[arg(long, default_value = "./scenarios", env = "TJ_SCENARIOS_DIR")]
    pub scenarios: PathBuf,

    /// Registry file path
    #[arg(long, default_value = "./scenarios/registry.yaml", env = "TJ_REGISTRY_PATH")]
    pub registry: PathBuf,

    /// Promote warnings to errors
    #[arg(long, env = "TJ_DOCS_STRICT")]
    pub strict: bool,
}
```

See TJ-SPEC-011 for complete documentation site specification.

### F-005: Server Default Subcommand

The system SHALL treat `run` as the default server subcommand.

**Acceptance Criteria:**
- `thoughtjack server --config x.yaml` is equivalent to `thoughtjack server run --config x.yaml`
- Explicit `run` still works
- Help shows both forms

**Implementation:**
```rust
#[derive(Subcommand)]
pub enum ServerSubcommand {
    /// Run an adversarial MCP server (default)
    #[command(alias = "")]
    Run(ServerRunArgs),
    
    /// Validate configuration files
    Validate(ServerValidateArgs),
    
    /// List available patterns
    List(ServerListArgs),
}

impl Default for ServerSubcommand {
    fn default() -> Self {
        ServerSubcommand::Run(ServerRunArgs::default())
    }
}
```

### F-006: Agent Command (Future)

The system SHALL reserve the `agent` subcommand for future A2A agent simulation.

**Acceptance Criteria:**
- Command exists but shows "coming soon" message
- Help describes intended functionality
- Non-zero exit code

**Usage:**
```bash
thoughtjack agent [OPTIONS]

Run an adversarial A2A (Agent-to-Agent) agent for testing agent orchestration
systems.

Status: Coming soon in a future release.

Track progress: https://github.com/thoughtgate/thoughtjack/issues/XXX
```

**Implementation:**
```rust
#[derive(Args)]
pub struct AgentCommand {
    /// Placeholder for future agent configuration
    #[arg(short, long)]
    pub config: Option<PathBuf>,
}

impl AgentCommand {
    pub fn run(&self) -> Result<(), ThoughtJackError> {
        eprintln!("{}",
            "The 'agent' command is coming soon in a future release.\n\n\
             This will enable simulation of malicious A2A agents for testing\n\
             agent orchestration security.\n\n\
             Track progress: https://github.com/thoughtgate/thoughtjack/issues/XXX"
                .yellow()
        );
        Err(ThoughtJackError::NotImplemented)
    }
}
```

### F-007: Completions Command

The system SHALL generate shell completions for common shells.

**Acceptance Criteria:**
- Supports bash, zsh, fish, powershell, elvish
- Outputs to stdout for easy piping
- Includes instructions in help

**Usage:**
```bash
thoughtjack completions <SHELL>

Arguments:
  <SHELL>  Shell to generate completions for [possible values: bash, zsh, fish, powershell, elvish]

Examples:
  # Bash (add to ~/.bashrc)
  thoughtjack completions bash >> ~/.bashrc

  # Zsh (add to ~/.zshrc)
  thoughtjack completions zsh > ~/.zfunc/_thoughtjack

  # Fish
  thoughtjack completions fish > ~/.config/fish/completions/thoughtjack.fish
```

**Implementation:**
```rust
#[derive(Args)]
pub struct CompletionsCommand {
    /// Shell to generate completions for
    #[arg(value_enum)]
    pub shell: Shell,
}

#[derive(Clone, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

impl CompletionsCommand {
    pub fn run(&self, cmd: &mut clap::Command) -> Result<(), ThoughtJackError> {
        let shell = match self.shell {
            Shell::Bash => clap_complete::Shell::Bash,
            Shell::Zsh => clap_complete::Shell::Zsh,
            Shell::Fish => clap_complete::Shell::Fish,
            Shell::PowerShell => clap_complete::Shell::PowerShell,
            Shell::Elvish => clap_complete::Shell::Elvish,
        };

        clap_complete::generate(shell, cmd, "thoughtjack", &mut std::io::stdout());
        Ok(())
    }
}
```

### F-008: Version Command

The system SHALL provide detailed version information.

**Acceptance Criteria:**
- Shows version, git commit, build date
- Shows feature flags
- Supports JSON output

**Usage:**
```bash
thoughtjack version [OPTIONS]

Options:
      --format <FORMAT>  Output format [default: human] [possible values: human, json]
  -h, --help             Print help
```

**Output (human):**
```
ThoughtJack 0.1.0

  Commit:      a1b2c3d4
  Build Date:  2025-02-04
  Target:      x86_64-unknown-linux-gnu
  Features:    http, generators
  Rust:        1.85.0
```

**Output (JSON):**
```json
{
  "name": "thoughtjack",
  "version": "0.1.0",
  "commit": "a1b2c3d4",
  "build_date": "2025-02-04",
  "target": "x86_64-unknown-linux-gnu",
  "features": ["http", "generators"],
  "rust_version": "1.85.0"
}
```

### F-009: Exit Codes

The system SHALL use consistent exit codes.

**Acceptance Criteria:**
- Exit codes are documented
- Exit codes are deterministic
- Exit codes distinguish error categories

**Exit Codes:**

| Code | Name | Description |
|------|------|-------------|
| 0 | `SUCCESS` | Command completed successfully |
| 1 | `GENERAL_ERROR` | Unspecified error |
| 2 | `CONFIG_ERROR` | Configuration validation failed |
| 3 | `IO_ERROR` | File or network I/O error |
| 4 | `TRANSPORT_ERROR` | Transport layer error |
| 5 | `PHASE_ERROR` | Phase engine failure (invalid transition, trigger error) |
| 10 | `GENERATOR_ERROR` | Generator error (limit exceeded, generation failed) |
| 11 | `HANDLER_ERROR` | Handler error (external handler failed) |
| 64 | `USAGE_ERROR` | Invalid command-line usage |
| 130 | `INTERRUPTED` | Interrupted by SIGINT (Ctrl+C) |
| 143 | `TERMINATED` | Terminated by SIGTERM |

**Implementation:**
```rust
pub struct ExitCode;

impl ExitCode {
    pub const SUCCESS: i32 = 0;
    pub const ERROR: i32 = 1;
    pub const CONFIG_ERROR: i32 = 2;
    pub const IO_ERROR: i32 = 3;
    pub const TRANSPORT_ERROR: i32 = 4;
    pub const PHASE_ERROR: i32 = 5;
    pub const GENERATOR_ERROR: i32 = 10;
    pub const HANDLER_ERROR: i32 = 11;
    pub const USAGE_ERROR: i32 = 64;
    pub const INTERRUPTED: i32 = 130;
    pub const TERMINATED: i32 = 143;
}

impl ThoughtJackError {
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) | Self::Json(_) | Self::Yaml(_) => ExitCode::CONFIG_ERROR,
            Self::Transport(_) => ExitCode::TRANSPORT_ERROR,
            Self::Phase(_) => ExitCode::PHASE_ERROR,
            Self::Generator(_) => ExitCode::GENERATOR_ERROR,
            Self::Handler(_) => ExitCode::HANDLER_ERROR,
            Self::Behavior(_) => ExitCode::ERROR,
            Self::Io(_) => ExitCode::IO_ERROR,
            Self::Usage(_) => ExitCode::USAGE_ERROR,
        }
    }
}
```

### F-010: Environment Variable Mapping

The system SHALL support environment variable alternatives for all flags.

**Acceptance Criteria:**
- All persistent flags have env var equivalents
- Env var naming follows `THOUGHTJACK_*` convention
- Command-line flags override env vars
- Env vars documented in help

**Environment Variables:**

| Variable | Flag | Description |
|----------|------|-------------|
| `THOUGHTJACK_CONFIG` | `--config` | Server configuration file |
| `THOUGHTJACK_LIBRARY` | `--library` | Library root directory |
| `THOUGHTJACK_SPOOF_CLIENT` | `--spoof-client` | Client name override |
| `THOUGHTJACK_BEHAVIOR` | `--behavior` | Delivery behavior override |
| `THOUGHTJACK_STATE_SCOPE` | `--state-scope` | Phase state scope (per-connection, global) |
| `THOUGHTJACK_METRICS_PORT` | `--metrics-port` | Prometheus metrics exporter port |
| `THOUGHTJACK_CAPTURE_DIR` | `--capture-dir` | Request/response capture directory |
| `THOUGHTJACK_ALLOW_EXTERNAL_HANDLERS` | `--allow-external-handlers` | Enable external handlers |
| `THOUGHTJACK_LOG_LEVEL` | `-v` | Logging verbosity |
| `THOUGHTJACK_COLOR` | `--color` | Color output preference |
| `THOUGHTJACK_TRANSPORT` | `--http` | Transport type |
| `THOUGHTJACK_HTTP_BIND` | `--http` | HTTP bind address |
| `THOUGHTJACK_MAX_PAYLOAD_BYTES` | — | Generator limit |
| `THOUGHTJACK_MAX_NEST_DEPTH` | — | Generator limit |
| `THOUGHTJACK_MAX_BATCH_SIZE` | — | Generator limit |

### F-011: Logging Verbosity

The system SHALL support configurable logging verbosity.

**Acceptance Criteria:**
- `-q` for quiet (errors only)
- Default shows warnings and info
- `-v` for debug
- `-vv` for trace
- `-vvv` for all (including dependencies)

**Verbosity Levels:**

| Flags | Level | Description |
|-------|-------|-------------|
| `-q` | error | Errors only |
| (none) | warn | Warnings and errors |
| `-v` | info | Informational messages |
| `-vv` | debug | Debug messages |
| `-vvv` | trace | All trace messages |

**Implementation:**
```rust
fn configure_logging(verbose: u8, quiet: bool) {
    let level = if quiet {
        tracing::Level::ERROR
    } else {
        match verbose {
            0 => tracing::Level::WARN,
            1 => tracing::Level::INFO,
            2 => tracing::Level::DEBUG,
            _ => tracing::Level::TRACE,
        }
    };
    
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(verbose >= 3)
        .init();
}
```

### F-012: Color Output

The system SHALL support configurable color output.

**Acceptance Criteria:**
- `auto` detects terminal capability
- `always` forces color (for CI logs)
- `never` disables color
- Respects `NO_COLOR` environment variable

**Implementation:**
```rust
#[derive(Clone, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

fn configure_color(choice: ColorChoice) {
    let enabled = match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::env::var("NO_COLOR").is_err() && atty::is(atty::Stream::Stdout)
        }
    };
    
    colored::control::set_override(enabled);
}
```

### F-013: Request/Response Capture

The system SHALL support capturing all requests and responses for debugging.

**Acceptance Criteria:**
- `--capture-dir` enables capture to specified directory
- Creates one NDJSON file per session: `capture-{timestamp}-{pid}.ndjson`
- Each line contains request/response pair with timing
- `--capture-redact` removes sensitive fields
- Captures include phase state at time of request

**Capture Format:**
```json
{"ts":"2025-02-04T10:15:31.123Z","type":"request","id":1,"method":"initialize","params":{...},"phase":"baseline"}
{"ts":"2025-02-04T10:15:31.125Z","type":"response","id":1,"result":{...},"duration_ms":2,"phase":"baseline"}
{"ts":"2025-02-04T10:15:32.456Z","type":"request","id":2,"method":"tools/call","params":{"name":"calculator","arguments":{"a":1,"b":2}},"phase":"trust_building"}
{"ts":"2025-02-04T10:15:32.460Z","type":"response","id":2,"result":{...},"duration_ms":4,"phase":"trust_building"}
{"ts":"2025-02-04T10:15:33.789Z","type":"notification","method":"notifications/tools/list_changed","params":{},"phase":"exploit"}
```

**Redaction (`--capture-redact`):**

When enabled, sensitive fields are replaced with `"[REDACTED]"`:
- `params.arguments.*` (tool call arguments)
- `result.content[*].text` (response text)
- `result.content[*].data` (base64 data)
- `params.uri` (resource URIs)

**Implementation:**
```rust
pub struct CaptureWriter {
    file: BufWriter<File>,
    redact: bool,
}

impl CaptureWriter {
    pub fn new(dir: &Path, redact: bool) -> io::Result<Self> {
        let filename = format!(
            "capture-{}-{}.ndjson",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );
        let path = dir.join(filename);
        let file = BufWriter::new(File::create(path)?);
        Ok(Self { file, redact })
    }
    
    pub fn write_request(&mut self, req: &JsonRpcRequest, phase: &str) -> io::Result<()> {
        let mut value = serde_json::to_value(req)?;
        if self.redact {
            self.redact_request(&mut value);
        }
        
        let entry = json!({
            "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "type": "request",
            "id": req.id,
            "method": req.method,
            "params": value.get("params"),
            "phase": phase,
        });
        
        serde_json::to_writer(&mut self.file, &entry)?;
        self.file.write_all(b"\n")?;
        self.file.flush()
    }
    
    fn redact_request(&self, value: &mut serde_json::Value) {
        if let Some(params) = value.get_mut("params") {
            if let Some(args) = params.get_mut("arguments") {
                *args = json!("[REDACTED]");
            }
            if let Some(uri) = params.get_mut("uri") {
                *uri = json!("[REDACTED]");
            }
        }
    }
}
```

### F-014: Signal Handling

The system SHALL handle signals gracefully using cooperative cancellation.

**Acceptance Criteria:**
- SIGINT (Ctrl+C) triggers graceful shutdown
- SIGTERM triggers graceful shutdown
- Second SIGINT/SIGTERM forces immediate exit
- Exit code 130 for SIGINT, 143 for SIGTERM
- Uses `tokio_util::sync::CancellationToken` for cooperative shutdown
- Cancellation propagates through all async tasks

**Implementation:**
```rust
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Create a single cancellation token shared across the entire process
    let cancel = CancellationToken::new();
    let cancel_for_signal = cancel.clone();

    // Spawn signal handler for graceful shutdown
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");

        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }

        cancel_for_signal.cancel();
        eprintln!("\nShutting down gracefully... (press Ctrl+C again to force)");

        // Second signal forces exit
        tokio::select! {
            _ = tokio::signal::ctrl_c() => std::process::exit(ExitCode::INTERRUPTED),
            _ = sigterm.recv() => std::process::exit(ExitCode::TERMINATED),
        }
    });

    let result = commands::dispatch(cli, cancel).await;

    match result {
        Ok(()) => std::process::exit(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(e.exit_code());
        }
    }
}
```

The `CancellationToken` is passed to all commands and the server runtime, allowing cooperative shutdown:

```rust
async fn run_server(config: Config, cancel: CancellationToken) -> Result<()> {
    let server = Server::new(config);

    tokio::select! {
        result = server.run() => result,
        _ = cancel.cancelled() => {
            server.shutdown().await;
            Ok(())
        }
    }
}
```

---

## 3. Edge Cases

### EC-CLI-001: No Subcommand

**Scenario:** User runs `thoughtjack` with no arguments  
**Expected:** Show help text, exit code 0

### EC-CLI-002: Invalid Subcommand

**Scenario:** User runs `thoughtjack invalid`  
**Expected:** Error message with suggestions, exit code 64

### EC-CLI-003: Missing Required Argument

**Scenario:** User runs `thoughtjack server run` without `--config` or `--tool`  
**Expected:** Error: "Either --config or --tool is required", exit code 64

### EC-CLI-004: Both --config and --tool

**Scenario:** User runs `thoughtjack server run --config x.yaml --tool y.yaml`  
**Expected:** Error: "--config and --tool are mutually exclusive", exit code 64

### EC-CLI-005: Config File Not Found

**Scenario:** User runs `thoughtjack server run --config nonexistent.yaml`  
**Expected:** Error: "Configuration file not found: nonexistent.yaml", exit code 3

### EC-CLI-006: Invalid Config File

**Scenario:** Config file has validation errors  
**Expected:** Error messages with details, exit code 2

### EC-CLI-007: Invalid HTTP Address

**Scenario:** User runs `thoughtjack server run --config x.yaml --http invalid`  
**Expected:** Error: "Invalid HTTP address: invalid", exit code 64

### EC-CLI-008: Port Already in Use

**Scenario:** `--http :8080` but port 8080 is in use  
**Expected:** Error: "Port 8080 already in use", exit code 3

### EC-CLI-009: Invalid Behavior Mode

**Scenario:** `--behavior invalid_mode`  
**Expected:** Error with list of valid modes, exit code 64

### EC-CLI-010: Environment Variable Invalid

**Scenario:** `THOUGHTJACK_BEHAVIOR=invalid`  
**Expected:** Error referencing the env var, exit code 64

### EC-CLI-011: Stdin Not a TTY (stdio mode)

**Scenario:** stdin is not connected (piped from empty source)  
**Expected:** Read EOF immediately, exit cleanly, exit code 0

### EC-CLI-012: Ctrl+C During Startup

**Scenario:** User presses Ctrl+C while loading config  
**Expected:** Cancel loading, exit code 130

### EC-CLI-013: Ctrl+C During Server Run

**Scenario:** User presses Ctrl+C while server is running  
**Expected:** Graceful shutdown, exit code 130

### EC-CLI-014: Double Ctrl+C

**Scenario:** User presses Ctrl+C twice quickly  
**Expected:** Force immediate exit, exit code 130

### EC-CLI-015: Validate Glob Pattern

**Scenario:** `thoughtjack server validate attacks/*.yaml` with no matches  
**Expected:** Error: "No files matching pattern", exit code 64

### EC-CLI-016: Validate Mixed Results

**Scenario:** Validating 3 files, 1 invalid  
**Expected:** Report all results, exit code 2

### EC-CLI-017: Library Directory Not Found

**Scenario:** `--library /nonexistent`  
**Expected:** Error: "Library directory not found", exit code 3

### EC-CLI-018: JSON Output With Error

**Scenario:** `--format json` and command fails  
**Expected:** JSON error object, appropriate exit code

### EC-CLI-019: Very Long Command Line

**Scenario:** Command line exceeds OS limits  
**Expected:** OS error, not crash

### EC-CLI-020: Unicode in Paths

**Scenario:** Config path contains Unicode characters  
**Expected:** Handled correctly on all platforms

---

## 4. Non-Functional Requirements

### NFR-001: Startup Time

- CLI SHALL start in < 100ms (before config loading)
- Help text SHALL display in < 50ms

### NFR-002: Memory Usage

- CLI framework overhead SHALL be < 10 MB
- Clap parsing SHALL not clone large strings unnecessarily

### NFR-003: Binary Size

- Release binary SHALL be < 20 MB (stripped)
- Debug binary size is not constrained

### NFR-004: Cross-Platform

- CLI SHALL work identically on Linux, macOS, Windows
- Path handling SHALL be platform-appropriate
- Signal handling SHALL be platform-appropriate

---

## 5. Command Reference

### 5.1 Full Command Tree

```
thoughtjack
│
├── server                          Run adversarial MCP server
│   │
│   ├── run [default]               Start server
│   │   ├── -c, --config <PATH>     Configuration file
│   │   ├── -t, --tool <PATH>       Single tool pattern
│   │   ├── -s, --scenario <NAME>   Built-in scenario name
│   │   ├── --http <ADDR>           HTTP transport address
│   │   ├── --spoof-client <NAME>   Override client name
│   │   ├── --behavior <MODE>       Override delivery behavior
│   │   ├── --state-scope <SCOPE>   Phase state scope
│   │   ├── --profile <PROFILE>     Server profile
│   │   ├── --library <PATH>        Library root directory
│   │   └── --metrics-port <PORT>   Prometheus metrics port
│   │
│   ├── validate                    Validate configuration
│   │   ├── <FILES>...              Files to validate
│   │   ├── --format <FORMAT>       Output format
│   │   ├── --strict                Treat warnings as errors
│   │   └── --library <PATH>        Library root directory
│   │
│   └── list                        List available patterns
│       ├── [CATEGORY]              Category to list
│       ├── --tag <TAG>             Filter by tag
│       ├── --format <FORMAT>       Output format
│       └── --library <PATH>        Library root directory
│
├── scenarios                       Built-in attack scenarios
│   │
│   ├── list                        List available scenarios
│   │   ├── --category <CATEGORY>   Filter by category
│   │   ├── --tag <TAG>             Filter by tag
│   │   └── --format <FORMAT>       Output format
│   │
│   └── show <NAME>                 Display scenario YAML
│
├── diagram <SCENARIO>              Generate Mermaid diagram
│   ├── --diagram-type <TYPE>       Diagram type override
│   └── -o, --output <PATH>        Write to file
│
├── docs                            Documentation generation
│   │
│   ├── generate                    Generate docs from scenarios
│   │   ├── --scenarios <PATH>      Scenarios directory
│   │   ├── --output <PATH>         Output directory
│   │   ├── --registry <PATH>       Registry file path
│   │   └── --strict                Promote warnings to errors
│   │
│   └── validate                    Validate scenario metadata
│       ├── --scenarios <PATH>      Scenarios directory
│       ├── --registry <PATH>       Registry file path
│       └── --strict                Promote warnings to errors
│
├── agent                           Run adversarial A2A agent
│   └── (coming soon)
│
├── completions <SHELL>             Generate shell completions
│
├── version                         Show version information
│   └── --format <FORMAT>           Output format
│
└── help [COMMAND]                  Show help
```

### 5.2 Global Options

| Option | Short | Environment | Description |
|--------|-------|-------------|-------------|
| `--verbose` | `-v` | `THOUGHTJACK_LOG_LEVEL` | Increase verbosity |
| `--quiet` | `-q` | — | Suppress output |
| `--color` | — | `THOUGHTJACK_COLOR` | Color mode |
| `--help` | `-h` | — | Show help |
| `--version` | `-V` | — | Show version |

---

## 6. Implementation Notes

### 6.1 Recommended Libraries

| Library | Purpose |
|---------|---------|
| `clap` | Command-line parsing (derive mode) |
| `clap_complete` | Shell completions |
| `colored` | Terminal colors |
| `atty` | TTY detection |
| `tokio` / `tokio_util` | Signal handling and cancellation |
| `tracing` / `tracing-subscriber` | Logging |

### 6.2 Project Structure

```
thoughtjack/
└── src/
    ├── main.rs              # Entry point
    ├── cli/
    │   ├── mod.rs           # CLI module
    │   ├── args.rs          # Argument definitions
    │   ├── commands/
    │   │   ├── mod.rs
    │   │   ├── server.rs    # Server subcommands
    │   │   ├── scenarios.rs # Scenarios subcommands
    │   │   ├── diagram.rs   # Diagram generation
    │   │   ├── docs.rs      # Documentation generation
    │   │   ├── agent.rs     # Agent subcommand
    │   │   ├── completions.rs
    │   │   └── version.rs
    │   ├── output.rs        # Output formatting
    │   └── error.rs         # CLI errors
    └── ...
```

### 6.3 Error Formatting

```rust
impl ThoughtJackError {
    pub fn format(&self, color: bool) -> String {
        let prefix = if color {
            "error:".red().bold().to_string()
        } else {
            "error:".to_string()
        };

        let message = match self {
            ThoughtJackError::Config(e) => format!("{} {}\n\n{}", prefix, "Configuration error", e),
            ThoughtJackError::Io(e) => format!("{} {}", prefix, e),
            ThoughtJackError::Usage(msg) => format!("{} {}\n\nFor more information, try '--help'", prefix, msg),
            // ...
        };

        message
    }
}
```

### 6.4 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Positional args for options | Hard to remember order | Use named flags |
| Exit code 1 for everything | Can't distinguish errors | Use specific exit codes |
| Panic on invalid input | Bad UX | Return Result, format error |
| Hardcoded colors | Breaks pipes and CI | Check TTY, respect NO_COLOR |
| Blocking on stdin check | Hangs if no input | Use non-blocking check or timeout |
| Global mutable state | Testing difficulties | Pass config through functions |
| Printing to stdout in library | Can't capture | Return data, let CLI print |

### 6.5 Testing Strategy

**Unit Tests:**
- Argument parsing for all commands
- Exit code mapping
- Error formatting
- Environment variable handling

**Integration Tests:**
- Full command execution
- Signal handling
- Output format validation
- Shell completion generation

**Snapshot Tests:**
- Help text
- Error messages
- Validation output

---

## 7. Definition of Done

- [ ] Root command with global options
- [ ] `server run` command with all flags
- [ ] `server validate` command with all flags
- [ ] `server list` command with all flags
- [ ] `server` defaults to `run` subcommand
- [ ] `scenarios list` and `scenarios show` commands
- [ ] `diagram` command with diagram type selection
- [ ] `docs generate` and `docs validate` commands
- [ ] `agent` placeholder command
- [ ] `completions` command for all shells
- [ ] `version` command with build info
- [ ] Exit codes documented and implemented
- [ ] Environment variable mapping complete
- [ ] Logging verbosity levels work
- [ ] Color output respects settings
- [ ] Signal handling (SIGINT, SIGTERM)
- [ ] All 20 edge cases (EC-CLI-001 through EC-CLI-020) have tests
- [ ] Startup time < 100ms (NFR-001)
- [ ] Binary size < 20 MB stripped (NFR-003)
- [ ] Works on Linux, macOS, Windows (NFR-004)
- [ ] Shell completions work correctly
- [ ] Help text is clear and complete
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [TJ-SPEC-001: Configuration Schema](./TJ-SPEC-001_Configuration_Schema.md)
- [TJ-SPEC-006: Configuration Loader](./TJ-SPEC-006_Configuration_Loader.md)
- [TJ-SPEC-008: Observability](./TJ-SPEC-008_Observability.md)
- [Clap Derive Tutorial](https://docs.rs/clap/latest/clap/_derive/)
- [CLI Guidelines](https://clig.dev/)
- [12 Factor CLI Apps](https://medium.com/@jdxcode/12-factor-cli-apps-dd3c227a0e46)

---

## Appendix A: Help Text Examples

### A.1 Root Help

```
ThoughtJack - Adversarial MCP server for security testing

Usage: thoughtjack [OPTIONS] <COMMAND>

Commands:
  server       Start or manage the adversarial MCP server
  scenarios    List and inspect built-in attack scenarios
  diagram      Generate a Mermaid diagram from a scenario file
  docs         Generate documentation site from scenarios
  completions  Generate shell completion scripts
  version      Display version and build information
  help         Print help for a command

Options:
  -v, --verbose...     Increase logging verbosity (-v, -vv, -vvv)
  -q, --quiet          Suppress non-essential output
      --color <WHEN>   Colorize output [default: auto] [possible values: auto, always, never]
  -h, --help           Print help
  -V, --version        Print version

Examples:
  # Run a rug-pull attack server
  thoughtjack server --config attacks/rug_pull.yaml

  # Run a built-in scenario
  thoughtjack server --scenario rug-pull

  # List all built-in scenarios
  thoughtjack scenarios list

  # Generate a Mermaid diagram from a scenario
  thoughtjack diagram attacks/rug_pull.yaml

  # Generate documentation site
  thoughtjack docs generate

  # Validate all attack configurations
  thoughtjack server validate attacks/*.yaml

  # Quick test with single tool
  thoughtjack server --tool library/tools/calculator/injection.yaml

Documentation: https://github.com/thoughtgate/thoughtjack
```

### A.2 Server Run Help

```
Run an adversarial MCP server

Usage: thoughtjack server run [OPTIONS]

Options:
  -c, --config <PATH>
          Server configuration file
          
          [env: THOUGHTJACK_CONFIG]

  -t, --tool <PATH>
          Single tool pattern (creates minimal server)
          
          Use this for quick testing without writing a full server config.

      --http <ADDR>
          Enable HTTP transport on specified address
          
          Examples: ":8080", "0.0.0.0:8080", "127.0.0.1:9000"
          
          [default: stdio transport]

      --spoof-client <NAME>
          Override client name in initialize response
          
          Useful for testing scanner evasion.
          
          [env: THOUGHTJACK_SPOOF_CLIENT]

      --behavior <MODE>
          Override delivery behavior

          [env: THOUGHTJACK_BEHAVIOR]
          [possible values: normal, slow-loris, unbounded-line, nested-json, response-delay]

      --library <PATH>
          Library root directory
          
          [default: ./library]
          [env: THOUGHTJACK_LIBRARY]

  -h, --help
          Print help (see more with '--help')
```

---

## Appendix B: Exit Code Reference

| Code | Constant | When |
|------|----------|------|
| 0 | `SUCCESS` | Command completed without error |
| 1 | `GENERAL_ERROR` | Unspecified runtime error |
| 2 | `CONFIG_ERROR` | Configuration invalid |
| 3 | `IO_ERROR` | File not found, permission denied, network error |
| 4 | `TRANSPORT_ERROR` | stdio or HTTP transport failure |
| 5 | `PHASE_ERROR` | Phase engine failure (invalid transition, trigger error) |
| 10 | `GENERATOR_ERROR` | Generator error (limit exceeded, generation failed) |
| 11 | `HANDLER_ERROR` | Handler error (external handler failed) |
| 64 | `USAGE_ERROR` | Invalid arguments or flags |
| 130 | `INTERRUPTED` | Interrupted by SIGINT (Ctrl+C) |
| 143 | `TERMINATED` | Terminated by SIGTERM |

---

## Appendix C: Environment Variable Reference

| Variable | Default | Description |
|----------|---------|-------------|
| `THOUGHTJACK_CONFIG` | — | Server configuration file path |
| `THOUGHTJACK_LIBRARY` | `./library` | Pattern library root |
| `THOUGHTJACK_SPOOF_CLIENT` | — | Client name to report |
| `THOUGHTJACK_BEHAVIOR` | `normal` | Delivery behavior override |
| `THOUGHTJACK_LOG_LEVEL` | `warn` | Logging level |
| `THOUGHTJACK_COLOR` | `auto` | Color output mode |
| `THOUGHTJACK_TRANSPORT` | `stdio` | Transport type |
| `THOUGHTJACK_HTTP_BIND` | `:8080` | HTTP bind address |
| `THOUGHTJACK_MAX_PAYLOAD_BYTES` | `100mb` | Max generated payload |
| `THOUGHTJACK_MAX_NEST_DEPTH` | `100000` | Max JSON nesting |
| `THOUGHTJACK_MAX_BATCH_SIZE` | `100000` | Max batch array size |
| `NO_COLOR` | — | Disable color (standard) |
