# TJ-SPEC-011: Documentation Site

| Metadata | Value |
|----------|-------|
| **ID** | `TJ-SPEC-011` |
| **Title** | Documentation Site |
| **Type** | Tooling Specification |
| **Status** | Draft |
| **Priority** | **Medium** |
| **Version** | v1.5.2 |
| **Depends On** | TJ-SPEC-001, TJ-SPEC-003, TJ-SPEC-004, TJ-SPEC-006, TJ-SPEC-007 |
| **Tags** | `#docs` `#docusaurus` `#diataxis` `#mermaid` `#metadata` `#mitre` `#owasp-mcp` `#visualization` `#ci` |

## 1. Context & Decision Rationale

This specification defines ThoughtJack's documentation site — an auto-generated Docusaurus website that serves as the primary documentation for the project, combining auto-generated attack scenario documentation with hand-authored product documentation structured around the Diátaxis framework.

### 1.1 Motivation

ThoughtJack needs two kinds of documentation that serve different audiences but belong in one cohesive site:

**Product documentation** — how to install, configure, and use ThoughtJack:

| Audience | Need | Current Gap |
|----------|------|-------------|
| **New users** | Step-by-step first experience (tutorials) | No onboarding path exists |
| **Practitioners** | Task-oriented guides for specific scenarios (how-tos) | Must reverse-engineer from specs |
| **Developers** | Technical reference for config schema, CLI, APIs | Specs exist but aren't browsable |
| **Security architects** | Conceptual understanding of attack patterns and design rationale (explanation) | Scattered across spec docs |

**Scenario documentation** — auto-generated from YAML attack definitions:

| Audience | Need | Current Gap |
|----------|------|-------------|
| **Security engineers** | Browse attack catalog, understand phases visually | Must read raw YAML |
| **Security leadership** | Framework coverage (MITRE, OWASP), gap analysis | No coverage mapping exists |
| **Compliance / GRC** | Evidence of testing scope for audits | No reportable artifacts |
| **Contributors** | Understand existing scenarios before adding new ones | Must grep through YAML files |
| **MCP ecosystem** | Reference catalog of known attack patterns | No public-facing documentation |

The documentation site treats YAML scenarios as the **single source of truth** for attack documentation (auto-generated, never hand-authored), while product documentation follows the Diátaxis framework and is hand-authored in Markdown/MDX.

### 1.2 Design Principles

| Principle | Rationale |
|-----------|-----------|
| **Diátaxis structure for product docs** | Tutorials, how-to guides, reference, and explanation are distinct content types with different authoring and reading patterns. Mixing them produces documentation that serves no audience well. |
| **YAML is the source of truth for scenarios** | Scenario documentation is derived, never hand-written. Prevents drift. |
| **Zero-friction contribution** | Add a YAML scenario → scenario docs update automatically. Edit an MDX file → product docs update. |
| **Diagram type matches attack structure** | Phased attacks → state diagrams, injection → sequence diagrams, DoS → flowcharts. |
| **Framework mappings are first-class** | MITRE ATT&CK and OWASP MCP Top 10 are indexed and cross-referenced. OWASP Agentic AI Top 10 is reserved for A2A extension. |
| **Progressive disclosure** | Visual overview first, phase breakdown second, raw YAML last. Non-technical readers never need to see YAML. |
| **CLI parity** | The same diagram generation available in the Docusaurus pipeline is also available as a standalone CLI command for ad-hoc/custom scenarios. |

### 1.3 Scope Boundaries

**In scope:**
- Diátaxis-structured product documentation (tutorials, how-to guides, reference, explanation)
- Metadata schema extension for attack scenarios (framework mappings, severity, classification)
- Mermaid diagram generation from YAML scenario structure
- MDX page generation per scenario (narrative, diagrams, detection guidance)
- Sidebar and navigation generation from scenario registry
- Coverage matrix generation (MITRE ATT&CK, OWASP MCP Top 10)
- CI/CD pipeline for automated site builds
- CLI subcommand for standalone diagram generation
- Docusaurus project structure and custom React components

**Out of scope:**
- Rust crate API docs — use `cargo doc` (linked from reference section)
- Interactive scenario editing in the browser — YAML files are edited in-repo
- Scenario execution from the documentation site — use `thoughtjack server`
- Custom theme development — use default Docusaurus theme with minimal customization

---

## 2. Functional Requirements

### F-001: Scenario Metadata Schema

The system SHALL extend TJ-SPEC-001's configuration schema with an optional top-level `metadata` block for attack classification and framework mappings.

**Acceptance Criteria:**
- `metadata` is optional — scenarios without it are still valid and executable
- The Config Loader (TJ-SPEC-006) parses `metadata` but does not use it at runtime
- The documentation generator requires `metadata` for inclusion in the site
- `metadata` is stripped before passing configuration to the server runtime

**Metadata Ownership Rule:**

`metadata` SHALL only be defined in **top-level scenario files** — never in `$include` targets (tool patterns, resource definitions, prompt definitions, or reusable fragments).

| File Type | `metadata` Allowed? | Rationale |
|---|---|---|
| Top-level scenario (e.g., `scenarios/phased/rug-pull.yaml`) | ✅ Yes | This is the attack definition — it owns the catalog entry |
| `$include` target (e.g., `tools/calculator/injection.yaml`) | ❌ No | Fragments are reusable building blocks, not catalog entries |
| `$include` target with `metadata` present | ⚠️ Warning | Config Loader logs warning and ignores the block |

This rule exists because `$include` targets are reused across multiple scenarios. A tool pattern like `tools/calculator/injection.yaml` may appear in three different attack scenarios — attaching metadata to it would create ambiguity about which catalog entry it belongs to. Metadata describes the *attack*, not the *component*.

The Config Loader (TJ-SPEC-006) SHALL:
1. Parse `metadata` from the top-level file if present
2. Ignore `metadata` in any `$include`-resolved file (log warning: "metadata block in $include target {path} will be ignored")
3. Expose `metadata` as an `Option<ScenarioMetadata>` on the parsed config struct
4. Never merge metadata across files

**Schema:**
```yaml
metadata:
  id: string                          # Required: Unique scenario identifier (e.g., "TJ-ATK-001")
  name: string                        # Required: Human-readable scenario name
  description: string                 # Required: One-paragraph summary of the attack
  author: string                      # Optional: Scenario author or team
  created: date                       # Optional: Creation date (YYYY-MM-DD)
  updated: date                       # Optional: Last modification date
  severity: low | medium | high | critical  # Required: Attack severity rating
  
  mitre_attack:                       # Optional: MITRE ATT&CK mappings
    tactics:
      - id: string                    # Tactic ID (e.g., "TA0001")
        name: string                  # Tactic name (e.g., "Initial Access")
    techniques:
      - id: string                    # Technique ID (e.g., "T1195.002")
        name: string                  # Technique name
        sub_technique: string         # Optional: Sub-technique ID
  
  owasp_mcp:                          # Optional: OWASP MCP Top 10 mappings
    - id: string                      # Risk ID (e.g., "MCP03")
      name: string                    # Risk name (e.g., "Tool Poisoning")
  
  owasp_agentic:                      # Reserved: OWASP Agentic AI Top 10 (for A2A extension)
    - id: string                      # Risk ID (e.g., "ASI01")
      name: string                    # Risk name (e.g., "Agentic Goal Hijacking")

  mcp_attack_surface:                 # Required: ThoughtJack-native classification
    vectors:                          # Required: Attack vectors used
      - enum: tool_injection | resource_injection | prompt_poisoning |
              capability_mutation | notification_abuse | schema_manipulation |
              description_hijack | response_delay | connection_abuse
    primitives:                       # Required: ThoughtJack behavioral primitives used
      - enum: rug_pull | sleeper | slow_loris | flood | nested_json |
              unbounded_line | fuzzing | id_collision | time_bomb
  
  tags:                               # Optional: Free-form tags for filtering
    - string

  detection_guidance:                 # Optional: What defenses should catch
    - string                          # Each entry is a detection signal description

  references:                         # Optional: External references
    - url: string
      title: string
```

**Serde Representation:**
```rust
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ScenarioMetadata {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub created: Option<NaiveDate>,
    #[serde(default)]
    pub updated: Option<NaiveDate>,
    pub severity: Severity,
    #[serde(default)]
    pub mitre_attack: Option<MitreAttackMapping>,
    #[serde(default)]
    pub owasp_mcp: Option<Vec<OwaspMcpEntry>>,
    /// Reserved for A2A extension. Parsed but not used in coverage
    /// generation until A2A scenario support is implemented.
    #[serde(default)]
    pub owasp_agentic: Option<Vec<OwaspAgenticEntry>>,
    pub mcp_attack_surface: McpAttackSurface,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub detection_guidance: Vec<String>,
    #[serde(default)]
    pub references: Vec<Reference>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MitreAttackMapping {
    #[serde(default)]
    pub tactics: Vec<MitreTactic>,
    #[serde(default)]
    pub techniques: Vec<MitreTechnique>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpAttackSurface {
    pub vectors: Vec<AttackVector>,
    pub primitives: Vec<AttackPrimitive>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackVector {
    ToolInjection,
    ResourceInjection,
    PromptPoisoning,
    CapabilityMutation,
    NotificationAbuse,
    SchemaManipulation,
    DescriptionHijack,
    ResponseDelay,
    ConnectionAbuse,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttackPrimitive {
    RugPull,
    Sleeper,
    SlowLoris,
    Flood,
    NestedJson,
    UnboundedLine,
    Fuzzing,
    IdCollision,
    TimeBomb,
}
```

### F-002: Mermaid Diagram Generation

The system SHALL generate Mermaid diagram markup from parsed YAML scenario configurations.

**Acceptance Criteria:**
- Diagram type is auto-selected based on scenario structure (see selection matrix)
- Diagram type can be overridden via CLI flag or metadata hint
- Generated Mermaid is valid and renderable by Mermaid.js v10+
- Diagrams include phase names, trigger conditions, tool/resource changes, and behavior modes
- Entry actions and side effects are rendered as annotations or notes

**Diagram Selection Matrix:**

| Scenario Structure | Detected By | Diagram Type | Rationale |
|--------------------|-------------|--------------|-----------|
| Phased server (`phases` present) | `config.phases.is_some()` | `stateDiagram-v2` | Phases are states, triggers are transitions |
| Simple server with `match` blocks | `has_conditional_responses(config)` | `sequenceDiagram` | Request/response flow with branching |
| DoS / behavioral only | `has_side_effects(config) && !config.phases.is_some()` | `flowchart TD` | Behavior decision tree |
| Simple static server | Fallback | `flowchart LR` | Tool/resource inventory diagram |

**State Diagram Generation (phased scenarios):**

For each phased scenario, the generator SHALL produce a `stateDiagram-v2` with:

1. A state node per phase, named by `phase.name`
2. Transition arrows labeled with trigger conditions:
   - Event triggers: `{event_type} × {count}`
   - Time triggers: `after {duration}`
   - Content match triggers: `match: {pattern}` (truncated to 40 chars)
3. State body notes listing:
   - Tools exposed (added/replaced/removed vs baseline)
   - Delivery behavior if non-default
   - Side effects if present
4. Entry actions rendered as `<<entry>>` annotations
5. Terminal phase rendering (phases with no `advance` key):
   - Terminal phases do NOT transition to `[*]` — they persist indefinitely
   - Render with a `<<terminal>>` annotation and a note: "Attack persists — no further transitions"
   - The final phase connects via `-->` from the previous phase but has no outgoing arrow

**Example — Rug Pull Input:**
```yaml
phases:
  - name: trust_building
    advance:
      on: tools/call
      count: 3
  - name: trigger
    on_enter:
      - send_notification: notifications/tools/list_changed
    advance:
      on: tools/list
      timeout: 30s
  - name: exploit
    replace_tools:
      calculator: tools/calculator/injection.yaml
    behavior:
      delivery: slow_loris
      byte_delay_ms: 100
```

**Expected Output:**
```
stateDiagram-v2
    [*] --> trust_building
    trust_building --> trigger : "tools/call × 3"
    trigger --> exploit : "tools/list (timeout 30s)"

    state trust_building {
        [*] --> tb_active
        note right of tb_active
            Tools: calculator (benign)
            Delivery: normal
        end note
    }

    state trigger {
        [*] --> tr_active
        note right of tr_active
            «entry» send notifications/tools/list_changed
        end note
    }

    state exploit {
        [*] --> ex_active
        note right of ex_active
            «terminal» Attack persists
            Replace: calculator → injection
            Delivery: slow_loris (100ms)
        end note
    }
```

> **Note on terminal phases:** The `exploit` phase has no `advance` key, so it persists indefinitely — this is where the attack payload is active. The diagram renders it without an outgoing arrow to `[*]`, unlike non-terminal phases. The `«terminal»` annotation makes this visually distinct from phases that simply haven't been reached yet.

**Supplementary Per-Phase Diagrams (phased scenarios with internal complexity):**

When a phased scenario contains phases with `match` blocks (conditional responses) or complex side effect chains, the primary `stateDiagram-v2` shows the high-level phase flow while supplementary diagrams provide detail on individual phases.

The generator SHALL:

1. Detect phases that contain `match` blocks (from TJ-SPEC-009 dynamic responses)
2. For each such phase, generate a supplementary `sequenceDiagram` showing the conditional request/response flow within that phase
3. Render supplementary diagrams **inside the Phase Breakdown section** (F-003), under the H3 subheading for the corresponding phase — NOT stacked below the primary state diagram. This keeps the overview clean and provides complex visual detail where the reader is already focused on that phase's logic
4. NOT attempt to nest diagrams inside Mermaid state nodes (unsupported by Mermaid's `stateDiagram-v2`)
5. Cap supplementary diagram complexity: if a `match` block has more than 8 arms, render a summary table (tool/pattern → response description) instead of a sequence diagram, with a note linking to the raw YAML for full detail

**Example — Phase with Match Block:**

A phase containing:
```yaml
- name: exploit
  replace_tools:
    web_search: tools/web_search/injection.yaml
```
Where `injection.yaml` has a `match` block, generates a supplementary diagram:
```
sequenceDiagram
    participant C as Client
    participant TJ as ThoughtJack (exploit phase)

    C->>TJ: tools/call "web_search"
    alt args.query matches /password|secret|api.?key/
        TJ->>C: Injected credentials + system prompt override
    else default
        TJ->>C: Normal search results (benign)
    end
```

**Sequence Diagram Generation (conditional response scenarios):**

For non-phased scenarios with `match` blocks, the generator SHALL produce a `sequenceDiagram` with:

1. Participants: `Client`, `ThoughtJack`, and optionally `Phase Engine`
2. Request arrows from Client → ThoughtJack
3. Alt/else blocks for match conditions
4. Response arrows from ThoughtJack → Client with payload summaries
5. Notes for side effects triggered during the exchange

**Flowchart Generation (DoS / behavioral scenarios):**

For behavioral-only scenarios, the generator SHALL produce a `flowchart TD` with:

1. Entry node for incoming request
2. Decision nodes for any conditional logic
3. Process nodes for delivery behaviors
4. Parallel nodes for side effects
5. Output node for response delivery

**Mermaid Label Escaping and State ID Slugification:**

All Mermaid label text SHALL be wrapped in double quotes (`"`) by default to prevent syntax breakage from special characters. Mermaid's parser is fragile with characters common in attack scenarios — regex patterns, tool names with punctuation, URI strings, etc.

**State ID Slugification:**

Mermaid's `stateDiagram-v2` requires bare identifiers for state IDs — they cannot contain spaces, dashes, or special characters. Phase names in YAML are free-form strings (per TJ-SPEC-003), so the `escape.rs` module MUST slugify phase names into valid Mermaid identifiers while preserving the human-readable name as a label.

Slugification rules:
1. Lowercase the entire string
2. Replace spaces and dashes with underscores
3. Strip any character not in `[a-z0-9_]`
4. Collapse consecutive underscores
5. Trim leading/trailing underscores
6. If result is empty, use `phase_{index}` (where `index` is the 0-based position in the `phases` array, matching `${phase.index}` from TJ-SPEC-009)

The original phase name is rendered as a Mermaid state description (label) using the `state "Human Name" as slug_id` syntax:

```
stateDiagram-v2
    state "Trust Building" as trust_building
    state "Trigger Phase" as trigger_phase
    state "Final Exploit" as final_exploit
    
    [*] --> trust_building
    trust_building --> trigger_phase : "tools/call × 3"
    trigger_phase --> final_exploit : "tools/list (timeout 30s)"
    final_exploit --> [*]
```

The `escape.rs` module SHALL implement diagram-type-aware escaping:

| Diagram Type | Quoting Strategy | Rationale |
|---|---|---|
| `stateDiagram-v2` | Slugify state IDs; quote transition labels; use `state "label" as id` for human-readable names | State IDs must be bare identifiers; Mermaid's alias syntax preserves readability |
| `sequenceDiagram` | Quote all message text | Message text supports quoted strings natively |
| `flowchart` | Quote all node labels | Node labels in `["quoted text"]` handle all special chars |

Additionally, the following characters SHALL be escaped within quoted strings:
- `"` → `#quot;` (Mermaid HTML entity syntax)
- `\n` → `<br/>` (Mermaid line break in labels)

The escaping module SHALL be tested against a fixture set containing: regex patterns with `()[]{}`, URIs with `://` and `?`, tool names with `-` and `_`, multi-line description strings, and phase names requiring slugification (spaces, dashes, mixed case, Unicode, empty-after-strip).

**Trait Interface:**
```rust
pub trait DiagramRenderer {
    /// Render the configuration into a Mermaid diagram string.
    fn render(&self, config: &ServerConfig, metadata: &ScenarioMetadata) -> Result<String, DiagramError>;
    
    /// Return the Mermaid diagram type identifier.
    fn diagram_type(&self) -> &'static str;
}

pub struct StateDiagramRenderer;
pub struct SequenceDiagramRenderer;
pub struct FlowchartRenderer;

/// Auto-select the best diagram type for a given configuration.
pub fn auto_select_renderer(config: &ServerConfig) -> Box<dyn DiagramRenderer> {
    if config.phases.is_some() {
        Box::new(StateDiagramRenderer)
    } else if has_conditional_responses(config) {
        Box::new(SequenceDiagramRenderer)
    } else if has_side_effects(config) {
        Box::new(FlowchartRenderer)
    } else {
        Box::new(FlowchartRenderer) // inventory mode
    }
}
```

### F-003: MDX Page Generation

The system SHALL generate one `.mdx` page per scenario, structured for progressive disclosure.

**Acceptance Criteria:**
- Each scenario produces exactly one MDX file at `docs/scenarios/{category}/{scenario-id}.mdx`
- Category is derived from the scenario's filesystem path (e.g., `scenarios/phased/` → `phased/`)
- MDX frontmatter includes `id`, `title`, `sidebar_label`, and `tags` from metadata
- Page imports and uses custom React components (see F-008)
- Generated files include a `<!-- AUTO-GENERATED — DO NOT EDIT -->` header comment

**Page Structure (top to bottom):**

| Section | Source | Component |
|---------|--------|-----------|
| **Metadata card** | `metadata.*` | `<AttackMetadataCard>` |
| **Overview** | `metadata.description` (expanded by generator) | Prose paragraph |
| **Attack flow diagram** | Generated Mermaid (F-002) — primary diagram only | `<MermaidDiagram>` |
| **Phase breakdown** | `phases[]` or tool/resource listing | Generated prose with H3 subheadings per phase. Supplementary diagrams (F-002) are rendered inside their corresponding phase H3, before the prose description for that phase. |
| **Detection guidance** | `metadata.detection_guidance[]` | Bulleted list |
| **Framework mappings** | `metadata.mitre_attack`, `metadata.owasp_mcp` | `<MitreMapping>`, `<OwaspMcpMapping>` pills |
| **Raw scenario** | Original YAML file | `<YamlSourceToggle>` collapsible |

**Phase Breakdown Layout (for phases with supplementary diagrams):**

```
## Phase Breakdown

### Trust Building
Prose description of trust building phase...

### Exploit Phase
[Supplementary Sequence Diagram — conditional response logic]
Prose description of exploit logic...

### Persistence
Prose description of terminal phase...
```

This layout keeps the Overview section clean (one high-level state diagram) while providing complex visual detail exactly where the reader is focused on a specific phase.

**Prose Generation:**

The generator SHALL produce human-readable prose from structural elements:

| YAML Structure | Generated Prose |
|----------------|-----------------|
| `advance: { on: tools/call, count: 3 }` | "Advances after 3 `tools/call` requests" |
| `advance: { after: 10s }` | "Advances 10 seconds after phase entry" |
| `advance: { on: tools/call, timeout: 30s }` | "Advances on `tools/call` or after 30 seconds of inactivity (whichever comes first)" |
| `advance: { on: tools/call, count: 5, timeout: 60s }` | "Advances after 5 `tools/call` requests or after 60 seconds of inactivity (whichever comes first)" |
| `advance: { on: tools/call, on_timeout: abort }` | "Advances on `tools/call`; aborts scenario on timeout" |
| `replace_tools: { calc: injection.yaml }` | "Swaps `calc` tool with injection variant" |
| `behavior: { delivery: slow_loris, byte_delay_ms: 100 }` | "Delivers response using slow-loris pattern (100ms per byte)" |
| `side_effects: [{ type: notification_flood, rate_per_sec: 10000 }]` | "Triggers notification flood side effect at 10,000 notifications/sec" |
| `on_enter: [send_notification: ...]` | "On phase entry: sends `notifications/tools/list_changed`" |
| `on_enter: [action_1, action_2, action_3]` | "On phase entry: (1) {action_1 prose}, (2) {action_2 prose}, (3) {action_3 prose}" |
| `$generate: { type: nested_json, depth: 100 }` | "Generates nested JSON payload (depth: 100)" |
| `$generate: { type: random_bytes, size: 10mb }` | "Generates random byte payload (10 MB)" |
| `$generate: { type: unbounded_line, length: 1mb }` | "Generates single-line string payload (1 MB)" |
| `$generate: { type: unicode_stress, ... }` | "Generates Unicode stress-test payload" |
| `match: [{ when: ..., content: $generate }]` | "When {condition}: generates {type} payload ({params}); otherwise: {default prose}" |
| `unknown_methods: ignore` | "Unknown MCP methods are silently ignored" |
| `unknown_methods: error` | "Unknown MCP methods return a JSON-RPC error" |

> **Note on `$generate` prose:** The prose generator describes payload *type and parameters*, never payload content. Generated payloads are often binary, extremely large, or intentionally malformed — attempting to describe or preview the data would be meaningless. The raw YAML (in the collapsible section) shows the full `$generate` directive for anyone who needs exact parameters.

> ⚠️ **Time Trigger Disambiguation**
>
> Per TJ-SPEC-003, `after` measures time since phase entry (absolute), while `timeout` measures time since the last matching event (inactivity). These have fundamentally different security implications — a `timeout` trigger means the attack activates when the client *stops* interacting, while `after` activates on a fixed schedule regardless of activity. The prose generator MUST NOT conflate these semantics.

This mapping is implemented as a Rust function per structural element, not a template engine. Prose is deterministic given the same input.

### F-004: Scenario Registry

The system SHALL use a `registry.yaml` file to control scenario ordering, categorization, and site navigation.

**Acceptance Criteria:**
- `registry.yaml` lives at `scenarios/registry.yaml`
- It defines category ordering and scenario ordering within categories
- Scenarios not listed in the registry are excluded from the site (opt-in, not opt-out)
- The generator validates that all registry entries point to existing scenario files
- The generator validates that all scenario files with `metadata` blocks are listed in the registry (warning on orphans)

**Schema:**
```yaml
# scenarios/registry.yaml
site:
  title: "ThoughtJack Attack Catalog"
  description: "Reference catalog of MCP attack scenarios"
  base_url: "/thoughtjack"

categories:
  - id: injection
    name: "Injection Attacks"
    description: "Prompt injection via tool responses, resources, and prompts"
    order: 1
    scenarios:
      - scenarios/injection/tool-response-injection.yaml
      - scenarios/injection/resource-injection.yaml
      - scenarios/injection/prompt-poisoning.yaml
      - scenarios/injection/multi-turn-context-poisoning.yaml

  - id: phased
    name: "Phased Attacks"
    description: "Temporal attacks that evolve over time"
    order: 2
    scenarios:
      - scenarios/phased/rug-pull-basic.yaml
      - scenarios/phased/sleeper-activation.yaml
      - scenarios/phased/capability-escalation.yaml
      - scenarios/phased/prompt-rug-pull.yaml

  - id: dos
    name: "Denial of Service"
    description: "Resource exhaustion and availability attacks"
    order: 3
    scenarios:
      - scenarios/dos/nested-json-bomb.yaml
      - scenarios/dos/notification-flood.yaml
      - scenarios/dos/slow-loris.yaml
      - scenarios/dos/unbounded-line.yaml

  - id: evasion
    name: "Evasion"
    description: "Techniques to bypass detection and filtering"
    order: 4
    scenarios:
      - scenarios/evasion/typosquat-tool.yaml
      - scenarios/evasion/description-hijack.yaml
```

**Generated Output:**
- `sidebars.js` — Docusaurus sidebar configuration reflecting category/scenario ordering
- `docs/scenarios/index.mdx` — Landing page with category cards

### F-005: Coverage Matrix Generation

The system SHALL generate cross-reference pages mapping scenarios to attack framework entries.

**Acceptance Criteria:**
- MITRE ATT&CK coverage page lists all referenced tactics and techniques with linked scenarios
- OWASP MCP Top 10 coverage page lists all 10 categories with linked scenarios (including gaps)
- MCP Attack Surface page lists all vectors and primitives with linked scenarios
- Coverage gaps (framework entries with zero scenarios) are visually highlighted
- Pages are generated at `docs/coverage/mitre-matrix.mdx`, `docs/coverage/owasp-mcp.mdx`, `docs/coverage/mcp-attack-surface.mdx`

**MITRE ATT&CK Coverage Page Structure:**

The page SHALL render a table grouped by tactic:

| Tactic | Technique | Scenarios |
|--------|-----------|-----------|
| Initial Access (TA0001) | Supply Chain Compromise (T1195.002) | [Rug Pull Basic](link), [Sleeper](link) |
| Defense Evasion (TA0005) | Masquerading (T1036) | [Typosquat Tool](link) |
| ... | ... | ... |

**OWASP MCP Top 10 Coverage Page Structure:**

The page SHALL render all 10 MCP-specific categories with scenario links and gap indicators. This is the primary framework for ThoughtJack since it maps the MCP protocol's own threat landscape.

| MCP ID | Risk | Scenarios | Coverage |
|--------|------|-----------|----------|
| MCP01 | Token Mismanagement & Secret Exposure | — | ⚠️ Gap |
| MCP02 | Privilege Escalation via Scope Creep | — | ⚠️ Gap |
| MCP03 | Tool Poisoning | [Rug Pull Basic](link), [Typosquat Tool](link), [Description Hijack](link) | ✅ Covered |
| MCP04 | Software Supply Chain Attacks & Dependency Tampering | — | ℹ️ Out of scope |
| MCP05 | Command Injection & Execution | — | ⚠️ Gap |
| MCP06 | Prompt Injection via Contextual Payloads | [Tool Injection](link), [Resource Injection](link), [Prompt Poisoning](link) | ✅ Covered |
| MCP07 | Insufficient Authentication & Authorization | — | ℹ️ Out of scope |
| MCP08 | Lack of Audit and Telemetry | — | ℹ️ Out of scope |
| MCP09 | Shadow MCP Servers | — | ℹ️ Out of scope |
| MCP10 | Context Injection & Over-Sharing | [Multi-Turn Context Poisoning](link) | ✅ Covered |

> **Note:** Several OWASP MCP categories (MCP01, MCP04, MCP07, MCP08, MCP09) represent infrastructure and governance risks rather than protocol-level attacks. ThoughtJack primarily covers protocol-level attack simulation, so gaps in these categories are expected. The coverage page SHALL use three distinct indicators:
> - ✅ **Covered** — one or more scenarios exercise this risk
> - ⚠️ **Gap** — risk is within ThoughtJack's scope but no scenario exists yet
> - ℹ️ **Out of scope** — risk belongs to a different domain (infrastructure, governance, deployment)

**MCP Attack Surface Coverage Page Structure:**

Two tables — one for vectors, one for primitives — each showing which scenarios exercise them.

### F-006: CLI Subcommand

The system SHALL provide a `thoughtjack diagram` CLI subcommand for standalone diagram generation.

**Acceptance Criteria:**
- Accepts a scenario YAML file path as argument
- Outputs Mermaid markdown to stdout by default
- Supports `--type` flag to override auto-selected diagram type
- Supports `--output` / `-o` flag to write to file
- Supports `--render svg` to invoke `mmdc` (Mermaid CLI) if installed
- Supports `--with-metadata` flag to include metadata panel in diagram
- Exit code 0 on success, 1 on invalid config, 2 on missing metadata

**Command Structure (extends TJ-SPEC-007 § 1.3):**
```
thoughtjack
├── server              # Existing
├── agent               # Existing (future)
├── completions         # Existing
├── version             # Existing
│
├── diagram             # NEW: Generate Mermaid diagrams
│   └── <scenario.yaml>
│       ├── --type <state|sequence|flowchart|auto>
│       ├── --output <path>
│       ├── --render <svg|png>
│       └── --with-metadata
│
└── docs                # NEW: Generate documentation site
    ├── generate        # Generate MDX from scenarios
    │   ├── --scenarios <dir>
    │   ├── --output <dir>
    │   ├── --registry <path>
    │   └── --strict
    ├── build           # Generate + Docusaurus build
    │   ├── --base-url <url>
    │   └── --strict
    └── serve           # Generate + Docusaurus dev server
        └── --port <number>
```

**`--strict` Mode:**

When `--strict` is passed to `docs generate` or `docs build`, the generator promotes orphan warnings to errors, causing the build to fail if any YAML scenario file in the search path is unaccounted for.

The orphan detection algorithm is `$include`-aware:

1. Walk all `.yaml` files under `--scenarios` directory recursively
2. Build the **accounted set**: all files referenced in `registry.yaml` (as scenarios) + all files referenced via `$include` from those scenarios (resolved transitively) + infrastructure files (`registry.yaml` itself)
3. Compute the **orphan set**: all `.yaml` files found in step 1 that are not in the accounted set from step 2
4. In default mode: log orphans as warnings
5. In `--strict` mode: log orphans as errors and exit with code 1

This prevents "invisible" scenarios — files that lack both a registry entry and metadata, which would otherwise silently go unnoticed.

**Environment Variable Fallbacks:**

| Flag | Environment Variable | Default |
|------|---------------------|---------|
| `--scenarios` | `TJ_SCENARIOS_DIR` | `./scenarios` |
| `--output` | `TJ_DOCS_OUTPUT_DIR` | `./docs-site` |
| `--registry` | `TJ_REGISTRY_PATH` | `./scenarios/registry.yaml` |
| `--strict` | `TJ_DOCS_STRICT` | `false` |

### F-007: Documentation Build Pipeline

The system SHALL provide a CI/CD-compatible build pipeline that generates the documentation site from scenarios.

**Acceptance Criteria:**
- `thoughtjack docs generate` produces MDX files from scenarios
- `thoughtjack docs build` runs generate + `npm run build` on the Docusaurus project
- Build fails if any registry entry points to a missing scenario
- Build warns if any scenario with `metadata` is not in the registry
- Build fails if any `metadata.id` values are duplicated across scenarios

**Generated Files are Ephemeral Build Artifacts:**

Auto-generated files (`docs/scenarios/`, `docs/coverage/`, `sidebars.js`) SHALL NOT be committed to version control. They are build artifacts, regenerated on every build from the YAML source of truth.

Rationale:
- Committing derived files creates noisy diffs and merge conflicts as the catalog grows
- Stale generated files (out of sync with YAML) are a category of bug that shouldn't exist
- The YAML → MDX pipeline is fast and deterministic — there's no caching benefit to committing output
- PR reviewers should review YAML changes, not generated MDX

The repository SHALL include:
```gitignore
# docs-site/.gitignore
docs/scenarios/         # Auto-generated from YAML
docs/coverage/          # Auto-generated from metadata
sidebars.js             # Auto-generated from registry
```

Hand-authored Diátaxis content (`docs/tutorials/`, `docs/how-to/`, `docs/reference/`, `docs/explanation/`) IS committed — these are source files, not build artifacts.

**Build-Time Diagram Validation (`--validate-diagrams`):**

`thoughtjack docs generate` and `thoughtjack docs build` SHALL support an optional `--validate-diagrams` flag that pipes each generated Mermaid string through `mmdc --validate` (Mermaid CLI) to catch syntax errors before deploy.

| Flag | Behavior |
|---|---|
| Not set | Diagrams are generated but not validated against Mermaid CLI |
| `--validate-diagrams` | Each `.mmd` file is validated via `mmdc -i {file} -o /dev/null`. Validation failure logs error with file path and Mermaid error message. Build fails on first validation error. |

`mmdc` availability is checked at startup. If `--validate-diagrams` is set but `mmdc` is not on `$PATH`, the build fails with: "Mermaid CLI (mmdc) not found. Install via `npm install -g @mermaid-js/mermaid-cli` or remove `--validate-diagrams` flag."

**Pipeline Sequence:**

```
┌──────────────┐     ┌───────────────┐     ┌──────────────┐     ┌──────────────┐     ┌──────────────┐
│  Parse YAML  │────▶│ Generate MDX   │────▶│ Validate     │────▶│ Generate     │────▶│  Docusaurus  │
│  scenarios   │     │ + Mermaid      │     │ diagrams     │     │ sidebars +   │     │  build       │
│  + metadata  │     │ per scenario   │     │ (optional)   │     │ coverage     │     │              │
└──────────────┘     └───────────────┘     └──────────────┘     └──────────────┘     └──────────────┘
       │                                                               │
       ▼                                                               ▼
  Validate registry                                              sidebars.js
  Check duplicates                                               coverage/*.mdx
  Warn on orphans                                                scenarios/index.mdx
```

**GitHub Actions Workflow:**
```yaml
name: Build Scenario Docs

on:
  push:
    paths:
      - 'scenarios/**'
      - 'tools/scenario-docs-gen/**'
      - 'docs-site/src/**'
      - 'docs-site/docs/tutorials/**'
      - 'docs-site/docs/how-to/**'
      - 'docs-site/docs/reference/**'
      - 'docs-site/docs/explanation/**'
  pull_request:
    paths:
      - 'scenarios/**'

jobs:
  build-docs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install Rust toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Build generator
        run: cargo build --release -p scenario-docs-gen

      - name: Generate docs from scenarios
        run: ./target/release/scenario-docs-gen generate \
          --scenarios ./scenarios \
          --output ./docs-site \
          --registry ./scenarios/registry.yaml \
          --strict

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: 20
          cache: npm
          cache-dependency-path: docs-site/package-lock.json

      - name: Install Mermaid CLI
        run: npm install -g @mermaid-js/mermaid-cli

      - name: Validate generated diagrams
        run: ./target/release/scenario-docs-gen generate \
          --scenarios ./scenarios \
          --output ./docs-site \
          --registry ./scenarios/registry.yaml \
          --validate-diagrams

      - name: Build Docusaurus
        working-directory: docs-site
        run: |
          npm ci
          npm run build

      - name: Deploy to GitHub Pages
        if: github.ref == 'refs/heads/main'
        uses: peaceiris/actions-gh-pages@v4
        with:
          github_token: ${{ secrets.GITHUB_TOKEN }}
          publish_dir: docs-site/build
```

### F-008: Docusaurus Custom Components

The system SHALL provide custom React components for rendering attack-specific content in MDX pages.

**Acceptance Criteria:**
- Components are TypeScript React functional components
- Components use Docusaurus theme tokens for consistent styling
- Components are self-contained (no external API calls at render time)

**Component Inventory:**

| Component | Purpose | Props |
|-----------|---------|-------|
| `AttackMetadataCard` | Severity badge, framework pills, vector/primitive tags | `id`, `name`, `severity`, `mitre`, `owaspMcp`, `mcpVectors`, `primitives`, `tags` |
| `MermaidDiagram` | Renders Mermaid chart string with Docusaurus theme integration | `chart` (string) |
| `YamlSourceToggle` | Collapsible raw YAML viewer with syntax highlighting | `yaml` (string), `filename` (string) |
| `MitreMapping` | Clickable MITRE ATT&CK technique pills linking to attack.mitre.org | `tactics`, `techniques` |
| `OwaspMcpMapping` | Clickable OWASP MCP risk pills linking to owasp.org/www-project-mcp-top-10 | `entries` |
| `CoverageMatrix` | Heatmap grid for coverage pages | `rows`, `columns`, `data` |
| `SeverityBadge` | Color-coded severity indicator | `level` |

**`AttackMetadataCard` Rendering:**

```
┌─────────────────────────────────────────────────────────────┐
│ TJ-ATK-003                                    ██ CRITICAL   │
│ Calculator Rug Pull                                         │
├─────────────────────────────────────────────────────────────┤
│ MITRE: T1195.002  T1036                                     │
│ OWASP MCP: MCP03  MCP06                                    │
│ Vectors: tool_injection  capability_mutation                │
│ Primitives: rug_pull  slow_loris                            │
│ Tags: #phased  #prompt-injection  #trust-building           │
└─────────────────────────────────────────────────────────────┘
```

Each pill is a clickable link:
- MITRE technique pills → `https://attack.mitre.org/techniques/{id}/`
- OWASP MCP pills → `https://owasp.org/www-project-mcp-top-10/2025/{id}`
- Vector/primitive pills → internal filter (show all scenarios with same vector)

**`MermaidDiagram` Implementation:**

Uses `@docusaurus/theme-mermaid` built-in support. The component wraps Mermaid code blocks with optional fullscreen toggle and download button:

```tsx
import Mermaid from '@theme/Mermaid';

interface MermaidDiagramProps {
  chart: string;
  title?: string;
}

export default function MermaidDiagram({ chart, title }: MermaidDiagramProps) {
  return (
    <div className="tj-mermaid-container">
      {title && <h4>{title}</h4>}
      <Mermaid value={chart} />
    </div>
  );
}
```

**`YamlSourceToggle` Implementation:**

Uses Docusaurus `<Details>` component with Prism syntax highlighting:

```tsx
import Details from '@theme/Details';
import CodeBlock from '@theme/CodeBlock';

interface YamlSourceToggleProps {
  yaml: string;
  filename: string;
}

export default function YamlSourceToggle({ yaml, filename }: YamlSourceToggleProps) {
  return (
    <Details summary={<summary>View raw scenario: <code>{filename}</code></summary>}>
      <CodeBlock language="yaml" title={filename}>
        {yaml}
      </CodeBlock>
    </Details>
  );
}
```

**`CoverageMatrix` Rendering:**

The coverage matrix uses a three-indicator color scheme to make security posture immediately scannable:

| Indicator | Color | Meaning | Usage |
|-----------|-------|---------|-------|
| ✅ Covered | Green (`#2e7d32` / `--ifm-color-success`) | One or more scenarios exercise this risk | Links to scenario pages |
| ⚠️ Gap | Amber (`#ed6c02` / `--ifm-color-warning`) | Risk is within ThoughtJack's scope but no scenario exists yet | Actionable — scenario should be written |
| ℹ️ Out of scope | Grey (`#757575` / `--ifm-color-secondary`) | Risk belongs to a different domain (infrastructure, governance) | Informational — not a missing test |

The component accepts a `scopeExclusions` prop — a list of framework IDs that are out of scope — to distinguish gaps from scope boundaries. This is set per coverage page (e.g., the OWASP MCP page excludes MCP04, MCP07, MCP08, MCP09).

### F-009: Docusaurus Project Configuration

The system SHALL maintain a Docusaurus project configured for the attack catalog use case.

**Acceptance Criteria:**
- Docusaurus v3 with `@docusaurus/preset-classic`
- Mermaid support via `@docusaurus/theme-mermaid`
- Search via `@docusaurus/plugin-search-local` or Algolia
- Dark mode support
- MDX v3 support for custom components
- Tag index pages auto-generated from scenario tags

**`docusaurus.config.js` Key Settings:**
```javascript
module.exports = {
  title: 'ThoughtJack Attack Catalog',
  tagline: 'MCP adversarial scenario reference',
  url: 'https://thoughtjack.github.io',
  baseUrl: '/thoughtjack/',
  
  markdown: {
    mermaid: true,
  },
  
  themes: ['@docusaurus/theme-mermaid'],
  
  themeConfig: {
    mermaid: {
      theme: { light: 'default', dark: 'dark' },
    },
    navbar: {
      title: 'ThoughtJack',
      items: [
        { type: 'dropdown', label: 'Documentation', position: 'left', items: [
          { to: '/docs/tutorials', label: 'Tutorials' },
          { to: '/docs/how-to', label: 'How-To Guides' },
          { to: '/docs/reference', label: 'Reference' },
          { to: '/docs/explanation', label: 'Explanation' },
        ]},
        { to: '/docs/scenarios', label: 'Attack Catalog', position: 'left' },
        { type: 'dropdown', label: 'Coverage', position: 'left', items: [
          { to: '/docs/coverage/owasp-mcp', label: 'OWASP MCP Top 10' },
          { to: '/docs/coverage/mitre-matrix', label: 'MITRE ATT&CK' },
          { to: '/docs/coverage/mcp-attack-surface', label: 'MCP Attack Surface' },
        ]},
        { href: 'https://github.com/thoughtjack/thoughtjack', label: 'GitHub', position: 'right' },
      ],
    },
  },
};
```

### F-010: Metadata Validation

The system SHALL validate metadata fields at documentation generation time.

**Acceptance Criteria:**
- `metadata.id` must match pattern `TJ-ATK-\d{3}` (e.g., `TJ-ATK-001`)
- `metadata.id` must be unique across all scenarios in the registry
- `metadata.severity` must be one of `low`, `medium`, `high`, `critical`
- `metadata.mitre_attack.tactics[].id` must match pattern `TA\d{4}`
- `metadata.mitre_attack.techniques[].id` must match pattern `T\d{4}(\.\d{3})?`
- `metadata.owasp_mcp[].id` must match pattern `MCP\d{2}`
- `metadata.owasp_agentic[].id` must match pattern `ASI\d{2}` (reserved — validated but not used in coverage generation)
- `metadata.mcp_attack_surface.vectors[]` must be from the defined enum
- `metadata.mcp_attack_surface.primitives[]` must be from the defined enum
- Validation errors are reported with scenario file path and field location
- Validation is run as part of `thoughtjack docs generate` and fails the build on error

**Partial Metadata is an Error:**

If a scenario file contains a `metadata` key, ALL required fields must be present and valid. A partially-populated metadata block (e.g., `id` present but `name` missing) produces a broken MDX page and is treated as a validation error, not a warning.

Required fields (must be present if `metadata` key exists):
- `id` — scenario identifier
- `name` — human-readable name
- `description` — one-paragraph summary
- `severity` — attack severity rating
- `mcp_attack_surface` — with at least one `vector` and one `primitive`

Optional fields (may be omitted without error):
- `author`, `created`, `updated`, `tags`, `detection_guidance`
- `mitre_attack`, `owasp_mcp`, `owasp_agentic`

This rule is enforced by the Serde struct — required fields have no `Option` wrapper and will fail deserialization. The generator SHALL catch `serde` errors and produce a clear message:

```
ERROR [TJ-SPEC-011] scenarios/phased/incomplete.yaml
  → metadata: missing required field "name"
  → metadata: missing required field "mcp_attack_surface"
  Hint: either add the missing fields or remove the metadata block entirely
```

**Error Format:**
```
ERROR [TJ-SPEC-011] scenarios/phased/rug-pull-basic.yaml
  → metadata.mitre_attack.techniques[0].id: "T119502" does not match pattern T\d{4}(\.\d{3})?
  → metadata.severity: "severe" is not one of: low, medium, high, critical

ERROR [TJ-SPEC-011] Duplicate metadata.id "TJ-ATK-003" found in:
  → scenarios/phased/rug-pull-basic.yaml
  → scenarios/injection/tool-response-injection.yaml
```

### F-011: Diátaxis Documentation Structure

The system SHALL organize hand-authored product documentation according to the Diátaxis framework, with four distinct content quadrants alongside the auto-generated scenario catalog.

**Acceptance Criteria:**
- Documentation is organized into four Diátaxis quadrants plus the scenario catalog
- Each quadrant has a distinct sidebar section with clear labeling
- Tutorials, how-to guides, and explanation pages are hand-authored MDX files (not auto-generated)
- Reference pages MAY be partially auto-generated from specs or CLI introspection
- The scenario catalog (auto-generated) is a separate top-level section, not nested under any Diátaxis quadrant
- Coverage matrices are a separate top-level section

**Site Information Architecture:**

```
ThoughtJack Documentation
│
├── Tutorials                          # Learning-oriented
│   ├── Getting Started                # Install, first scenario, first run
│   ├── Your First Rug Pull            # End-to-end phased attack walkthrough
│   ├── Testing a Proxy with ThoughtJack  # Real-world integration tutorial
│   └── Writing Your First Scenario    # Creating a custom YAML scenario
│
├── How-To Guides                      # Task-oriented
│   ├── Run a Scenario via stdio       # Specific task: connect via stdio
│   ├── Run a Scenario via HTTP        # Specific task: HTTP transport
│   ├── Create a Phased Attack         # Task: multi-phase config
│   ├── Add Prompt Injection Payloads  # Task: injection content
│   ├── Generate DoS Payloads          # Task: $generate directive
│   ├── Use Dynamic Responses          # Task: match blocks, templates
│   ├── Record and Replay Traffic      # Task: VCR-style testing
│   ├── Add Scenarios to the Catalog   # Task: metadata + registry
│   └── Integrate with CI/CD           # Task: automation setup
│
├── Reference                          # Information-oriented
│   ├── Configuration Schema           # Full YAML schema (from TJ-SPEC-001)
│   ├── CLI Reference                  # All commands, flags, env vars (from TJ-SPEC-007)
│   ├── Phase Engine                   # Trigger types, diff operations (from TJ-SPEC-003)
│   ├── Behavioral Modes               # Delivery modes, side effects (from TJ-SPEC-004)
│   ├── Payload Generation             # $generate types and params (from TJ-SPEC-005)
│   ├── Dynamic Responses              # Templates, matching, handlers (from TJ-SPEC-009)
│   ├── Observability                  # Log formats, metrics, events (from TJ-SPEC-008)
│   ├── Record & Replay Format         # NDJSON schema, commands (from TJ-SPEC-010)
│   └── Metadata Schema                # Scenario metadata fields (from TJ-SPEC-011)
│
├── Explanation                        # Understanding-oriented
│   ├── Why ThoughtJack Exists         # Problem space, threat landscape
│   ├── MCP Attack Surface             # Conceptual model of MCP threats
│   ├── ThoughtJack vs ThoughtGate     # Offense/defense relationship
│   ├── Design Decisions               # Why YAML, why linear phases, why Rust
│   ├── Attack Pattern Taxonomy        # How vectors/primitives map to real threats
│   └── Framework Mapping Guide        # How to map scenarios to MITRE/OWASP
│
├── Attack Catalog                     # AUTO-GENERATED from YAML scenarios
│   ├── Injection Attacks
│   ├── Phased Attacks
│   ├── Denial of Service
│   └── Evasion
│
└── Coverage                           # AUTO-GENERATED from metadata
    ├── OWASP MCP Top 10
    ├── MITRE ATT&CK Matrix
    └── MCP Attack Surface
```

**Diátaxis Quadrant Rules:**

| Quadrant | Orientation | Author | Build |
|----------|------------|--------|-------|
| **Tutorials** | Learning | Hand-authored MDX | Static (committed to repo) |
| **How-To Guides** | Task | Hand-authored MDX | Static (committed to repo) |
| **Reference** | Information | Hand-authored MDX, optionally enriched from specs | Static (committed to repo) |
| **Explanation** | Understanding | Hand-authored MDX | Static (committed to repo) |
| **Attack Catalog** | N/A (auto-generated) | Generated from YAML scenarios | Dynamic (build artifact) |
| **Coverage** | N/A (auto-generated) | Generated from metadata cross-references | Dynamic (build artifact) |

**Content Guidelines per Quadrant:**

**Tutorials** answer "Can you teach me to...?" They are:
- Reproducible end-to-end walkthroughs with a concrete goal
- Written for someone who has never used ThoughtJack
- Sequential — each step builds on the previous one
- Focused on *doing*, not explaining why

**How-To Guides** answer "How do I...?" They are:
- Focused on a single task with a clear outcome
- Written for someone who already understands the basics
- Non-sequential — can be read independently
- Practical — minimum theory, maximum actionable steps

**Reference** answers "What is the exact...?" It is:
- Comprehensive and accurate — every flag, every field, every option
- Structured for lookup, not reading front-to-back
- Austere — descriptions, not explanations
- Kept in sync with specs (reference pages SHOULD cite their source spec)

**Explanation** answers "Why does...?" and "What is the concept behind...?" It is:
- Discursive and conceptual — reasoning, context, trade-offs
- Not tied to specific tasks or step-by-step procedures
- Where design decisions and architectural rationale live
- Free to reference other quadrants but not duplicate them

---

## 3. Edge Cases

### EC-DOCS-001: Scenario Without Metadata Block

**Scenario:** YAML scenario file has no `metadata` key  
**Expected:** Scenario is valid for execution but excluded from documentation site. Warning logged during `docs generate` if file is listed in registry.

### EC-DOCS-002: Scenario in Registry But File Missing

**Scenario:** `registry.yaml` references `scenarios/phased/missing.yaml` which does not exist  
**Expected:** Build fails with error: "Registry entry points to missing file: scenarios/phased/missing.yaml"

### EC-DOCS-003: Scenario With Metadata But Not in Registry

**Scenario:** `scenarios/experimental/new-attack.yaml` has a `metadata` block but is not listed in `registry.yaml`  
**Expected:** Warning logged: "Scenario with metadata not in registry: scenarios/experimental/new-attack.yaml". Scenario excluded from site. In `--strict` mode, this is an error.

### EC-DOCS-003a: Scenario File Without Metadata and Not in Registry

**Scenario:** `scenarios/phased/forgotten-attack.yaml` has no `metadata` block and is not in `registry.yaml` or referenced as a `$include` target  
**Expected:** In default mode, warning logged: "Unaccounted file: scenarios/phased/forgotten-attack.yaml (not in registry, not a $include target)". In `--strict` mode, this is an error and the build fails. This catches "invisible" scenarios that would otherwise go unnoticed.

### EC-DOCS-003b: Pattern Fragment File Not in Registry

**Scenario:** `scenarios/tools/calculator/benign.yaml` is a `$include` target used by `scenarios/phased/rug-pull-basic.yaml`, but is not itself in the registry  
**Expected:** File is recognized as a `$include` target and added to the accounted set. No warning or error — pattern fragments are not standalone scenarios and should not be in the registry.

### EC-DOCS-004: Empty Phases Array in Diagram Generation

**Scenario:** Phased server config with `phases: []`  
**Expected:** Diagram renderer falls back to flowchart (inventory mode) instead of empty state diagram.

### EC-DOCS-005: Very Long Phase Names or Trigger Patterns

**Scenario:** Phase name is 100+ characters, or regex pattern in content match trigger is 200+ characters  
**Expected:** Diagram renderer truncates to 40 characters with `...` suffix in diagram labels. Full text available in phase breakdown prose. Phase names containing spaces, dashes, or special characters are slugified for Mermaid state IDs (e.g., `Trust Building` → `trust_building`) with the original name preserved as a quoted label via `state "Trust Building" as trust_building`. If slugification produces an empty string (e.g., phase name is all special characters), the fallback ID `phase_{index}` is used.

### EC-DOCS-006: Scenario With No Tools, Resources, or Prompts

**Scenario:** Phased server config where all phases only modify behavior/capabilities, no tool definitions  
**Expected:** Diagram renders phase transitions only, state body notes show behavior changes. Phase breakdown prose omits tool sections.

### EC-DOCS-007: Duplicate Tags Across Scenarios

**Scenario:** Multiple scenarios share the same tag (e.g., `#prompt-injection`)  
**Expected:** Docusaurus tag index page aggregates all scenarios with that tag. No deduplication needed at generation time.

### EC-DOCS-008: Mermaid Rendering Failure

**Scenario:** Generated Mermaid syntax is invalid (e.g., due to special characters in tool names)  
**Expected:** Generator escapes special Mermaid characters (`{`, `}`, `[`, `]`, `(`, `)`, `#`, `;`) in labels. If rendering still fails, the page falls back to a code block showing raw Mermaid source.

### EC-DOCS-009: Circular Include in Scenario

**Scenario:** Scenario uses `$include` that creates a circular reference  
**Expected:** Config Loader (TJ-SPEC-006) catches this before diagram generation. Diagram generator receives a resolved config, never raw includes.

### EC-DOCS-010: Metadata References Unknown MITRE Technique

**Scenario:** `metadata.mitre_attack.techniques[].id` is syntactically valid but not a real ATT&CK technique  
**Expected:** Validation passes (we validate format, not existence). The generated link to attack.mitre.org may 404. Future enhancement: validate against a bundled ATT&CK STIX dataset.

### EC-DOCS-011: Registry Category With Zero Scenarios

**Scenario:** A category in `registry.yaml` has an empty `scenarios` list  
**Expected:** Category is excluded from sidebar and index page. Warning logged.

### EC-DOCS-012: Multiple Diagram Types Applicable

**Scenario:** Phased server with conditional responses in one of its phases  
**Expected:** Auto-select chooses `stateDiagram-v2` (phased takes priority) as the primary diagram. Phases containing `match` blocks additionally generate supplementary `sequenceDiagram` diagrams rendered below the primary diagram (see F-002: Supplementary Per-Phase Diagrams). User can also generate a standalone sequence diagram via CLI `--type sequence` for the full scenario.

### EC-DOCS-013: Concurrent Documentation Builds

**Scenario:** Two CI jobs run `thoughtjack docs generate` simultaneously  
**Expected:** Each job writes to its own output directory. No shared state between builds. No file locking required.

### EC-DOCS-014: Scenario YAML Contains Mermaid-Unsafe Characters in Strings

**Scenario:** Tool name contains characters like `#`, `:`, `()`, `[]`, or newlines  
**Expected:** All Mermaid labels are wrapped in double quotes (`"`) by default, using diagram-type-aware quoting rules defined in F-002 (Mermaid Label Escaping). Internal `"` characters are escaped as `#quot;`. The `escape.rs` module is tested against a fixture set including regex patterns, URIs, and multi-line strings. The generator NEVER emits unquoted label text — defensive quoting is the default, not a special case.

### EC-DOCS-015: Very Large Scenario Count (100+)

**Scenario:** Registry contains 100+ scenarios across many categories  
**Expected:** Sidebar remains navigable via category grouping. Index page uses pagination or card grid. Build time scales linearly (no N² operations in generation).

### EC-DOCS-016: Metadata Block in `$include` Target

**Scenario:** A top-level scenario includes a fragment file (e.g., `$include: tools/calculator/injection.yaml`) and the fragment file contains its own `metadata` block  
**Expected:** Per the Metadata Ownership Rule (F-001), the Config Loader logs a warning ("metadata block in $include target tools/calculator/injection.yaml will be ignored") and discards the fragment's metadata. Only the top-level file's metadata is used for catalog generation. No merge, no error.

---

## 4. Repository Structure

```
thoughtjack/
├── scenarios/                          # Source of truth for attacks
│   ├── registry.yaml                   # Catalog ordering and site config
│   ├── injection/
│   │   ├── tool-response-injection.yaml
│   │   ├── resource-injection.yaml
│   │   ├── prompt-poisoning.yaml
│   │   └── multi-turn-context-poisoning.yaml
│   ├── phased/
│   │   ├── rug-pull-basic.yaml
│   │   ├── sleeper-activation.yaml
│   │   ├── capability-escalation.yaml
│   │   └── prompt-rug-pull.yaml
│   ├── dos/
│   │   ├── nested-json-bomb.yaml
│   │   ├── notification-flood.yaml
│   │   ├── slow-loris.yaml
│   │   └── unbounded-line.yaml
│   └── evasion/
│       ├── typosquat-tool.yaml
│       └── description-hijack.yaml
│
├── docs-site/                          # Docusaurus project
│   ├── docusaurus.config.js
│   ├── sidebars.js                     # Partially auto-generated (scenarios + coverage)
│   ├── package.json
│   ├── src/
│   │   ├── components/
│   │   │   ├── AttackMetadataCard.tsx
│   │   │   ├── MermaidDiagram.tsx
│   │   │   ├── YamlSourceToggle.tsx
│   │   │   ├── MitreMapping.tsx
│   │   │   ├── OwaspMcpMapping.tsx
│   │   │   ├── CoverageMatrix.tsx
│   │   │   └── SeverityBadge.tsx
│   │   ├── css/
│   │   │   └── custom.css
│   │   └── pages/
│   │       └── index.tsx               # Landing page
│   ├── docs/                           
│   │   ├── tutorials/                  # HAND-AUTHORED — Diátaxis: Tutorials
│   │   │   ├── index.mdx
│   │   │   ├── getting-started.mdx
│   │   │   ├── first-rug-pull.mdx
│   │   │   ├── testing-a-proxy.mdx
│   │   │   └── writing-a-scenario.mdx
│   │   ├── how-to/                     # HAND-AUTHORED — Diátaxis: How-To Guides
│   │   │   ├── index.mdx
│   │   │   ├── run-via-stdio.mdx
│   │   │   ├── run-via-http.mdx
│   │   │   ├── create-phased-attack.mdx
│   │   │   ├── add-injection-payloads.mdx
│   │   │   ├── generate-dos-payloads.mdx
│   │   │   ├── use-dynamic-responses.mdx
│   │   │   ├── record-replay-traffic.mdx
│   │   │   ├── add-to-catalog.mdx
│   │   │   └── integrate-ci-cd.mdx
│   │   ├── reference/                  # HAND-AUTHORED — Diátaxis: Reference
│   │   │   ├── index.mdx
│   │   │   ├── configuration-schema.mdx
│   │   │   ├── cli.mdx
│   │   │   ├── phase-engine.mdx
│   │   │   ├── behavioral-modes.mdx
│   │   │   ├── payload-generation.mdx
│   │   │   ├── dynamic-responses.mdx
│   │   │   ├── observability.mdx
│   │   │   ├── record-replay-format.mdx
│   │   │   └── metadata-schema.mdx
│   │   ├── explanation/                # HAND-AUTHORED — Diátaxis: Explanation
│   │   │   ├── index.mdx
│   │   │   ├── why-thoughtjack.mdx
│   │   │   ├── mcp-attack-surface.mdx
│   │   │   ├── thoughtjack-vs-thoughtgate.mdx
│   │   │   ├── design-decisions.mdx
│   │   │   ├── attack-pattern-taxonomy.mdx
│   │   │   └── framework-mapping-guide.mdx
│   │   ├── scenarios/                  # AUTO-GENERATED — do not edit
│   │   │   ├── index.mdx
│   │   │   ├── injection/
│   │   │   │   ├── tool-response-injection.mdx
│   │   │   │   └── ...
│   │   │   ├── phased/
│   │   │   │   ├── rug-pull-basic.mdx
│   │   │   │   └── ...
│   │   │   ├── dos/
│   │   │   │   └── ...
│   │   │   └── evasion/
│   │   │       └── ...
│   │   └── coverage/                   # AUTO-GENERATED — do not edit
│   │       ├── mitre-matrix.mdx
│   │       ├── owasp-mcp.mdx
│   │       └── mcp-attack-surface.mdx
│   └── static/
│       └── img/
│           └── logo.svg
│
├── tools/
│   └── scenario-docs-gen/              # Rust documentation generator
│       ├── Cargo.toml
│       ├── src/
│       │   ├── main.rs                 # CLI entry point
│       │   ├── config.rs               # Scenario + metadata parsing
│       │   ├── mermaid/
│       │   │   ├── mod.rs
│       │   │   ├── state_diagram.rs    # stateDiagram-v2 renderer
│       │   │   ├── sequence_diagram.rs # sequenceDiagram renderer
│       │   │   ├── flowchart.rs        # flowchart renderer
│       │   │   └── escape.rs           # Mermaid character escaping
│       │   ├── mdx/
│       │   │   ├── mod.rs
│       │   │   ├── page_renderer.rs    # Per-scenario MDX generation
│       │   │   ├── prose_gen.rs        # Structural element → prose
│       │   │   └── frontmatter.rs      # MDX frontmatter generation
│       │   ├── coverage/
│       │   │   ├── mod.rs
│       │   │   ├── mitre_matrix.rs     # MITRE cross-reference
│       │   │   ├── owasp_mcp.rs        # OWASP MCP cross-reference
│       │   │   └── mcp_surface.rs      # MCP-native cross-reference
│       │   ├── sidebar_gen.rs          # sidebars.js generation
│       │   ├── registry.rs             # registry.yaml parsing
│       │   └── validate.rs             # Metadata validation (F-010)
│       └── tests/
│           ├── mermaid_tests.rs
│           ├── mdx_tests.rs
│           └── fixtures/
│               ├── rug-pull.yaml
│               └── expected/
│                   ├── rug-pull.mdx
│                   └── rug-pull.mmd
```

---

## 5. Cross-References

| Spec | Relationship |
|------|-------------|
| **TJ-SPEC-001** | Extended by: `metadata` block added as optional top-level key. Referenced by: Reference section (Configuration Schema page). |
| **TJ-SPEC-002** | Referenced by: Reference section (Transport page), How-To guides (stdio/HTTP guides). |
| **TJ-SPEC-003** | Consumed by: Phase structure drives `stateDiagram-v2` generation. Referenced by: Reference section (Phase Engine page). |
| **TJ-SPEC-004** | Consumed by: Behavioral modes drive flowchart and prose generation. Referenced by: Reference section (Behavioral Modes page). |
| **TJ-SPEC-005** | Consumed by: `$generate` directives described in prose sections. Referenced by: Reference section (Payload Generation page). |
| **TJ-SPEC-006** | Dependency: Config Loader must parse and expose `metadata` block. |
| **TJ-SPEC-007** | Extended by: `diagram` and `docs` subcommands added to CLI hierarchy. Referenced by: Reference section (CLI page). |
| **TJ-SPEC-008** | Complementary: Observability captures runtime data; docs describe design-time intent. Referenced by: Reference section (Observability page). |
| **TJ-SPEC-009** | Consumed by: Dynamic response `match` blocks drive sequence diagram generation. Referenced by: Reference section (Dynamic Responses page). |
| **TJ-SPEC-010** | Complementary: Recorded traffic could feed into scenario documentation as examples. Referenced by: Reference section (Record & Replay page). |

---

## 6. Future Enhancements

| Enhancement | Description | Blocked By |
|-------------|-------------|------------|
| **Auto-generated reference pages** | Generate Reference section pages directly from TJ-SPEC-* Markdown files | Spec format standardization |
| **ATT&CK STIX validation** | Validate MITRE technique IDs against bundled STIX dataset | Requires STIX parser crate |
| **Interactive diagrams** | Click-to-expand phases in Mermaid diagrams | Mermaid.js interactivity support |
| **Scenario comparison** | Side-by-side diff of two scenario diagrams | UI design needed |
| **Test result integration** | Link docs pages to CI test results for each scenario | TJ-SPEC-008 report format |
| **Community contributions page** | Auto-generated contributor attribution from git history | Git log parsing |
| **Versioned scenarios** | Docusaurus versioned docs for scenario schema changes | Schema versioning policy |
| **STIX/TAXII export** | Export scenario metadata as STIX bundles for threat intel sharing | STIX serialization crate |
| **PDF export** | Generate printable PDF catalog from Docusaurus build | Docusaurus PDF plugin |
| **Search analytics** | Track which docs pages / scenarios are most viewed to prioritize content | Docusaurus analytics plugin |
| **OWASP Agentic AI Top 10 activation** | Enable `owasp_agentic` coverage page and metadata usage when A2A scenario support ships (field is already reserved in schema) | A2A protocol support |
| **OWASP LLM Top 10 mapping** | Add optional OWASP LLM Top 10 coverage page if user demand warrants it (documented in Explanation section for now) | Community feedback |
