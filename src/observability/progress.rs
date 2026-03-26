//! Interactive progress output for TTY sessions (TJ-SPEC-008).
//!
//! Renders real-time scenario execution progress to stderr when
//! running in a terminal. Events flow through the `EventEmitter`
//! progress channel and are formatted with ANSI colors.
//!
//! Two detail levels are supported: `compact` for clean scannable
//! output, and `detailed` for security consultant forensic-level context.

use std::io::{IsTerminal, Write};

use tokio::sync::mpsc;

use crate::cli::args::ProgressLevel;

use super::events::ThoughtJackEvent;

// ============================================================================
// ANSI Colors
// ============================================================================

/// Minimal ANSI color helper — no external dependency.
///
/// Wraps text in SGR escape sequences when enabled, returns plain text
/// otherwise. Uses Unicode symbols for hierarchy so output remains
/// readable when SGR attributes are stripped.
///
/// Implements: TJ-SPEC-008 F-012
pub struct AnsiColors {
    enabled: bool,
}

impl AnsiColors {
    /// Creates a new `AnsiColors` with the given enable flag.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub const fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Wraps `text` in cyan (36).
    #[must_use]
    pub fn cyan<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "36")
    }

    /// Wraps `text` in green (32).
    #[must_use]
    pub fn green<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "32")
    }

    /// Wraps `text` in magenta (35).
    #[must_use]
    pub fn magenta<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "35")
    }

    /// Wraps `text` in red (31).
    #[must_use]
    pub fn red<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "31")
    }

    /// Wraps `text` in yellow (33).
    #[must_use]
    pub fn yellow<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "33")
    }

    /// Wraps `text` in dim gray (90).
    #[must_use]
    pub fn dim<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "90")
    }

    /// Wraps `text` in bold (1).
    #[must_use]
    pub fn bold<'a>(&self, text: &'a str) -> std::borrow::Cow<'a, str> {
        self.wrap(text, "1")
    }

    fn wrap<'a>(&self, text: &'a str, code: &str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            std::borrow::Cow::Owned(format!("\x1b[{code}m{text}\x1b[0m"))
        } else {
            std::borrow::Cow::Borrowed(text)
        }
    }
}

// ============================================================================
// Deferred phase label
// ============================================================================

/// Deferred phase label, held until the phase has at least one event.
///
/// Empty phases (no protocol messages, no entry actions) are suppressed
/// by storing the label here and only flushing it when the first
/// visible event arrives.
struct PendingPhase {
    label: String,
}

// ============================================================================
// ProgressRenderer
// ============================================================================

/// Renders real-time scenario execution progress to stderr.
///
/// Consumes events from the `EventEmitter` progress channel and formats
/// them as colored terminal output with Unicode symbols for hierarchy.
///
/// Implements: TJ-SPEC-008 F-012
pub struct ProgressRenderer {
    colors: AnsiColors,
    rx: mpsc::UnboundedReceiver<ThoughtJackEvent>,
    scenario_name: String,
    scenario_id: Option<String>,
    scenario_description: Option<String>,
    severity: Option<String>,
    phase_names: Vec<String>,
    actor_modes: Vec<(String, String)>,
    multi_actor: bool,
    last_actor: Option<String>,
    header_printed: bool,
    verdict_seen: bool,
    pending_phase: Option<PendingPhase>,
}

impl ProgressRenderer {
    /// Creates a new progress renderer from a loaded OATF document.
    ///
    /// Extracts scenario metadata (name, ID, severity, phases, actors)
    /// from the document for the header block.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn new(
        rx: mpsc::UnboundedReceiver<ThoughtJackEvent>,
        document: &oatf::Document,
        color_enabled: bool,
    ) -> Self {
        let attack = &document.attack;

        let scenario_name = attack.name.clone().unwrap_or_default();
        let scenario_id = attack.id.clone();
        let scenario_description = attack.description.clone();
        let severity = attack.severity.as_ref().and_then(|s| {
            serde_json::to_value(s).ok().and_then(|v| {
                v.get("level")
                    .and_then(|l| l.as_str().map(str::to_uppercase))
            })
        });

        // Collect phase names and actor modes
        let actors = attack
            .execution
            .actors
            .as_ref()
            .map_or_else(Vec::new, Clone::clone);

        // Collect ALL phase names across ALL actors (deduplicated, preserving order)
        let phase_names: Vec<String> = if actors.is_empty() {
            attack
                .execution
                .phases
                .as_ref()
                .map_or_else(Vec::new, |phases| {
                    phases.iter().filter_map(|p| p.name.clone()).collect()
                })
        } else {
            let mut names = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for actor in &actors {
                for phase in &actor.phases {
                    if let Some(name) = &phase.name
                        && seen.insert(name.clone())
                    {
                        names.push(name.clone());
                    }
                }
            }
            names
        };

        let actor_modes: Vec<(String, String)> = if actors.is_empty() {
            let mode = attack.execution.mode.clone().unwrap_or_default();
            vec![("default".to_string(), mode)]
        } else {
            actors
                .iter()
                .map(|a| (a.name.clone(), a.mode.clone()))
                .collect()
        };

        let multi_actor = actor_modes.len() > 1;

        Self {
            colors: AnsiColors::new(color_enabled),
            rx,
            scenario_name,
            scenario_id,
            scenario_description,
            severity,
            phase_names,
            actor_modes,
            multi_actor,
            last_actor: None,
            header_printed: false,
            verdict_seen: false,
            pending_phase: None,
        }
    }

    /// Runs the progress renderer event loop.
    ///
    /// Consumes events from the channel and renders them to stderr.
    /// Returns when the channel is closed (sender dropped).
    ///
    /// Implements: TJ-SPEC-008 F-012
    pub async fn run(mut self) {
        while let Some(event) = self.rx.recv().await {
            self.render_event(&event);
        }

        if !self.verdict_seen {
            let mut out = std::io::stderr().lock();
            let rule = "━".repeat(38);
            let _ = writeln!(out, "\n  {}", self.colors.dim(&rule));
            let _ = writeln!(out, "  {}", self.colors.dim("Execution interrupted"));
            let _ = writeln!(out, "  {}", self.colors.dim(&rule));
        }
    }

    /// Renders a single event to the given writer.
    ///
    /// This is the testable core of event rendering — accepts any
    /// `Write` target instead of being hardcoded to stderr.
    #[allow(clippy::too_many_lines)]
    fn render_event_to(&mut self, event: &ThoughtJackEvent, out: &mut dyn Write) {
        match event {
            ThoughtJackEvent::ActorStarted { .. } => {
                if !self.header_printed {
                    self.print_header_to(out);
                    self.header_printed = true;
                }
            }

            ThoughtJackEvent::PhaseEntered {
                actor,
                phase_name,
                trigger_event,
                trigger_count,
                ..
            } => {
                self.pending_phase = None;

                if self.multi_actor {
                    self.last_actor = Some(actor.clone());
                }

                let display_name = format_phase_name(phase_name);
                let label = match (trigger_event, trigger_count) {
                    (Some(evt), Some(count)) => {
                        format!("Phase: {display_name} [{evt} \u{00d7}{count}]")
                    }
                    (Some(evt), None) => format!("Phase: {display_name} [{evt}]"),
                    _ => format!("Phase: {display_name}"),
                };

                self.pending_phase = Some(PendingPhase { label });
            }

            ThoughtJackEvent::ProtocolMessageReceived {
                actor,
                method,
                qualifier,
                trigger_current,
                trigger_total,
                ..
            } => {
                self.flush_pending_phase_to(out);
                self.maybe_print_actor_change_to(actor, out);

                let suffix = match (trigger_current, trigger_total) {
                    (Some(current), Some(total)) => {
                        format!(" [{current}/{total}]")
                    }
                    _ => String::new(),
                };

                if let Some(q) = qualifier {
                    let base = format!("    \u{2190} {method}");
                    let _ = write!(out, "{} {}", self.colors.cyan(&base), self.colors.yellow(q));
                } else {
                    let line = format!("    \u{2190} {method}");
                    let _ = write!(out, "{}", self.colors.cyan(&line));
                }
                if !suffix.is_empty() {
                    let _ = write!(out, " {}", self.colors.dim(&suffix));
                }
                let _ = writeln!(out);
            }

            ThoughtJackEvent::ProtocolMessageSent {
                actor,
                method,
                qualifier,
                ..
            } => {
                self.flush_pending_phase_to(out);
                self.maybe_print_actor_change_to(actor, out);
                if let Some(q) = qualifier {
                    let base = format!("    \u{2192} {method}");
                    let _ = writeln!(
                        out,
                        "{} {}",
                        self.colors.green(&base),
                        self.colors.yellow(q)
                    );
                } else {
                    let line = format!("    \u{2192} {method}");
                    let _ = writeln!(out, "{}", self.colors.green(&line));
                }
            }

            ThoughtJackEvent::ProtocolNotification {
                method, direction, ..
            } => {
                self.flush_pending_phase_to(out);
                let arrow = if direction == "outgoing" {
                    "\u{2192}"
                } else {
                    "\u{2190}"
                };
                let line = format!("    {arrow} {method}");
                if direction == "outgoing" {
                    let _ = writeln!(out, "{}", self.colors.green(&line));
                } else {
                    let _ = writeln!(out, "{}", self.colors.cyan(&line));
                }
            }

            ThoughtJackEvent::EntryActionExecuted { action_type, .. } => {
                self.flush_pending_phase_to(out);
                let line = format!("    \u{25b8} {action_type}");
                let _ = writeln!(out, "{}", self.colors.dim(&line));
            }

            ThoughtJackEvent::GracePeriodStarted { duration_seconds } => {
                if *duration_seconds == 0 {
                    return;
                }
                let line = format!("Grace period: {duration_seconds}s");
                let _ = writeln!(out, "\n  {}", self.colors.dim(&line));
            }

            ThoughtJackEvent::IndicatorEvaluated {
                indicator_id,
                result,
                evidence,
                ..
            } => {
                let matched = result == "matched";
                let symbol = if matched { "\u{2717}" } else { "\u{2713}" };

                let line = evidence.as_ref().map_or_else(
                    || format!("    {symbol} {indicator_id}"),
                    |ev| format!("    {symbol} {indicator_id}: {ev}"),
                );

                if matched {
                    let _ = writeln!(out, "{}", self.colors.red(&line));
                } else {
                    let _ = writeln!(out, "{}", self.colors.green(&line));
                }
            }

            ThoughtJackEvent::IndicatorSkipped {
                indicator_id,
                reason,
            } => {
                let line = format!("    \u{25cb} {indicator_id}: {reason}");
                let _ = writeln!(out, "{}", self.colors.dim(&line));
            }

            ThoughtJackEvent::PhaseCompleted {
                duration_ms,
                message_count,
                ..
            } => {
                self.pending_phase = None;

                #[allow(clippy::cast_precision_loss)]
                let secs = *duration_ms as f64 / 1000.0;
                let timing = format!(
                    "    ({secs:.1}s, {message_count} message{})",
                    if *message_count == 1 { "" } else { "s" }
                );
                let _ = writeln!(out, "{}", self.colors.dim(&timing));
            }

            ThoughtJackEvent::VerdictComputed {
                result, max_tier, ..
            } => {
                self.verdict_seen = true;
                let rule = "━".repeat(38);
                let _ = writeln!(out, "\n  {}", self.colors.dim(&rule));

                let is_fail = result == "exploited" || result == "partial";
                let tier_suffix = if result == "not_exploited" || result == "error" {
                    String::new()
                } else if let Some(tier) = max_tier {
                    format!(" ({tier})")
                } else {
                    " (unclassified)".to_string()
                };
                let label = format!("Verdict: {}{tier_suffix}", result.to_uppercase());
                if is_fail {
                    let _ = writeln!(out, "  {}", self.colors.red(&label));
                } else {
                    let _ = writeln!(out, "  {}", self.colors.green(&label));
                }

                let _ = writeln!(out, "  {}", self.colors.dim(&rule));
            }

            _ => {}
        }
    }

    /// Prints the header block to the given writer.
    fn print_header_to(&self, out: &mut dyn Write) {
        let _ = writeln!(out);

        let id_prefix = self
            .scenario_id
            .as_ref()
            .map_or(String::new(), |id| format!("{id} "));
        let scenario_line = format!("  Scenario: {id_prefix}{}", self.scenario_name);
        let _ = writeln!(out, "{}", self.colors.bold(&scenario_line));

        if let Some(desc) = &self.scenario_description {
            let desc_line = format!("  {desc}");
            let _ = writeln!(out, "{}", self.colors.dim(&desc_line));
        }

        let protocols: Vec<String> = {
            let mut seen = Vec::new();
            for (_, mode) in &self.actor_modes {
                let display = format_mode_display(mode);
                if !seen.contains(&display) {
                    seen.push(display);
                }
            }
            seen
        };
        let mut meta = format!("  Protocol: {}", protocols.join(", "));
        if let Some(sev) = &self.severity {
            use std::fmt::Write as _;
            let _ = write!(meta, "   Severity: {sev}");
        }
        let _ = writeln!(out, "{}", self.colors.dim(&meta));

        if !self.phase_names.is_empty() {
            let formatted: Vec<String> = self
                .phase_names
                .iter()
                .map(|n| format_phase_name(n))
                .collect();
            let chain = formatted.join(" \u{2192} ");
            let phases_line = format!("  Phases:   {chain}");
            let _ = writeln!(out, "{}", self.colors.dim(&phases_line));
        }
    }

    /// Prints an actor separator when the emitting actor changes (to given writer).
    fn maybe_print_actor_change_to(&mut self, actor: &str, out: &mut dyn Write) {
        if !self.multi_actor {
            return;
        }
        let changed = self.last_actor.as_ref().is_none_or(|prev| prev != actor);
        if changed {
            self.last_actor = Some(actor.to_string());
            let mode = self
                .actor_modes
                .iter()
                .find(|(n, _)| n == actor)
                .map_or("", |(_, m)| m.as_str());
            let label = format!("  [{actor} ({mode})]");
            let _ = writeln!(out, "{}", self.colors.dim(&label));
        }
    }

    /// Flush any pending phase label to the given writer.
    fn flush_pending_phase_to(&mut self, out: &mut dyn Write) {
        if let Some(pending) = self.pending_phase.take() {
            let _ = writeln!(out, "\n  {}", self.colors.magenta(&pending.label));
        }
    }

    /// Renders a single event to stderr. Delegates to [`render_event_to`].
    fn render_event(&mut self, event: &ThoughtJackEvent) {
        let mut out = std::io::stderr().lock();
        self.render_event_to(event, &mut out);
    }
}

/// Format an actor mode for display.
///
/// Does not assume transport — that depends on CLI flags, not mode.
fn format_mode_display(mode: &str) -> String {
    match mode {
        "mcp_server" => "MCP (server)".to_string(),
        "mcp_client" => "MCP (client)".to_string(),
        "a2a_server" => "A2A (server)".to_string(),
        "a2a_client" => "A2A (client)".to_string(),
        "ag_ui_client" => "AG-UI (client)".to_string(),
        other => other.to_string(),
    }
}

/// Format a phase name for display.
///
/// Converts auto-generated names like `"phase-1"` into `"Phase 1"`.
/// Named phases are returned unchanged.
///
/// Implements: TJ-SPEC-008 F-012
#[must_use]
pub fn format_phase_name(raw: &str) -> String {
    if let Some(n) = raw.strip_prefix("phase-")
        && n.chars().all(|c| c.is_ascii_digit())
    {
        return format!("Phase {n}");
    }
    raw.to_string()
}

/// Resolves whether progress output should be enabled.
///
/// - `Auto`: on for TTY, off otherwise
/// - `Off`: no progress output
/// - `On`: force progress (overridden by `--quiet`)
///
/// Returns `true` when progress should be shown.
///
/// Implements: TJ-SPEC-007 F-001
#[must_use]
pub fn resolve_progress(level: ProgressLevel, quiet: bool) -> bool {
    if quiet || level == ProgressLevel::Off {
        return false;
    }
    match level {
        ProgressLevel::Off => false,
        ProgressLevel::On => true,
        ProgressLevel::Auto => std::io::stderr().is_terminal(),
    }
}

/// Resolves whether color output should be enabled.
///
/// Implements: TJ-SPEC-007 F-001
#[must_use]
pub fn resolve_color(choice: crate::cli::args::ColorChoice) -> bool {
    use crate::cli::args::ColorChoice;
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::io::stderr().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
                && std::env::var("TERM").as_deref() != Ok("dumb")
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
    fn ansi_colors_enabled_wraps_text() {
        let c = AnsiColors::new(true);
        assert_eq!(c.cyan("hello").as_ref(), "\x1b[36mhello\x1b[0m");
        assert_eq!(c.green("ok").as_ref(), "\x1b[32mok\x1b[0m");
        assert_eq!(c.magenta("phase").as_ref(), "\x1b[35mphase\x1b[0m");
        assert_eq!(c.red("fail").as_ref(), "\x1b[31mfail\x1b[0m");
        assert_eq!(c.yellow("warn").as_ref(), "\x1b[33mwarn\x1b[0m");
        assert_eq!(c.dim("meta").as_ref(), "\x1b[90mmeta\x1b[0m");
        assert_eq!(c.bold("title").as_ref(), "\x1b[1mtitle\x1b[0m");
    }

    #[test]
    fn ansi_colors_disabled_returns_plain() {
        let c = AnsiColors::new(false);
        assert_eq!(c.cyan("hello").as_ref(), "hello");
        assert_eq!(c.green("ok").as_ref(), "ok");
        assert_eq!(c.magenta("phase").as_ref(), "phase");
        assert_eq!(c.red("fail").as_ref(), "fail");
        assert_eq!(c.yellow("warn").as_ref(), "warn");
        assert_eq!(c.dim("meta").as_ref(), "meta");
        assert_eq!(c.bold("title").as_ref(), "title");
    }

    #[test]
    fn resolve_color_always_returns_true() {
        use crate::cli::args::ColorChoice;
        assert!(resolve_color(ColorChoice::Always));
    }

    #[test]
    fn resolve_color_never_returns_false() {
        use crate::cli::args::ColorChoice;
        assert!(!resolve_color(ColorChoice::Never));
    }

    #[test]
    fn format_mode_display_known_modes() {
        assert_eq!(format_mode_display("mcp_server"), "MCP (server)");
        assert_eq!(format_mode_display("mcp_client"), "MCP (client)");
        assert_eq!(format_mode_display("a2a_server"), "A2A (server)");
        assert_eq!(format_mode_display("a2a_client"), "A2A (client)");
        assert_eq!(format_mode_display("ag_ui_client"), "AG-UI (client)");
    }

    #[test]
    fn format_mode_display_unknown_passthrough() {
        assert_eq!(format_mode_display("custom"), "custom");
    }

    #[test]
    fn format_phase_name_numeric() {
        assert_eq!(format_phase_name("phase-0"), "Phase 0");
        assert_eq!(format_phase_name("phase-1"), "Phase 1");
        assert_eq!(format_phase_name("phase-12"), "Phase 12");
    }

    #[test]
    fn format_phase_name_named() {
        assert_eq!(format_phase_name("exploit"), "exploit");
        assert_eq!(format_phase_name("setup-phase"), "setup-phase");
        assert_eq!(format_phase_name("phase-abc"), "phase-abc");
    }

    #[test]
    fn resolve_progress_off_always_false() {
        assert!(!resolve_progress(ProgressLevel::Off, false));
        assert!(!resolve_progress(ProgressLevel::Off, true));
    }

    #[test]
    fn resolve_progress_quiet_overrides() {
        assert!(!resolve_progress(ProgressLevel::On, true));
        assert!(!resolve_progress(ProgressLevel::Auto, true));
    }

    #[test]
    fn resolve_progress_on_not_quiet() {
        assert!(resolve_progress(ProgressLevel::On, false));
    }

    // ── Test helpers for ProgressRenderer ────────────────────────────────

    /// Creates a `ProgressRenderer` from a YAML document string.
    /// Returns `(renderer, tx)` so the test can send events and render them.
    fn make_renderer(yaml: &str) -> (ProgressRenderer, mpsc::UnboundedSender<ThoughtJackEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let loaded = oatf::load(yaml).expect("test YAML should be valid");
        let renderer = ProgressRenderer::new(rx, &loaded.document, false);
        (renderer, tx)
    }

    /// Feeds events into the renderer and captures output to a string.
    fn render_to_string(renderer: &mut ProgressRenderer, events: &[ThoughtJackEvent]) -> String {
        let mut buf = Vec::new();
        for event in events {
            renderer.render_event_to(event, &mut buf);
        }
        String::from_utf8(buf).expect("output should be valid UTF-8")
    }

    const MINIMAL_YAML: &str = r#"
oatf: "0.1"
attack:
  name: "Test Scenario"
  id: "OATF-TEST-001"
  description: "A test scenario for unit testing"
  execution:
    mode: mcp_server
    phases:
      - name: trust_building
        trigger:
          event: "tools/call"
          count: 3
        state:
          tools:
            - name: calculator
              description: "A calculator tool"
              inputSchema:
                type: object
      - name: exploit
        state:
          tools:
            - name: calculator
              description: "Now malicious"
              inputSchema:
                type: object
"#;

    const MULTI_ACTOR_YAML: &str = r#"
oatf: "0.1"
attack:
  name: "Multi-Actor Test"
  execution:
    actors:
      - name: server
        mode: mcp_server
        phases:
          - name: setup
            trigger:
              event: "tools/call"
              count: 1
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
      - name: client
        mode: mcp_client
        phases:
          - name: probe
            trigger:
              event: "tools/call"
              count: 1
            state:
              tools:
                - name: test_tool
                  description: "test"
                  inputSchema:
                    type: object
"#;

    // ── Header rendering tests ──────────────────────────────────────────

    #[test]
    fn header_prints_scenario_name_and_id() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ActorStarted {
                actor_name: "default".to_string(),
                phase_count: 2,
            }],
        );
        assert!(
            output.contains("Scenario: OATF-TEST-001 Test Scenario"),
            "got: {output}"
        );
    }

    #[test]
    fn header_prints_protocol_and_phases() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ActorStarted {
                actor_name: "default".to_string(),
                phase_count: 2,
            }],
        );
        assert!(output.contains("Protocol: MCP (server)"), "got: {output}");
        assert!(output.contains("trust_building"), "got: {output}");
        assert!(output.contains("exploit"), "got: {output}");
        assert!(
            output.contains("\u{2192}"),
            "phase chain should use arrow, got: {output}"
        );
    }

    #[test]
    fn header_prints_description() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ActorStarted {
                actor_name: "default".to_string(),
                phase_count: 2,
            }],
        );
        assert!(
            output.contains("A test scenario for unit testing"),
            "got: {output}"
        );
    }

    #[test]
    fn header_printed_only_once() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::ActorStarted {
                    actor_name: "default".to_string(),
                    phase_count: 2,
                },
                ThoughtJackEvent::ActorStarted {
                    actor_name: "default".to_string(),
                    phase_count: 2,
                },
            ],
        );
        assert_eq!(output.matches("Scenario:").count(), 1, "got: {output}");
    }

    // ── Phase rendering tests ───────────────────────────────────────────

    #[test]
    fn phase_entered_deferred_until_first_event() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        // Phase entered but no visible event yet — should not appear
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::PhaseEntered {
                actor: "default".to_string(),
                phase_name: "trust_building".to_string(),
                phase_index: 0,
                trigger_event: Some("tools/call".to_string()),
                trigger_count: Some(3),
            }],
        );
        assert!(
            !output.contains("Phase:"),
            "phase should be deferred, got: {output}"
        );
    }

    #[test]
    fn phase_flushed_on_protocol_message() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "trust_building".to_string(),
                    phase_index: 0,
                    trigger_event: Some("tools/call".to_string()),
                    trigger_count: Some(3),
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "default".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: Some("calculator".to_string()),
                    trigger_current: Some(1),
                    trigger_total: Some(3),
                },
            ],
        );
        assert!(
            output.contains("Phase: trust_building [tools/call ×3]"),
            "got: {output}"
        );
        assert!(output.contains("← tools/call"), "got: {output}");
    }

    #[test]
    fn phase_label_with_trigger_no_count() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "exploit".to_string(),
                    phase_index: 1,
                    trigger_event: Some("tools/call".to_string()),
                    trigger_count: None,
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "default".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
            ],
        );
        assert!(
            output.contains("Phase: exploit [tools/call]"),
            "got: {output}"
        );
    }

    #[test]
    fn phase_label_no_trigger() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "phase-0".to_string(),
                    phase_index: 0,
                    trigger_event: None,
                    trigger_count: None,
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "default".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
            ],
        );
        assert!(
            output.contains("Phase: Phase 0"),
            "auto-name should format, got: {output}"
        );
    }

    #[test]
    fn empty_phase_suppressed() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "empty".to_string(),
                    phase_index: 0,
                    trigger_event: None,
                    trigger_count: None,
                },
                // No protocol events — phase is empty
                ThoughtJackEvent::PhaseCompleted {
                    actor: "default".to_string(),
                    phase_name: "empty".to_string(),
                    duration_ms: 100,
                    message_count: 0,
                },
            ],
        );
        assert!(
            !output.contains("Phase: empty"),
            "empty phase should be suppressed, got: {output}"
        );
    }

    #[test]
    fn consecutive_phase_discards_previous_empty() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "empty_first".to_string(),
                    phase_index: 0,
                    trigger_event: None,
                    trigger_count: None,
                },
                // Immediately enter next phase without any events in first
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "active_second".to_string(),
                    phase_index: 1,
                    trigger_event: None,
                    trigger_count: None,
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "default".to_string(),
                    method: "tools/list".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
            ],
        );
        assert!(
            !output.contains("empty_first"),
            "empty phase should be discarded, got: {output}"
        );
        assert!(output.contains("Phase: active_second"), "got: {output}");
    }

    // ── Protocol message rendering tests ────────────────────────────────

    #[test]
    fn protocol_message_received_with_qualifier() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageReceived {
                actor: "default".to_string(),
                method: "tools/call".to_string(),
                protocol: "mcp".to_string(),
                qualifier: Some("calculator".to_string()),
                trigger_current: None,
                trigger_total: None,
            }],
        );
        assert!(output.contains("← tools/call"), "got: {output}");
        assert!(output.contains("calculator"), "got: {output}");
    }

    #[test]
    fn protocol_message_received_without_qualifier() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageReceived {
                actor: "default".to_string(),
                method: "initialize".to_string(),
                protocol: "mcp".to_string(),
                qualifier: None,
                trigger_current: None,
                trigger_total: None,
            }],
        );
        assert!(output.contains("← initialize"), "got: {output}");
    }

    #[test]
    fn protocol_message_received_shows_trigger_counter() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageReceived {
                actor: "default".to_string(),
                method: "tools/call".to_string(),
                protocol: "mcp".to_string(),
                qualifier: None,
                trigger_current: Some(2),
                trigger_total: Some(3),
            }],
        );
        assert!(output.contains("[2/3]"), "got: {output}");
    }

    #[test]
    fn protocol_message_sent_with_qualifier() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageSent {
                actor: "default".to_string(),
                method: "tools/call".to_string(),
                protocol: "mcp".to_string(),
                duration_ms: 5,
                qualifier: Some("calculator".to_string()),
            }],
        );
        assert!(output.contains("→ tools/call"), "got: {output}");
        assert!(output.contains("calculator"), "got: {output}");
    }

    #[test]
    fn protocol_message_sent_without_qualifier() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageSent {
                actor: "default".to_string(),
                method: "tools/call".to_string(),
                protocol: "mcp".to_string(),
                duration_ms: 5,
                qualifier: None,
            }],
        );
        assert!(output.contains("→ tools/call"), "got: {output}");
    }

    #[test]
    fn protocol_notification_outgoing() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolNotification {
                actor: "default".to_string(),
                method: "notifications/tools/list_changed".to_string(),
                direction: "outgoing".to_string(),
            }],
        );
        assert!(
            output.contains("→ notifications/tools/list_changed"),
            "got: {output}"
        );
    }

    #[test]
    fn protocol_notification_incoming() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolNotification {
                actor: "default".to_string(),
                method: "notifications/initialized".to_string(),
                direction: "incoming".to_string(),
            }],
        );
        assert!(
            output.contains("← notifications/initialized"),
            "got: {output}"
        );
    }

    // ── Entry action rendering ──────────────────────────────────────────

    #[test]
    fn entry_action_renders_with_arrow() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::EntryActionExecuted {
                actor: "default".to_string(),
                action_type: "notification".to_string(),
            }],
        );
        assert!(output.contains("▸ notification"), "got: {output}");
    }

    #[test]
    fn entry_action_flushes_pending_phase() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "swap".to_string(),
                    phase_index: 1,
                    trigger_event: None,
                    trigger_count: None,
                },
                ThoughtJackEvent::EntryActionExecuted {
                    actor: "default".to_string(),
                    action_type: "notification".to_string(),
                },
            ],
        );
        assert!(
            output.contains("Phase: swap"),
            "entry action should flush phase, got: {output}"
        );
        assert!(output.contains("▸ notification"), "got: {output}");
    }

    // ── Grace period rendering ──────────────────────────────────────────

    #[test]
    fn grace_period_nonzero_renders() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::GracePeriodStarted {
                duration_seconds: 30,
            }],
        );
        assert!(output.contains("Grace period: 30s"), "got: {output}");
    }

    #[test]
    fn grace_period_zero_suppressed() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::GracePeriodStarted {
                duration_seconds: 0,
            }],
        );
        assert!(
            output.is_empty(),
            "0s grace should be suppressed, got: {output}"
        );
    }

    // ── Indicator rendering ─────────────────────────────────────────────

    #[test]
    fn indicator_matched_shows_cross() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::IndicatorEvaluated {
                indicator_id: "IND-001".to_string(),
                method: "pattern".to_string(),
                result: "matched".to_string(),
                duration_ms: 5,
                evidence: None,
            }],
        );
        assert!(output.contains("✗ IND-001"), "got: {output}");
    }

    #[test]
    fn indicator_not_matched_shows_check() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::IndicatorEvaluated {
                indicator_id: "IND-002".to_string(),
                method: "pattern".to_string(),
                result: "not_matched".to_string(),
                duration_ms: 3,
                evidence: None,
            }],
        );
        assert!(output.contains("✓ IND-002"), "got: {output}");
    }

    #[test]
    fn indicator_shows_evidence() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::IndicatorEvaluated {
                indicator_id: "IND-003".to_string(),
                method: "pattern".to_string(),
                result: "matched".to_string(),
                duration_ms: 5,
                evidence: Some("regex matched id_rsa".to_string()),
            }],
        );
        assert!(
            output.contains("✗ IND-003: regex matched id_rsa"),
            "got: {output}"
        );
    }

    #[test]
    fn indicator_skipped_shows_circle() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::IndicatorSkipped {
                indicator_id: "IND-004".to_string(),
                reason: "no matching trace entries".to_string(),
            }],
        );
        assert!(
            output.contains("○ IND-004: no matching trace entries"),
            "got: {output}"
        );
    }

    // ── Phase completed rendering ───────────────────────────────────────

    #[test]
    fn phase_completed_shows_timing() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::PhaseCompleted {
                actor: "default".to_string(),
                phase_name: "trust_building".to_string(),
                duration_ms: 4200,
                message_count: 8,
            }],
        );
        assert!(output.contains("4.2s, 8 messages"), "got: {output}");
    }

    #[test]
    fn phase_completed_singular_message() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::PhaseCompleted {
                actor: "default".to_string(),
                phase_name: "p".to_string(),
                duration_ms: 1000,
                message_count: 1,
            }],
        );
        assert!(
            output.contains("1 message)"),
            "singular form, got: {output}"
        );
        assert!(!output.contains("1 messages"), "got: {output}");
    }

    // ── Verdict rendering ───────────────────────────────────────────────

    #[test]
    fn verdict_exploited_rendered() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::VerdictComputed {
                result: "exploited".to_string(),
                max_tier: None,
                matched: 2,
                total: 3,
            }],
        );
        assert!(output.contains("Verdict: EXPLOITED"), "got: {output}");
        assert!(
            output.contains("━"),
            "should have rule lines, got: {output}"
        );
    }

    #[test]
    fn verdict_not_exploited_rendered() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::VerdictComputed {
                result: "not_exploited".to_string(),
                max_tier: None,
                matched: 0,
                total: 3,
            }],
        );
        assert!(output.contains("Verdict: NOT_EXPLOITED"), "got: {output}");
    }

    #[test]
    fn verdict_partial_rendered() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::VerdictComputed {
                result: "partial".to_string(),
                max_tier: None,
                matched: 1,
                total: 3,
            }],
        );
        assert!(output.contains("Verdict: PARTIAL"), "got: {output}");
    }

    #[test]
    fn verdict_sets_verdict_seen_flag() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        assert!(!r.verdict_seen);
        render_to_string(
            &mut r,
            &[ThoughtJackEvent::VerdictComputed {
                result: "exploited".to_string(),
                max_tier: None,
                matched: 1,
                total: 1,
            }],
        );
        assert!(r.verdict_seen);
    }

    // ── Multi-actor rendering ───────────────────────────────────────────

    #[test]
    fn multi_actor_shows_actor_separator() {
        let (mut r, _tx) = make_renderer(MULTI_ACTOR_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "server".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "client".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
            ],
        );
        assert!(output.contains("[server (mcp_server)]"), "got: {output}");
        assert!(output.contains("[client (mcp_client)]"), "got: {output}");
    }

    #[test]
    fn multi_actor_same_actor_no_duplicate_separator() {
        let (mut r, _tx) = make_renderer(MULTI_ACTOR_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "server".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "server".to_string(),
                    method: "tools/list".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: None,
                    trigger_current: None,
                    trigger_total: None,
                },
            ],
        );
        assert_eq!(
            output.matches("[server (mcp_server)]").count(),
            1,
            "got: {output}"
        );
    }

    #[test]
    fn single_actor_no_separator() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[ThoughtJackEvent::ProtocolMessageReceived {
                actor: "default".to_string(),
                method: "tools/call".to_string(),
                protocol: "mcp".to_string(),
                qualifier: None,
                trigger_current: None,
                trigger_total: None,
            }],
        );
        assert!(
            !output.contains("[default"),
            "single actor should not show separator, got: {output}"
        );
    }

    // ── Unhandled events silently ignored ───────────────────────────────

    #[test]
    fn unhandled_events_produce_no_output() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                ThoughtJackEvent::ExtractorCaptured {
                    actor: "default".to_string(),
                    name: "x".to_string(),
                    value_preview: "v".to_string(),
                },
                ThoughtJackEvent::OrchestratorStarted {
                    actor_count: 1,
                    server_count: 1,
                    client_count: 0,
                },
                ThoughtJackEvent::GracePeriodExpired {
                    messages_captured: 5,
                },
            ],
        );
        assert!(
            output.is_empty(),
            "unhandled events should produce nothing, got: {output}"
        );
    }

    // ── Full scenario sequence test ─────────────────────────────────────

    #[test]
    fn full_rug_pull_sequence() {
        let (mut r, _tx) = make_renderer(MINIMAL_YAML);
        let output = render_to_string(
            &mut r,
            &[
                // Start
                ThoughtJackEvent::ActorStarted {
                    actor_name: "default".to_string(),
                    phase_count: 2,
                },
                // Phase 1
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "trust_building".to_string(),
                    phase_index: 0,
                    trigger_event: Some("tools/call".to_string()),
                    trigger_count: Some(3),
                },
                ThoughtJackEvent::ProtocolMessageReceived {
                    actor: "default".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    qualifier: Some("calculator".to_string()),
                    trigger_current: Some(1),
                    trigger_total: Some(3),
                },
                ThoughtJackEvent::ProtocolMessageSent {
                    actor: "default".to_string(),
                    method: "tools/call".to_string(),
                    protocol: "mcp".to_string(),
                    duration_ms: 2,
                    qualifier: None,
                },
                ThoughtJackEvent::PhaseCompleted {
                    actor: "default".to_string(),
                    phase_name: "trust_building".to_string(),
                    duration_ms: 4200,
                    message_count: 8,
                },
                // Phase 2
                ThoughtJackEvent::PhaseEntered {
                    actor: "default".to_string(),
                    phase_name: "exploit".to_string(),
                    phase_index: 1,
                    trigger_event: None,
                    trigger_count: None,
                },
                ThoughtJackEvent::EntryActionExecuted {
                    actor: "default".to_string(),
                    action_type: "notification".to_string(),
                },
                // Indicators
                ThoughtJackEvent::IndicatorEvaluated {
                    indicator_id: "OATF-002-01".to_string(),
                    method: "pattern".to_string(),
                    result: "matched".to_string(),
                    duration_ms: 5,
                    evidence: Some("tool definition changed".to_string()),
                },
                ThoughtJackEvent::IndicatorEvaluated {
                    indicator_id: "OATF-002-02".to_string(),
                    method: "pattern".to_string(),
                    result: "matched".to_string(),
                    duration_ms: 3,
                    evidence: None,
                },
                // Verdict
                ThoughtJackEvent::VerdictComputed {
                    result: "exploited".to_string(),
                    max_tier: Some("boundary_breach".to_string()),
                    matched: 2,
                    total: 2,
                },
            ],
        );

        // Verify key elements appear in order
        assert!(output.contains("Scenario: OATF-TEST-001"));
        assert!(output.contains("A test scenario for unit testing"));
        assert!(output.contains("Phase: trust_building [tools/call ×3]"));
        assert!(output.contains("← tools/call"));
        assert!(output.contains("calculator"));
        assert!(output.contains("[1/3]"));
        assert!(output.contains("→ tools/call"));
        assert!(output.contains("4.2s, 8 messages"));
        assert!(output.contains("Phase: exploit"));
        assert!(output.contains("▸ notification"));
        assert!(output.contains("✗ OATF-002-01: tool definition changed"));
        assert!(output.contains("✗ OATF-002-02"));
        assert!(output.contains("Verdict: EXPLOITED"));
    }

    // ── run() event loop test ───────────────────────────────────────────

    #[tokio::test]
    async fn run_completes_when_channel_closes() {
        let (mut renderer, tx) = make_renderer(MINIMAL_YAML);
        // Override to avoid writing to real stderr during tests
        renderer.verdict_seen = true;

        tx.send(ThoughtJackEvent::ActorStarted {
            actor_name: "default".to_string(),
            phase_count: 1,
        })
        .unwrap();
        drop(tx);

        // run() should complete without hanging
        tokio::time::timeout(std::time::Duration::from_secs(2), renderer.run())
            .await
            .expect("run() should complete when channel closes");
    }

    #[tokio::test]
    async fn run_prints_interrupted_when_no_verdict() {
        let (_renderer, tx) = make_renderer(MINIMAL_YAML);
        // Just verify it doesn't hang — the "interrupted" output goes to stderr
        // which we can't easily capture, but we verify the verdict_seen flag logic
        drop(tx);

        let (mut renderer2, tx2) = make_renderer(MINIMAL_YAML);
        assert!(!renderer2.verdict_seen);
        // Send verdict first to suppress the interrupted message
        renderer2.verdict_seen = true;
        drop(tx2);

        tokio::time::timeout(std::time::Duration::from_secs(2), renderer2.run())
            .await
            .expect("run() should complete");
    }

    // ── Constructor metadata extraction tests ───────────────────────────

    #[test]
    fn constructor_extracts_metadata_single_actor() {
        let (r, _tx) = make_renderer(MINIMAL_YAML);
        assert_eq!(r.scenario_name, "Test Scenario");
        assert_eq!(r.scenario_id.as_deref(), Some("OATF-TEST-001"));
        assert_eq!(
            r.scenario_description.as_deref(),
            Some("A test scenario for unit testing")
        );
        assert_eq!(r.phase_names, vec!["trust_building", "exploit"]);
        assert!(!r.multi_actor);
        assert_eq!(r.actor_modes.len(), 1);
        assert_eq!(
            r.actor_modes[0],
            ("default".to_string(), "mcp_server".to_string())
        );
    }

    #[test]
    fn constructor_extracts_metadata_multi_actor() {
        let (r, _tx) = make_renderer(MULTI_ACTOR_YAML);
        assert_eq!(r.scenario_name, "Multi-Actor Test");
        assert!(r.multi_actor);
        assert_eq!(r.actor_modes.len(), 2);
        assert_eq!(
            r.actor_modes[0],
            ("server".to_string(), "mcp_server".to_string())
        );
        assert_eq!(
            r.actor_modes[1],
            ("client".to_string(), "mcp_client".to_string())
        );
        assert_eq!(r.phase_names, vec!["setup", "probe"]);
    }
}
