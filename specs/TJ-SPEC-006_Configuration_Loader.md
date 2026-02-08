# TJ-SPEC-006: Configuration Loader

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-006` |
| **Title** | Configuration Loader |
| **Type** | Core Specification |
| **Status** | Draft |
| **Priority** | **High** |
| **Version** | v1.0.0 |
| **Tags** | `#config` `#yaml` `#include` `#validation` `#parsing` `#directives` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's configuration loader — the system that parses YAML configuration files, resolves directives (`$include`, `$file`, `$generate`, `${ENV}`), validates the result, and produces runtime-ready server configurations.

### 1.1 Motivation

ThoughtJack configurations use several special directives that must be processed before the server can run:

| Directive | Purpose | Example |
|-----------|---------|---------|
| `$include` | Compose from reusable patterns | `$include: tools/calculator/benign.yaml` |
| `$file` | Load external files (schemas, images) | `$file: schemas/tool.json` |
| `$generate` | Generate payloads at load time | `$generate: { type: nested_json, depth: 1000 }` |
| `${ENV}` | Substitute environment variables | `text: "Host: ${TARGET_HOST}"` |

The configuration loader handles all directive resolution, producing a fully-expanded, validated configuration that the runtime components (Phase Engine, Transport, Behaviors) can consume directly.

### 1.2 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Parse → Resolve → Validate → Freeze** | Clear pipeline stages, errors caught early |
| **Fail fast with context** | Report all errors with file/line context |
| **Lazy include resolution** | Only load referenced files |
| **Cycle detection** | Prevent infinite include loops |
| **Immutable output** | Loaded config is frozen, no runtime mutation |

### 1.3 Loading Pipeline

```
┌─────────────────────────────────────────────────────────────────┐
│                    Configuration Loading Pipeline                │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  0. PRE-PROCESS     1. PARSE           2. RESOLVE              │
│  ───────────────    ───────────────    ───────────────         │
│  Text-level         YAML → AST         $include                 │
│  ${ENV} expansion   Syntax errors      $file                    │
│  (before parsing)                      $generate                │
│                                                                 │
│        ↓                  ↓                    ↓                │
│   SubstitutedText    RawConfig         ResolvedConfig           │
│                                                                 │
│                      3. VALIDATE       4. FREEZE                │
│                      ───────────────   ───────────────          │
│                      Schema checks     Arc<ServerConfig>        │
│                      Semantic checks                            │
│                      Cross-references                           │
│                      Limit checks                               │
│                                                                 │
│                            ↓                                    │
│                      ValidatedConfig                            │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Key: Environment variables are expanded BEFORE YAML parsing to ensure correct type inference.**

### 1.4 Scope Boundaries

**In scope:**
- YAML parsing with error recovery
- Directive resolution (`$include`, `$file`, `$generate`, `${ENV}`)
- Include cycle detection
- Schema validation
- Semantic validation (cross-references, phase consistency)
- Error collection and reporting
- Configuration freezing for runtime

**Out of scope:**
- Runtime configuration changes (config is immutable)
- Configuration schema definition (TJ-SPEC-001)
- Payload generation implementation (TJ-SPEC-005)

---

## 2. Functional Requirements

### F-001: YAML Parsing

The system SHALL parse YAML configuration files with detailed error reporting.

**Acceptance Criteria:**
- Parse YAML 1.2 syntax
- Preserve source location (file, line, column) for error reporting
- Handle UTF-8 encoded files
- Detect duplicate keys and emit warning (last value wins per YAML spec)
- Support multi-document YAML (first document only)

**Duplicate Key Handling:**

Standard YAML parsers (`serde_yaml`) silently overwrite duplicate keys. The current implementation relies on `serde_yaml`'s default behavior (last value wins, YAML 1.2 compliant). Duplicate key detection and warnings are not yet implemented.

**Implementation:**

Error reporting uses the file path and `serde_yaml` error positions (line/column) rather than a custom source map. The `serde_yaml` parser provides location information on parse errors via `e.location()`.

```rust
impl ConfigLoader {
    pub fn load(&mut self, path: &Path) -> Result<LoadResult, ConfigError> {
        let raw_content = std::fs::read_to_string(path)
            .map_err(|_| ConfigError::MissingFile { path: path.to_path_buf() })?;

        // Handle UTF-8 BOM
        let raw_content = raw_content.strip_prefix('\u{feff}').unwrap_or(&raw_content);

        // Stage 1: Environment variable substitution (before YAML parsing)
        let mut env_sub = EnvSubstitution::new();
        let substituted = env_sub.substitute(raw_content, path)?;

        // Stage 2: YAML parsing
        let root: serde_yaml::Value = serde_yaml::from_str(&substituted)
            .map_err(|e| ConfigError::ParseError {
                path: path.to_path_buf(),
                line: e.location().map(|l| l.line()),
                message: e.to_string(),
            })?;

        // ... directive resolution, validation, freeze ...
    }
}
```

### F-002: Include Resolution

The system SHALL resolve `$include` directives by loading and merging referenced files.

**Acceptance Criteria:**
- `$include: path` loads YAML file relative to library root
- `override:` block deep-merges with included content
- Circular includes detected and rejected
- Missing includes produce clear error with path
- Include resolution is cached (same file loaded once)
- Environment variable expansion applies to included files (B44)

**Environment Variable Expansion in Includes (B44):**

When an included file is loaded, environment variable expansion (`${VAR}`, `${VAR:-default}`, `${VAR:?msg}`) is applied to the included file's content before YAML parsing. This ensures that included files can reference environment variables just like the root configuration file. The expansion occurs:

1. On the raw text of the included file (before YAML parsing)
2. Before the included content is merged with the parent configuration
3. Recursively for nested includes

This allows library patterns to use environment variables for dynamic behavior:

```yaml
# library/tools/api_client.yaml
tool:
  name: "api_client"
  description: "Calls ${API_HOST:-api.example.com}"
  # ${API_HOST} is expanded when this file is included
```

**Syntax:**
```yaml
# Simple include
tools:
  - $include: tools/calculator/benign.yaml

# Include with override
tools:
  - $include: tools/calculator/benign.yaml
    override:
      tool:
        name: "calc_typo"  # Override the name
```

**Implementation:**
```rust
pub struct IncludeResolver {
    library_root: PathBuf,
    cache: HashMap<PathBuf, Arc<serde_yaml::Value>>,
    resolution_stack: Vec<PathBuf>,  // For cycle detection
}

impl IncludeResolver {
    pub fn resolve(
        &mut self,
        value: &mut serde_yaml::Value,
        current_file: &Path,
    ) -> Result<(), ResolveError> {
        match value {
            serde_yaml::Value::Mapping(map) => {
                // Check for $include directive
                if let Some(include_path) = map.get(&serde_yaml::Value::String("$include".into())) {
                    let path = self.resolve_path(include_path, current_file)?;
                    
                    // Cycle detection
                    if self.resolution_stack.contains(&path) {
                        let cycle = self.format_cycle(&path);
                        return Err(ResolveError::CircularInclude { cycle });
                    }
                    
                    // Load included file
                    let included = self.load_cached(&path)?;
                    
                    // Apply override if present
                    let override_value = map.remove(&serde_yaml::Value::String("override".into()));
                    
                    // Replace current value with included content
                    *value = (*included).clone();
                    
                    // Deep merge override
                    if let Some(overrides) = override_value {
                        self.deep_merge(value, &overrides)?;
                    }
                    
                    // Recursively resolve includes in the loaded content
                    self.resolution_stack.push(path.clone());
                    self.resolve(value, &path)?;
                    self.resolution_stack.pop();
                } else {
                    // Recurse into map values
                    for (_, v) in map.iter_mut() {
                        self.resolve(v, current_file)?;
                    }
                }
            }
            serde_yaml::Value::Sequence(seq) => {
                for item in seq.iter_mut() {
                    self.resolve(item, current_file)?;
                }
            }
            _ => {}
        }
        
        Ok(())
    }
    
    fn deep_merge(
        &self,
        base: &mut serde_yaml::Value,
        override_val: &serde_yaml::Value,
    ) -> Result<(), ResolveError> {
        match (base, override_val) {
            (serde_yaml::Value::Mapping(base_map), serde_yaml::Value::Mapping(override_map)) => {
                for (key, override_value) in override_map {
                    if let Some(base_value) = base_map.get_mut(key) {
                        self.deep_merge(base_value, override_value)?;
                    } else {
                        base_map.insert(key.clone(), override_value.clone());
                    }
                }
            }
            (base, override_val) => {
                *base = override_val.clone();
            }
        }
        Ok(())
    }
}
```

### F-003: File Reference Resolution

The system SHALL resolve `$file` directives by loading external file contents.

**Acceptance Criteria:**
- `$file: path` loads file content
- For JSON files: parse and embed as Value
- For binary files (images): base64 encode
- For text files: embed as string
- File type inferred from extension
- Missing files produce clear error

**Syntax:**
```yaml
# JSON schema from file
inputSchema:
  $file: schemas/calculator.json

# Image from file (base64 encoded)
response:
  content:
    - type: image
      mimeType: image/png
      data:
        $file: assets/chart.png
```

**Implementation:**
```rust
pub struct FileResolver {
    library_root: PathBuf,
    cache: HashMap<PathBuf, Arc<FileContent>>,
}

pub enum FileContent {
    Json(serde_json::Value),
    Text(String),
    Binary(Vec<u8>),
}

impl FileResolver {
    pub fn resolve(
        &mut self,
        value: &mut serde_yaml::Value,
        current_file: &Path,
    ) -> Result<(), ResolveError> {
        match value {
            serde_yaml::Value::Mapping(map) => {
                if let Some(file_path) = map.get(&serde_yaml::Value::String("$file".into())) {
                    let path = self.resolve_path(file_path, current_file)?;
                    let content = self.load_file(&path)?;
                    
                    *value = match content.as_ref() {
                        FileContent::Json(v) => yaml_from_json(v),
                        FileContent::Text(s) => serde_yaml::Value::String(s.clone()),
                        FileContent::Binary(b) => {
                            serde_yaml::Value::String(base64::encode(b))
                        }
                    };
                } else {
                    for (_, v) in map.iter_mut() {
                        self.resolve(v, current_file)?;
                    }
                }
            }
            serde_yaml::Value::Sequence(seq) => {
                for item in seq.iter_mut() {
                    self.resolve(item, current_file)?;
                }
            }
            _ => {}
        }
        
        Ok(())
    }
    
    fn load_file(&mut self, path: &Path) -> Result<Arc<FileContent>, ResolveError> {
        if let Some(cached) = self.cache.get(path) {
            return Ok(cached.clone());
        }
        
        let content = match path.extension().and_then(|e| e.to_str()) {
            Some("json") => {
                let text = std::fs::read_to_string(path)?;
                let json: serde_json::Value = serde_json::from_str(&text)?;
                FileContent::Json(json)
            }
            Some("yaml" | "yml") => {
                let text = std::fs::read_to_string(path)?;
                let yaml: serde_yaml::Value = serde_yaml::from_str(&text)?;
                FileContent::Json(yaml_to_json(&yaml))
            }
            Some("png" | "jpg" | "jpeg" | "gif" | "webp") => {
                let bytes = std::fs::read(path)?;
                FileContent::Binary(bytes)
            }
            _ => {
                let text = std::fs::read_to_string(path)?;
                FileContent::Text(text)
            }
        };
        
        let content = Arc::new(content);
        self.cache.insert(path.to_owned(), content.clone());
        Ok(content)
    }
}
```

### F-004: Generate Directive Resolution

The system SHALL resolve `$generate` directives by creating payload generator instances.

**Acceptance Criteria:**
- `$generate: { type: ..., ... }` validates generator configuration at load time
- Generator configuration (including type and params) stored in config as `GeneratorConfig`
- Actual payload generation deferred to response time
- Generator errors include directive context
- `estimated_size()` called at load time for limit checking

**Syntax:**
```yaml
response:
  content:
    - type: text
      text:
        $generate:
          type: nested_json
          depth: 10000
          structure: object
```

**IMPORTANT:** The configuration stores generator factories, not bytes. This prevents OOM on startup when configs contain large payload definitions like `$generate: { type: garbage, bytes: 1gb }`.

**Resolution Pipeline:**
```
$generate directive
       │
       ▼
┌─────────────────┐
│ Config Loader   │  Creates Generator instance
│ (load time)     │  Validates parameters
│                 │  Checks estimated_size() against limits
│                 │  Does NOT call generate()
└────────┬────────┘
         │
         ▼ GeneratorHandle (lazy reference)
┌─────────────────┐
│ Request Handler │  Calls generator.generate() on demand
│ (response time) │  Streaming generators return iterator
└────────┬────────┘
         │
         ▼ GeneratedPayload::Buffered | ::Streamed
```

**Implementation:**

The loader validates `$generate` directives at load time by parsing the `GeneratorConfig`, checking estimated sizes, and actually creating the generator to catch constructor errors — but does NOT materialize bytes. The `$generate` node remains in the YAML tree as a `GeneratorConfig` value that is deserialized into the typed config.

```rust
/// Validates `$generate` directives without materializing bytes.
///
/// Creates each generator to catch constructor-level errors
/// (e.g., invalid parameters, seed validation) at config load time.
fn validate_generators(
    value: &Value,
    limits: &GeneratorLimits,
    depth: usize,
) -> Result<(), ConfigError> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(ConfigError::InvalidValue { /* ... */ });
    }

    match value {
        Value::Mapping(map) => {
            if let Some(generate_value) = map.get(Value::String("$generate".to_string())) {
                // Parse the generator config to validate it
                let config: GeneratorConfig = serde_yaml::from_value(generate_value.clone())
                    .map_err(|e| ConfigError::InvalidValue { /* ... */ })?;

                // Check estimated size against limits
                let estimated_size = estimate_generator_size(&config);
                if estimated_size > limits.max_payload_bytes {
                    return Err(ConfigError::InvalidValue { /* ... */ });
                }

                // Actually create the generator to catch constructor errors
                create_generator(&config, limits)?;
            } else {
                for (_, v) in map {
                    validate_generators(v, limits, depth + 1)?;
                }
            }
        }
        Value::Sequence(seq) => {
            for item in seq {
                validate_generators(item, limits, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}
```

### F-005: Environment Variable Expansion

The system SHALL expand `${VAR}` patterns via text-level substitution BEFORE YAML parsing.

**Acceptance Criteria:**
- `${VAR}` expands to environment variable value
- `${VAR:-default}` expands to default if VAR unset
- `${VAR:?error}` fails with error if VAR unset
- Missing variable without default expands to empty string with warning
- **Substitution occurs on raw text BEFORE YAML parsing**
- Literal `$` escaped as `$$`
- Recursive expansion NOT supported (result is not re-scanned)

**IMPORTANT: Pre-Parse Substitution:**

Environment variables are substituted in the **raw YAML text** before the YAML parser runs. This ensures correct type inference:

```yaml
# In config file:
count: ${MAX_RETRIES}

# With MAX_RETRIES=5, becomes (before parsing):
count: 5

# YAML parser then infers `count` as integer, not string
```

If substitution occurred after parsing, `count` would be typed as a string `"5"` instead of integer `5`, causing schema validation failures.

> ⚠️ **Security Warning: Configuration Injection Risk**
>
> Because substitution occurs before YAML parsing, environment variables containing YAML syntax characters can alter the document structure. This is effectively a "configuration injection" vulnerability.
>
> **Example of malicious input:**
> ```bash
> export TARGET="localhost\n  malicious_key: attacker_value"
> ```
> ```yaml
> server:
>   host: ${TARGET}
> ```
> **Becomes after substitution:**
> ```yaml
> server:
>   host: localhost
>   malicious_key: attacker_value
> ```
>
> **Mitigation:**
> - Only use environment variables from trusted sources
> - ThoughtJack is a local attack testing tool; this risk assumes you control your own environment
> - Avoid using env vars for multi-line or structured content
> - Characters to be wary of: `:`, `\n`, `[`, `]`, `{`, `}`, `#`, `-` at line start

**Warning: String Fields with Numeric Values:**

Because YAML infers types from the substituted text, environment variables containing numeric values will be parsed as numbers, not strings. This can cause schema validation failures for fields that expect strings:

```yaml
# PROBLEM: USER=12345 (numeric username)
name: ${USER}           # Parsed as integer 12345, fails if name expects string

# SOLUTION: Quote the variable for string fields
name: "${USER}"         # Parsed as string "12345"
```

**Rule of thumb:** Always quote `"${VAR}"` for fields that MUST be strings, especially when the environment value might be numeric.

**Resolution Order:**
1. Read raw file content (text)
2. Substitute `${VAR}` patterns in text
3. Parse substituted text as YAML
4. Resolve other directives (`$include`, `$file`, `$generate`)

**Recursive Expansion:**
Expansion is NOT recursive. If `${VAR}` expands to `${OTHER}`, the result is the literal string `${OTHER}`, not the value of `OTHER`. This prevents infinite loops and makes behavior predictable.

**Syntax:**
```yaml
response:
  content:
    - type: text
      text: "Connecting to ${TARGET_HOST:-localhost}:${TARGET_PORT:?PORT required}"
```

**Implementation:**
```rust
/// Pre-parse environment variable substitution.
/// This runs on RAW TEXT before YAML parsing to preserve type inference.
pub struct EnvSubstitution {
    warnings: Vec<EnvWarning>,
}

impl EnvSubstitution {
    /// Substitute env vars in raw YAML text BEFORE parsing.
    /// Returns the substituted text ready for YAML parsing.
    pub fn substitute(&mut self, raw_yaml: &str) -> Result<String, SubstitutionError> {
        let mut result = String::with_capacity(raw_yaml.len());
        let mut chars = raw_yaml.chars().peekable();
        
        while let Some(c) = chars.next() {
            if c == '$' {
                match chars.peek() {
                    Some('$') => {
                        // Escaped $$ → literal $
                        chars.next();
                        result.push('$');
                    }
                    Some('{') => {
                        chars.next();
                        let (var_name, default, required) = self.parse_var_spec(&mut chars)?;
                        
                        match std::env::var(&var_name) {
                            Ok(value) => result.push_str(&value),
                            Err(_) => {
                                if let Some(default_val) = default {
                                    result.push_str(&default_val);
                                } else if required {
                                    return Err(SubstitutionError::RequiredEnvVar {
                                        var: var_name,
                                    });
                                } else {
                                    self.warnings.push(EnvWarning::MissingVar {
                                        var: var_name.clone(),
                                    });
                                    // Missing var without default → empty string
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
    
    fn parse_var_spec(
        &self,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> Result<(String, Option<String>, bool), SubstitutionError> {
        let mut var_name = String::new();
        let mut default = None;
        let mut required = false;
        
        // Read variable name
        while let Some(&c) = chars.peek() {
            match c {
                '}' => {
                    chars.next();
                    return Ok((var_name, default, required));
                }
                ':' => {
                    chars.next();
                    match chars.peek() {
                        Some('-') => {
                            chars.next();
                            default = Some(self.read_until_close(chars)?);
                            return Ok((var_name, default, false));
                        }
                        Some('?') => {
                            chars.next();
                            required = true;
                            let _ = self.read_until_close(chars)?;
                            return Ok((var_name, None, true));
                        }
                        _ => var_name.push(':'),
                    }
                }
                _ => {
                    chars.next();
                    var_name.push(c);
                }
            }
        }
        
        Err(ResolveError::UnclosedEnvVar)
    }
    
    fn read_until_close(
        &self,
        chars: &mut std::iter::Peekable<std::str::Chars>,
    ) -> Result<String, ResolveError> {
        let mut value = String::new();
        let mut depth = 1;
        
        while let Some(c) = chars.next() {
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
        
        Err(ResolveError::UnclosedEnvVar)
    }
}
```

### F-006: Schema Validation

The system SHALL validate resolved configuration against the schema.

**Acceptance Criteria:**
- Validate required fields are present
- Validate field types match schema
- Validate enum values are valid
- Validate numeric ranges
- Collect all errors (don't stop at first)
- Errors include field path and expected type

**Implementation:**
```rust
pub struct SchemaValidator {
    errors: Vec<ValidationError>,
}

impl SchemaValidator {
    pub fn validate(&mut self, config: &serde_yaml::Value) -> Result<(), ValidationErrors> {
        self.validate_server_config(config)?;
        
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(ValidationErrors { errors: std::mem::take(&mut self.errors) })
        }
    }
    
    fn validate_server_config(&mut self, config: &serde_yaml::Value) {
        let map = match config.as_mapping() {
            Some(m) => m,
            None => {
                self.errors.push(ValidationError::TypeMismatch {
                    path: "".into(),
                    expected: "mapping",
                    actual: config.type_name(),
                });
                return;
            }
        };
        
        // Validate 'server' section
        if let Some(server) = map.get("server") {
            self.validate_server_section(server, "server");
        } else {
            self.errors.push(ValidationError::MissingField {
                path: "".into(),
                field: "server".into(),
            });
        }
        
        // Check for simple vs phased server
        let has_baseline = map.contains_key("baseline");
        let has_phases = map.contains_key("phases");
        let has_tools = map.contains_key("tools");
        
        if has_baseline && has_tools {
            self.errors.push(ValidationError::MixedServerForms {
                message: "Cannot have both 'baseline' and top-level 'tools'".into(),
            });
        }
        
        if has_baseline || has_phases {
            self.validate_phased_server(map);
        } else {
            self.validate_simple_server(map);
        }
    }
    
    fn validate_tool(&mut self, tool: &serde_yaml::Value, path: &str) {
        let map = match tool.as_mapping() {
            Some(m) => m,
            None => {
                self.errors.push(ValidationError::TypeMismatch {
                    path: path.into(),
                    expected: "mapping",
                    actual: tool.type_name(),
                });
                return;
            }
        };
        
        // Required: name
        self.require_string(map, "name", &format!("{}.name", path));
        
        // Required: description
        self.require_string(map, "description", &format!("{}.description", path));
        
        // Required: inputSchema
        if let Some(schema) = map.get("inputSchema") {
            self.validate_json_schema(schema, &format!("{}.inputSchema", path));
        } else {
            self.errors.push(ValidationError::MissingField {
                path: path.into(),
                field: "inputSchema".into(),
            });
        }
    }
    
    fn require_string(&mut self, map: &serde_yaml::Mapping, key: &str, path: &str) {
        match map.get(key) {
            Some(serde_yaml::Value::String(_)) => {}
            Some(v) => {
                self.errors.push(ValidationError::TypeMismatch {
                    path: path.into(),
                    expected: "string",
                    actual: v.type_name(),
                });
            }
            None => {
                self.errors.push(ValidationError::MissingField {
                    path: path.rsplit_once('.').map(|(p, _)| p).unwrap_or("").into(),
                    field: key.into(),
                });
            }
        }
    }
}
```

### F-007: Semantic Validation

The system SHALL perform semantic validation beyond schema checks.

**Acceptance Criteria:**
- Validate phase `replace_tools` targets exist in baseline
- Validate phase `remove_tools` targets exist in baseline
- Validate event names in triggers are valid
- Validate duration formats
- Validate generator parameters
- Cross-reference tool/resource/prompt names

**Implementation:**
```rust
impl SchemaValidator {
    fn validate_semantic(&mut self, config: &ResolvedConfig) {
        // Collect baseline tool names
        let baseline_tools: HashSet<String> = config.baseline.tools
            .iter()
            .map(|t| t.name.clone())
            .collect();
        
        // Validate each phase
        for (idx, phase) in config.phases.iter().enumerate() {
            let path = format!("phases[{}]", idx);
            
            // Check replace_tools targets exist
            for tool_name in phase.diff.replace_tools.keys() {
                if !baseline_tools.contains(tool_name) {
                    self.errors.push(ValidationError::UnknownTarget {
                        path: format!("{}.replace_tools.{}", path, tool_name),
                        target_type: "tool",
                        target_name: tool_name.clone(),
                    });
                }
            }
            
            // Check remove_tools targets exist
            for tool_name in &phase.diff.remove_tools {
                if !baseline_tools.contains(tool_name) {
                    self.errors.push(ValidationError::UnknownTarget {
                        path: format!("{}.remove_tools", path),
                        target_type: "tool",
                        target_name: tool_name.clone(),
                    });
                }
            }
            
            // Validate trigger event names
            if let Some(trigger) = &phase.advance {
                if let Some(event) = &trigger.on {
                    if let Err(e) = EventType::parse(event) {
                        self.errors.push(ValidationError::InvalidEventType {
                            path: format!("{}.advance.on", path),
                            event: event.clone(),
                            message: e.to_string(),
                        });
                    }
                }
            }
        }
        
        // Validate phase names are unique
        let mut phase_names = HashSet::new();
        for (idx, phase) in config.phases.iter().enumerate() {
            if !phase_names.insert(&phase.name) {
                self.errors.push(ValidationError::DuplicateName {
                    path: format!("phases[{}].name", idx),
                    name: phase.name.clone(),
                    name_type: "phase",
                });
            }
        }
    }
}
```

### F-007a: Configuration Limits Validation

The system SHALL enforce hard limits on configuration size to prevent resource exhaustion.

**Rationale:** A malicious or fuzzer-generated configuration could define thousands of phases, tools, or resources, causing memory exhaustion in the metrics system (metric cardinality explosion) or runtime performance degradation.

**Limits:**

| Entity | Default Limit | Env Override | Rationale |
|--------|---------------|--------------|-----------|
| Phases | 100 | `THOUGHTJACK_MAX_PHASES` | Metric cardinality bound |
| Tools | 1,000 | `THOUGHTJACK_MAX_TOOLS` | Memory bound |
| Resources | 1,000 | `THOUGHTJACK_MAX_RESOURCES` | Memory bound |
| Prompts | 500 | `THOUGHTJACK_MAX_PROMPTS` | Memory bound |
| Include depth | 10 | `THOUGHTJACK_MAX_INCLUDE_DEPTH` | Stack overflow prevention |
| Config file size | 10MB | `THOUGHTJACK_MAX_CONFIG_SIZE` | Parse time bound |

**Hard Upper Bounds (B42):**

The implementation enforces hard upper bounds using `.min()` caps on all user-configurable limits. Even if a user sets `THOUGHTJACK_MAX_PHASES=999999`, the actual limit is capped at a safe maximum (e.g., 10,000 for phases). This prevents accidental or malicious resource exhaustion via environment variables. The hard caps are:

- `max_phases`: 10,000
- `max_tools`: 100,000
- `max_resources`: 100,000
- `max_prompts`: 50,000
- `max_include_depth`: 100
- `max_config_size`: 100 MB

**Implementation:**
```rust
pub struct ConfigLimits {
    pub max_phases: usize,
    pub max_tools: usize,
    pub max_resources: usize,
    pub max_prompts: usize,
    pub max_include_depth: usize,
    pub max_config_size: usize,
}

impl Default for ConfigLimits {
    fn default() -> Self {
        Self {
            max_phases: env_or("THOUGHTJACK_MAX_PHASES", 100).min(10_000),
            max_tools: env_or("THOUGHTJACK_MAX_TOOLS", 1000).min(100_000),
            max_resources: env_or("THOUGHTJACK_MAX_RESOURCES", 1000).min(100_000),
            max_prompts: env_or("THOUGHTJACK_MAX_PROMPTS", 500).min(50_000),
            max_include_depth: env_or("THOUGHTJACK_MAX_INCLUDE_DEPTH", 10).min(100),
            max_config_size: env_or("THOUGHTJACK_MAX_CONFIG_SIZE", 10 * 1024 * 1024)
                .min(100 * 1024 * 1024),
        }
    }
}

impl SchemaValidator {
    fn validate_limits(&mut self, config: &ResolvedConfig, limits: &ConfigLimits) {
        if config.phases.len() > limits.max_phases {
            self.errors.push(ValidationError::LimitExceeded {
                entity: "phases",
                count: config.phases.len(),
                limit: limits.max_phases,
            });
        }
        
        let tool_count = config.baseline.tools.len() 
            + config.phases.iter().map(|p| p.diff.add_tools.len()).sum::<usize>();
        if tool_count > limits.max_tools {
            self.errors.push(ValidationError::LimitExceeded {
                entity: "tools",
                count: tool_count,
                limit: limits.max_tools,
            });
        }
        
        // Similar for resources, prompts...
    }
}
```

**Error Message:**
```
Error: Configuration limit exceeded
  --> attack.yaml
  |
  | phases: [... 150 phases ...]
  |
  = error: too many phases (150 > 100)
  = help: reduce phase count or set THOUGHTJACK_MAX_PHASES=200
```

### F-008: Error Collection and Reporting

The system SHALL collect all errors and report them with context.

**Acceptance Criteria:**
- Continue validation after first error (collect all)
- Each error includes file path, line number if available
- Errors grouped by severity (error, warning)
- Human-readable error messages
- Suggest fixes for common mistakes

**Warning Structure:**

Warnings during loading are collected in `LoadWarning` structs and returned alongside the configuration in `LoadResult`:

```rust
/// Warning during configuration loading.
#[derive(Debug, Clone)]
pub struct LoadWarning {
    /// Warning message.
    pub message: String,

    /// Location where the warning occurred (e.g., file path or field path).
    pub location: Option<String>,
}

/// Result of loading a configuration file.
#[derive(Debug)]
pub struct LoadResult {
    /// The loaded and validated configuration.
    pub config: Arc<ServerConfig>,

    /// Warnings encountered during loading.
    pub warnings: Vec<LoadWarning>,
}
```

**Error Type:**

All loader errors use the shared `ConfigError` enum (defined in `error.rs`). Error reporting uses the file path and `serde_yaml` error positions rather than a custom source map. Key variants used by the loader include:

```rust
pub enum ConfigError {
    MissingFile { path: PathBuf },
    ParseError { path: PathBuf, line: Option<usize>, message: String },
    CircularInclude { cycle: Vec<PathBuf> },
    InvalidValue { field: String, value: String, expected: String },
    EnvVarNotSet { var: String, location: String },
    ValidationError { path: String, errors: Vec<String> },
    // ...
}
```

### F-009: Configuration Freezing

The system SHALL produce an immutable, runtime-ready configuration.

**Acceptance Criteria:**
- Final config wrapped in `Arc` for sharing
- No `$` directives remain (all resolved)
- All paths resolved to absolute
- All includes inlined
- All generates expanded
- Config is `Send + Sync` for multi-threaded use

**In-Memory Loading (B43):**

In addition to loading from files via `load()`, the system provides a `load_from_str(&mut self, yaml: &str)` method that enables loading configurations from in-memory strings. This is used for embedded scenario loading (TJ-SPEC-010).

Key differences from file-based loading:
- Skips file I/O
- When `options.embedded == true`, environment variable substitution is skipped since `${...}` syntax is reserved for template interpolation at runtime (not env var expansion at load time)
- When `options.embedded == true`, `$include` and `$file` directives are rejected with errors
- `$generate` directives are allowed in embedded mode

This enables built-in attack scenarios to be embedded directly in the binary without requiring external YAML files or library filesystem access.

**Implementation:**
```rust
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub server: ServerMeta,
    pub baseline: ServerState,
    pub phases: Vec<Phase>,
    pub logging: LoggingConfig,
    pub unknown_methods: UnknownMethodsPolicy,
}

impl ServerConfig {
    /// Freeze the configuration for runtime use
    pub fn freeze(self) -> Arc<Self> {
        Arc::new(self)
    }
}

pub struct ConfigLoader {
    options: LoaderOptions,
    include_cache: HashMap<PathBuf, Value>,
    file_cache: HashMap<PathBuf, FileContent>,
}

impl ConfigLoader {
    pub fn load(&mut self, path: &Path) -> Result<LoadResult, ConfigError> {
        let mut warnings = Vec::new();

        // Check file size limit
        // Stage 0: Read raw file content
        // Stage 1: Environment variable substitution (before YAML parsing)
        let mut env_sub = EnvSubstitution::new();
        let substituted = env_sub.substitute(raw_content, path)?;
        warnings.extend(env_sub.warnings);

        // Stage 2: YAML parsing
        let mut root: Value = serde_yaml::from_str(&substituted)
            .map_err(|e| ConfigError::ParseError { ... })?;

        // Stage 3: $include resolution
        let mut include_resolver = IncludeResolver::new(...);
        include_resolver.resolve(&mut root, &mut self.include_cache)?;

        // Stage 4: $file resolution
        let file_resolver = FileResolver::new(...);
        file_resolver.resolve(&mut root, path, &mut self.file_cache, 0)?;

        // Stage 5: $generate validation (stores GeneratorConfig, not bytes)
        validate_generators(&root, &self.options.generator_limits, 0)?;

        // Stage 6: Deserialize to typed config
        let config: ServerConfig = serde_yaml::from_value(root)?;

        // Stage 7: Validation
        let mut validator = Validator::new();
        let validation_result = validator.validate(&config, &self.options.config_limits);

        // Stage 8: Freeze
        Ok(LoadResult { config: Arc::new(config), warnings })
    }

    /// Load configuration from in-memory YAML string (B43)
    ///
    /// Runs the same pipeline as `load()` but skips file I/O.
    /// In embedded mode (`options.embedded == true`), `$include` and `$file`
    /// directives are rejected, and environment variable substitution is skipped.
    pub fn load_from_str(&mut self, yaml: &str) -> Result<LoadResult, ConfigError> {
        // Same pipeline as load(), but from string
        // Environment substitution skipped if embedded == true
        // Filesystem directives rejected if embedded == true
        // $generate directives are allowed in embedded mode
        // ...
    }
}
```

### F-010: Library Path Resolution

The system SHALL resolve paths relative to the library root.

**Acceptance Criteria:**
- Library root configurable via `--library` flag or `THOUGHTJACK_LIBRARY` env
- Default library root is `./library` relative to CWD
- `$include` paths relative to library root
- `$file` paths relative to including file's directory
- Absolute paths used as-is

**Path Resolution:**
```rust
impl IncludeResolver {
    fn resolve_path(
        &self,
        path_value: &serde_yaml::Value,
        current_file: &Path,
    ) -> Result<PathBuf, ResolveError> {
        let path_str = path_value.as_str()
            .ok_or(ResolveError::InvalidPath)?;
        
        let path = Path::new(path_str);
        
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        
        // $include paths are relative to library root
        let resolved = self.library_root.join(path);
        
        if resolved.exists() {
            Ok(resolved.canonicalize()?)
        } else {
            Err(ResolveError::IncludeNotFound {
                path: resolved,
                referenced_from: current_file.to_owned(),
            })
        }
    }
}

impl FileResolver {
    fn resolve_path(
        &self,
        path_value: &serde_yaml::Value,
        current_file: &Path,
    ) -> Result<PathBuf, ResolveError> {
        let path_str = path_value.as_str()
            .ok_or(ResolveError::InvalidPath)?;
        
        let path = Path::new(path_str);
        
        if path.is_absolute() {
            return Ok(path.to_owned());
        }
        
        // $file paths are relative to the current file's directory
        let base_dir = current_file.parent().unwrap_or(Path::new("."));
        let resolved = base_dir.join(path);
        
        if resolved.exists() {
            Ok(resolved.canonicalize()?)
        } else {
            // Fall back to library root
            let resolved = self.library_root.join(path);
            if resolved.exists() {
                Ok(resolved.canonicalize()?)
            } else {
                Err(ResolveError::FileNotFound {
                    path: PathBuf::from(path_str),
                    referenced_from: current_file.to_owned(),
                })
            }
        }
    }
}
```

### F-011: Validation Command

The system SHALL support a standalone validation command.

**Acceptance Criteria:**
- `thoughtjack server validate config.yaml` validates without running
- Exit code 0 if valid, non-zero if errors
- Print all errors and warnings
- `--quiet` suppresses warnings
- `--json` outputs machine-readable format

**Implementation:**
```rust
pub fn validate_command(args: &ValidateArgs) -> Result<(), ThoughtJackError> {
    let loader = ConfigLoader::new(&args.library);

    match loader.load(&args.config) {
        Ok(_config) => {
            if !args.quiet {
                println!("✓ Configuration is valid");
            }
            Ok(())
        }
        Err(errors) => {
            if args.json {
                println!("{}", serde_json::to_string_pretty(&errors)?);
            } else {
                eprintln!("{}", errors.format_report());
            }
            Err(ThoughtJackError::ValidationFailed)
        }
    }
}
```

---

## 3. Edge Cases

### EC-LOAD-001: Empty YAML File

**Scenario:** Config file is empty  
**Expected:** Validation error: "Configuration file is empty"

### EC-LOAD-002: YAML Syntax Error

**Scenario:** Invalid YAML syntax (unmatched brackets, bad indentation)  
**Expected:** Parse error with line number and context

### EC-LOAD-003: Include File Not Found

**Scenario:** `$include: nonexistent.yaml`  
**Expected:** Error: "Include not found: nonexistent.yaml (referenced from config.yaml)"

### EC-LOAD-004: Circular Include (Self)

**Scenario:** File includes itself  
**Expected:** Error: "Circular include detected: a.yaml → a.yaml"

### EC-LOAD-005: Circular Include (Indirect)

**Scenario:** a.yaml → b.yaml → c.yaml → a.yaml  
**Expected:** Error: "Circular include detected: a.yaml → b.yaml → c.yaml → a.yaml"

### EC-LOAD-006: Include With Invalid Override

**Scenario:** Override tries to add key that doesn't make sense  
**Expected:** Deep merge succeeds; validation may fail later

### EC-LOAD-007: File Reference Binary as Text

**Scenario:** `$file: image.png` used where text expected  
**Expected:** Warning: "Binary file loaded as base64"

### EC-LOAD-008: Environment Variable Missing

**Scenario:** `${UNDEFINED_VAR}` and variable not set  
**Expected:** Warning logged, expands to empty string

### EC-LOAD-009: Environment Variable Required Missing

**Scenario:** `${REQUIRED_VAR:?must be set}` and variable not set  
**Expected:** Error: "Required environment variable: REQUIRED_VAR"

### EC-LOAD-010: Generate Exceeds Limit

**Scenario:** `$generate: { type: garbage, bytes: 1000000000 }`  
**Expected:** Error: "Generated payload too large: 1GB (limit: 100MB)"

### EC-LOAD-011: Duplicate Keys in YAML

**Scenario:** Same key appears twice in mapping  
**Expected:** Warning: "Duplicate key 'x', using last value"

### EC-LOAD-012: Unknown Directive

**Scenario:** `$unknown: something`  
**Expected:** Treated as regular key (no special handling)

### EC-LOAD-013: Nested Include Override

**Scenario:** Include with override that itself contains $include  
**Expected:** Nested includes resolved after override merge

### EC-LOAD-014: File Reference Absolute Path

**Scenario:** `$file: /etc/passwd`  
**Expected:** Loads from absolute path (security consideration for library use)

### EC-LOAD-015: Unicode in Paths

**Scenario:** `$include: tools/计算器/benign.yaml`  
**Expected:** Handled correctly (UTF-8 paths)

### EC-LOAD-016: Very Deep Include Nesting

**Scenario:** Include chain 100 levels deep (no cycles)  
**Expected:** Works but may emit warning about depth

### EC-LOAD-017: Generate in Include Override

**Scenario:** Override block contains `$generate`  
**Expected:** Generate resolved after include merge

### EC-LOAD-018: Mixed Line Endings

**Scenario:** YAML file has mixed CRLF and LF  
**Expected:** Parsed correctly

### EC-LOAD-019: BOM in YAML File

**Scenario:** YAML file starts with UTF-8 BOM  
**Expected:** BOM stripped, file parsed correctly

### EC-LOAD-020: Validation With Warnings Only

**Scenario:** Config has warnings but no errors  
**Expected:** Load succeeds, warnings returned separately

### EC-LOAD-021: Duplicate YAML Keys

**Scenario:** Config contains `{ name: "a", name: "b" }`  
**Expected:** Warning logged: "Duplicate key 'name' at line X, using last value". Config loads successfully with `name: "b"` (last value wins per YAML spec).

### EC-LOAD-022: Large Generator at Load Time

**Scenario:** Config contains `$generate: { type: garbage, bytes: 1gb }`  
**Expected:** Generator factory created (not bytes). `estimated_size()` checked against limits. Actual generation deferred to response time. No OOM at startup.

---

## 4. Non-Functional Requirements

### NFR-001: Load Performance

- Configuration with 100 includes SHALL load in < 1 second
- Include cache prevents redundant file reads
- Generate cache prevents redundant payload generation

### NFR-002: Memory Usage

- Loader SHALL not hold entire include tree in memory
- Process includes depth-first, release after merge
- Large generated payloads use streaming

### NFR-003: Error Recovery

- Continue after non-fatal errors to collect all issues
- Provide actionable error messages
- Suggest fixes for common mistakes

### NFR-004: Path Security

- Reject path traversal attempts (`../../../etc/passwd`)
- Warn on absolute paths outside library
- Document security implications

---

## 5. Configuration Loading Reference

### 5.1 Directive Summary

| Directive | Syntax | Resolution |
|-----------|--------|------------|
| `$include` | `$include: path` | Load and merge YAML file |
| `$include` + `override` | `$include: path` + `override: {...}` | Load, merge, then apply overrides |
| `$file` | `$file: path` | Load file contents (JSON/text/binary) |
| `$generate` | `$generate: { type: ..., ... }` | Invoke payload generator |
| `${VAR}` | `${VAR}` | Expand environment variable |
| `${VAR:-default}` | `${VAR:-value}` | Expand with default |
| `${VAR:?error}` | `${VAR:?message}` | Require variable or error |
| `$$` | `$$` | Literal `$` character |

### 5.2 Resolution Order

0. **Env** — `${VAR}` expansion (before parsing, on raw YAML text)
1. **Parse** — YAML to AST
2. **Include** — `$include` directives (recursive, with override merge)
3. **File** — `$file` directives
4. **Generate** — `$generate` directives
5. **Validate** — Schema and semantic checks
6. **Freeze** — Produce immutable config

### 5.3 Configuration Transformation Pipeline

This section clarifies the types at each stage of configuration loading. This is critical for implementers to understand what data structure exists at each point:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    Configuration Transformation Pipeline                      │
└─────────────────────────────────────────────────────────────────────────────┘

 Input File (YAML text with ${ENV}, $include, $file, $generate)
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 1: Environment Substitution (F-005)                                 │
 │   Input:  String (raw YAML text)                                          │
 │   Output: String (YAML text with ${ENV} replaced)                         │
 │   Note:   Happens BEFORE parsing to preserve type inference               │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 2: YAML Parsing (F-001)                                             │
 │   Input:  String (substituted YAML text)                                  │
 │   Output: serde_yaml::Value (untyped AST)                                 │
 │   Note:   Directives ($include, etc.) are still Value nodes               │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 3: Include Resolution (F-002)                                       │
 │   Input:  serde_yaml::Value (with $include nodes)                         │
 │   Output: serde_yaml::Value ($include replaced with merged content)       │
 │   Note:   Recursive, tracks visited paths for cycle detection             │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 4: File Resolution (F-003)                                          │
 │   Input:  serde_yaml::Value (with $file nodes)                            │
 │   Output: serde_yaml::Value ($file replaced with content)                 │
 │   Note:   Binary files become base64 strings, JSON files parsed           │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 5: Generate Resolution (F-004)                                      │
 │   Input:  serde_yaml::Value (with $generate nodes)                        │
 │   Output: serde_yaml::Value ($generate replaced with ResolvedContent)     │
 │   Note:   Creates generator FACTORIES, not actual bytes                   │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 6: Deserialization                                                  │
 │   Input:  serde_yaml::Value (fully resolved)                              │
 │   Output: ServerConfig (typed Rust struct - see TJ-SPEC-001 Section 9.4)  │
 │   Note:   serde derives handle the conversion                             │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 7: Validation (F-006, F-007, F-007a)                                │
 │   Input:  ServerConfig (typed)                                            │
 │   Output: ServerConfig (validated) or ValidationErrors                    │
 │   Note:   Schema validation, semantic validation, limits check            │
 └───────────────────────────────────────────────────────────────────────────┘
         │
         ▼
 ┌───────────────────────────────────────────────────────────────────────────┐
 │ Stage 8: Freeze (F-009)                                                   │
 │   Input:  ServerConfig (validated)                                        │
 │   Output: Arc<ServerConfig> (immutable, Send + Sync)                      │
 │   Note:   Ready for use by PhaseEngine and handlers                       │
 └───────────────────────────────────────────────────────────────────────────┘
```

**Key Type Aliases:**

```rust
/// Raw YAML text (pre-parsing)
type RawYaml = String;

/// Parsed but unresolved YAML (contains directive nodes)
type ParsedYaml = serde_yaml::Value;

/// Fully resolved YAML (no directives remain)
type ResolvedYaml = serde_yaml::Value;

/// Typed configuration (see TJ-SPEC-001 Section 9.4 for full definition)
/// This is the type used throughout the runtime.
pub struct ServerConfig { /* ... */ }

/// Frozen configuration for concurrent access
type FrozenConfig = Arc<ServerConfig>;
```

**ContentValue Transformation:**

During Stage 5 (Generate Resolution), `$generate` directives become `ContentValue::Generated`:

```rust
// In YAML:
// text:
//   $generate:
//     type: garbage
//     bytes: 1mb

// Becomes (after resolution):
ContentValue::Generated {
    generator: Box::new(GarbageGenerator { bytes: 1_048_576 }),
    estimated_size: 1_048_576,
}

// NOT materialized bytes - generator is a factory
```

### 5.4 Path Resolution Rules

| Directive | Base Path | Example |
|-----------|-----------|---------|
| `$include` | Library root | `$include: tools/calc.yaml` → `./library/tools/calc.yaml` |
| `$file` | Current file's directory | `$file: schema.json` → `./library/tools/schema.json` |
| `$file` (fallback) | Library root | If not found relative to file |
| Absolute | As-is | `$file: /data/schema.json` |

---

## 6. Implementation Notes

### 6.1 Recommended Libraries

| Library | Purpose |
|---------|---------|
| `serde_yaml` | YAML parsing |
| `serde_json` | JSON file handling |
| `base64` | Binary file encoding |
| `thiserror` | Error types |
| `tracing` | Logging |

### 6.2 Caching Strategy

```rust
pub struct LoaderCaches {
    /// Parsed YAML files (by canonical path)
    yaml_cache: HashMap<PathBuf, Arc<serde_yaml::Value>>,
    
    /// Loaded file contents (by canonical path)
    file_cache: HashMap<PathBuf, Arc<FileContent>>,
    
    /// Generated payloads (by generator config hash)
    generate_cache: HashMap<u64, Arc<GeneratedPayload>>,
}

impl LoaderCaches {
    pub fn clear(&mut self) {
        self.yaml_cache.clear();
        self.file_cache.clear();
        // Note: generate_cache is preserved across loads
    }
}
```

### 6.3 Anti-Patterns

| Anti-Pattern | Why | Correct Approach |
|--------------|-----|------------------|
| Stopping at first error | Poor UX, multiple fix cycles | Collect all errors |
| Resolving directives in wrong order | Include may contain generate | Order: include → file → generate → env |
| Not caching includes | Same file parsed multiple times | Cache by canonical path |
| Using string paths internally | Platform differences | Use `PathBuf` throughout |
| Mutable config after load | Race conditions at runtime | Freeze with `Arc` |
| Panicking on invalid config | Crashes validation command | Return `Result` with errors |
| Relative paths from CWD | Confusing behavior | Relative to library root or current file |
| Silent path traversal | Security risk | Detect and reject `..` sequences |

### 6.4 Testing Strategy

**Unit Tests:**
- Each directive resolver in isolation
- Path resolution edge cases
- Error message formatting
- Deep merge algorithm

**Integration Tests:**
- Load real example configs
- Circular include detection
- Full validation pipeline
- Cache effectiveness

**Property Tests:**
- Parse → serialize → parse roundtrip
- Deep merge associativity
- Env expansion idempotence

---

## 7. Definition of Done

- [ ] YAML parsing with source location tracking
- [ ] `$include` resolution with cycle detection
- [ ] `$include` override deep merge
- [ ] `$file` resolution for JSON, text, binary
- [ ] `$generate` resolution with limit checking
- [ ] `${VAR}` expansion with defaults and required
- [ ] Schema validation for all config fields
- [ ] Semantic validation (cross-references, phase consistency)
- [ ] Error collection (continue after errors)
- [ ] Error messages include file/line context
- [ ] Configuration freezing (Arc-wrapped, immutable)
- [ ] Include cache prevents redundant reads
- [ ] `thoughtjack server validate` command works
- [ ] `--json` output for validation errors
- [ ] Path resolution follows documented rules
- [ ] All 22 edge cases (EC-LOAD-001 through EC-LOAD-022) have tests
- [ ] Load performance meets NFR-001
- [ ] Memory usage meets NFR-002
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo test` passes

---

## 8. References

- [TJ-SPEC-001: Configuration Schema](./TJ-SPEC-001_Configuration_Schema.md)
- [TJ-SPEC-005: Payload Generation](./TJ-SPEC-005_Payload_Generation.md)
- [YAML 1.2 Specification](https://yaml.org/spec/1.2.2/)
- [serde_yaml Documentation](https://docs.rs/serde_yaml/)
- [JSON Schema](https://json-schema.org/)

---

## Appendix A: Error Message Examples

### A.1 Missing Required Field

```
Error at phases[2].advance:
  Missing required field 'on' or 'after'
  
  At least one trigger type is required:
    - 'on: event' for event-based trigger
    - 'after: duration' for time-based trigger
  
  Example:
    advance:
      on: tools/call
      count: 3
```

### A.2 Circular Include

```
Error: Circular include detected

  Include chain:
    1. servers/attack.yaml
    2. phases/setup.yaml
    3. tools/shared.yaml
    4. servers/attack.yaml  ← cycle

  Remove one of these includes to break the cycle.
```

### A.3 Unknown Tool Reference

```
Error at phases[1].replace_tools.calculator:
  Cannot replace unknown tool 'calculator'
  
  Tool 'calculator' is not defined in baseline.tools
  
  Available tools:
    - calc
    - data_export
    - read_file
  
  Did you mean 'calc'?
```

### A.4 Environment Variable Required

```
Error: Required environment variable not set

  Variable: TARGET_HOST
  Location: response.content[0].text
  
  The configuration requires TARGET_HOST to be set:
    text: "Connecting to ${TARGET_HOST:?must specify target}"
  
  Set the variable and retry:
    export TARGET_HOST=example.com
```

---

## Appendix B: Deep Merge Algorithm

```
deep_merge(base, override):
    if both are mappings:
        for each key in override:
            if key exists in base:
                base[key] = deep_merge(base[key], override[key])
            else:
                base[key] = override[key]
        return base
    else:
        return override  # Override wins for non-mappings
```

**Examples:**

```yaml
# Base (from $include)
tool:
  name: calculator
  description: "Basic calc"
  inputSchema:
    type: object
    properties:
      a: { type: number }

# Override
override:
  tool:
    name: "calc_v2"
    inputSchema:
      properties:
        b: { type: number }

# Result (deep merged)
tool:
  name: "calc_v2"              # Overridden
  description: "Basic calc"    # Preserved from base
  inputSchema:
    type: object               # Preserved from base
    properties:
      a: { type: number }      # Preserved from base
      b: { type: number }      # Added from override
```
