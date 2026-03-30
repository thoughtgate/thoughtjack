//! CLI argument definitions (TJ-SPEC-007 v2)
//!
//! All Clap derive structs for `ThoughtJack` command-line parsing.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

use crate::scenarios::ScenarioCategory;

// ============================================================================
// Root CLI
// ============================================================================

/// Adversarial agent security testing tool.
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
    #[arg(short, long, action = ArgAction::Count, global = true, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Suppress all non-error output.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Color output control.
    #[arg(long, default_value = "auto", global = true, env = "THOUGHTJACK_COLOR")]
    pub color: ColorChoice,

    /// Log output format.
    #[arg(
        long,
        default_value = "human",
        global = true,
        env = "THOUGHTJACK_LOG_FORMAT"
    )]
    pub log_format: LogFormatChoice,
}

// ============================================================================
// Top-Level Commands
// ============================================================================

/// Top-level subcommands.
///
/// Implements: TJ-SPEC-007 F-001
#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Execute an OATF scenario against a target agent.
    Run(Box<RunArgs>),

    /// List, show, or run built-in attack scenarios.
    Scenarios(ScenariosCommand),

    /// Validate an OATF document without executing.
    Validate(ValidateArgs),

    /// Display version and build information.
    Version(VersionArgs),
}

// ============================================================================
// Run Command
// ============================================================================

/// Arguments for `run` — execute an OATF scenario.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Args, Debug, Clone)]
pub struct ExecutionArgs {
    /// MCP server HTTP listen address (omit for stdio).
    #[arg(long, value_name = "ADDR:PORT")]
    pub mcp_server: Option<String>,

    /// Spawn MCP client by running a command (supports inline args, e.g.,
    /// `"npx -y @modelcontextprotocol/server-everything"`).
    #[arg(long, value_name = "CMD")]
    pub mcp_client_command: Option<String>,

    /// Extra arguments for `--mcp-client-command`.
    #[arg(long, value_name = "ARGS", requires = "mcp_client_command")]
    pub mcp_client_args: Option<String>,

    /// Connect MCP client to an HTTP endpoint instead of spawning.
    #[arg(long, value_name = "URL", conflicts_with = "mcp_client_command")]
    pub mcp_client_endpoint: Option<String>,

    /// Connect AG-UI client to an endpoint.
    #[arg(long, value_name = "URL")]
    pub agui_client_endpoint: Option<String>,

    /// A2A server listen address [default: 127.0.0.1:9090].
    #[arg(long, value_name = "ADDR:PORT")]
    pub a2a_server: Option<String>,

    /// A2A client target endpoint.
    #[arg(long, value_name = "URL")]
    pub a2a_client_endpoint: Option<String>,

    /// Override document grace period.
    #[arg(long, value_name = "DURATION")]
    pub grace_period: Option<humantime::Duration>,

    /// Safety timeout for entire session [default: 5m].
    #[arg(long, value_name = "DURATION", default_value = "5m")]
    pub max_session: humantime::Duration,

    /// Timeout for server readiness gate [default: 30s].
    #[arg(long, value_name = "DURATION", default_value = "30s")]
    pub readiness_timeout: humantime::Duration,

    /// Write JSON verdict to file (use `-` for stdout).
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<String>,

    /// HTTP headers for client transports (repeatable).
    #[arg(long, value_name = "KEY:VALUE")]
    pub header: Vec<String>,

    /// Disable semantic (LLM-as-judge) indicator evaluation.
    #[arg(long)]
    pub no_semantic: bool,

    /// Bypass synthesize output validation (allows malformed responses).
    #[arg(long)]
    pub raw_synthesize: bool,

    /// Enable Prometheus metrics endpoint on the specified port.
    #[arg(long, env = "THOUGHTJACK_METRICS_PORT")]
    pub metrics_port: Option<u16>,

    /// Write structured events to a JSONL file instead of stderr.
    #[arg(long, env = "THOUGHTJACK_EVENTS_FILE")]
    pub events_file: Option<PathBuf>,

    /// Progress output [default: auto (on for TTY)].
    #[arg(long, default_value = "auto", env = "THOUGHTJACK_PROGRESS")]
    pub progress: ProgressLevel,

    /// Write full protocol trace (with payloads) to a JSONL file.
    ///
    /// Each line is a JSON object with seq, timestamp, actor, phase,
    /// direction, method, and content fields (see TJ-SPEC-015 §5.4).
    /// Use `-` for stdout.
    ///
    /// Implements: TJ-SPEC-014 F-010
    #[arg(long, value_name = "PATH", env = "THOUGHTJACK_EXPORT_TRACE")]
    pub export_trace: Option<String>,

    // ---- Context-mode flags (TJ-SPEC-022) ----
    /// Enable context-mode (call LLM API directly instead of real protocol infrastructure).
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long)]
    pub context: bool,

    /// LLM model identifier for context-mode (required with --context).
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "MODEL", env = "THOUGHTJACK_CONTEXT_MODEL")]
    pub context_model: Option<String>,

    /// API key for context-mode LLM provider.
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "KEY", env = "THOUGHTJACK_CONTEXT_API_KEY")]
    pub context_api_key: Option<String>,

    /// Base URL for context-mode LLM provider (overrides default).
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "URL", env = "THOUGHTJACK_CONTEXT_BASE_URL")]
    pub context_base_url: Option<String>,

    /// LLM provider type for context-mode [default: openai].
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(
        long,
        value_name = "TYPE",
        env = "THOUGHTJACK_CONTEXT_PROVIDER",
        default_value = "openai"
    )]
    pub context_provider: String,

    /// Sampling temperature for context-mode [default: 0.0].
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "FLOAT", env = "THOUGHTJACK_CONTEXT_TEMPERATURE")]
    pub context_temperature: Option<f32>,

    /// Max tokens per LLM response in context-mode [default: 4096].
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "TOKENS", env = "THOUGHTJACK_CONTEXT_MAX_TOKENS")]
    pub context_max_tokens: Option<u32>,

    /// System prompt for context-mode (simulates agent framework instructions).
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "PROMPT", env = "THOUGHTJACK_CONTEXT_SYSTEM_PROMPT")]
    pub context_system_prompt: Option<String>,

    /// Per-request timeout in seconds for context-mode LLM calls [default: 120].
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "SECONDS", env = "THOUGHTJACK_CONTEXT_TIMEOUT")]
    pub context_timeout: Option<u64>,

    /// Maximum LLM conversation turns in context-mode [default: 20].
    ///
    /// Implements: TJ-SPEC-022 F-001
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub max_turns: Option<u32>,
}

/// Arguments for `run` — execute an OATF scenario.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Args, Debug)]
pub struct RunArgs {
    /// Path to OATF scenario YAML document.
    #[arg(env = "THOUGHTJACK_SCENARIO", value_name = "SCENARIO")]
    pub scenario: PathBuf,

    /// Shared runtime and output options.
    #[command(flatten)]
    pub execution: ExecutionArgs,
}

// ============================================================================
// Scenarios Command
// ============================================================================

/// Built-in attack scenario commands.
///
/// Implements: TJ-SPEC-010 F-004, F-005, F-008
#[derive(Args, Debug)]
pub struct ScenariosCommand {
    /// Scenarios subcommand.
    #[command(subcommand)]
    pub subcommand: ScenariosSubcommand,
}

/// Scenarios subcommands.
///
/// Implements: TJ-SPEC-010 F-004, F-005
#[derive(Subcommand, Debug)]
pub enum ScenariosSubcommand {
    /// List available built-in scenarios.
    List(ScenariosListArgs),

    /// Display the YAML configuration for a built-in scenario.
    Show(ScenariosShowArgs),

    /// Run a built-in scenario by name.
    Run(Box<ScenariosRunArgs>),
}

/// Arguments for `scenarios list`.
///
/// Implements: TJ-SPEC-010 F-004
#[derive(Args, Debug)]
pub struct ScenariosListArgs {
    /// Filter by category.
    #[arg(long)]
    pub category: Option<ScenarioCategory>,

    /// Filter by tag.
    #[arg(long)]
    pub tag: Option<String>,

    /// Output format.
    #[arg(long, default_value = "human")]
    pub format: OutputFormat,
}

/// Arguments for `scenarios show`.
///
/// Implements: TJ-SPEC-010 F-005
#[derive(Args, Debug)]
pub struct ScenariosShowArgs {
    /// Scenario name to display.
    pub name: String,
}

/// Arguments for `scenarios run`.
///
/// Runs a built-in scenario with the same flags as `run`.
/// Does not accept a positional scenario path (uses built-in YAML).
///
/// Implements: TJ-SPEC-010 F-008
#[derive(Args, Debug)]
pub struct ScenariosRunArgs {
    /// Built-in scenario name.
    pub name: String,

    /// Shared runtime and output options (same as `thoughtjack run`, minus the positional scenario path).
    #[command(flatten)]
    pub execution: ExecutionArgs,
}

// ============================================================================
// Validate Command
// ============================================================================

/// Arguments for `validate` — validate an OATF document.
///
/// Implements: TJ-SPEC-007 F-003
#[derive(Args, Debug)]
pub struct ValidateArgs {
    /// Path to OATF scenario YAML document.
    pub path: PathBuf,

    /// Normalize and print the resolved document.
    #[arg(long)]
    pub normalize: bool,
}

// ============================================================================
// Version Command
// ============================================================================

/// Arguments for version display.
///
/// Implements: TJ-SPEC-007 F-005
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
/// Implements: TJ-SPEC-007 F-001
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

/// Log output format choice.
///
/// Implements: TJ-SPEC-008 F-002, F-003
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum LogFormatChoice {
    /// Human-readable format with optional ANSI colors.
    #[default]
    Human,
    /// Newline-delimited JSON for machine consumption.
    Json,
}

/// Progress output level.
///
/// Controls whether real-time progress information is shown during
/// scenario execution.
///
/// Implements: TJ-SPEC-007 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ProgressLevel {
    /// No progress output.
    Off,
    /// Show progress output.
    On,
    /// Auto-detect: on for TTY, off otherwise.
    #[default]
    Auto,
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_with_config() {
        let cli = Cli::try_parse_from(["thoughtjack", "run", "test.yaml"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
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
    fn test_color_choices_parse() {
        for variant in ["auto", "always", "never"] {
            let cli = Cli::try_parse_from([
                "thoughtjack",
                "--color",
                variant,
                "run",
                                "x.yaml",
            ]);
            assert!(cli.is_ok(), "Failed to parse color={variant}");
        }
    }

    #[test]
    fn test_verbose_count() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "-vvv", "run", "x.yaml"]).unwrap();
        assert_eq!(cli.verbose, 3);
    }

    #[test]
    fn test_quiet_flag() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "--quiet", "run", "x.yaml"]).unwrap();
        assert!(cli.quiet);
    }

    /// EC-CLI-001: No arguments at all should fail (subcommand required).
    #[test]
    fn test_no_args_fails() {
        let result = Cli::try_parse_from(["thoughtjack"]);
        assert!(result.is_err(), "Expected error when no subcommand given");
    }

    /// EC-CLI-002: Unknown subcommand should fail.
    #[test]
    fn test_unknown_subcommand_fails() {
        let result = Cli::try_parse_from(["thoughtjack", "foobar"]);
        assert!(result.is_err(), "Expected error for unknown subcommand");
    }

    /// EC-CLI-005: --verbose and --quiet conflict.
    #[test]
    fn test_verbose_quiet_conflict() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "--verbose",
            "--quiet",
            "run",
                        "x.yaml",
        ]);
        assert!(result.is_err(), "Expected conflict error for -v + -q");
    }

    /// EC-CLI-006: Excessive verbosity still parses (count = 4).
    #[test]
    fn test_excessive_verbosity_clamps() {
        let cli =
            Cli::try_parse_from(["thoughtjack", "-vvvv", "run", "x.yaml"]).unwrap();
        assert_eq!(cli.verbose, 4, "Expected verbosity count of 4");
    }

    /// EC-CLI-007: All valid --color values parse correctly.
    #[test]
    fn test_color_values() {
        let expected = [
            ("auto", ColorChoice::Auto),
            ("always", ColorChoice::Always),
            ("never", ColorChoice::Never),
        ];
        for (input, variant) in expected {
            let cli =
                Cli::try_parse_from(["thoughtjack", "--color", input, "run", "x.yaml"])
                    .unwrap();
            assert_eq!(cli.color, variant, "Unexpected color variant for {input}");
        }
    }

    /// Invalid --color value should fail.
    #[test]
    fn test_invalid_color_value() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "--color",
            "rainbow",
            "run",
                        "x.yaml",
        ]);
        assert!(result.is_err(), "Expected error for invalid color value");
    }

    #[test]
    fn test_scenarios_list_command() {
        let cli = Cli::try_parse_from(["thoughtjack", "scenarios", "list"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    #[test]
    fn test_scenarios_list_with_category() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "scenarios",
            "list",
            "--category",
            "injection",
        ]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    #[test]
    fn test_scenarios_show_command() {
        let cli = Cli::try_parse_from(["thoughtjack", "scenarios", "show", "rug-pull"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// EC-CLI-013: `version` subcommand parses.
    #[test]
    fn test_version_command() {
        let cli = Cli::try_parse_from(["thoughtjack", "version"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Version(_)),
            "Expected Version command"
        );
    }

    /// Test --log-format accepts both human and json values.
    #[test]
    fn test_log_format_values() {
        let expected = [
            ("human", LogFormatChoice::Human),
            ("json", LogFormatChoice::Json),
        ];
        for (input, variant) in expected {
            let cli = Cli::try_parse_from([
                "thoughtjack",
                "--log-format",
                input,
                "run",
                                "x.yaml",
            ])
            .unwrap();
            assert_eq!(cli.log_format, variant, "Unexpected log-format for {input}");
        }
    }

    /// Validate command parses with path.
    #[test]
    fn test_validate_command() {
        let cli = Cli::try_parse_from(["thoughtjack", "validate", "scenario.yaml"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// Validate command with --normalize flag.
    #[test]
    fn test_validate_normalize() {
        let cli = Cli::try_parse_from(["thoughtjack", "validate", "scenario.yaml", "--normalize"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// Run with --max-session duration.
    #[test]
    fn test_run_max_session() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "run",
                        "x.yaml",
            "--max-session",
            "10m",
        ]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// Run with --grace-period duration.
    #[test]
    fn test_run_grace_period() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "run",
                        "x.yaml",
            "--grace-period",
            "30s",
        ]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// MCP client flags: --mcp-client-args requires --mcp-client-command.
    #[test]
    fn test_mcp_client_args_requires_command() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "run",
                        "x.yaml",
            "--mcp-client-args",
            "foo",
        ]);
        assert!(
            result.is_err(),
            "Expected error: --mcp-client-args requires --mcp-client-command"
        );
    }

    /// MCP client flags: --mcp-client-command and --mcp-client-endpoint conflict.
    #[test]
    fn test_mcp_client_command_endpoint_conflict() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "run",
                        "x.yaml",
            "--mcp-client-command",
            "npx server",
            "--mcp-client-endpoint",
            "http://localhost:3000",
        ]);
        assert!(
            result.is_err(),
            "Expected conflict: --mcp-client-command vs --mcp-client-endpoint"
        );
    }

    /// EC-CLI-018: --log-format json is parsed correctly.
    #[test]
    fn test_json_log_format() {
        let cli = Cli::try_parse_from([
            "thoughtjack",
            "--log-format",
            "json",
            "run",
                        "x.yaml",
        ])
        .unwrap();
        assert_eq!(cli.log_format, LogFormatChoice::Json);

        // Invalid log format is rejected
        let invalid = Cli::try_parse_from([
            "thoughtjack",
            "--log-format",
            "xml",
            "run",
                        "x.yaml",
        ]);
        assert!(
            invalid.is_err(),
            "Expected parse error for invalid log format 'xml'"
        );
    }

    /// Scenarios run rejects a positional path (built-in YAML only).
    #[test]
    fn test_scenarios_run_rejects_config() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "scenarios",
            "run",
            "rug-pull",
                        "x.yaml",
        ]);
        assert!(
            result.is_err(),
            "Expected clap parse error: scenarios run should not accept positional path"
        );
    }

    /// Scenarios run works without --config (uses built-in YAML).
    #[test]
    fn test_scenarios_run_without_config() {
        let cli = Cli::try_parse_from(["thoughtjack", "scenarios", "run", "rug-pull"]);
        assert!(cli.is_ok(), "Failed to parse: {cli:?}");
    }

    /// `run` without a scenario path should fail at parse time.
    #[test]
    fn test_run_without_config_fails() {
        let cli = Cli::try_parse_from(["thoughtjack", "run"]);
        assert!(cli.is_err(), "Expected clap parse error");
    }

    /// All valid --progress values parse correctly.
    #[test]
    fn test_progress_values() {
        let expected = [
            ("off", ProgressLevel::Off),
            ("on", ProgressLevel::On),
            ("auto", ProgressLevel::Auto),
        ];
        for (input, variant) in expected {
            let cli = Cli::try_parse_from([
                "thoughtjack",
                "run",
                                "x.yaml",
                "--progress",
                input,
            ])
            .unwrap();
            match cli.command {
                Commands::Run(args) => {
                    assert_eq!(args.execution.progress, variant, "for --progress {input}");
                }
                _ => panic!("Expected Run command"),
            }
        }
    }

    /// Invalid --progress value should fail.
    #[test]
    fn test_invalid_progress_value() {
        let result = Cli::try_parse_from([
            "thoughtjack",
            "run",
                        "x.yaml",
            "--progress",
            "verbose",
        ]);
        assert!(result.is_err(), "Expected error for invalid progress value");
    }

    /// --progress defaults to auto.
    #[test]
    fn test_progress_default_auto() {
        let cli = Cli::try_parse_from(["thoughtjack", "run", "x.yaml"]).unwrap();
        match cli.command {
            Commands::Run(args) => {
                assert_eq!(args.execution.progress, ProgressLevel::Auto);
            }
            _ => panic!("Expected Run command"),
        }
    }
}
