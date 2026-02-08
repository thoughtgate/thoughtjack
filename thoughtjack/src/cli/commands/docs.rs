//! Documentation generation command handlers.
//!
//! TJ-SPEC-011 F-006

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use crate::cli::args::{DocsGenerateArgs, DocsValidateArgs};
use crate::error::ThoughtJackError;
use thoughtjack_core::config::schema::ServerConfig;
use thoughtjack_docs::coverage;
use thoughtjack_docs::mdx::page::generate_scenario_page;
use thoughtjack_docs::registry;
use thoughtjack_docs::sidebar;
use thoughtjack_docs::validate;

/// Resolve the base directory from the scenarios path.
fn base_dir(scenarios: &Path) -> &Path {
    scenarios.parent().unwrap_or_else(|| Path::new("."))
}

/// Execute `docs generate`.
///
/// Scans scenario files, generates MDX pages, coverage matrices,
/// sidebar config, and validates metadata.
///
/// # Errors
///
/// Returns an error if file I/O, parsing, or validation fails.
///
/// Implements: TJ-SPEC-011 F-006
pub fn generate(args: &DocsGenerateArgs) -> Result<(), ThoughtJackError> {
    eprintln!("Generating documentation...");
    eprintln!("  scenarios: {}", args.scenarios.display());
    eprintln!("  output:    {}", args.output.display());
    eprintln!("  registry:  {}", args.registry.display());

    let reg = load_registry(&args.registry)?;
    let configs = load_scenarios(&reg, base_dir(&args.scenarios))?;

    check_registry(&reg, base_dir(&args.scenarios), &configs, args.strict)?;
    check_metadata(&configs, args.strict)?;

    let generated = write_scenario_pages(&configs, &args.output)?;
    write_coverage_pages(&configs, &args.output)?;
    write_sidebar(&reg, &args.output)?;

    eprintln!("Generated {generated} scenario pages");
    eprintln!("Generated 3 coverage pages");
    eprintln!("Generated sidebars.js");

    Ok(())
}

/// Execute `docs validate`.
///
/// Validates scenario metadata and registry without generating output.
///
/// # Errors
///
/// Returns an error if validation fails in strict mode.
///
/// Implements: TJ-SPEC-011 F-006
pub fn validate_cmd(args: &DocsValidateArgs) -> Result<(), ThoughtJackError> {
    eprintln!("Validating documentation metadata...");

    let reg = load_registry(&args.registry)?;
    let scenario_paths = registry::all_scenario_paths(&reg);
    let mut error_count = 0;
    let mut scenarios_for_dup: Vec<(String, thoughtjack_core::config::schema::ScenarioMetadata)> =
        Vec::new();

    for scenario_path in &scenario_paths {
        let full_path = base_dir(&args.scenarios).join(scenario_path);
        let yaml = match fs::read_to_string(&full_path) {
            Ok(y) => y,
            Err(e) => {
                eprintln!("ERROR: cannot read {}: {e}", full_path.display());
                error_count += 1;
                continue;
            }
        };

        let config: ServerConfig = match serde_yaml::from_str(&yaml) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("ERROR: cannot parse {}: {e}", full_path.display());
                error_count += 1;
                continue;
            }
        };

        if let Some(ref metadata) = config.metadata {
            let errors = validate::validate_metadata(metadata, &full_path);
            for error in &errors {
                eprintln!("{error}");
                error_count += 1;
            }
            scenarios_for_dup.push((full_path.display().to_string(), metadata.clone()));
        }
    }

    let dup_errors = validate::detect_duplicate_ids(&scenarios_for_dup);
    for error in &dup_errors {
        eprintln!("{error}");
        error_count += 1;
    }

    // Registry structure validation
    let validation = registry::validate_registry(
        &reg,
        base_dir(&args.scenarios),
        &HashSet::new(),
        &HashSet::new(),
    );

    for missing in &validation.missing_files {
        eprintln!("ERROR: missing file: {}", missing.display());
        error_count += 1;
    }

    for empty in &validation.empty_categories {
        eprintln!("WARNING: empty category: {empty}");
        if args.strict {
            error_count += 1;
        }
    }

    if error_count > 0 {
        eprintln!("\n{error_count} error(s) found");
        if args.strict {
            return Err(ThoughtJackError::Io(std::io::Error::other(format!(
                "{error_count} validation error(s)"
            ))));
        }
    } else {
        eprintln!("Validation passed");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load and parse the registry YAML file.
fn load_registry(path: &Path) -> Result<thoughtjack_docs::registry::Registry, ThoughtJackError> {
    let content = fs::read_to_string(path).map_err(|e| {
        ThoughtJackError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read registry {}: {e}", path.display()),
        ))
    })?;

    registry::parse_registry(&content).map_err(|e| {
        ThoughtJackError::Io(std::io::Error::other(format!("registry parse error: {e}")))
    })
}

/// Load and parse all scenarios referenced in the registry.
fn load_scenarios(
    reg: &thoughtjack_docs::registry::Registry,
    base: &Path,
) -> Result<Vec<(String, ServerConfig)>, ThoughtJackError> {
    let scenario_paths = registry::all_scenario_paths(reg);
    let mut configs = Vec::new();

    for scenario_path in &scenario_paths {
        let full_path = base.join(scenario_path);
        let yaml = fs::read_to_string(&full_path).map_err(|e| {
            ThoughtJackError::Io(std::io::Error::new(
                e.kind(),
                format!("failed to read {}: {e}", full_path.display()),
            ))
        })?;

        let config: ServerConfig = serde_yaml::from_str(&yaml).map_err(|e| {
            ThoughtJackError::Config(thoughtjack_core::error::ConfigError::ParseError {
                path: full_path.clone(),
                line: None,
                message: e.to_string(),
            })
        })?;

        configs.push((full_path.display().to_string(), config));
    }

    Ok(configs)
}

/// Validate registry entries against the filesystem.
fn check_registry(
    reg: &thoughtjack_docs::registry::Registry,
    base: &Path,
    configs: &[(String, ServerConfig)],
    strict: bool,
) -> Result<(), ThoughtJackError> {
    let metadata_files: HashSet<_> = configs
        .iter()
        .filter(|(_, c)| c.metadata.is_some())
        .filter_map(|(p, _)| Path::new(p).strip_prefix(base).ok().map(Path::to_path_buf))
        .collect();

    let validation = registry::validate_registry(reg, base, &metadata_files, &HashSet::new());

    if !validation.missing_files.is_empty() {
        for missing in &validation.missing_files {
            eprintln!(
                "ERROR: missing file referenced in registry: {}",
                missing.display()
            );
        }
        if strict {
            return Err(ThoughtJackError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "registry references missing files",
            )));
        }
    }

    if !validation.orphan_scenarios.is_empty() {
        for orphan in &validation.orphan_scenarios {
            eprintln!(
                "WARNING: orphan scenario (has metadata, not in registry): {}",
                orphan.display()
            );
        }
        if strict {
            return Err(ThoughtJackError::Io(std::io::Error::other(
                "orphan scenarios found in strict mode",
            )));
        }
    }

    Ok(())
}

/// Validate metadata across all scenarios.
fn check_metadata(
    configs: &[(String, ServerConfig)],
    strict: bool,
) -> Result<(), ThoughtJackError> {
    let mut has_errors = false;

    let scenarios_for_dup: Vec<_> = configs
        .iter()
        .filter_map(|(path, config)| config.metadata.as_ref().map(|m| (path.clone(), m.clone())))
        .collect();

    for (path, config) in configs {
        if let Some(ref metadata) = config.metadata {
            let errors = validate::validate_metadata(metadata, Path::new(path));
            for error in &errors {
                eprintln!("{error}");
                has_errors = true;
            }
        }
    }

    let dup_errors = validate::detect_duplicate_ids(&scenarios_for_dup);
    for error in &dup_errors {
        eprintln!("{error}");
        has_errors = true;
    }

    if has_errors && strict {
        return Err(ThoughtJackError::Io(std::io::Error::other(
            "metadata validation errors in strict mode",
        )));
    }

    Ok(())
}

/// Generate and write MDX scenario pages. Returns count generated.
fn write_scenario_pages(
    configs: &[(String, ServerConfig)],
    output: &Path,
) -> Result<usize, ThoughtJackError> {
    let scenarios_out = output.join("docs/scenarios");
    fs::create_dir_all(&scenarios_out).map_err(ThoughtJackError::Io)?;

    let mut generated = 0;
    for (path, config) in configs {
        let Some(ref metadata) = config.metadata else {
            continue;
        };

        let yaml_source = fs::read_to_string(path).unwrap_or_default();
        match generate_scenario_page(config, &yaml_source) {
            Ok(mdx) => {
                let out_file = scenarios_out.join(format!("{}.mdx", metadata.id.to_lowercase()));
                fs::write(&out_file, mdx).map_err(ThoughtJackError::Io)?;
                generated += 1;
            }
            Err(e) => {
                eprintln!("WARNING: failed to generate page for {path}: {e}");
            }
        }
    }

    Ok(generated)
}

/// Generate and write coverage MDX pages.
fn write_coverage_pages(
    configs: &[(String, ServerConfig)],
    output: &Path,
) -> Result<(), ThoughtJackError> {
    let coverage_out = output.join("docs/coverage");
    fs::create_dir_all(&coverage_out).map_err(ThoughtJackError::Io)?;

    let server_configs: Vec<_> = configs.iter().map(|(_, c)| c.clone()).collect();
    let matrix = coverage::build_coverage_matrix(&server_configs);

    fs::write(
        coverage_out.join("mitre-matrix.mdx"),
        coverage::mitre::generate_mitre_coverage(&matrix),
    )
    .map_err(ThoughtJackError::Io)?;

    fs::write(
        coverage_out.join("owasp-mcp.mdx"),
        coverage::owasp_mcp::generate_owasp_mcp_coverage(&matrix, &[]),
    )
    .map_err(ThoughtJackError::Io)?;

    fs::write(
        coverage_out.join("mcp-attack-surface.mdx"),
        coverage::mcp_surface::generate_mcp_surface_coverage(&matrix),
    )
    .map_err(ThoughtJackError::Io)?;

    Ok(())
}

/// Generate and write sidebar configuration.
fn write_sidebar(
    reg: &thoughtjack_docs::registry::Registry,
    output: &Path,
) -> Result<(), ThoughtJackError> {
    let sidebars_js = sidebar::generate_sidebars_js(reg);
    fs::write(output.join("sidebars.js"), sidebars_js).map_err(ThoughtJackError::Io)
}
