//! CLI argument definitions (TJ-SPEC-007)
//!
//! All Clap derive structs for `ThoughtJack` command-line parsing.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::config::schema::StateScope;

// ============================================================================
// Root CLI
// ============================================================================

/// Adversarial MCP server for security testing.
///
/// Implements: TJ-SPEC-007 F-001
#[derive(Parser, Debug)]
#[command(name = "thoughtjack", author, version, about)]
#[command(propagate_version = true)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Commands,

    /// Increase verbosity (-v info, -vv debug, -vvv trace).
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress all non-error output.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Color output control.
    #[arg(long, default_value = "auto", global = true, env = "THOUGHTJACK_COLOR")]
    pub color: ColorChoice,
}

// ============================================================================
// Top-Level Commands
// ============================================================================

/// Top-level subcommands.
///
/// Implements: TJ-SPEC-007 F-001
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start or manage the adversarial MCP server.
    Server(ServerCommand),

    /// Run as an MCP client agent (coming soon).
    Agent(AgentCommand),

    /// Generate shell completion scripts.
    Completions(CompletionsArgs),

    /// Display version and build information.
    Version(VersionArgs),
}

// ============================================================================
// Server Command
// ============================================================================

/// Server management commands.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Args, Debug)]
pub struct ServerCommand {
    /// Server subcommand.
    #[command(subcommand)]
    pub subcommand: ServerSubcommand,
}

/// Server subcommands.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Subcommand, Debug)]
pub enum ServerSubcommand {
    /// Start the adversarial MCP server.
    Run(ServerRunArgs),

    /// Validate configuration files without starting the server.
    Validate(ServerValidateArgs),

    /// List available attack patterns from the library.
    List(ServerListArgs),
}

/// Arguments for `server run`.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Args, Debug)]
#[command(group = clap::ArgGroup::new("source").multiple(false))]
pub struct ServerRunArgs {
    /// Path to YAML configuration file.
    #[arg(short, long, group = "source", env = "THOUGHTJACK_CONFIG")]
    pub config: Option<PathBuf>,

    /// Path to a single tool definition file (quick-start mode).
    #[arg(short, long, group = "source")]
    pub tool: Option<PathBuf>,

    /// Bind HTTP transport on `[host:]port` instead of stdio.
    #[arg(long)]
    pub http: Option<String>,

    /// Spoof client identity string for MCP initialization.
    #[arg(long, env = "THOUGHTJACK_SPOOF_CLIENT")]
    pub spoof_client: Option<String>,

    /// Override delivery behavior for all responses.
    #[arg(long, env = "THOUGHTJACK_BEHAVIOR")]
    pub behavior: Option<DeliveryMode>,

    /// Phase state scope.
    #[arg(
        long,
        default_value = "per-connection",
        env = "THOUGHTJACK_STATE_SCOPE"
    )]
    pub state_scope: StateScope,

    /// Server profile preset.
    #[arg(long, default_value = "default")]
    pub profile: ServerProfile,

    /// Path to the attack pattern library directory.
    #[arg(long, default_value = "./library", env = "THOUGHTJACK_LIBRARY")]
    pub library: PathBuf,

    /// Directory to capture request/response traffic.
    #[arg(long, env = "THOUGHTJACK_CAPTURE_DIR")]
    pub capture_dir: Option<PathBuf>,

    /// Redact sensitive data in captured traffic.
    #[arg(long, requires = "capture_dir")]
    pub capture_redact: bool,

    /// Allow external handler scripts.
    #[arg(long, env = "THOUGHTJACK_ALLOW_EXTERNAL_HANDLERS")]
    pub allow_external_handlers: bool,
}

/// Arguments for `server validate`.
///
/// Implements: TJ-SPEC-007 F-003
#[derive(Args, Debug)]
pub struct ServerValidateArgs {
    /// Configuration files to validate.
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// Output format.
    #[arg(short, long, default_value = "human")]
    pub format: OutputFormat,

    /// Enable strict validation (warnings become errors).
    #[arg(long)]
    pub strict: bool,

    /// Path to the attack pattern library directory.
    #[arg(long, default_value = "./library")]
    pub library: PathBuf,
}

/// Arguments for `server list`.
///
/// Implements: TJ-SPEC-007 F-004
#[derive(Args, Debug)]
pub struct ServerListArgs {
    /// Category to list.
    #[arg(default_value = "all")]
    pub category: ListCategory,

    /// Filter by tag.
    #[arg(long)]
    pub tag: Option<String>,

    /// Output format.
    #[arg(short, long, default_value = "human")]
    pub format: OutputFormat,

    /// Path to the attack pattern library directory.
    #[arg(long, default_value = "./library")]
    pub library: PathBuf,
}

// ============================================================================
// Agent Command
// ============================================================================

/// Agent mode arguments (placeholder).
///
/// Implements: TJ-SPEC-007 F-006
#[derive(Args, Debug)]
pub struct AgentCommand {
    /// Path to agent configuration file.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
}

// ============================================================================
// Completions / Version
// ============================================================================

/// Arguments for shell completion generation.
///
/// Implements: TJ-SPEC-007 F-007
#[derive(Args, Debug)]
pub struct CompletionsArgs {
    /// Target shell for completion script.
    pub shell: Shell,
}

/// Arguments for version display.
///
/// Implements: TJ-SPEC-007 F-008
#[derive(Args, Debug)]
pub struct VersionArgs {
    /// Output format.
    #[arg(short, long, default_value = "human")]
    pub format: OutputFormat,
}

// ============================================================================
// CLI-Local Enums
// ============================================================================

/// Color output choice.
///
/// Implements: TJ-SPEC-007 F-012
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ColorChoice {
    /// Auto-detect terminal support.
    #[default]
    Auto,
    /// Always use color.
    Always,
    /// Never use color.
    Never,
}

/// Delivery behavior override for CLI.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DeliveryMode {
    /// Standard immediate delivery.
    Normal,
    /// Slow loris drip-feed attack.
    SlowLoris,
    /// Never-ending line (no newline terminator).
    UnboundedLine,
    /// Deeply nested JSON response.
    NestedJson,
    /// Fixed delay before response.
    ResponseDelay,
}

/// Server profile preset.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ServerProfile {
    /// Balanced defaults.
    #[default]
    Default,
    /// Maximum attack intensity.
    Aggressive,
    /// Low-detection mode.
    Stealth,
}

/// Output format for structured output.
///
/// Implements: TJ-SPEC-007 F-003
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output.
    #[default]
    Human,
    /// JSON output.
    Json,
}

/// Library item category.
///
/// Implements: TJ-SPEC-007 F-004
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ListCategory {
    /// Server configurations.
    Servers,
    /// Tool definitions.
    Tools,
    /// Resource definitions.
    Resources,
    /// Prompt definitions.
    Prompts,
    /// Delivery behaviors.
    Behaviors,
    /// All categories.
    #[default]
    All,
}

/// Shell type for completion generation.
///
/// Implements: TJ-SPEC-007 F-007
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// Bash shell.
    Bash,
    /// Zsh shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// `PowerShell`.
    #[value(name = "powershell")]
    PowerShell,
    /// Elvish shell.
    Elvish,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_run_with_config() {
        let cli = Cli::try_parse_from(["thoughtjack", "server", "run", "--config", "test.yaml"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    #[test]
    fn test_server_run_with_tool() {
        let cli = Cli::try_parse_from(["thoughtjack", "server", "run", "--tool", "tool.yaml"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_config_and_tool_mutually_exclusive() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "server",
            "run",
            "--config",
            "c.yaml",
            "--tool",
            "t.yaml",
        ]);
        assert!(cli.is_err(), "Expected mutual exclusion error");
    }

    #[test]
    fn test_help_output() {
        let result = Cli::try_parse_from(["thoughtjack", "--help"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
    }

    #[test]
    fn test_version_output() {
        let result = Cli::try_parse_from(["thoughtjack", "--version"]);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn test_default_state_scope() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "server", "run", "--config", "test.yaml"]).unwrap();

        if let Commands::Server(cmd) = cli.command {
            if let ServerSubcommand::Run(args) = cmd.subcommand {
                assert_eq!(args.state_scope, StateScope::PerConnection);
                return;
            }
        }
        panic!("Expected ServerRunArgs");
    }

    #[test]
    fn test_default_profile() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "server", "run", "--config", "test.yaml"]).unwrap();

        if let Commands::Server(cmd) = cli.command {
            if let ServerSubcommand::Run(args) = cmd.subcommand {
                assert_eq!(args.profile, ServerProfile::Default);
                return;
            }
        }
        panic!("Expected ServerRunArgs");
    }

    #[test]
    fn test_color_choices_parse() {
        for variant in ["auto", "always", "never"] {
            let cli = Cli::try_parse_from([
                "thoughtjack",
                "--color",
                variant,
                "server",
                "run",
                "--config",
                "x.yaml",
            ]);
            assert!(cli.is_ok(), "Failed to parse color={variant}");
        }
    }

    #[test]
    fn test_delivery_modes_parse() {
        for mode in [
            "normal",
            "slow-loris",
            "unbounded-line",
            "nested-json",
            "response-delay",
        ] {
            let cli = Cli::try_parse_from([
                "thoughtjack",
                "server",
                "run",
                "--config",
                "x.yaml",
                "--behavior",
                mode,
            ]);
            assert!(cli.is_ok(), "Failed to parse behavior={mode}");
        }
    }

    #[test]
    fn test_server_validate_requires_files() {
        let result = Cli::try_parse_from(["thoughtjack", "server", "validate"]);
        assert!(result.is_err(), "Expected error for missing files");
    }

    #[test]
    fn test_completions_shells_parse() {
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let cli = Cli::try_parse_from(["thoughtjack", "completions", shell]);
            assert!(cli.is_ok(), "Failed to parse shell={shell}");
        }
    }

    #[test]
    fn test_verbose_count() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "-vvv", "server", "run", "--config", "x.yaml"])
                .unwrap();
        assert_eq!(cli.verbose, 3);
    }

    #[test]
    fn test_quiet_flag() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "--quiet",
            "server",
            "run",
            "--config",
            "x.yaml",
        ])
        .unwrap();
        assert!(cli.quiet);
    }

    #[test]
    fn test_exit_code_mapping() {
        use crate::error::{
            BehaviorError, ConfigError, ExitCode, GeneratorError, PhaseError, ThoughtJackError,
            TransportError,
        };

        let cases: Vec<(ThoughtJackError, i32)> = vec![
            (
                ConfigError::MissingFile {
                    path: PathBuf::from("/x"),
                }
                .into(),
                ExitCode::CONFIG_ERROR,
            ),
            (
                TransportError::ConnectionFailed("x".into()).into(),
                ExitCode::TRANSPORT_ERROR,
            ),
            (
                PhaseError::NotFound("x".into()).into(),
                ExitCode::PHASE_ERROR,
            ),
            (
                GeneratorError::LimitExceeded("x".into()).into(),
                ExitCode::GENERATOR_ERROR,
            ),
            (
                BehaviorError::ExecutionFailed("x".into()).into(),
                ExitCode::ERROR,
            ),
            (
                std::io::Error::new(std::io::ErrorKind::NotFound, "x").into(),
                ExitCode::IO_ERROR,
            ),
        ];

        for (err, expected) in cases {
            assert_eq!(err.exit_code(), expected, "Wrong exit code for {err}");
        }
    }
}
