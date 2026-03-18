//! Interactive progress output for TTY sessions (TJ-SPEC-008).
//!
//! Renders real-time scenario execution progress to stderr when
//! running in a terminal. Events flow through the `EventEmitter`
//! progress channel and are formatted with ANSI colors.

use std::io::{IsTerminal, Write};

use tokio::sync::mpsc;

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
    severity: Option<String>,
    phase_names: Vec<String>,
    actor_modes: Vec<(String, String)>,
    multi_actor: bool,
    last_actor: Option<String>,
    header_printed: bool,
    verdict_seen: bool,
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
            severity,
            phase_names,
            actor_modes,
            multi_actor,
            last_actor: None,
            header_printed: false,
            verdict_seen: false,
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

    /// Prints an actor separator when the emitting actor changes.
    fn maybe_print_actor_change(
        &mut self,
        actor: &str,
        out: &mut std::io::StderrLock<'_>,
    ) {
        if !self.multi_actor {
            return;
        }
        let changed = self
            .last_actor
            .as_ref()
            .is_none_or(|prev| prev != actor);
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

    #[allow(clippy::too_many_lines)]
    fn render_event(&mut self, event: &ThoughtJackEvent) {
        let mut out = std::io::stderr().lock();
        match event {
            ThoughtJackEvent::ActorStarted { .. } => {
                if !self.header_printed {
                    self.print_header(&mut out);
                    self.header_printed = true;
                }
            }

            ThoughtJackEvent::PhaseEntered {
                actor, phase_name, ..
            } => {
                if self.multi_actor {
                    self.last_actor = Some(actor.clone());
                }
                let label = format!("Phase: {phase_name}");
                let _ = writeln!(out, "\n  {}", self.colors.magenta(&label));
            }

            ThoughtJackEvent::ProtocolMessageReceived {
                actor,
                method,
                qualifier,
                ..
            } => {
                self.maybe_print_actor_change(actor, &mut out);
                if let Some(q) = qualifier {
                    let base = format!("    \u{2190} {method}");
                    let _ = writeln!(
                        out,
                        "{} {}",
                        self.colors.cyan(&base),
                        self.colors.yellow(q)
                    );
                } else {
                    let line = format!("    \u{2190} {method}");
                    let _ = writeln!(out, "{}", self.colors.cyan(&line));
                }
            }

            ThoughtJackEvent::ProtocolMessageSent {
                actor,
                method,
                qualifier,
                ..
            } => {
                self.maybe_print_actor_change(actor, &mut out);
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
                let line = format!("    \u{25b8} {action_type}");
                let _ = writeln!(out, "{}", self.colors.dim(&line));
            }

            ThoughtJackEvent::GracePeriodStarted { duration_seconds } => {
                let line = format!("Grace period: {duration_seconds}s");
                let _ = writeln!(out, "\n  {}", self.colors.dim(&line));
            }

            ThoughtJackEvent::IndicatorEvaluated {
                indicator_id,
                result,
                ..
            } => {
                let matched = result == "matched";
                let symbol = if matched { "\u{2717}" } else { "\u{2713}" };
                let line = format!("    {symbol} {indicator_id}");
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

            ThoughtJackEvent::VerdictComputed { result, .. } => {
                self.verdict_seen = true;
                let rule = "━".repeat(38);
                let _ = writeln!(out, "\n  {}", self.colors.dim(&rule));

                let is_fail = result == "exploited" || result == "partial";
                let label = format!("Verdict: {}", result.to_uppercase());
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

    fn print_header(&self, out: &mut std::io::StderrLock<'_>) {
        let _ = writeln!(out);

        // Scenario line
        let id_prefix = self
            .scenario_id
            .as_ref()
            .map_or(String::new(), |id| format!("{id} "));
        let scenario_line = format!("  Scenario: {id_prefix}{}", self.scenario_name);
        let _ = writeln!(out, "{}", self.colors.bold(&scenario_line));

        // Protocol line: list unique protocols from all actors
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

        // Phase chain
        if !self.phase_names.is_empty() {
            let chain = self.phase_names.join(" \u{2192} ");
            let phases_line = format!("  Phases:   {chain}");
            let _ = writeln!(out, "{}", self.colors.dim(&phases_line));
        }
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
}
