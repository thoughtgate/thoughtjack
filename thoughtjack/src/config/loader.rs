//! Configuration loader (TJ-SPEC-006)
//!
//! This module implements the configuration loading pipeline:
//! 1. Environment variable expansion (pre-parse, on raw text)
//! 2. YAML parsing
//! 3. `$include` directive resolution
//! 4. `$file` directive resolution
//! 5. `$generate` directive handling (stores factory, not bytes)
//! 6. Deserialization to typed config
//! 7. Validation
//! 8. Freeze with `Arc`

use crate::config::schema::{GeneratorConfig, GeneratorLimits, ServerConfig};
use crate::config::validation::Validator;
use crate::error::ConfigError;

use base64::Engine;
use serde_yaml::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

// ============================================================================
// Public API
// ============================================================================

/// Options for the configuration loader.
///
/// Implements: TJ-SPEC-006 F-001
#[derive(Debug, Clone)]
pub struct LoaderOptions {
    /// Root directory for resolving `$include` paths.
    pub library_root: PathBuf,

    /// Limits for payload generators.
    pub generator_limits: GeneratorLimits,

    /// Limits for configuration size.
    pub config_limits: ConfigLimits,
}

impl Default for LoaderOptions {
    fn default() -> Self {
        Self {
            library_root: PathBuf::from("library"),
            generator_limits: GeneratorLimits::default(),
            config_limits: ConfigLimits::default(),
        }
    }
}

/// Limits for configuration size to prevent resource exhaustion.
///
/// Implements: TJ-SPEC-006 F-007a
#[derive(Debug, Clone)]
pub struct ConfigLimits {
    /// Maximum number of phases.
    pub max_phases: usize,

    /// Maximum number of tools (baseline + added).
    pub max_tools: usize,

    /// Maximum number of resources.
    pub max_resources: usize,

    /// Maximum number of prompts.
    pub max_prompts: usize,

    /// Maximum include nesting depth.
    pub max_include_depth: usize,

    /// Maximum configuration file size in bytes.
    pub max_config_size: usize,
}

impl Default for ConfigLimits {
    fn default() -> Self {
        Self {
            max_phases: env_or("THOUGHTJACK_MAX_PHASES", 100),
            max_tools: env_or("THOUGHTJACK_MAX_TOOLS", 1000),
            max_resources: env_or("THOUGHTJACK_MAX_RESOURCES", 1000),
            max_prompts: env_or("THOUGHTJACK_MAX_PROMPTS", 500),
            max_include_depth: env_or("THOUGHTJACK_MAX_INCLUDE_DEPTH", 10),
            max_config_size: env_or("THOUGHTJACK_MAX_CONFIG_SIZE", 10 * 1024 * 1024),
        }
    }
}

/// Result of loading a configuration file.
///
/// Implements: TJ-SPEC-006 F-008
#[derive(Debug)]
pub struct LoadResult {
    /// The loaded and validated configuration.
    pub config: Arc<ServerConfig>,

    /// Warnings encountered during loading.
    pub warnings: Vec<LoadWarning>,
}

/// Warning during configuration loading.
///
/// Implements: TJ-SPEC-006 F-008
#[derive(Debug, Clone)]
pub struct LoadWarning {
    /// Warning message.
    pub message: String,

    /// Location where the warning occurred.
    pub location: Option<String>,
}

/// Configuration loader.
///
/// Handles the full loading pipeline from YAML file to frozen `ServerConfig`.
///
/// Implements: TJ-SPEC-006 F-001
#[derive(Debug)]
pub struct ConfigLoader {
    options: LoaderOptions,
    include_cache: HashMap<PathBuf, Value>,
    file_cache: HashMap<PathBuf, FileContent>,
}

impl ConfigLoader {
    /// Creates a new configuration loader with the given options.
    ///
    /// Implements: TJ-SPEC-006 F-001
    #[must_use]
    pub fn new(options: LoaderOptions) -> Self {
        Self {
            options,
            include_cache: HashMap::new(),
            file_cache: HashMap::new(),
        }
    }

    /// Creates a new configuration loader with default options.
    ///
    /// Implements: TJ-SPEC-006 F-001
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(LoaderOptions::default())
    }

    /// Loads a configuration file and returns the frozen configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The file cannot be read
    /// - YAML parsing fails
    /// - Directive resolution fails (circular includes, missing files)
    /// - Validation fails
    ///
    /// Implements: TJ-SPEC-006 F-001
    pub fn load(&mut self, path: &Path) -> Result<LoadResult, ConfigError> {
        let mut warnings = Vec::new();

        // Check file size limit
        let metadata = std::fs::metadata(path).map_err(|_| ConfigError::MissingFile {
            path: path.to_path_buf(),
        })?;

        let file_size =
            usize::try_from(metadata.len()).unwrap_or(self.options.config_limits.max_config_size);
        if file_size > self.options.config_limits.max_config_size {
            return Err(ConfigError::InvalidValue {
                field: "file_size".to_string(),
                value: format!("{file_size} bytes"),
                expected: format!(
                    "at most {} bytes",
                    self.options.config_limits.max_config_size
                ),
            });
        }

        // Stage 0: Read raw file content
        let raw_content = std::fs::read_to_string(path).map_err(|_| ConfigError::MissingFile {
            path: path.to_path_buf(),
        })?;

        // Handle UTF-8 BOM
        let raw_content = raw_content.strip_prefix('\u{feff}').unwrap_or(&raw_content);

        // Stage 1: Environment variable substitution (before YAML parsing)
        let mut env_sub = EnvSubstitution::new();
        let substituted = env_sub.substitute(raw_content, path)?;
        warnings.extend(env_sub.warnings);

        // Stage 2: YAML parsing
        let mut root: Value =
            serde_yaml::from_str(&substituted).map_err(|e| ConfigError::ParseError {
                path: path.to_path_buf(),
                line: e.location().map(|l| l.line()),
                message: e.to_string(),
            })?;

        // Check for empty config
        if root.is_null() {
            return Err(ConfigError::ParseError {
                path: path.to_path_buf(),
                line: None,
                message: "Configuration file is empty".to_string(),
            });
        }

        // Stage 3: $include resolution
        let mut include_resolver = IncludeResolver::new(
            self.options.library_root.clone(),
            self.options.config_limits.max_include_depth,
        );
        include_resolver.resolve(&mut root, &mut self.include_cache)?;

        // Stage 4: $file resolution
        let file_resolver = FileResolver::new(self.options.library_root.clone());
        file_resolver.resolve(&mut root, path, &mut self.file_cache)?;

        // Stage 5: $generate validation (stores GeneratorConfig, not bytes)
        // Note: We don't actually generate bytes here, just validate the config
        validate_generators(&root, &self.options.generator_limits)?;

        // Stage 6: Deserialize to typed config
        let config: ServerConfig =
            serde_yaml::from_value(root).map_err(|e| ConfigError::ParseError {
                path: path.to_path_buf(),
                line: None,
                message: format!("Failed to deserialize configuration: {e}"),
            })?;

        // Stage 7: Validation
        let mut validator = Validator::new();
        let validation_result = validator.validate(&config, &self.options.config_limits);

        if validation_result.has_errors() {
            return Err(ConfigError::ValidationError {
                path: path.display().to_string(),
                errors: validation_result.errors,
            });
        }

        // Add validation warnings
        for issue in validation_result.warnings {
            warnings.push(LoadWarning {
                message: issue.message,
                location: Some(issue.path),
            });
        }

        // Stage 8: Freeze
        Ok(LoadResult {
            config: Arc::new(config),
            warnings,
        })
    }
}

/// Validates `$generate` directives without materializing bytes.
///
/// In addition to checking estimated sizes against limits, this function
/// actually creates each generator to catch constructor-level errors
/// (e.g., invalid parameters, seed validation) at config load time.
fn validate_generators(value: &Value, limits: &GeneratorLimits) -> Result<(), ConfigError> {
    match value {
        Value::Mapping(map) => {
            // Check if this is a $generate directive
            if let Some(generate_value) = map.get(Value::String("$generate".to_string())) {
                // Parse the generator config to validate it
                let config: GeneratorConfig = serde_yaml::from_value(generate_value.clone())
                    .map_err(|e| ConfigError::InvalidValue {
                        field: "$generate".to_string(),
                        value: format!("{generate_value:?}"),
                        expected: format!("valid generator config: {e}"),
                    })?;

                // Check estimated size against limits
                let estimated_size = estimate_generator_size(&config);
                if estimated_size > limits.max_payload_bytes {
                    return Err(ConfigError::InvalidValue {
                        field: "$generate".to_string(),
                        value: format!("{estimated_size} bytes (estimated)"),
                        expected: format!("at most {} bytes", limits.max_payload_bytes),
                    });
                }

                // Actually create the generator to catch constructor errors
                crate::generator::create_generator(&config, limits).map_err(|e| {
                    ConfigError::InvalidValue {
                        field: "$generate".to_string(),
                        value: format!("{:?}", config.type_),
                        expected: format!("valid generator: {e}"),
                    }
                })?;
            } else {
                // Recurse into map values
                for (_, v) in map {
                    validate_generators(v, limits)?;
                }
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                validate_generators(item, limits)?;
            }
        }
        _ => {}
    }
    Ok(())
}

// ============================================================================
// Environment Variable Substitution
// ============================================================================

/// Pre-parse environment variable substitution.
///
/// Runs on raw YAML text BEFORE parsing to preserve type inference.
struct EnvSubstitution {
    warnings: Vec<LoadWarning>,
}

impl EnvSubstitution {
    const fn new() -> Self {
        Self {
            warnings: Vec::new(),
        }
    }

    /// Substitutes environment variables in raw YAML text.
    ///
    /// Supports:
    /// - `${VAR}` - expand to value (empty string if unset with warning)
    /// - `${VAR:-default}` - expand to default if unset
    /// - `${VAR:?message}` - fail if unset
    /// - `$$` - literal `$`
    fn substitute(&mut self, raw_yaml: &str, source_path: &Path) -> Result<String, ConfigError> {
        let mut result = String::with_capacity(raw_yaml.len());
        let mut chars = raw_yaml.chars().peekable();
        let mut position = 0usize;

        while let Some(c) = chars.next() {
            position += 1;
            if c == '$' {
                match chars.peek() {
                    Some('$') => {
                        // Escaped $$ -> literal $
                        chars.next();
                        position += 1;
                        result.push('$');
                    }
                    Some('{') => {
                        chars.next();
                        position += 1;
                        let (var_name, default, error_msg) =
                            Self::parse_var_spec(&mut chars, &mut position)?;

                        match std::env::var(&var_name) {
                            Ok(value) => result.push_str(&value),
                            Err(_) => {
                                if let Some(default_val) = default {
                                    result.push_str(&default_val);
                                } else if let Some(msg) = error_msg {
                                    return Err(ConfigError::EnvVarNotSet {
                                        var: var_name,
                                        location: msg,
                                    });
                                } else {
                                    // Missing var without default -> empty string with warning
                                    self.warnings.push(LoadWarning {
                                        message: format!(
                                            "Environment variable '{var_name}' is not set, using empty string"
                                        ),
                                        location: Some(source_path.display().to_string()),
                                    });
                                }
                            }
                        }
                    }
                    _ => result.push(c),
                }
            } else {
                result.push(c);
            }
        }

        Ok(result)
    }

    /// Parses a variable specification from `${...}`.
    ///
    /// Returns (`var_name`, `default_value`, `error_message`).
    fn parse_var_spec(
        chars: &mut std::iter::Peekable<std::str::Chars>,
        position: &mut usize,
    ) -> Result<(String, Option<String>, Option<String>), ConfigError> {
        let mut var_name = String::new();

        // Read variable name
        while let Some(&c) = chars.peek() {
            match c {
                '}' => {
                    chars.next();
                    *position += 1;
                    return Ok((var_name, None, None));
                }
                ':' => {
                    chars.next();
                    *position += 1;
                    match chars.peek() {
                        Some('-') => {
                            chars.next();
                            *position += 1;
                            let default = Self::read_until_close(chars, position)?;
                            return Ok((var_name, Some(default), None));
                        }
                        Some('?') => {
                            chars.next();
                            *position += 1;
                            let msg = Self::read_until_close(chars, position)?;
                            return Ok((var_name, None, Some(msg)));
                        }
                        _ => var_name.push(':'),
                    }
                }
                _ => {
                    chars.next();
                    *position += 1;
                    var_name.push(c);
                }
            }
        }

        Err(ConfigError::ParseError {
            path: PathBuf::new(),
            line: None,
            message: format!("Unclosed environment variable reference: ${{{var_name}"),
        })
    }

    /// Reads content until closing `}`, handling nested braces.
    fn read_until_close(
        chars: &mut std::iter::Peekable<std::str::Chars>,
        position: &mut usize,
    ) -> Result<String, ConfigError> {
        let mut value = String::new();
        let mut depth = 1;

        for c in chars.by_ref() {
            *position += 1;
            match c {
                '{' => {
                    depth += 1;
                    value.push(c);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(value);
                    }
                    value.push(c);
                }
                _ => value.push(c),
            }
        }

        Err(ConfigError::ParseError {
            path: PathBuf::new(),
            line: None,
            message: "Unclosed environment variable reference".to_string(),
        })
    }
}

// ============================================================================
// Include Resolution
// ============================================================================

/// Resolves `$include` directives in YAML values.
struct IncludeResolver {
    library_root: PathBuf,
    max_depth: usize,
    resolution_stack: Vec<PathBuf>,
}

impl IncludeResolver {
    const fn new(library_root: PathBuf, max_depth: usize) -> Self {
        Self {
            library_root,
            max_depth,
            resolution_stack: Vec::new(),
        }
    }

    /// Resolves all `$include` directives in the given value.
    fn resolve(
        &mut self,
        value: &mut Value,
        cache: &mut HashMap<PathBuf, Value>,
    ) -> Result<(), ConfigError> {
        match value {
            Value::Mapping(map) => {
                // Check for $include directive
                let include_key = Value::String("$include".to_string());
                if let Some(include_path_value) = map.get(&include_key).cloned() {
                    let path = self.resolve_path(&include_path_value)?;

                    // Cycle detection
                    if self.resolution_stack.contains(&path) {
                        let mut cycle = self.resolution_stack.clone();
                        cycle.push(path.clone());
                        return Err(ConfigError::CircularInclude { cycle });
                    }

                    // Depth check
                    if self.resolution_stack.len() >= self.max_depth {
                        return Err(ConfigError::InvalidValue {
                            field: "$include depth".to_string(),
                            value: format!("{}", self.resolution_stack.len() + 1),
                            expected: format!("at most {} levels", self.max_depth),
                        });
                    }

                    // Load included file (with caching)
                    let included = Self::load_cached(&path, cache)?;

                    // Get override if present
                    let override_key = Value::String("override".to_string());
                    let override_value = map.get(&override_key).cloned();

                    // Replace current value with included content
                    *value = included;

                    // Apply override if present (deep merge)
                    if let Some(overrides) = override_value {
                        deep_merge(value, &overrides);
                    }

                    // Recursively resolve includes in the loaded content
                    self.resolution_stack.push(path.clone());
                    self.resolve(value, cache)?;
                    self.resolution_stack.pop();
                } else {
                    // Recurse into map values
                    let keys: Vec<Value> = map.keys().cloned().collect();
                    for key in keys {
                        if let Some(v) = map.get_mut(&key) {
                            self.resolve(v, cache)?;
                        }
                    }
                }
            }
            Value::Sequence(seq) => {
                for item in seq {
                    self.resolve(item, cache)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Resolves a path relative to the library root.
    fn resolve_path(&self, path_value: &Value) -> Result<PathBuf, ConfigError> {
        let path_str = path_value
            .as_str()
            .ok_or_else(|| ConfigError::InvalidValue {
                field: "$include".to_string(),
                value: format!("{path_value:?}"),
                expected: "string path".to_string(),
            })?;

        let path = Path::new(path_str);

        // Reject path traversal attempts (string-level check)
        if path_str.contains("..") {
            return Err(ConfigError::InvalidValue {
                field: "$include".to_string(),
                value: path_str.to_string(),
                expected: "path without '..' traversal".to_string(),
            });
        }

        if path.is_absolute() {
            if path.exists() {
                return Ok(path.to_path_buf());
            }
            return Err(ConfigError::MissingFile {
                path: path.to_path_buf(),
            });
        }

        // $include paths are relative to library root
        let resolved = self.library_root.join(path);

        if !resolved.exists() {
            return Err(ConfigError::MissingFile { path: resolved });
        }

        // Canonicalize and verify the path stays within the library root
        verify_within_base(&resolved, &self.library_root, "$include")?;

        Ok(resolved)
    }

    /// Loads a file with caching.
    fn load_cached(path: &Path, cache: &mut HashMap<PathBuf, Value>) -> Result<Value, ConfigError> {
        if let Some(cached) = cache.get(path) {
            return Ok(cached.clone());
        }

        let content = std::fs::read_to_string(path).map_err(|_| ConfigError::MissingFile {
            path: path.to_path_buf(),
        })?;

        // Handle UTF-8 BOM
        let content = content.strip_prefix('\u{feff}').unwrap_or(&content);

        // Environment substitution on included file
        let mut env_sub = EnvSubstitution::new();
        let substituted = env_sub.substitute(content, path)?;

        let value: Value =
            serde_yaml::from_str(&substituted).map_err(|e| ConfigError::ParseError {
                path: path.to_path_buf(),
                line: e.location().map(|l| l.line()),
                message: e.to_string(),
            })?;

        cache.insert(path.to_path_buf(), value.clone());
        Ok(value)
    }
}

// ============================================================================
// File Resolution
// ============================================================================

/// Content loaded from a file.
#[derive(Debug, Clone)]
enum FileContent {
    Json(serde_json::Value),
    Text(String),
    Binary(Vec<u8>),
}

/// Resolves `$file` directives in YAML values.
struct FileResolver {
    library_root: PathBuf,
}

impl FileResolver {
    const fn new(library_root: PathBuf) -> Self {
        Self { library_root }
    }

    /// Resolves all `$file` directives in the given value.
    fn resolve(
        &self,
        value: &mut Value,
        current_file: &Path,
        cache: &mut HashMap<PathBuf, FileContent>,
    ) -> Result<(), ConfigError> {
        match value {
            Value::Mapping(map) => {
                let file_key = Value::String("$file".to_string());
                if let Some(file_path_value) = map.get(&file_key).cloned() {
                    let path = self.resolve_path(&file_path_value, current_file)?;
                    let content = Self::load_file(&path, cache)?;

                    *value = match &content {
                        FileContent::Json(v) => json_to_yaml(v),
                        FileContent::Text(s) => Value::String(s.clone()),
                        FileContent::Binary(b) => {
                            Value::String(base64::engine::general_purpose::STANDARD.encode(b))
                        }
                    };
                } else {
                    // Recurse into map values
                    let keys: Vec<Value> = map.keys().cloned().collect();
                    for key in keys {
                        if let Some(v) = map.get_mut(&key) {
                            self.resolve(v, current_file, cache)?;
                        }
                    }
                }
            }
            Value::Sequence(seq) => {
                for item in seq {
                    self.resolve(item, current_file, cache)?;
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Resolves a path relative to the current file or library root.
    fn resolve_path(
        &self,
        path_value: &Value,
        current_file: &Path,
    ) -> Result<PathBuf, ConfigError> {
        let path_str = path_value
            .as_str()
            .ok_or_else(|| ConfigError::InvalidValue {
                field: "$file".to_string(),
                value: format!("{path_value:?}"),
                expected: "string path".to_string(),
            })?;

        let path = Path::new(path_str);

        // Reject path traversal attempts (string-level check)
        if path_str.contains("..") {
            return Err(ConfigError::InvalidValue {
                field: "$file".to_string(),
                value: path_str.to_string(),
                expected: "path without '..' traversal".to_string(),
            });
        }

        if path.is_absolute() {
            if path.exists() {
                return Ok(path.to_path_buf());
            }
            return Err(ConfigError::MissingFile {
                path: path.to_path_buf(),
            });
        }

        // $file paths are relative to current file's directory first
        let base_dir = current_file.parent().unwrap_or_else(|| Path::new("."));
        let resolved = base_dir.join(path);

        if resolved.exists() {
            // Verify resolved path stays within either base_dir or library_root
            verify_within_base(&resolved, base_dir, "$file")?;
            return Ok(resolved);
        }

        // Fall back to library root
        let resolved = self.library_root.join(path);
        if resolved.exists() {
            verify_within_base(&resolved, &self.library_root, "$file")?;
            return Ok(resolved);
        }

        Err(ConfigError::MissingFile {
            path: PathBuf::from(path_str),
        })
    }

    /// Loads a file with caching.
    fn load_file(
        path: &Path,
        cache: &mut HashMap<PathBuf, FileContent>,
    ) -> Result<FileContent, ConfigError> {
        if let Some(cached) = cache.get(path) {
            return Ok(cached.clone());
        }

        let content = match path.extension().and_then(|e| e.to_str()) {
            Some("json") => {
                let text = std::fs::read_to_string(path).map_err(|_| ConfigError::MissingFile {
                    path: path.to_path_buf(),
                })?;
                let json: serde_json::Value =
                    serde_json::from_str(&text).map_err(|e| ConfigError::ParseError {
                        path: path.to_path_buf(),
                        line: None,
                        message: format!("Invalid JSON: {e}"),
                    })?;
                FileContent::Json(json)
            }
            Some("yaml" | "yml") => {
                let text = std::fs::read_to_string(path).map_err(|_| ConfigError::MissingFile {
                    path: path.to_path_buf(),
                })?;
                let yaml: serde_yaml::Value =
                    serde_yaml::from_str(&text).map_err(|e| ConfigError::ParseError {
                        path: path.to_path_buf(),
                        line: e.location().map(|l| l.line()),
                        message: e.to_string(),
                    })?;
                FileContent::Json(yaml_to_json(&yaml))
            }
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" | "bmp") => {
                let bytes = std::fs::read(path).map_err(|_| ConfigError::MissingFile {
                    path: path.to_path_buf(),
                })?;
                FileContent::Binary(bytes)
            }
            _ => {
                let text = std::fs::read_to_string(path).map_err(|_| ConfigError::MissingFile {
                    path: path.to_path_buf(),
                })?;
                FileContent::Text(text)
            }
        };

        cache.insert(path.to_path_buf(), content.clone());
        Ok(content)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Verifies that a resolved path stays within the given base directory.
///
/// Canonicalizes both paths and checks that the resolved path is a
/// descendant of the base. This prevents symlink-based path traversal
/// that would bypass the string-level `..` check.
fn verify_within_base(resolved: &Path, base: &Path, directive: &str) -> Result<(), ConfigError> {
    let canonical = resolved
        .canonicalize()
        .map_err(|_| ConfigError::MissingFile {
            path: resolved.to_path_buf(),
        })?;
    let canonical_base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());

    if !canonical.starts_with(&canonical_base) {
        return Err(ConfigError::InvalidValue {
            field: directive.to_string(),
            value: resolved.display().to_string(),
            expected: format!("path within {}", canonical_base.display()),
        });
    }

    Ok(())
}

/// Parses an environment variable with a default value.
fn env_or<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Deep merges override into base.
///
/// For mappings: recursively merge keys.
/// For other types: override replaces base.
fn deep_merge(base: &mut Value, override_val: &Value) {
    match (base, override_val) {
        (Value::Mapping(base_map), Value::Mapping(override_map)) => {
            for (key, override_value) in override_map {
                if let Some(base_value) = base_map.get_mut(key) {
                    deep_merge(base_value, override_value);
                } else {
                    base_map.insert(key.clone(), override_value.clone());
                }
            }
        }
        (base, override_val) => {
            *base = override_val.clone();
        }
    }
}

/// Converts a `serde_yaml::Value` to `serde_json::Value`.
#[allow(clippy::option_if_let_else)]
fn yaml_to_json(yaml: &serde_yaml::Value) -> serde_json::Value {
    match yaml {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f)
                    .map_or(serde_json::Value::Null, serde_json::Value::Number)
            } else {
                serde_json::Value::Null
            }
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Sequence(seq) => serde_json::Value::Array(seq.iter().map(yaml_to_json).collect()),
        Value::Mapping(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .filter_map(|(k, v)| k.as_str().map(|ks| (ks.to_string(), yaml_to_json(v))))
                .collect();
            serde_json::Value::Object(obj)
        }
        Value::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

/// Converts a `serde_json::Value` to `serde_yaml::Value`.
#[allow(clippy::option_if_let_else)]
fn json_to_yaml(json: &serde_json::Value) -> serde_yaml::Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                Value::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                Value::Number(serde_yaml::Number::from(f))
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => Value::Sequence(arr.iter().map(json_to_yaml).collect()),
        serde_json::Value::Object(obj) => {
            let map: serde_yaml::Mapping = obj
                .iter()
                .map(|(k, v)| (Value::String(k.clone()), json_to_yaml(v)))
                .collect();
            Value::Mapping(map)
        }
    }
}

/// Estimates the size of generated content without materializing it.
#[allow(clippy::cast_possible_truncation)]
fn estimate_generator_size(config: &GeneratorConfig) -> usize {
    use crate::config::schema::GeneratorType;

    match config.type_ {
        GeneratorType::NestedJson => {
            // Each level adds ~10-20 bytes for {"key": } or [ ]
            let depth = config
                .params
                .get("depth")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(100) as usize;
            depth * 15
        }
        GeneratorType::Garbage | GeneratorType::UnicodeSpam => config
            .params
            .get("bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1000)
            as usize,
        GeneratorType::BatchNotifications => {
            let count = config
                .params
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10) as usize;
            // Each notification is ~100 bytes
            count * 100
        }
        GeneratorType::RepeatedKeys => {
            let count = config
                .params
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(100) as usize;
            let key_length = config
                .params
                .get("key_length")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10) as usize;
            // Each key-value pair: "key": "value",
            count * (key_length + 20)
        }
        GeneratorType::AnsiEscape => {
            let count = config
                .params
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10) as usize;
            // Each ANSI sequence is ~10-50 bytes
            count * 30
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_substitution_simple() {
        // Use PATH which is always set on Unix/Windows
        let mut sub = EnvSubstitution::new();
        let result = sub
            .substitute("path: ${PATH}", Path::new("test.yaml"))
            .unwrap();
        // PATH should be expanded (not empty, not the literal ${PATH})
        assert!(!result.contains("${PATH}"));
        assert!(result.starts_with("path: "));
        assert!(result.len() > "path: ".len());
    }

    #[test]
    fn test_env_substitution_default() {
        // Use a unique variable name that won't be set
        let mut sub = EnvSubstitution::new();
        let result = sub
            .substitute(
                "value: ${THOUGHTJACK_TEST_NONEXISTENT_VAR_XYZ123:-default}",
                Path::new("test.yaml"),
            )
            .unwrap();
        assert_eq!(result, "value: default");
    }

    #[test]
    fn test_env_substitution_required_missing() {
        // Use a unique variable name that won't be set
        let mut sub = EnvSubstitution::new();
        let result = sub.substitute(
            "value: ${THOUGHTJACK_TEST_REQUIRED_XYZ123:?must be set}",
            Path::new("test.yaml"),
        );
        assert!(result.is_err());
        match result {
            Err(ConfigError::EnvVarNotSet { var, .. }) => {
                assert_eq!(var, "THOUGHTJACK_TEST_REQUIRED_XYZ123");
            }
            _ => panic!("Expected EnvVarNotSet error"),
        }
    }

    #[test]
    fn test_env_substitution_escaped_dollar() {
        let mut sub = EnvSubstitution::new();
        let result = sub
            .substitute("price: $$100", Path::new("test.yaml"))
            .unwrap();
        assert_eq!(result, "price: $100");
    }

    #[test]
    fn test_env_substitution_missing_warning() {
        // Use a unique variable name that won't be set
        let mut sub = EnvSubstitution::new();
        let result = sub
            .substitute(
                "value: ${THOUGHTJACK_TEST_WARN_XYZ123}",
                Path::new("test.yaml"),
            )
            .unwrap();
        assert_eq!(result, "value: ");
        assert_eq!(sub.warnings.len(), 1);
        assert!(
            sub.warnings[0]
                .message
                .contains("THOUGHTJACK_TEST_WARN_XYZ123")
        );
    }

    #[test]
    fn test_deep_merge_simple() {
        let mut base = serde_yaml::from_str::<Value>("a: 1\nb: 2").unwrap();
        let override_val = serde_yaml::from_str::<Value>("b: 3\nc: 4").unwrap();
        deep_merge(&mut base, &override_val);

        let result = base.as_mapping().unwrap();
        assert_eq!(
            result.get(Value::String("a".to_string())).unwrap(),
            &Value::Number(1.into())
        );
        assert_eq!(
            result.get(Value::String("b".to_string())).unwrap(),
            &Value::Number(3.into())
        );
        assert_eq!(
            result.get(Value::String("c".to_string())).unwrap(),
            &Value::Number(4.into())
        );
    }

    #[test]
    fn test_deep_merge_nested() {
        let mut base = serde_yaml::from_str::<Value>(
            r"
            outer:
              inner1: a
              inner2: b
            ",
        )
        .unwrap();
        let override_val = serde_yaml::from_str::<Value>(
            r"
            outer:
              inner2: c
              inner3: d
            ",
        )
        .unwrap();
        deep_merge(&mut base, &override_val);

        let outer = base
            .as_mapping()
            .unwrap()
            .get(Value::String("outer".to_string()))
            .unwrap()
            .as_mapping()
            .unwrap();

        assert_eq!(
            outer.get(Value::String("inner1".to_string())).unwrap(),
            &Value::String("a".to_string())
        );
        assert_eq!(
            outer.get(Value::String("inner2".to_string())).unwrap(),
            &Value::String("c".to_string())
        );
        assert_eq!(
            outer.get(Value::String("inner3".to_string())).unwrap(),
            &Value::String("d".to_string())
        );
    }

    #[test]
    fn test_yaml_to_json_conversion() {
        let yaml: Value = serde_yaml::from_str(
            r"
            string: hello
            number: 42
            float: 3.14
            bool: true
            null_val: null
            array:
              - 1
              - 2
            object:
              nested: value
            ",
        )
        .unwrap();

        let json = yaml_to_json(&yaml);

        assert_eq!(json["string"], "hello");
        assert_eq!(json["number"], 42);
        assert!((json["float"].as_f64().unwrap() - 3.14).abs() < 0.001);
        assert_eq!(json["bool"], true);
        assert!(json["null_val"].is_null());
        assert_eq!(json["array"][0], 1);
        assert_eq!(json["object"]["nested"], "value");
    }

    #[test]
    fn test_json_to_yaml_conversion() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "string": "hello",
                "number": 42,
                "bool": true,
                "array": [1, 2]
            }"#,
        )
        .unwrap();

        let yaml = json_to_yaml(&json);
        let map = yaml.as_mapping().unwrap();

        assert_eq!(
            map.get(Value::String("string".to_string())).unwrap(),
            &Value::String("hello".to_string())
        );
        assert_eq!(
            map.get(Value::String("number".to_string())).unwrap(),
            &Value::Number(42.into())
        );
        assert_eq!(
            map.get(Value::String("bool".to_string())).unwrap(),
            &Value::Bool(true)
        );
    }

    #[test]
    fn test_estimate_generator_size_garbage() {
        let config = GeneratorConfig {
            type_: crate::config::schema::GeneratorType::Garbage,
            params: [("bytes".to_string(), serde_json::json!(1000))]
                .into_iter()
                .collect(),
        };
        assert_eq!(estimate_generator_size(&config), 1000);
    }

    #[test]
    fn test_estimate_generator_size_nested_json() {
        let config = GeneratorConfig {
            type_: crate::config::schema::GeneratorType::NestedJson,
            params: [("depth".to_string(), serde_json::json!(100))]
                .into_iter()
                .collect(),
        };
        // 100 * 15 = 1500
        assert_eq!(estimate_generator_size(&config), 1500);
    }

    #[test]
    fn test_config_limits_default() {
        let limits = ConfigLimits::default();
        assert_eq!(limits.max_phases, 100);
        assert_eq!(limits.max_tools, 1000);
        assert_eq!(limits.max_include_depth, 10);
    }

    #[test]
    fn test_loader_options_default() {
        let opts = LoaderOptions::default();
        assert_eq!(opts.library_root, PathBuf::from("library"));
    }

    #[test]
    fn test_file_resolver_rejects_path_traversal() {
        let resolver = FileResolver::new(PathBuf::from("/fake/library"));
        let traversal = Value::String("../../../etc/passwd".to_string());
        let result = resolver.resolve_path(&traversal, Path::new("/fake/config.yaml"));

        assert!(result.is_err());
        match result {
            Err(ConfigError::InvalidValue {
                field, expected, ..
            }) => {
                assert_eq!(field, "$file");
                assert!(expected.contains(".."));
            }
            _ => panic!("Expected InvalidValue error for path traversal"),
        }
    }
}
