//! Build script for capturing build metadata and discovering OATF scenarios.

use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

fn main() {
    built::write_built_file().expect("Failed to acquire build-time information");
    discover_scenarios();
}

// ============================================================================
// OATF scenario discovery (TJ-SPEC-010)
// ============================================================================

/// Metadata extracted from a valid OATF document at build time.
struct ScenarioEntry {
    name: String,
    description: String,
    /// Rust expression for the category variant, e.g. `ScenarioCategory::Injection`.
    category: &'static str,
    tags: Vec<String>,
    /// Absolute path to the YAML file (for `include_str!`).
    yaml_path: String,
}

/// Walk `scenarios/library/`, validate each YAML file via `oatf::load()`,
/// and generate `$OUT_DIR/builtin_scenarios.rs` with only the valid ones.
fn discover_scenarios() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let library_dir = Path::new(&manifest_dir).join("scenarios").join("library");

    println!("cargo:rerun-if-changed=scenarios/library");

    let mut scenarios = Vec::new();

    if library_dir.exists() {
        walk_yaml_files(&library_dir, &mut scenarios);
    } else {
        println!(
            "cargo:warning=scenarios/library/ not found — no built-in scenarios will be \
             embedded. Run `git submodule update --init scenarios` to fetch the OATF scenario library."
        );
    }

    // Deterministic ordering by scenario name.
    scenarios.sort_by(|a, b| a.name.cmp(&b.name));

    let output_path = Path::new(&out_dir).join("builtin_scenarios.rs");
    let mut f = std::fs::File::create(&output_path).unwrap();
    write_registry(&mut f, &scenarios);
}

fn walk_yaml_files(dir: &Path, scenarios: &mut Vec<ScenarioEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_yaml_files(&path, scenarios);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "yaml" || ext == "yml")
        {
            match process_scenario(&path) {
                Ok(entry) => scenarios.push(entry),
                Err(err) => {
                    println!("cargo:warning=Skipping {}: {err}", path.display());
                }
            }
        }
    }
}

fn process_scenario(path: &Path) -> Result<ScenarioEntry, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    let load_result = oatf::load(&content).map_err(|errors| {
        errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    })?;

    let attack = &load_result.document.attack;

    let name = attack
        .id
        .as_deref()
        .ok_or("missing attack.id")?
        .to_lowercase();

    let description = attack
        .name
        .as_deref()
        .or(attack.description.as_deref())
        .unwrap_or("No description")
        .to_string();

    let category = attack
        .classification
        .as_ref()
        .and_then(|c| c.category.as_ref())
        .map_or("ScenarioCategory::Protocol", map_category);

    let tags = attack
        .classification
        .as_ref()
        .and_then(|c| c.tags.as_ref())
        .cloned()
        .unwrap_or_default();

    let yaml_path = path
        .canonicalize()
        .map_err(|e| format!("canonicalize error: {e}"))?
        .to_str()
        .ok_or("non-UTF-8 path")?
        .to_string();

    Ok(ScenarioEntry {
        name,
        description,
        category,
        tags,
        yaml_path,
    })
}

const fn map_category(cat: &oatf::enums::Category) -> &'static str {
    match cat {
        oatf::enums::Category::CapabilityPoisoning
        | oatf::enums::Category::ResponseFabrication
        | oatf::enums::Category::ContextManipulation => "ScenarioCategory::Injection",
        oatf::enums::Category::TemporalManipulation => "ScenarioCategory::Temporal",
        oatf::enums::Category::AvailabilityDisruption => "ScenarioCategory::DoS",
        oatf::enums::Category::OversightBypass => "ScenarioCategory::Protocol",
        oatf::enums::Category::CrossProtocolChain => "ScenarioCategory::MultiVector",
    }
}

fn write_registry(f: &mut std::fs::File, scenarios: &[ScenarioEntry]) {
    writeln!(f, "&[").unwrap();
    for s in scenarios {
        writeln!(f, "    BuiltinScenario {{").unwrap();
        writeln!(f, "        name: {:?},", s.name).unwrap();
        writeln!(f, "        description: {:?},", s.description).unwrap();
        writeln!(f, "        category: {},", s.category).unwrap();

        // Tags as a static slice of &str.
        let mut tags_literal = String::from("&[");
        for (i, tag) in s.tags.iter().enumerate() {
            if i > 0 {
                tags_literal.push_str(", ");
            }
            let _ = write!(tags_literal, "{tag:?}");
        }
        tags_literal.push(']');
        writeln!(f, "        tags: {tags_literal},").unwrap();

        writeln!(f, "        yaml: include_str!({:?}),", s.yaml_path).unwrap();
        writeln!(f, "    }},").unwrap();
    }
    writeln!(f, "]").unwrap();
}
