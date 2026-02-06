//! Server command handlers (TJ-SPEC-007)
//!
//! Implements `server run`, `server validate`, and `server list`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::cli::args::{
    DeliveryMode, OutputFormat, ServerListArgs, ServerRunArgs, ServerValidateArgs,
};
use crate::config::loader::{ConfigLoader, LoaderOptions};
use crate::config::schema::{
    BehaviorConfig, DeliveryConfig, GeneratorLimits, ServerConfig, ServerMetadata, ToolPattern,
};
use crate::error::ThoughtJackError;
use crate::observability::events::EventEmitter;
use crate::server::Server;
use crate::transport::http::{HttpConfig, parse_bind_addr};
use crate::transport::{DEFAULT_MAX_MESSAGE_SIZE, HttpTransport, StdioTransport, Transport};

/// Start the adversarial MCP server.
///
/// # Errors
///
/// Returns a usage error if neither `--config` nor `--tool` is provided,
/// or a transport/phase error if the server fails during operation.
///
/// Implements: TJ-SPEC-007 F-002
pub async fn run(args: &ServerRunArgs, cancel: CancellationToken) -> Result<(), ThoughtJackError> {
    // EC-CLI-003: require at least one source
    if args.config.is_none() && args.tool.is_none() {
        return Err(ThoughtJackError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "either --config or --tool is required",
        )));
    }

    // Initialize Prometheus metrics if --metrics-port is provided
    if let Some(port) = args.metrics_port {
        crate::observability::init_metrics(Some(port))?;
        tracing::info!(port, "Prometheus metrics endpoint started");
    }

    let generator_limits = build_generator_limits(args);

    let config = if let Some(ref path) = args.config {
        tracing::info!(config = %path.display(), "loading configuration");
        let options = LoaderOptions {
            library_root: args.library.clone(),
            generator_limits,
            ..LoaderOptions::default()
        };
        let mut loader = ConfigLoader::new(options);
        let load_result = loader.load(path)?;

        for warning in &load_result.warnings {
            tracing::warn!(
                location = warning.location.as_deref().unwrap_or("<unknown>"),
                "{}",
                warning.message
            );
        }

        load_result.config
    } else if let Some(ref path) = args.tool {
        tracing::info!(tool = %path.display(), "loading single tool definition");
        let raw = std::fs::read_to_string(path)?;
        let tool_pattern: ToolPattern = serde_yaml::from_str(&raw)?;
        Arc::new(ServerConfig {
            server: ServerMetadata {
                name: tool_pattern.tool.name.clone(),
                version: Some("0.0.0".to_string()),
                state_scope: None,
                capabilities: None,
            },
            baseline: None,
            tools: Some(vec![tool_pattern]),
            resources: None,
            prompts: None,
            phases: None,
            behavior: None,
            logging: None,
            unknown_methods: None,
        })
    } else {
        unreachable!("validated above");
    };

    // Convert CLI delivery mode to BehaviorConfig override
    let cli_behavior = args.behavior.map(|mode| BehaviorConfig {
        delivery: Some(delivery_mode_to_config(mode)),
        side_effects: None,
    });

    let transport: Arc<dyn Transport> = if let Some(ref bind_addr) = args.http {
        let addr = parse_bind_addr(bind_addr);
        let http_config = HttpConfig {
            bind_addr: addr,
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
        };
        let (http_transport, bound_addr) = HttpTransport::bind(http_config, cancel.clone()).await?;
        tracing::info!(%bound_addr, "HTTP server listening");
        Arc::new(http_transport)
    } else {
        Arc::new(StdioTransport::new())
    };

    let event_emitter = if let Some(ref path) = args.events_file {
        EventEmitter::from_file(path)?
    } else {
        EventEmitter::stderr()
    };

    let server = Server::new(config, transport, cli_behavior, event_emitter, cancel);
    server.run().await
}

/// Validate configuration files without starting the server.
///
/// # Errors
///
/// Returns an I/O error if any file does not exist, or a config error
/// if validation fails.
///
/// Implements: TJ-SPEC-007 F-003
#[allow(clippy::unused_async)] // will use async when config loader gains async support
pub async fn validate(args: &ServerValidateArgs) -> Result<(), ThoughtJackError> {
    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut valid_count: usize = 0;
    let mut invalid_count: usize = 0;

    for path in &args.files {
        if !path.exists() {
            if args.format == OutputFormat::Json {
                results.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "valid": false,
                    "error": format!("file not found: {}", path.display()),
                    "warnings": [],
                }));
                invalid_count += 1;
                continue;
            }
            return Err(ThoughtJackError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("file not found: {}", path.display()),
            )));
        }
        tracing::info!(file = %path.display(), "validating configuration");

        let options = LoaderOptions {
            library_root: args.library.clone(),
            ..LoaderOptions::default()
        };
        let mut loader = ConfigLoader::new(options);

        match loader.load(path) {
            Ok(load_result) => {
                let warnings: Vec<String> = load_result
                    .warnings
                    .iter()
                    .map(|w| w.message.clone())
                    .collect();

                for warning in &load_result.warnings {
                    tracing::warn!(
                        location = warning.location.as_deref().unwrap_or("<unknown>"),
                        "{}",
                        warning.message
                    );
                }

                if args.strict && !warnings.is_empty() {
                    invalid_count += 1;
                    if args.format == OutputFormat::Json {
                        results.push(serde_json::json!({
                            "path": path.display().to_string(),
                            "valid": false,
                            "error": "strict mode: warnings present",
                            "warnings": warnings,
                        }));
                    }
                } else {
                    valid_count += 1;
                    tracing::info!(file = %path.display(), "configuration valid");
                    if args.format == OutputFormat::Json {
                        results.push(serde_json::json!({
                            "path": path.display().to_string(),
                            "valid": true,
                            "warnings": warnings,
                        }));
                    }
                }
            }
            Err(e) => {
                invalid_count += 1;
                if args.format == OutputFormat::Json {
                    results.push(serde_json::json!({
                        "path": path.display().to_string(),
                        "valid": false,
                        "error": e.to_string(),
                        "warnings": [],
                    }));
                } else {
                    return Err(e.into());
                }
            }
        }
    }

    if args.format == OutputFormat::Json {
        print_validation_json(&results, args.files.len(), valid_count, invalid_count)?;
    } else if invalid_count > 0 {
        return Err(ThoughtJackError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{invalid_count} file(s) failed validation"),
        )));
    }

    Ok(())
}

/// Prints JSON validation summary to stdout.
fn print_validation_json(
    results: &[serde_json::Value],
    total: usize,
    valid: usize,
    invalid: usize,
) -> Result<(), ThoughtJackError> {
    let output = serde_json::json!({
        "files": results,
        "summary": { "total": total, "valid": valid, "invalid": invalid }
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&output)
            .map_err(|e| ThoughtJackError::Io(std::io::Error::other(e.to_string())))?
    );
    Ok(())
}

/// List available attack patterns from the library.
///
/// # Errors
///
/// Returns an I/O error if the library directory is inaccessible.
///
/// Implements: TJ-SPEC-007 F-004
#[allow(clippy::unused_async)] // will use async when library scanning is wired in
pub async fn list(_args: &ServerListArgs) -> Result<(), ThoughtJackError> {
    Err(ThoughtJackError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "server list is not yet implemented",
    )))
}

/// Builds [`GeneratorLimits`] from CLI arguments, falling back to defaults.
fn build_generator_limits(args: &ServerRunArgs) -> GeneratorLimits {
    let defaults = GeneratorLimits::default();
    GeneratorLimits {
        max_nest_depth: args.max_nest_depth.unwrap_or(defaults.max_nest_depth),
        max_payload_bytes: args.max_payload_bytes.unwrap_or(defaults.max_payload_bytes),
        max_batch_size: args.max_batch_size.unwrap_or(defaults.max_batch_size),
    }
}

/// Converts a CLI `DeliveryMode` to a `DeliveryConfig`.
const fn delivery_mode_to_config(mode: DeliveryMode) -> DeliveryConfig {
    match mode {
        DeliveryMode::Normal => DeliveryConfig::Normal,
        DeliveryMode::SlowLoris => DeliveryConfig::SlowLoris {
            byte_delay_ms: Some(100),
            chunk_size: Some(1),
        },
        DeliveryMode::UnboundedLine => DeliveryConfig::UnboundedLine {
            target_bytes: Some(1_000_000),
            padding_char: None,
        },
        DeliveryMode::NestedJson => DeliveryConfig::NestedJson {
            depth: 10_000,
            key: None,
        },
        DeliveryMode::ResponseDelay => DeliveryConfig::ResponseDelay { delay_ms: 5000 },
    }
}
