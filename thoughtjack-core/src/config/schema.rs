//! Configuration schema types (TJ-SPEC-001)
//!
//! This module defines the core configuration types for `ThoughtJack` servers.
//! These types are deserialized from YAML configuration files.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Top-Level Configuration (F-001, F-002)
// ============================================================================

/// Root configuration for a `ThoughtJack` server.
///
/// Supports two forms:
/// - **Simple Server** (F-001): Uses top-level `tools`, `resources`, `prompts`
/// - **Phased Server** (F-002): Uses `baseline` and `phases` for temporal attacks
///
/// These forms are mutually exclusive (EC-CFG-016).
///
/// Implements: TJ-SPEC-001 F-001, F-002
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerConfig {
    /// Server metadata (required)
    pub server: ServerMetadata,

    /// Baseline state for phased servers (mutually exclusive with top-level tools/resources/prompts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline: Option<BaselineState>,

    /// Tool definitions for simple servers (mutually exclusive with baseline/phases)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolPattern>>,

    /// Resource definitions for simple servers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<ResourcePattern>>,

    /// Prompt definitions for simple servers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Vec<PromptPattern>>,

    /// Phase definitions for phased servers
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<Phase>>,

    /// Default behavior configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,

    /// Logging configuration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging: Option<LoggingConfig>,

    /// How to handle unknown MCP methods (F-013)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_methods: Option<UnknownMethodHandling>,

    /// Scenario metadata for documentation generation (TJ-SPEC-011).
    ///
    /// Optional — scenarios without metadata are still valid and executable.
    /// The documentation generator requires metadata for inclusion in the site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ScenarioMetadata>,
}

// ============================================================================
// Server Metadata
// ============================================================================

/// Server identification and capabilities.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ServerMetadata {
    /// Server name (required)
    pub name: String,

    /// Server version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Phase state scope (per-connection or global)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "stateScope")]
    pub state_scope: Option<StateScope>,

    /// MCP capabilities to advertise
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
}

/// Phase state scope - determines how phase state is managed.
///
/// Implements: TJ-SPEC-002 F-015
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum StateScope {
    /// Each connection maintains its own phase state (default)
    #[default]
    PerConnection,
    /// All connections share the same phase state
    Global,
}

/// MCP server capabilities.
///
/// These structs serialize to **camelCase** (MCP wire format) but accept
/// **`snake_case`** aliases in YAML config files for consistency with the
/// rest of the configuration schema.
///
/// Implements: TJ-SPEC-001 F-014
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Tools capability
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,

    /// Resources capability
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,

    /// Prompts capability
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
}

/// Tools capability configuration.
///
/// Implements: TJ-SPEC-001 F-014
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the server supports `tools/list_changed` notifications
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "list_changed"
    )]
    pub list_changed: Option<bool>,
}

/// Resources capability configuration.
///
/// Implements: TJ-SPEC-001 F-014
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    /// Whether the server supports resource subscriptions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,

    /// Whether the server supports `resources/list_changed` notifications
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "list_changed"
    )]
    pub list_changed: Option<bool>,
}

/// Prompts capability configuration.
///
/// Implements: TJ-SPEC-001 F-014
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    /// Whether the server supports `prompts/list_changed` notifications
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "list_changed"
    )]
    pub list_changed: Option<bool>,
}

// ============================================================================
// Tool Pattern (F-003)
// ============================================================================

/// A tool pattern defining an MCP tool and its response.
///
/// Implements: TJ-SPEC-001 F-003
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPattern {
    /// MCP tool definition
    pub tool: ToolDefinition,

    /// Response configuration
    pub response: ResponseConfig,

    /// Tool-specific behavior override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
}

/// MCP tool definition sent in tools/list response.
///
/// Implements: TJ-SPEC-001 F-003
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDefinition {
    /// Tool name (unique identifier)
    pub name: String,

    /// Tool description (attack surface for injection)
    pub description: String,

    /// JSON Schema for tool arguments (Draft 7+)
    pub input_schema: serde_json::Value,
}

// ============================================================================
// Response Configuration (F-004)
// ============================================================================

/// Response configuration for tool calls.
///
/// Supports static content, conditional matching, response sequences,
/// and external handlers for dynamic response generation.
///
/// Implements: TJ-SPEC-001 F-004, TJ-SPEC-009 F-001
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseConfig {
    /// Content items in the response (default if no dynamic features match)
    #[serde(default)]
    pub content: Vec<ContentItem>,

    /// Whether this response represents an error
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,

    /// Conditional match block for request-based response selection
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_block: Option<Vec<MatchBranchConfig>>,

    /// Response sequence for successive calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Vec<SequenceEntryConfig>>,

    /// Behavior when sequence is exhausted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<ExhaustedBehavior>,

    /// External handler for dynamic response generation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<HandlerConfig>,
}

/// Content item in a response.
///
/// Supports text, image, and embedded resource content types.
///
/// Implements: TJ-SPEC-001 F-004
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentItem {
    /// Text content
    Text {
        /// Text value (static, generated, or from file)
        text: ContentValue,
    },

    /// Image content
    Image {
        /// MIME type of the image
        #[serde(rename = "mimeType")]
        mime_type: String,

        /// Image data (static, generated, or from file)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<ContentValue>,
    },

    /// Embedded resource content
    Resource {
        /// The embedded resource
        resource: EmbeddedResource,
    },
}

/// Content value supporting static text, generated content, or file references.
///
/// This enum handles the `$generate` and `$file` directives in configuration.
///
/// Implements: TJ-SPEC-001 F-004
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentValue {
    /// Static string content
    Static(String),

    /// Generated content via `$generate` directive (F-012)
    Generated {
        /// Generator configuration
        #[serde(rename = "$generate")]
        generator: GeneratorConfig,
    },

    /// Content loaded from file via `$file` directive
    File {
        /// Path to the file
        #[serde(rename = "$file")]
        path: PathBuf,
    },
}

/// Embedded resource in a response.
///
/// Implements: TJ-SPEC-001 F-004
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    /// Resource URI
    pub uri: String,

    /// Resource MIME type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,

    /// Text content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Binary content (base64 encoded)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

// ============================================================================
// Generator Configuration (TJ-SPEC-005)
// ============================================================================

/// Configuration for payload generators.
///
/// Generators are configured at load time; payloads are generated lazily at
/// response time. All generators are seeded for reproducibility.
///
/// YAML example:
/// ```yaml
/// $generate:
///   type: nested_json
///   depth: 10000
///   structure: mixed
/// ```
///
/// Implements: TJ-SPEC-005 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratorConfig {
    /// Generator type
    #[serde(rename = "type")]
    pub type_: GeneratorType,

    /// Type-specific parameters (flattened from YAML)
    #[serde(flatten)]
    pub params: GeneratorParams,
}

/// Generator type identifier.
///
/// Implements: TJ-SPEC-005 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratorType {
    /// Generate deeply nested JSON structures
    NestedJson,

    /// Generate batch of JSON-RPC notifications
    BatchNotifications,

    /// Generate random garbage bytes
    Garbage,

    /// Generate JSON with repeated keys (hash collision attack)
    RepeatedKeys,

    /// Generate Unicode attack sequences
    UnicodeSpam,

    /// Generate ANSI escape sequences (terminal attacks)
    AnsiEscape,
}

/// Generator parameters (TJ-SPEC-005).
///
/// Parameters are type-specific and flattened from YAML.
/// The `HashMap` allows flexible parameters for each generator type.
pub type GeneratorParams = std::collections::HashMap<String, serde_json::Value>;

/// Structure type for nested JSON generation.
///
/// Implements: TJ-SPEC-005 F-002
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NestedStructure {
    /// Nested objects: `{"key": {"key": ...}}`
    #[default]
    Object,

    /// Nested arrays: `[[...]]`
    Array,

    /// Alternating objects and arrays
    Mixed,
}

/// Character set for garbage generation.
///
/// Implements: TJ-SPEC-005 F-004
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Charset {
    /// ASCII printable characters (0x20-0x7E)
    #[default]
    Ascii,

    /// Valid UTF-8 characters
    Utf8,

    /// Raw binary bytes (0x00-0xFF)
    Binary,

    /// Numeric characters only (0-9)
    Numeric,

    /// Alphanumeric characters (a-z, A-Z, 0-9)
    Alphanumeric,
}

/// Unicode category for spam generation.
///
/// Implements: TJ-SPEC-005 F-006
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnicodeCategory {
    /// Zero-width characters (U+200B, U+FEFF, etc.)
    #[default]
    ZeroWidth,

    /// Homoglyphs (visually similar characters)
    Homoglyph,

    /// Combining characters (diacritics, zalgo text)
    Combining,

    /// Right-to-left override characters (text direction attacks)
    Rtl,

    /// Emoji and emoji modifiers
    Emoji,
}

/// ANSI escape sequence types for terminal attacks.
///
/// Implements: TJ-SPEC-005 F-007
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnsiSequenceType {
    /// Cursor movement sequences
    CursorMove,

    /// Color/style sequences
    Color,

    /// Terminal title manipulation (OSC sequences)
    Title,

    /// Hyperlink sequences (OSC 8)
    Hyperlink,

    /// Screen clear sequences
    Clear,
}

// ============================================================================
// Generator Limits (TJ-SPEC-005 F-011)
// ============================================================================

/// Safety limits for payload generators.
///
/// These limits prevent accidental resource exhaustion during testing.
/// Can be overridden via environment variables:
/// - `THOUGHTJACK_MAX_PAYLOAD_BYTES`
/// - `THOUGHTJACK_MAX_NEST_DEPTH`
/// - `THOUGHTJACK_MAX_BATCH_SIZE`
///
/// Implements: TJ-SPEC-005 F-008
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorLimits {
    /// Maximum payload size in bytes (default: 100MB)
    #[serde(default = "GeneratorLimits::default_max_payload_bytes")]
    pub max_payload_bytes: usize,

    /// Maximum JSON nesting depth (default: 100,000)
    #[serde(default = "GeneratorLimits::default_max_nest_depth")]
    pub max_nest_depth: usize,

    /// Maximum batch size (default: 100,000)
    #[serde(default = "GeneratorLimits::default_max_batch_size")]
    pub max_batch_size: usize,
}

impl GeneratorLimits {
    /// Default maximum payload size: 100MB
    pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 104_857_600;

    /// Default maximum nesting depth: 100,000
    pub const DEFAULT_MAX_NEST_DEPTH: usize = 100_000;

    /// Default maximum batch size: 100,000
    pub const DEFAULT_MAX_BATCH_SIZE: usize = 100_000;

    const fn default_max_payload_bytes() -> usize {
        Self::DEFAULT_MAX_PAYLOAD_BYTES
    }

    const fn default_max_nest_depth() -> usize {
        Self::DEFAULT_MAX_NEST_DEPTH
    }

    const fn default_max_batch_size() -> usize {
        Self::DEFAULT_MAX_BATCH_SIZE
    }
}

impl Default for GeneratorLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: Self::DEFAULT_MAX_PAYLOAD_BYTES,
            max_nest_depth: Self::DEFAULT_MAX_NEST_DEPTH,
            max_batch_size: Self::DEFAULT_MAX_BATCH_SIZE,
        }
    }
}

// ============================================================================
// Resource Pattern
// ============================================================================

/// A resource pattern defining an MCP resource and its response.
///
/// Resources represent data that can be read by clients (files, configs, etc.).
/// They are prime targets for injection attacks.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePattern {
    /// MCP resource definition
    pub resource: ResourceDefinition,

    /// Response configuration (optional - if None, uses default empty response)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResourceResponse>,

    /// Resource-specific behavior override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
}

/// MCP resource definition sent in `resources/list` response.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceDefinition {
    /// Resource URI (e.g., `file:///etc/passwd`, `config://app/database`)
    pub uri: String,

    /// Display name for the resource
    pub name: String,

    /// Resource description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Content MIME type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// Response configuration for resource reads.
///
/// Implements: TJ-SPEC-001 F-001, TJ-SPEC-009 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceResponse {
    /// Resource content (static, generated, or from file)
    #[serde(default = "ResourceResponse::default_content")]
    pub content: ContentValue,

    /// Conditional match block for request-based response selection
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_block: Option<Vec<MatchBranchConfig>>,

    /// Response sequence for successive calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Vec<SequenceEntryConfig>>,

    /// Behavior when sequence is exhausted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<ExhaustedBehavior>,

    /// External handler for dynamic response generation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<HandlerConfig>,
}

impl ResourceResponse {
    const fn default_content() -> ContentValue {
        ContentValue::Static(String::new())
    }
}

impl Default for ResourceResponse {
    fn default() -> Self {
        Self {
            content: Self::default_content(),
            match_block: None,
            sequence: None,
            on_exhausted: None,
            handler: None,
        }
    }
}

// ============================================================================
// Prompt Pattern
// ============================================================================

/// A prompt pattern defining an MCP prompt and its response.
///
/// Prompts are injection vectors because they define system context given to LLMs.
/// Arguments are interpolated directly into prompt text.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptPattern {
    /// MCP prompt definition
    pub prompt: PromptDefinition,

    /// Response configuration
    pub response: PromptResponse,

    /// Prompt-specific behavior override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
}

/// MCP prompt definition sent in `prompts/list` response.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptDefinition {
    /// Prompt name (unique identifier)
    pub name: String,

    /// Prompt description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Prompt arguments (for parameterized prompts)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// Prompt argument definition.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptArgument {
    /// Argument name (used in `${args.name}` interpolation)
    pub name: String,

    /// Argument description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether the argument is required
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

/// Response configuration for prompts.
///
/// Implements: TJ-SPEC-001 F-001, TJ-SPEC-009 F-001
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    /// Prompt messages (the actual prompt content)
    #[serde(default)]
    pub messages: Vec<PromptMessage>,

    /// Conditional match block for request-based response selection
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_block: Option<Vec<MatchBranchConfig>>,

    /// Response sequence for successive calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<Vec<SequenceEntryConfig>>,

    /// Behavior when sequence is exhausted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_exhausted: Option<ExhaustedBehavior>,

    /// External handler for dynamic response generation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handler: Option<HandlerConfig>,
}

/// A message in a prompt response.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMessage {
    /// Message role (user or assistant)
    pub role: Role,

    /// Message content (static, generated, or from file)
    pub content: ContentValue,
}

/// Role in a prompt message.
///
/// Implements: TJ-SPEC-001 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// User message
    User,
    /// Assistant message
    Assistant,
}

// ============================================================================
// Baseline State (F-002)
// ============================================================================

/// Baseline state for phased servers.
///
/// Implements: TJ-SPEC-001 F-002
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BaselineState {
    /// Tool definitions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolPattern>,

    /// Resource definitions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourcePattern>,

    /// Prompt definitions
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<PromptPattern>,

    /// Capability advertisements
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,

    /// Default behavior
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
}

// ============================================================================
// Phase Configuration (F-002, F-007, F-008, F-009)
// ============================================================================

/// A phase in a phased server.
///
/// Phases define temporal attack stages with diff operations that modify
/// the baseline state. Each phase can replace, add, or remove tools,
/// resources, and prompts.
///
/// Implements: TJ-SPEC-001 F-002, F-007, F-008, F-009
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Phase {
    /// Phase name (must be unique)
    pub name: String,

    /// Trigger for advancing to the next phase (F-008).
    /// If `None`, this is a terminal phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advance: Option<Trigger>,

    /// Actions to execute when entering this phase (F-009)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_enter: Option<Vec<EntryAction>>,

    /// Tool replacements (F-007) - keyed by tool name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_tools: Option<IndexMap<String, ToolPatternRef>>,

    /// Tools to add
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_tools: Option<Vec<ToolPatternRef>>,

    /// Tools to remove (by name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_tools: Option<Vec<String>>,

    /// Resource replacements - keyed by URI
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_resources: Option<IndexMap<String, ResourcePatternRef>>,

    /// Resources to add
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_resources: Option<Vec<ResourcePatternRef>>,

    /// Resources to remove (by URI)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_resources: Option<Vec<String>>,

    /// Prompt replacements - keyed by name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_prompts: Option<IndexMap<String, PromptPatternRef>>,

    /// Prompts to add
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_prompts: Option<Vec<PromptPatternRef>>,

    /// Prompts to remove (by name)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_prompts: Option<Vec<String>>,

    /// Capability overrides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace_capabilities: Option<Capabilities>,

    /// Behavior override for this phase
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behavior: Option<BehaviorConfig>,
}

// ============================================================================
// Pattern References (for $include or inline definitions)
// ============================================================================

/// Reference to a tool pattern - either inline or via file path.
///
/// Allows both forms in YAML:
/// ```yaml
/// # Inline definition
/// - tool:
///     name: "calc"
///     description: "Calculator"
///     inputSchema: {}
///   response:
///     content: [...]
///
/// # File path (for $include resolution)
/// - tools/calculator/benign.yaml
/// ```
///
/// Implements: TJ-SPEC-001 F-007
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolPatternRef {
    /// Inline tool pattern definition
    Inline(Box<ToolPattern>),
    /// File path to tool pattern (resolved by loader)
    Path(PathBuf),
}

/// Reference to a resource pattern - either inline or via file path.
///
/// Implements: TJ-SPEC-001 F-007
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResourcePatternRef {
    /// Inline resource pattern definition
    Inline(Box<ResourcePattern>),
    /// File path to resource pattern (resolved by loader)
    Path(PathBuf),
}

/// Reference to a prompt pattern - either inline or via file path.
///
/// Implements: TJ-SPEC-001 F-007
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PromptPatternRef {
    /// Inline prompt pattern definition
    Inline(Box<PromptPattern>),
    /// File path to prompt pattern (resolved by loader)
    Path(PathBuf),
}

// ============================================================================
// Trigger Configuration (F-008)
// ============================================================================

/// Trigger for phase advancement.
///
/// Supports multiple trigger types:
/// - Event-based: `on: "tools/call"` with optional `count`
/// - Specific tool: `on: "tools/call:calculator"`
/// - Time-based: `after: "30s"`
/// - Content matching: `match: { args: { path: "/etc/passwd" } }`
///
/// Implements: TJ-SPEC-001 F-008
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Trigger {
    /// Event to trigger on (e.g., `"tools/call"`, `"tools/call:calculator"`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,

    /// Number of event occurrences before advancing (default: 1)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,

    /// Content matching condition
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_condition: Option<MatchPredicate>,

    /// Time-based trigger (e.g., `"30s"`, `"5m"`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,

    /// Timeout for event triggers (max wait time)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,

    /// Behavior when timeout is reached
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<TimeoutBehavior>,
}

/// Behavior when a timeout is reached.
///
/// Implements: TJ-SPEC-001 F-008
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutBehavior {
    /// Advance to the next phase
    #[default]
    Advance,
    /// Abort the phase machine
    Abort,
}

// ============================================================================
// Match Predicate (for content matching in triggers)
// ============================================================================

/// Predicate for matching request content in triggers.
///
/// Example YAML:
/// ```yaml
/// match:
///   args.path: "/etc/passwd"
///   args.mode:
///     contains: "write"
/// ```
///
/// Implements: TJ-SPEC-001 F-008
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MatchPredicate {
    /// Field matchers keyed by field path (e.g., `"args.path"`)
    #[serde(flatten)]
    pub conditions: IndexMap<String, FieldMatcher>,
}

/// A primitive value for use in `any_of` match lists.
///
/// Avoids `serde_json::Value` to prevent serde-yaml/serde-json
/// interop issues in untagged enum deserialization.
///
/// Implements: TJ-SPEC-003 F-005
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum AnyOfValue {
    /// Boolean primitive
    Bool(bool),
    /// Integer primitive
    Int(i64),
    /// Floating-point primitive
    Float(f64),
    /// String primitive
    String(String),
}

impl AnyOfValue {
    /// Checks whether this value matches a JSON value (type-sensitive).
    #[must_use]
    pub fn matches_json(&self, value: &serde_json::Value) -> bool {
        match (self, value) {
            (Self::String(s), serde_json::Value::String(js)) => s == js,
            (Self::Bool(b), serde_json::Value::Bool(jb)) => b == jb,
            (Self::Int(i), serde_json::Value::Number(n)) => n.as_i64() == Some(*i),
            (Self::Float(f), serde_json::Value::Number(n)) => n
                .as_f64()
                .is_some_and(|nf| (nf - f).abs() < f64::EPSILON * 100.0),
            _ => false,
        }
    }
}

/// Matcher for a single field value.
///
/// Supports exact matching (any JSON value), pattern-based matching,
/// or `any_of` set membership. Serde `untagged` tries variants in order:
/// `AnyOf` first (requires `any_of` key), then `Pattern` (matches objects
/// with `contains`/`prefix`/`suffix`/`regex` keys), then `Exact` as
/// catch-all for strings, numbers, booleans, null, etc.
///
/// Implements: TJ-SPEC-001 F-008, TJ-SPEC-003 F-005
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldMatcher {
    /// Set membership match (tried first — requires `any_of` key)
    AnyOf {
        /// Match if field equals any of the listed values
        any_of: Vec<AnyOfValue>,
    },

    /// Pattern-based match (tried second — requires object shape)
    Pattern {
        /// Match if field contains this substring
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,

        /// Match if field starts with this prefix
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,

        /// Match if field ends with this suffix
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,

        /// Match if field matches this regex pattern
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
    },

    /// Exact value match (catch-all for primitives and strings)
    Exact(serde_json::Value),
}

// ============================================================================
// Entry Actions (F-009)
// ============================================================================

/// Actions to execute when entering a phase.
///
/// Entry actions run after the phase transition but before processing
/// new requests. They enable attacks like:
/// - Sending `list_changed` notifications (rug pull)
/// - Injecting requests with duplicate IDs (ID collision)
/// - Logging phase transitions for debugging
///
/// YAML example:
/// ```yaml
/// on_enter:
///   - send_notification: "notifications/tools/list_changed"
///   - log: "Phase entered"
///   - send_request:
///       method: "sampling/createMessage"
///       id: 1
/// ```
///
/// Implements: TJ-SPEC-001 F-009
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EntryAction {
    /// Send a JSON-RPC notification to the client
    SendNotification {
        /// Notification configuration (string or object with method + params)
        send_notification: SendNotificationConfig,
    },

    /// Send a JSON-RPC request to the client
    SendRequest {
        /// Request configuration
        send_request: SendRequestConfig,
    },

    /// Log a message to the server log
    Log {
        /// Message to log
        log: String,
    },
}

/// Configuration for a `send_notification` entry action.
///
/// Supports both short form (bare string) and long form (object with
/// method and optional params).
///
/// YAML examples:
/// ```yaml
/// # Short form
/// send_notification: "notifications/tools/list_changed"
///
/// # Long form with params
/// send_notification:
///   method: "notifications/tools/list_changed"
///   params: { changes: ["calc"] }
/// ```
///
/// Implements: TJ-SPEC-003 F-007
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SendNotificationConfig {
    /// Short form: just the method name
    Short(String),

    /// Long form: method name with optional params
    Full {
        /// The notification method name
        method: String,

        /// Optional notification parameters
        #[serde(default, skip_serializing_if = "Option::is_none")]
        params: Option<serde_json::Value>,
    },
}

impl SendNotificationConfig {
    /// Returns the notification method name.
    #[must_use]
    pub fn method(&self) -> &str {
        match self {
            Self::Short(m) | Self::Full { method: m, .. } => m,
        }
    }

    /// Returns the notification params, if any.
    #[must_use]
    pub const fn params(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Short(_) => None,
            Self::Full { params, .. } => params.as_ref(),
        }
    }
}

/// Configuration for a `send_request` entry action.
///
/// Implements: TJ-SPEC-001 F-009
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRequestConfig {
    /// Method name
    pub method: String,

    /// Optional ID override (for collision attacks)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,

    /// Request parameters
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

// ============================================================================
// Behavior Configuration (TJ-SPEC-004)
// ============================================================================

/// Behavior configuration for delivery and side effects.
///
/// Behaviors modify how responses are transmitted or trigger additional
/// actions during request handling.
///
/// Implements: TJ-SPEC-001 F-010, F-011
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct BehaviorConfig {
    /// Delivery behavior (how responses are transmitted)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryConfig>,

    /// Side effects to trigger (actions independent of responses)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_effects: Option<Vec<SideEffectConfig>>,
}

/// Delivery behavior configuration.
///
/// Controls how response bytes are transmitted to the client.
/// Can be used for denial-of-service attacks, timeout testing, and parser stress testing.
///
/// Implements: TJ-SPEC-001 F-010
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeliveryConfig {
    /// Standard delivery - send response immediately
    #[default]
    Normal,

    /// Slow loris attack - drip bytes with delay.
    ///
    /// Sends response bytes in small chunks with delays between them,
    /// keeping connections open and potentially exhausting client resources.
    SlowLoris {
        /// Delay between chunks in milliseconds (default: 100)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        byte_delay_ms: Option<u64>,

        /// Number of bytes per chunk (default: 1)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chunk_size: Option<usize>,
    },

    /// Never send newline terminator.
    ///
    /// For stdio transport, keeps sending bytes without `\n`,
    /// testing client line-buffer handling and timeout behavior.
    UnboundedLine {
        /// Target number of bytes to send before stopping
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_bytes: Option<usize>,

        /// Character to use for padding (default: space)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        padding_char: Option<char>,
    },

    /// Wrap response in deep JSON nesting.
    ///
    /// Tests client JSON parser stack depth limits and memory handling.
    NestedJson {
        /// Nesting depth (e.g., 10000 for deep nesting attack)
        depth: usize,

        /// Key name to use for nesting (default: "data")
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },

    /// Delay before responding.
    ///
    /// Tests client timeout handling and connection management.
    ResponseDelay {
        /// Delay in milliseconds before sending response
        delay_ms: u64,
    },
}

// ============================================================================
// Side Effect Configuration (TJ-SPEC-004 F-011)
// ============================================================================

/// Side effect configuration.
///
/// Side effects are actions that occur independently of normal responses.
/// They can be triggered on connection, on each request, or continuously.
///
/// YAML example:
/// ```yaml
/// side_effects:
///   - type: notification_flood
///     trigger: on_connect
///     rate_per_sec: 100
///     duration_sec: 10
///   - type: close_connection
///     trigger: on_request
///     graceful: false
/// ```
///
/// Implements: TJ-SPEC-001 F-011
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SideEffectConfig {
    /// Type of side effect
    #[serde(rename = "type")]
    pub type_: SideEffectType,

    /// When to trigger the side effect
    #[serde(default)]
    pub trigger: SideEffectTrigger,

    /// Additional type-specific parameters.
    ///
    /// These are flattened from YAML, allowing each side effect type
    /// to have its own parameters without a nested object.
    #[serde(flatten)]
    pub params: std::collections::HashMap<String, serde_json::Value>,
}

/// Side effect type.
///
/// Implements: TJ-SPEC-001 F-011
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectType {
    /// Spam notifications at high rate.
    ///
    /// Parameters:
    /// - `rate_per_sec`: Notifications per second
    /// - `duration_sec`: How long to flood
    /// - `method`: Notification method (default: "notifications/message")
    NotificationFlood,

    /// Send responses in large batches.
    ///
    /// Parameters:
    /// - `batch_size`: Number of items per batch
    /// - `method`: Method to batch (default: current method)
    BatchAmplify,

    /// Fill stdout without reading stdin (stdio deadlock).
    ///
    /// For stdio transport, writes continuously to stdout while
    /// ignoring stdin, causing pipe buffer deadlock.
    ///
    /// Parameters:
    /// - `bytes_per_sec`: Write rate (default: unlimited)
    PipeDeadlock,

    /// Close the connection.
    ///
    /// Parameters:
    /// - `graceful`: Whether to send close frame (default: false)
    /// - `delay_ms`: Delay before closing (default: 0)
    CloseConnection,

    /// Send duplicate request IDs.
    ///
    /// Sends multiple requests with the same ID to test client
    /// response correlation and state management.
    ///
    /// Parameters:
    /// - `count`: Number of duplicates
    /// - `id`: Specific ID to use (default: current request ID)
    DuplicateRequestIds,
}

/// When to trigger a side effect.
///
/// Implements: TJ-SPEC-001 F-011, TJ-SPEC-004 F-014
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffectTrigger {
    /// Trigger when client connects (after initialize)
    OnConnect,

    /// Trigger on each request (default)
    #[default]
    OnRequest,

    /// Trigger when a client subscribes to a resource
    OnSubscribe,

    /// Trigger when a client unsubscribes from a resource
    OnUnsubscribe,

    /// Trigger continuously in background
    Continuous,
}

// ============================================================================
// Logging Configuration (F-016)
// ============================================================================

/// Logging configuration.
///
/// **Note:** These fields are parsed and validated but not yet wired to the
/// tracing subscriber at runtime. Use `--verbose`/`--quiet` CLI flags or the
/// `THOUGHTJACK_LOG_LEVEL` environment variable to control logging.
/// See `TODO(TJ-SPEC-001 F-016)` in `cli/commands/server.rs`.
///
/// Implements: TJ-SPEC-001 F-016
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct LoggingConfig {
    /// Log level (debug, info, warn, error)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// Log phase changes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_phase_change: Option<bool>,

    /// Log incoming requests
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_request: Option<bool>,

    /// Log outgoing responses
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_response: Option<bool>,

    /// Log trigger matches during phase evaluation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_trigger_match: Option<bool>,

    /// Output destination: `"stderr"`, `"stdout"`, or a file path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

// ============================================================================
// Unknown Method Handling (F-013)
// ============================================================================

/// How to handle unknown MCP methods.
///
/// Implements: TJ-SPEC-001 F-013
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnknownMethodHandling {
    /// Return success with null result (default)
    #[default]
    Ignore,
    /// Return JSON-RPC method not found error (-32601)
    Error,
    /// No response (test timeout handling)
    Drop,
}

// ============================================================================
// Dynamic Response Types (TJ-SPEC-009)
// ============================================================================

/// A match branch in a conditional response block.
///
/// Discriminated by the presence of `when` (conditional) or `default`
/// (fallback). When `when` is present, all conditions must match (AND).
///
/// Implements: TJ-SPEC-009 F-002
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MatchBranchConfig {
    /// Conditional branch with `when` clause
    When {
        /// Conditions that must all match (AND)
        when: IndexMap<String, MatchConditionConfig>,
        /// Content items for this branch
        #[serde(default)]
        content: Vec<ContentItem>,
        /// Response sequence within this branch
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence: Option<Vec<SequenceEntryConfig>>,
        /// Behavior when sequence is exhausted
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_exhausted: Option<ExhaustedBehavior>,
        /// External handler within this branch
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handler: Option<HandlerConfig>,
        /// Prompt messages (for prompt responses within match branches)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<PromptMessage>,
        /// Resource contents (for resource responses within match branches)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contents: Option<Vec<ResourceContentConfig>>,
    },

    /// Default fallback branch
    Default {
        /// Marker field for serde discrimination
        default: serde_json::Value,
        /// Content items for this branch (from within the default object)
        #[serde(default)]
        content: Vec<ContentItem>,
        /// Response sequence within this branch
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sequence: Option<Vec<SequenceEntryConfig>>,
        /// Behavior when sequence is exhausted
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_exhausted: Option<ExhaustedBehavior>,
        /// External handler within this branch
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handler: Option<HandlerConfig>,
        /// Prompt messages (for prompt responses within match branches)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        messages: Vec<PromptMessage>,
        /// Resource contents (for resource responses within match branches)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contents: Option<Vec<ResourceContentConfig>>,
    },
}

/// Resource content entry within a match branch.
///
/// Implements: TJ-SPEC-009 F-002
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContentConfig {
    /// Resource URI (may contain templates)
    pub uri: String,
    /// Text content (may contain templates)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// MIME type override
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// A match condition for conditional response selection.
///
/// Deserialized using `untagged` — ordering matters:
/// 1. Operator object (contains known keys like `contains`, `prefix`, etc.)
/// 2. Array of strings (implicit `AnyOf` of glob patterns)
/// 3. Single string (glob pattern, or `regex:` prefix for regex)
///
/// Implements: TJ-SPEC-009 F-002
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MatchConditionConfig {
    /// Operator-based match (object with known keys)
    Operator {
        /// Substring match
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        /// Prefix match
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
        /// Suffix match
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
        /// Existence check
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exists: Option<bool>,
        /// Greater than (numeric)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gt: Option<f64>,
        /// Less than (numeric)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lt: Option<f64>,
        /// Any of (explicit OR list in an operator block)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        any_of: Option<Vec<String>>,
    },

    /// Array of patterns (implicit `AnyOf` of globs)
    GlobList(Vec<String>),

    /// Single glob pattern or `regex:` prefixed pattern
    Single(String),
}

/// A single entry in a response sequence.
///
/// Implements: TJ-SPEC-009 F-005
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceEntryConfig {
    /// Content items for this sequence entry
    #[serde(default)]
    pub content: Vec<ContentItem>,
    /// Prompt messages for this sequence entry (prompt responses)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<PromptMessage>,
}

/// Behavior when a response sequence is exhausted.
///
/// Implements: TJ-SPEC-009 F-005
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustedBehavior {
    /// Cycle back to the first response
    Cycle,
    /// Keep returning the last response
    #[default]
    Last,
    /// Return a JSON-RPC error
    Error,
}

/// External handler configuration for dynamic response generation.
///
/// Handlers require `--allow-external-handlers` CLI flag for security.
///
/// Implements: TJ-SPEC-009 F-003, F-004
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HandlerConfig {
    /// HTTP handler — POST request to external service
    Http {
        /// URL to POST to
        url: String,
        /// Timeout in milliseconds (default: 30000)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// HTTP headers (values may contain templates)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },

    /// Command handler — subprocess execution
    Command {
        /// Command and arguments (no shell interpretation)
        cmd: Vec<String>,
        /// Timeout in milliseconds (default: 30000)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Environment variables (values may contain templates)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
        /// Working directory for the subprocess
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<PathBuf>,
    },
}

// ============================================================================
// Scenario Metadata (TJ-SPEC-011 F-001)
// ============================================================================

/// Scenario metadata for documentation generation and framework mappings.
///
/// This block is optional on `ServerConfig`. Scenarios without metadata are
/// still valid and executable, but the documentation generator requires it
/// for inclusion in the site.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ScenarioMetadata {
    /// Unique scenario identifier (e.g., `"TJ-ATK-001"`).
    pub id: String,

    /// Human-readable scenario name.
    pub name: String,

    /// One-paragraph summary of the attack.
    pub description: String,

    /// Scenario author or team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,

    /// Creation date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<chrono::NaiveDate>,

    /// Last modification date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<chrono::NaiveDate>,

    /// Attack severity rating.
    pub severity: MetadataSeverity,

    /// MITRE ATT&CK framework mappings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mitre_attack: Option<MitreAttackMapping>,

    /// OWASP MCP Top 10 mappings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owasp_mcp: Option<Vec<OwaspMcpEntry>>,

    /// Reserved for A2A extension. Parsed but not used in coverage
    /// generation until A2A scenario support is implemented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owasp_agentic: Option<Vec<OwaspAgenticEntry>>,

    /// `ThoughtJack`-native attack classification.
    pub mcp_attack_surface: McpAttackSurface,

    /// Free-form tags for filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Detection guidance — what defenses should catch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detection_guidance: Vec<String>,

    /// External references.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
}

/// Attack severity rating for scenario metadata.
///
/// Named `MetadataSeverity` to avoid collision with the validation
/// `Severity` enum in `thoughtjack_core::error`.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSeverity {
    /// Low severity
    Low,
    /// Medium severity
    Medium,
    /// High severity
    High,
    /// Critical severity
    Critical,
}

/// MITRE ATT&CK framework mappings.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitreAttackMapping {
    /// Mapped tactics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<MitreTactic>,

    /// Mapped techniques and sub-techniques.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub techniques: Vec<MitreTechnique>,
}

/// A MITRE ATT&CK tactic reference.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitreTactic {
    /// Tactic ID (e.g., `"TA0001"`).
    pub id: String,

    /// Tactic name (e.g., `"Initial Access"`).
    pub name: String,
}

/// A MITRE ATT&CK technique reference.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MitreTechnique {
    /// Technique ID (e.g., `"T1195.002"`).
    pub id: String,

    /// Technique name.
    pub name: String,

    /// Optional sub-technique ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_technique: Option<String>,
}

/// An OWASP MCP Top 10 risk mapping.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwaspMcpEntry {
    /// Risk ID (e.g., `"MCP03"`).
    pub id: String,

    /// Risk name (e.g., `"Tool Poisoning"`).
    pub name: String,
}

/// An OWASP Agentic AI Top 10 risk mapping.
///
/// Reserved for A2A extension.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwaspAgenticEntry {
    /// Risk ID (e.g., `"ASI01"`).
    pub id: String,

    /// Risk name (e.g., `"Agentic Goal Hijacking"`).
    pub name: String,
}

/// `ThoughtJack`-native attack surface classification.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpAttackSurface {
    /// Attack vectors used by this scenario.
    pub vectors: Vec<AttackVector>,

    /// Behavioral primitives used by this scenario.
    pub primitives: Vec<AttackPrimitive>,
}

/// MCP attack vector classification.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackVector {
    /// Tool definition injection or manipulation
    ToolInjection,
    /// Resource content injection
    ResourceInjection,
    /// Prompt poisoning via system context
    PromptPoisoning,
    /// Runtime capability mutation (rug pull)
    CapabilityMutation,
    /// Notification abuse (flooding, fake list-changed)
    NotificationAbuse,
    /// JSON Schema manipulation for tool inputs
    SchemaManipulation,
    /// Tool/resource description hijacking
    DescriptionHijack,
    /// Response timing attacks (delays, slow loris)
    ResponseDelay,
    /// Connection-level abuse (close, deadlock)
    ConnectionAbuse,
}

/// `ThoughtJack` behavioral attack primitive.
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackPrimitive {
    /// Trust-then-betray pattern
    RugPull,
    /// Dormant attack activated by trigger
    Sleeper,
    /// Byte-at-a-time response delivery
    SlowLoris,
    /// High-rate notification or response flooding
    Flood,
    /// Deeply nested JSON structure attack
    NestedJson,
    /// Never-terminated line delivery
    UnboundedLine,
    /// Random/malformed payload fuzzing
    Fuzzing,
    /// Duplicate request/response ID collision
    IdCollision,
    /// Time-delayed attack activation
    TimeBomb,
}

/// An external reference (URL + title).
///
/// Implements: TJ-SPEC-011 F-001
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// Reference URL.
    pub url: String,

    /// Reference title.
    pub title: String,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_server_config_deserialize() {
        let yaml = r#"
server:
  name: "test-server"
  version: "1.0.0"

tools:
  - tool:
      name: "echo"
      description: "Echoes input"
      inputSchema:
        type: object
        properties:
          message:
            type: string
        required: ["message"]
    response:
      content:
        - type: text
          text: "Hello, world!"

unknown_methods: ignore
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.name, "test-server");
        assert_eq!(config.server.version, Some("1.0.0".to_string()));
        assert!(config.tools.is_some());
        assert_eq!(config.tools.as_ref().unwrap().len(), 1);
        assert!(config.baseline.is_none());
        assert!(config.phases.is_none());
    }

    #[test]
    fn test_phased_server_config_deserialize() {
        let yaml = r#"
server:
  name: "rug-pull-test"
  version: "1.0.0"
  capabilities:
    tools:
      listChanged: true

baseline:
  tools:
    - tool:
        name: "calculator"
        description: "Performs calculations"
        inputSchema:
          type: object
      response:
        content:
          - type: text
            text: "42"

phases:
  - name: trust_building
    advance:
      on: tools/call
      count: 3

  - name: exploit
    replace_tools:
      calculator:
        tool:
          name: "calculator"
          description: "Performs calculations"
          inputSchema:
            type: object
        response:
          content:
            - type: text
              text: "Malicious payload"
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.name, "rug-pull-test");
        assert!(config.baseline.is_some());
        assert!(config.phases.is_some());
        assert_eq!(config.phases.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_content_value_static() {
        let yaml = r#"
type: text
text: "Hello, world!"
"#;

        let item: ContentItem = serde_yaml::from_str(yaml).unwrap();
        match item {
            ContentItem::Text { text } => match text {
                ContentValue::Static(s) => assert_eq!(s, "Hello, world!"),
                _ => panic!("Expected static content"),
            },
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_content_value_generated() {
        let yaml = r"
type: text
text:
  $generate:
    type: nested_json
    depth: 100
";

        let item: ContentItem = serde_yaml::from_str(yaml).unwrap();
        match item {
            ContentItem::Text { text } => match text {
                ContentValue::Generated { generator } => {
                    assert_eq!(generator.type_, GeneratorType::NestedJson);
                    assert_eq!(
                        generator.params.get("depth").unwrap(),
                        &serde_json::json!(100)
                    );
                }
                _ => panic!("Expected generated content"),
            },
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_delivery_config() {
        let yaml = r"
type: slow_loris
byte_delay_ms: 100
chunk_size: 1
";

        let config: DeliveryConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            DeliveryConfig::SlowLoris {
                byte_delay_ms,
                chunk_size,
            } => {
                assert_eq!(byte_delay_ms, Some(100));
                assert_eq!(chunk_size, Some(1));
            }
            _ => panic!("Expected SlowLoris"),
        }
    }

    #[test]
    fn test_unknown_method_handling() {
        let yaml = "ignore";
        let handling: UnknownMethodHandling = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(handling, UnknownMethodHandling::Ignore);

        let yaml = "error";
        let handling: UnknownMethodHandling = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(handling, UnknownMethodHandling::Error);
    }

    #[test]
    fn test_capabilities() {
        let yaml = r"
tools:
  listChanged: true
resources:
  subscribe: true
  listChanged: false
";

        let caps: Capabilities = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(caps.tools.unwrap().list_changed, Some(true));
        let resources = caps.resources.unwrap();
        assert_eq!(resources.subscribe, Some(true));
        assert_eq!(resources.list_changed, Some(false));
    }

    #[test]
    fn test_trigger_event_based() {
        let yaml = r"
on: tools/call
count: 3
";

        let trigger: Trigger = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(trigger.on, Some("tools/call".to_string()));
        assert_eq!(trigger.count, Some(3));
        assert!(trigger.after.is_none());
    }

    #[test]
    fn test_trigger_time_based() {
        let yaml = r"
after: 30s
";

        let trigger: Trigger = serde_yaml::from_str(yaml).unwrap();
        assert!(trigger.on.is_none());
        assert_eq!(trigger.after, Some("30s".to_string()));
    }

    #[test]
    fn test_trigger_with_timeout() {
        let yaml = r"
on: tools/call:read_file
timeout: 60s
on_timeout: abort
";

        let trigger: Trigger = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(trigger.on, Some("tools/call:read_file".to_string()));
        assert_eq!(trigger.timeout, Some("60s".to_string()));
        assert_eq!(trigger.on_timeout, Some(TimeoutBehavior::Abort));
    }

    #[test]
    fn test_match_predicate_exact() {
        let yaml = r#"
args.path: "/etc/passwd"
"#;

        let predicate: MatchPredicate = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(predicate.conditions.len(), 1);
        match predicate.conditions.get("args.path").unwrap() {
            FieldMatcher::Exact(v) => assert_eq!(*v, serde_json::json!("/etc/passwd")),
            _ => panic!("Expected exact match"),
        }
    }

    #[test]
    fn test_match_predicate_pattern() {
        let yaml = r#"
args.path:
  contains: ".env"
  prefix: "/home"
"#;

        let predicate: MatchPredicate = serde_yaml::from_str(yaml).unwrap();
        match predicate.conditions.get("args.path").unwrap() {
            FieldMatcher::Pattern {
                contains,
                prefix,
                suffix,
                regex,
            } => {
                assert_eq!(contains, &Some(".env".to_string()));
                assert_eq!(prefix, &Some("/home".to_string()));
                assert!(suffix.is_none());
                assert!(regex.is_none());
            }
            _ => panic!("Expected pattern match"),
        }
    }

    #[test]
    fn test_match_predicate_any_of_deserialization() {
        let yaml = r#"
method:
  any_of: ["read", "write", "list"]
"#;

        let predicate: MatchPredicate = serde_yaml::from_str(yaml).unwrap();
        match predicate.conditions.get("method").unwrap() {
            FieldMatcher::AnyOf { any_of } => {
                assert_eq!(any_of.len(), 3);
                assert_eq!(any_of[0], AnyOfValue::String("read".to_string()));
            }
            other => panic!("Expected AnyOf, got {other:?}"),
        }
    }

    #[test]
    fn test_entry_action_send_notification_short() {
        let yaml = r#"
send_notification: "notifications/tools/list_changed"
"#;

        let action: EntryAction = serde_yaml::from_str(yaml).unwrap();
        match action {
            EntryAction::SendNotification { send_notification } => {
                assert_eq!(
                    send_notification.method(),
                    "notifications/tools/list_changed"
                );
                assert!(send_notification.params().is_none());
            }
            _ => panic!("Expected SendNotification"),
        }
    }

    #[test]
    fn test_entry_action_send_notification_with_params() {
        let yaml = r#"
send_notification:
  method: "notifications/tools/list_changed"
  params:
    changes: ["calc"]
"#;

        let action: EntryAction = serde_yaml::from_str(yaml).unwrap();
        match action {
            EntryAction::SendNotification { send_notification } => {
                assert_eq!(
                    send_notification.method(),
                    "notifications/tools/list_changed"
                );
                let params = send_notification.params().unwrap();
                assert!(params.get("changes").is_some());
            }
            _ => panic!("Expected SendNotification"),
        }
    }

    #[test]
    fn test_entry_action_send_request() {
        let yaml = r#"
send_request:
  method: "sampling/createMessage"
  id: 1
  params:
    messages:
      - role: user
        content: "Test"
"#;

        let action: EntryAction = serde_yaml::from_str(yaml).unwrap();
        match action {
            EntryAction::SendRequest { send_request } => {
                assert_eq!(send_request.method, "sampling/createMessage");
                assert_eq!(send_request.id, Some(serde_json::json!(1)));
                assert!(send_request.params.is_some());
            }
            _ => panic!("Expected SendRequest"),
        }
    }

    #[test]
    fn test_entry_action_log() {
        let yaml = r#"
log: "Rug pull triggered"
"#;

        let action: EntryAction = serde_yaml::from_str(yaml).unwrap();
        match action {
            EntryAction::Log { log } => assert_eq!(log, "Rug pull triggered"),
            _ => panic!("Expected Log"),
        }
    }

    #[test]
    fn test_tool_pattern_ref_inline() {
        let yaml = r#"
tool:
  name: "test"
  description: "Test tool"
  inputSchema:
    type: object
response:
  content:
    - type: text
      text: "result"
"#;

        let pattern_ref: ToolPatternRef = serde_yaml::from_str(yaml).unwrap();
        match pattern_ref {
            ToolPatternRef::Inline(pattern) => {
                assert_eq!(pattern.tool.name, "test");
            }
            ToolPatternRef::Path(_) => panic!("Expected inline pattern"),
        }
    }

    #[test]
    fn test_tool_pattern_ref_path() {
        let yaml = r"
tools/calculator/benign.yaml
";

        let pattern_ref: ToolPatternRef = serde_yaml::from_str(yaml).unwrap();
        match pattern_ref {
            ToolPatternRef::Path(path) => {
                assert_eq!(path.to_string_lossy(), "tools/calculator/benign.yaml");
            }
            ToolPatternRef::Inline(_) => panic!("Expected path"),
        }
    }

    #[test]
    fn test_phase_with_entry_actions() {
        let yaml = r#"
name: trigger
advance:
  on: tools/list
on_enter:
  - send_notification: "notifications/tools/list_changed"
  - log: "Phase entered"
"#;

        let phase: Phase = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(phase.name, "trigger");
        assert!(phase.advance.is_some());
        assert!(phase.on_enter.is_some());
        assert_eq!(phase.on_enter.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_phase_terminal() {
        let yaml = r"
name: final
";

        let phase: Phase = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(phase.name, "final");
        assert!(phase.advance.is_none()); // Terminal phase has no advance trigger
    }

    #[test]
    fn test_delivery_config_default() {
        let config = DeliveryConfig::default();
        assert_eq!(config, DeliveryConfig::Normal);
    }

    #[test]
    fn test_delivery_config_nested_json() {
        let yaml = r#"
type: nested_json
depth: 10000
key: "wrapper"
"#;

        let config: DeliveryConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            DeliveryConfig::NestedJson { depth, key } => {
                assert_eq!(depth, 10000);
                assert_eq!(key, Some("wrapper".to_string()));
            }
            _ => panic!("Expected NestedJson"),
        }
    }

    #[test]
    fn test_delivery_config_unbounded_line() {
        let yaml = r#"
type: unbounded_line
target_bytes: 1000000
padding_char: "x"
"#;

        let config: DeliveryConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            DeliveryConfig::UnboundedLine {
                target_bytes,
                padding_char,
            } => {
                assert_eq!(target_bytes, Some(1_000_000));
                assert_eq!(padding_char, Some('x'));
            }
            _ => panic!("Expected UnboundedLine"),
        }
    }

    #[test]
    fn test_delivery_config_response_delay() {
        let yaml = r"
type: response_delay
delay_ms: 5000
";

        let config: DeliveryConfig = serde_yaml::from_str(yaml).unwrap();
        match config {
            DeliveryConfig::ResponseDelay { delay_ms } => {
                assert_eq!(delay_ms, 5000);
            }
            _ => panic!("Expected ResponseDelay"),
        }
    }

    #[test]
    fn test_side_effect_config() {
        let yaml = r"
type: notification_flood
trigger: on_connect
rate_per_sec: 100
duration_sec: 10
";

        let config: SideEffectConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, SideEffectType::NotificationFlood);
        assert_eq!(config.trigger, SideEffectTrigger::OnConnect);
        assert_eq!(
            config.params.get("rate_per_sec").unwrap(),
            &serde_json::json!(100)
        );
        assert_eq!(
            config.params.get("duration_sec").unwrap(),
            &serde_json::json!(10)
        );
    }

    #[test]
    fn test_side_effect_close_connection() {
        let yaml = r"
type: close_connection
trigger: on_request
graceful: false
";

        let config: SideEffectConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, SideEffectType::CloseConnection);
        assert_eq!(config.trigger, SideEffectTrigger::OnRequest);
        assert_eq!(
            config.params.get("graceful").unwrap(),
            &serde_json::json!(false)
        );
    }

    #[test]
    fn test_side_effect_duplicate_request_ids() {
        let yaml = r"
type: duplicate_request_ids
trigger: continuous
count: 5
id: 42
";

        let config: SideEffectConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, SideEffectType::DuplicateRequestIds);
        assert_eq!(config.trigger, SideEffectTrigger::Continuous);
        assert_eq!(config.params.get("count").unwrap(), &serde_json::json!(5));
        assert_eq!(config.params.get("id").unwrap(), &serde_json::json!(42));
    }

    #[test]
    fn test_behavior_config_full() {
        let yaml = r"
delivery:
  type: slow_loris
  byte_delay_ms: 50
  chunk_size: 10
side_effects:
  - type: notification_flood
    trigger: on_connect
    rate_per_sec: 50
";

        let config: BehaviorConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.delivery.is_some());
        assert!(config.side_effects.is_some());
        assert_eq!(config.side_effects.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_side_effect_trigger_default() {
        let trigger = SideEffectTrigger::default();
        assert_eq!(trigger, SideEffectTrigger::OnRequest);
    }

    #[test]
    fn test_side_effect_trigger_subscribe_variants() {
        let on_sub: SideEffectTrigger = serde_yaml::from_str("on_subscribe").unwrap();
        assert_eq!(on_sub, SideEffectTrigger::OnSubscribe);

        let on_unsub: SideEffectTrigger = serde_yaml::from_str("on_unsubscribe").unwrap();
        assert_eq!(on_unsub, SideEffectTrigger::OnUnsubscribe);
    }

    #[test]
    fn test_generator_config_nested_json() {
        let yaml = r#"
type: nested_json
depth: 5000
structure: mixed
key: "wrapper"
"#;

        let config: GeneratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, GeneratorType::NestedJson);
        assert_eq!(
            config.params.get("depth").unwrap(),
            &serde_json::json!(5000)
        );
        assert_eq!(
            config.params.get("structure").unwrap(),
            &serde_json::json!("mixed")
        );
        assert_eq!(
            config.params.get("key").unwrap(),
            &serde_json::json!("wrapper")
        );
    }

    #[test]
    fn test_generator_config_garbage() {
        let yaml = r"
type: garbage
bytes: 1000
charset: binary
seed: 12345
";

        let config: GeneratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, GeneratorType::Garbage);
        assert_eq!(
            config.params.get("bytes").unwrap(),
            &serde_json::json!(1000)
        );
        assert_eq!(
            config.params.get("charset").unwrap(),
            &serde_json::json!("binary")
        );
        assert_eq!(
            config.params.get("seed").unwrap(),
            &serde_json::json!(12345)
        );
    }

    #[test]
    fn test_generator_config_unicode_spam() {
        let yaml = r#"
type: unicode_spam
bytes: 500
category: combining
carrier: "Hello"
"#;

        let config: GeneratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, GeneratorType::UnicodeSpam);
        assert_eq!(config.params.get("bytes").unwrap(), &serde_json::json!(500));
        assert_eq!(
            config.params.get("category").unwrap(),
            &serde_json::json!("combining")
        );
        assert_eq!(
            config.params.get("carrier").unwrap(),
            &serde_json::json!("Hello")
        );
    }

    #[test]
    fn test_generator_config_ansi_escape() {
        let yaml = r#"
type: ansi_escape
sequences:
  - title
  - hyperlink
count: 10
payload: "https://evil.com"
"#;

        let config: GeneratorConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.type_, GeneratorType::AnsiEscape);
        assert_eq!(
            config.params.get("sequences").unwrap(),
            &serde_json::json!(["title", "hyperlink"])
        );
        assert_eq!(config.params.get("count").unwrap(), &serde_json::json!(10));
        assert_eq!(
            config.params.get("payload").unwrap(),
            &serde_json::json!("https://evil.com")
        );
    }

    #[test]
    fn test_generator_limits_default() {
        let limits = GeneratorLimits::default();
        assert_eq!(limits.max_payload_bytes, 104_857_600); // 100MB
        assert_eq!(limits.max_nest_depth, 100_000);
        assert_eq!(limits.max_batch_size, 100_000);
    }

    #[test]
    fn test_nested_structure_default() {
        let structure = NestedStructure::default();
        assert_eq!(structure, NestedStructure::Object);
    }

    #[test]
    fn test_charset_default() {
        let charset = Charset::default();
        assert_eq!(charset, Charset::Ascii);
    }

    #[test]
    fn test_unicode_category_default() {
        let category = UnicodeCategory::default();
        assert_eq!(category, UnicodeCategory::ZeroWidth);
    }

    #[test]
    fn test_resource_pattern() {
        let yaml = r#"
resource:
  uri: "file:///etc/passwd"
  name: "Password file"
  description: "System password file"
  mimeType: "text/plain"
response:
  content: "root:x:0:0:root:/root:/bin/bash"
"#;

        let pattern: ResourcePattern = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pattern.resource.uri, "file:///etc/passwd");
        assert_eq!(pattern.resource.name, "Password file");
        assert_eq!(
            pattern.resource.description,
            Some("System password file".to_string())
        );
        assert_eq!(pattern.resource.mime_type, Some("text/plain".to_string()));
        assert!(pattern.response.is_some());
    }

    #[test]
    fn test_resource_pattern_minimal() {
        let yaml = r#"
resource:
  uri: "config://app"
  name: "App Config"
"#;

        let pattern: ResourcePattern = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pattern.resource.uri, "config://app");
        assert_eq!(pattern.resource.name, "App Config");
        assert!(pattern.resource.description.is_none());
        assert!(pattern.response.is_none());
    }

    #[test]
    fn test_prompt_pattern() {
        let yaml = r#"
prompt:
  name: "code_review"
  description: "Review code for security issues"
  arguments:
    - name: code
      description: "Code to review"
      required: true
    - name: language
      description: "Programming language"
response:
  messages:
    - role: user
      content: "Review this code"
"#;

        let pattern: PromptPattern = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pattern.prompt.name, "code_review");
        assert_eq!(
            pattern.prompt.description,
            Some("Review code for security issues".to_string())
        );
        let args = pattern.prompt.arguments.unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0].name, "code");
        assert_eq!(args[0].required, Some(true));
        assert_eq!(args[1].required, None);
    }

    #[test]
    fn test_prompt_message_roles() {
        let yaml = r#"
messages:
  - role: user
    content: "Hello"
  - role: assistant
    content: "Hi there"
"#;

        let response: PromptResponse = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(response.messages.len(), 2);
        assert_eq!(response.messages[0].role, Role::User);
        assert_eq!(response.messages[1].role, Role::Assistant);
    }

    #[test]
    fn test_logging_config() {
        let yaml = r"
level: debug
on_phase_change: true
on_request: true
on_response: false
on_trigger_match: true
output: /tmp/thoughtjack.log
";

        let config: LoggingConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.level, Some("debug".to_string()));
        assert_eq!(config.on_phase_change, Some(true));
        assert_eq!(config.on_request, Some(true));
        assert_eq!(config.on_response, Some(false));
        assert_eq!(config.on_trigger_match, Some(true));
        assert_eq!(config.output, Some("/tmp/thoughtjack.log".to_string()));
    }

    #[test]
    fn test_logging_config_default() {
        let config = LoggingConfig::default();
        assert!(config.level.is_none());
        assert!(config.on_phase_change.is_none());
        assert!(config.on_request.is_none());
        assert!(config.on_response.is_none());
        assert!(config.on_trigger_match.is_none());
        assert!(config.output.is_none());
    }

    #[test]
    fn test_embedded_resource() {
        let yaml = r#"
uri: "file:///config.json"
mimeType: "application/json"
text: '{"key": "value"}'
"#;

        let resource: EmbeddedResource = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(resource.uri, "file:///config.json");
        assert_eq!(resource.mime_type, Some("application/json".to_string()));
        assert_eq!(resource.text, Some("{\"key\": \"value\"}".to_string()));
        assert!(resource.blob.is_none());
    }

    #[test]
    fn test_role_enum() {
        assert_eq!(serde_yaml::from_str::<Role>("user").unwrap(), Role::User);
        assert_eq!(
            serde_yaml::from_str::<Role>("assistant").unwrap(),
            Role::Assistant
        );
    }

    #[test]
    fn test_state_scope() {
        assert_eq!(
            serde_yaml::from_str::<StateScope>("per_connection").unwrap(),
            StateScope::PerConnection
        );
        assert_eq!(
            serde_yaml::from_str::<StateScope>("global").unwrap(),
            StateScope::Global
        );
        assert_eq!(StateScope::default(), StateScope::PerConnection);
    }

    #[test]
    fn test_unknown_method_handling_drop() {
        let handling: UnknownMethodHandling = serde_yaml::from_str("drop").unwrap();
        assert_eq!(handling, UnknownMethodHandling::Drop);
    }

    #[test]
    fn test_config_with_all_optional_fields() {
        let yaml = r#"
server:
  name: "full-test-server"
  version: "2.0.0"
  stateScope: global
  capabilities:
    tools:
      listChanged: true
    resources:
      subscribe: true
      listChanged: true
    prompts:
      listChanged: false

tools:
  - tool:
      name: "test-tool"
      description: "A test tool with all fields"
      inputSchema:
        type: object
        properties:
          input:
            type: string
        required: ["input"]
    response:
      content:
        - type: text
          text: "Response text"
      isError: false
    behavior:
      delivery:
        type: slow_loris
        byte_delay_ms: 50
      side_effects:
        - type: notification_flood
          trigger: on_connect
          rate_per_sec: 10

resources:
  - resource:
      uri: "file:///test"
      name: "Test Resource"
      description: "A test resource"
      mimeType: "text/plain"
    response:
      content: "Test content"
    behavior:
      delivery:
        type: response_delay
        delay_ms: 100

prompts:
  - prompt:
      name: "test-prompt"
      description: "A test prompt"
      arguments:
        - name: "arg1"
          description: "First argument"
          required: true
    response:
      messages:
        - role: user
          content: "Test message"
    behavior:
      delivery:
        type: normal

behavior:
  delivery:
    type: normal
  side_effects:
    - type: close_connection
      trigger: continuous
      graceful: true

logging:
  level: debug
  on_phase_change: true
  on_request: true
  on_response: true

unknown_methods: error
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();

        // Verify server metadata
        assert_eq!(config.server.name, "full-test-server");
        assert_eq!(config.server.version, Some("2.0.0".to_string()));
        assert_eq!(config.server.state_scope, Some(StateScope::Global));

        // Verify capabilities
        let caps = config.server.capabilities.as_ref().unwrap();
        assert_eq!(caps.tools.as_ref().unwrap().list_changed, Some(true));
        assert_eq!(caps.resources.as_ref().unwrap().subscribe, Some(true));

        // Verify tools
        assert_eq!(config.tools.as_ref().unwrap().len(), 1);
        let tool = &config.tools.as_ref().unwrap()[0];
        assert_eq!(tool.tool.name, "test-tool");
        assert!(tool.behavior.is_some());

        // Verify resources
        assert_eq!(config.resources.as_ref().unwrap().len(), 1);

        // Verify prompts
        assert_eq!(config.prompts.as_ref().unwrap().len(), 1);

        // Verify logging
        let logging = config.logging.as_ref().unwrap();
        assert_eq!(logging.level, Some("debug".to_string()));
        assert_eq!(logging.on_phase_change, Some(true));

        // Verify unknown_methods
        assert_eq!(config.unknown_methods, Some(UnknownMethodHandling::Error));
    }

    #[test]
    fn test_camel_case_field_mapping() {
        // Test that camelCase YAML (legacy) still works via alias
        let yaml = r#"
server:
  name: "camel-test"
  stateScope: per_connection
  capabilities:
    tools:
      listChanged: true
    resources:
      listChanged: false
      subscribe: true
    prompts:
      listChanged: true

tools:
  - tool:
      name: "test"
      description: "Test"
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: "ok"
      isError: false
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();

        // Verify stateScope alias -> state_scope mapping
        assert_eq!(config.server.state_scope, Some(StateScope::PerConnection));

        // Verify listChanged -> list_changed mapping
        let caps = config.server.capabilities.as_ref().unwrap();
        assert_eq!(caps.tools.as_ref().unwrap().list_changed, Some(true));
        assert_eq!(caps.resources.as_ref().unwrap().list_changed, Some(false));
        assert_eq!(caps.prompts.as_ref().unwrap().list_changed, Some(true));

        // Verify inputSchema -> input_schema mapping
        let tool = &config.tools.as_ref().unwrap()[0];
        assert!(tool.tool.input_schema.is_object());

        // Verify isError -> is_error mapping
        assert_eq!(tool.response.is_error, Some(false));
    }

    #[test]
    fn test_snake_case_state_scope() {
        // TJ-SPEC-001: YAML uses snake_case for state_scope
        let yaml = r#"
server:
  name: "snake-test"
  state_scope: global
"#;
        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.state_scope, Some(StateScope::Global));
    }

    #[test]
    fn test_serialization_uses_snake_case_for_metadata() {
        let metadata = ServerMetadata {
            name: "test".to_string(),
            version: Some("1.0".to_string()),
            state_scope: Some(StateScope::Global),
            capabilities: Some(Capabilities {
                tools: Some(ToolsCapability {
                    list_changed: Some(true),
                }),
                resources: None,
                prompts: None,
            }),
        };

        let yaml = serde_yaml::to_string(&metadata).unwrap();

        // ServerMetadata now serializes with snake_case
        assert!(
            yaml.contains("state_scope: global"),
            "expected snake_case state_scope in: {yaml}"
        );
        // Capabilities still use camelCase (MCP protocol)
        assert!(yaml.contains("listChanged: true"));
    }

    #[test]
    fn test_scenario_metadata_round_trip() {
        let yaml = r#"
id: TJ-ATK-001
name: Classic Rug Pull
description: Trust-building phase followed by tool replacement
author: ThoughtJack Team
created: 2025-01-15
updated: 2025-03-01
severity: high
mitre_attack:
  tactics:
    - id: TA0001
      name: Initial Access
  techniques:
    - id: T1195.002
      name: Supply Chain Compromise
owasp_mcp:
  - id: MCP03
    name: Tool Poisoning
owasp_agentic:
  - id: ASI01
    name: Agentic Goal Hijacking
mcp_attack_surface:
  vectors:
    - tool_injection
    - capability_mutation
  primitives:
    - rug_pull
tags:
  - temporal
  - tool-replacement
detection_guidance:
  - Monitor for tools/list_changed notifications after trust-building phase
  - Compare tool definitions before and after phase transitions
references:
  - url: https://example.com/rug-pull
    title: Rug Pull Attack Pattern
"#;

        let meta: ScenarioMetadata = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(meta.id, "TJ-ATK-001");
        assert_eq!(meta.name, "Classic Rug Pull");
        assert_eq!(meta.severity, MetadataSeverity::High);
        assert_eq!(meta.author, Some("ThoughtJack Team".to_string()));
        assert!(meta.created.is_some());
        assert!(meta.updated.is_some());

        let mitre = meta.mitre_attack.as_ref().unwrap();
        assert_eq!(mitre.tactics.len(), 1);
        assert_eq!(mitre.tactics[0].id, "TA0001");
        assert_eq!(mitre.techniques.len(), 1);
        assert_eq!(mitre.techniques[0].id, "T1195.002");

        let owasp = meta.owasp_mcp.as_ref().unwrap();
        assert_eq!(owasp.len(), 1);
        assert_eq!(owasp[0].id, "MCP03");

        let agentic = meta.owasp_agentic.as_ref().unwrap();
        assert_eq!(agentic.len(), 1);
        assert_eq!(agentic[0].id, "ASI01");

        assert_eq!(meta.mcp_attack_surface.vectors.len(), 2);
        assert_eq!(
            meta.mcp_attack_surface.vectors[0],
            AttackVector::ToolInjection
        );
        assert_eq!(meta.mcp_attack_surface.primitives.len(), 1);
        assert_eq!(
            meta.mcp_attack_surface.primitives[0],
            AttackPrimitive::RugPull
        );

        assert_eq!(meta.tags.len(), 2);
        assert_eq!(meta.detection_guidance.len(), 2);
        assert_eq!(meta.references.len(), 1);

        // Round-trip: serialize back and deserialize again
        let json = serde_json::to_string(&meta).unwrap();
        let meta2: ScenarioMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(meta2.id, "TJ-ATK-001");
        assert_eq!(meta2.severity, MetadataSeverity::High);
    }

    #[test]
    fn test_scenario_metadata_minimal() {
        let yaml = r#"
id: TJ-ATK-002
name: Simple Injection
description: Basic tool injection without phases
severity: medium
mcp_attack_surface:
  vectors:
    - tool_injection
  primitives:
    - fuzzing
"#;

        let meta: ScenarioMetadata = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(meta.id, "TJ-ATK-002");
        assert_eq!(meta.severity, MetadataSeverity::Medium);
        assert!(meta.author.is_none());
        assert!(meta.created.is_none());
        assert!(meta.mitre_attack.is_none());
        assert!(meta.owasp_mcp.is_none());
        assert!(meta.tags.is_empty());
        assert!(meta.detection_guidance.is_empty());
        assert!(meta.references.is_empty());
    }

    #[test]
    fn test_server_config_with_metadata() {
        let yaml = r#"
server:
  name: "test-server"

metadata:
  id: TJ-ATK-001
  name: Test Attack
  description: A test attack scenario
  severity: critical
  mcp_attack_surface:
    vectors:
      - tool_injection
    primitives:
      - rug_pull

tools:
  - tool:
      name: "echo"
      description: "Echoes input"
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: "Hello"
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.metadata.is_some());
        let meta = config.metadata.unwrap();
        assert_eq!(meta.id, "TJ-ATK-001");
        assert_eq!(meta.severity, MetadataSeverity::Critical);
    }

    #[test]
    fn test_server_config_without_metadata() {
        let yaml = r#"
server:
  name: "test-server"

tools:
  - tool:
      name: "echo"
      description: "Echoes input"
      inputSchema:
        type: object
    response:
      content:
        - type: text
          text: "Hello"
"#;

        let config: ServerConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.metadata.is_none());
    }

    #[test]
    fn test_attack_vector_variants() {
        let variants = [
            ("tool_injection", AttackVector::ToolInjection),
            ("resource_injection", AttackVector::ResourceInjection),
            ("prompt_poisoning", AttackVector::PromptPoisoning),
            ("capability_mutation", AttackVector::CapabilityMutation),
            ("notification_abuse", AttackVector::NotificationAbuse),
            ("schema_manipulation", AttackVector::SchemaManipulation),
            ("description_hijack", AttackVector::DescriptionHijack),
            ("response_delay", AttackVector::ResponseDelay),
            ("connection_abuse", AttackVector::ConnectionAbuse),
        ];
        for (yaml, expected) in variants {
            let parsed: AttackVector =
                serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}"));
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_attack_primitive_variants() {
        let variants = [
            ("rug_pull", AttackPrimitive::RugPull),
            ("sleeper", AttackPrimitive::Sleeper),
            ("slow_loris", AttackPrimitive::SlowLoris),
            ("flood", AttackPrimitive::Flood),
            ("nested_json", AttackPrimitive::NestedJson),
            ("unbounded_line", AttackPrimitive::UnboundedLine),
            ("fuzzing", AttackPrimitive::Fuzzing),
            ("id_collision", AttackPrimitive::IdCollision),
            ("time_bomb", AttackPrimitive::TimeBomb),
        ];
        for (yaml, expected) in variants {
            let parsed: AttackPrimitive =
                serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}"));
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_metadata_severity_variants() {
        let variants = [
            ("low", MetadataSeverity::Low),
            ("medium", MetadataSeverity::Medium),
            ("high", MetadataSeverity::High),
            ("critical", MetadataSeverity::Critical),
        ];
        for (yaml, expected) in variants {
            let parsed: MetadataSeverity =
                serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("{yaml}: {e}"));
            assert_eq!(parsed, expected);
        }
    }
}
