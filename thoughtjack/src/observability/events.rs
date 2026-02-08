//! Structured event stream for `ThoughtJack` (TJ-SPEC-008 F-011 / F-012).
//!
//! Discrete, typed events emitted during server operation.  Events are
//! serialized as newline-delimited JSON (JSONL) and include a monotonically
//! increasing sequence number for ordering guarantees.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Why the server stopped.
///
/// Implements: TJ-SPEC-008 F-011
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Normal shutdown (completed, no more work).
    Completed,
    /// Interrupted by SIGINT.
    Interrupted,
    /// Terminated by SIGTERM.
    Terminated,
    /// Terminal phase reached.
    TerminalPhase,
    /// Unrecoverable error.
    Error,
}

/// Summary statistics emitted when the server stops.
///
/// Implements: TJ-SPEC-008 F-011
#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    /// Total requests processed.
    pub requests_handled: u64,
    /// Total responses sent.
    pub responses_sent: u64,
    /// Number of phase transitions.
    pub phase_transitions: u64,
    /// Number of attacks triggered.
    pub attacks_triggered: u64,
    /// Uptime in seconds.
    pub uptime_secs: f64,
}

impl std::fmt::Display for RunSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "requests={} responses={} transitions={} attacks={} uptime={:.1}s",
            self.requests_handled,
            self.responses_sent,
            self.phase_transitions,
            self.attacks_triggered,
            self.uptime_secs,
        )
    }
}

/// Information about the trigger that caused a phase transition.
///
/// Implements: TJ-SPEC-008 F-006
#[derive(Debug, Clone, Serialize)]
pub struct TriggerInfo {
    /// Trigger kind (e.g. `"event"`, `"timer"`, `"timeout"`).
    pub kind: String,
    /// The event or timer that matched, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Event count at the time of the trigger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Event variants
// ---------------------------------------------------------------------------

/// A discrete event emitted during `ThoughtJack` operation.
///
/// Each variant is tagged with `"type"` when serialized to JSON so consumers
/// can dispatch on the event kind.
///
/// Implements: TJ-SPEC-008 F-011
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum Event {
    /// The server has started and is ready to accept connections.
    ServerStarted {
        /// When the server started.
        timestamp: DateTime<Utc>,
        /// Configured server name.
        server_name: String,
        /// Transport type (e.g. `"stdio"`, `"http"`).
        transport: String,
    },

    /// The server has stopped.
    ServerStopped {
        /// When the server stopped.
        timestamp: DateTime<Utc>,
        /// Why the server stopped.
        reason: StopReason,
        /// Run summary statistics.
        #[serde(skip_serializing_if = "Option::is_none")]
        summary: Option<RunSummary>,
    },

    /// A new phase has been entered.
    PhaseEntered {
        /// When the transition occurred.
        timestamp: DateTime<Utc>,
        /// Name of the phase that was entered.
        phase_name: String,
        /// Zero-based index of the phase.
        phase_index: usize,
        /// What caused the transition.
        #[serde(skip_serializing_if = "Option::is_none")]
        trigger: Option<TriggerInfo>,
    },

    /// An attack behavior was triggered (delivery or side effect).
    AttackTriggered {
        /// When the attack was triggered.
        timestamp: DateTime<Utc>,
        /// Attack type (e.g. `"prompt_injection"`, `"rug_pull"`, `"slow_loris"`).
        attack_type: String,
        /// Additional context about the attack.
        details: String,
        /// Phase during which the attack was triggered.
        phase: String,
    },

    /// An MCP request was received.
    RequestReceived {
        /// When the request arrived.
        timestamp: DateTime<Utc>,
        /// JSON-RPC request id (may be number, string, or null).
        request_id: serde_json::Value,
        /// MCP method name.
        method: String,
    },

    /// An MCP response was sent.
    ResponseSent {
        /// When the response was sent.
        timestamp: DateTime<Utc>,
        /// Matching request id.
        request_id: serde_json::Value,
        /// Whether the response indicates success.
        success: bool,
        /// Processing time in milliseconds.
        duration_ms: u64,
    },

    /// A side-effect action was triggered.
    SideEffectTriggered {
        /// When the side effect fired.
        timestamp: DateTime<Utc>,
        /// Kind of side effect (e.g. `"notification_flood"`).
        effect_type: String,
        /// Phase during which the side effect was triggered.
        phase: String,
    },
}

// ---------------------------------------------------------------------------
// Envelope (adds sequence number via serde flatten)
// ---------------------------------------------------------------------------

/// Wraps an [`Event`] with a monotonically increasing sequence number.
#[derive(Debug, Serialize)]
struct EventEnvelope {
    /// Zero-based, monotonically increasing sequence counter.
    sequence: u64,
    /// The wrapped event (flattened into the same JSON object).
    #[serde(flatten)]
    event: Event,
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Thread-safe, buffered JSONL event writer.
///
/// Each call to [`emit`](Self::emit) atomically increments the sequence
/// counter, serializes the event as a single JSON line, and flushes the
/// underlying writer.  Serialization or I/O failures are silently dropped
/// because observability must never crash the server.
///
/// Implements: TJ-SPEC-008 F-012
pub struct EventEmitter {
    writer: Mutex<BufWriter<Box<dyn Write + Send>>>,
    sequence: AtomicU64,
}

// Box<dyn Write> is not Debug — provide a manual impl.
impl std::fmt::Debug for EventEmitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventEmitter")
            .field("sequence", &self.sequence.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl EventEmitter {
    /// Creates an emitter that writes to the given writer.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        Self {
            writer: Mutex::new(BufWriter::new(writer)),
            sequence: AtomicU64::new(0),
        }
    }

    /// Creates an emitter that writes to stdout.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn stdout() -> Self {
        Self::new(Box::new(std::io::stdout()))
    }

    /// Creates an emitter that writes to stderr.
    ///
    /// This is the default for server operation — stderr does not conflict
    /// with the stdio transport which uses stdout for MCP JSON-RPC messages.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn stderr() -> Self {
        Self::new(Box::new(std::io::stderr()))
    }

    /// Creates an emitter that silently discards all events.
    ///
    /// Useful for quiet mode or when events are not needed.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn noop() -> Self {
        Self::new(Box::new(std::io::sink()))
    }

    /// Creates an emitter that writes to a file at `path`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be created or opened.
    ///
    /// Implements: TJ-SPEC-008 F-012
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self::new(Box::new(file)))
    }

    /// Emits an event as a single JSONL line.
    ///
    /// Failures are silently dropped — observability must not crash the server.
    ///
    /// Implements: TJ-SPEC-008 F-012, NFR-004
    pub fn emit(&self, event: Event) {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let envelope = EventEnvelope {
            sequence: seq,
            event,
        };

        if let Ok(mut w) = self.writer.lock() {
            if let Ok(line) = serde_json::to_string(&envelope) {
                let _ = writeln!(w, "{line}");
                let _ = w.flush();
            }
        }
    }

    /// Returns the number of events emitted so far.
    ///
    /// Implements: TJ-SPEC-008 F-012
    #[must_use]
    pub fn event_count(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }

    /// Flushes the underlying writer.
    ///
    /// Call this before shutdown to ensure all buffered events reach disk.
    /// Flush failures are silently ignored (observability must not crash the server).
    ///
    /// Implements: TJ-SPEC-008 F-012
    pub fn flush(&self) {
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.flush();
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use super::*;

    /// In-memory writer for capturing emitter output in tests.
    #[derive(Clone)]
    struct TestWriter(Arc<StdMutex<Vec<u8>>>);

    impl TestWriter {
        fn new() -> Self {
            Self(Arc::new(StdMutex::new(Vec::new())))
        }

        fn contents(&self) -> String {
            let buf = self.0.lock().unwrap();
            String::from_utf8_lossy(&buf).into_owned()
        }
    }

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sample_event() -> Event {
        Event::ServerStarted {
            timestamp: DateTime::parse_from_rfc3339("2025-02-04T10:15:30Z")
                .unwrap()
                .with_timezone(&Utc),
            server_name: "test-server".to_owned(),
            transport: "stdio".to_owned(),
        }
    }

    #[test]
    fn event_serializes_with_type_tag() {
        let json = serde_json::to_string(&sample_event()).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["type"], "ServerStarted");
        assert_eq!(parsed["server_name"], "test-server");
    }

    #[test]
    fn emitter_writes_valid_jsonl() {
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        emitter.emit(sample_event());

        let output = tw.contents();
        let parsed: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(parsed["type"], "ServerStarted");
        assert_eq!(parsed["server_name"], "test-server");
        assert_eq!(parsed["transport"], "stdio");
        assert_eq!(parsed["sequence"], 0);
    }

    #[test]
    fn emitter_increments_sequence() {
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        emitter.emit(sample_event());
        emitter.emit(Event::ServerStopped {
            timestamp: Utc::now(),
            reason: StopReason::Completed,
            summary: None,
        });

        assert_eq!(emitter.event_count(), 2);

        let lines: Vec<serde_json::Value> = tw
            .contents()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["sequence"], 0);
        assert_eq!(lines[1]["sequence"], 1);
    }

    #[test]
    fn all_event_variants_serialize_to_valid_json() {
        let now = Utc::now();
        let variants: Vec<Event> = vec![
            Event::ServerStarted {
                timestamp: now,
                server_name: "s".to_owned(),
                transport: "stdio".to_owned(),
            },
            Event::ServerStopped {
                timestamp: now,
                reason: StopReason::Interrupted,
                summary: Some(RunSummary {
                    requests_handled: 10,
                    responses_sent: 10,
                    phase_transitions: 2,
                    attacks_triggered: 1,
                    uptime_secs: 5.5,
                }),
            },
            Event::PhaseEntered {
                timestamp: now,
                phase_name: "exploit".to_owned(),
                phase_index: 2,
                trigger: Some(TriggerInfo {
                    kind: "event".to_owned(),
                    event: Some("tools/call".to_owned()),
                    count: Some(5),
                }),
            },
            Event::AttackTriggered {
                timestamp: now,
                attack_type: "prompt_injection".to_owned(),
                details: "Injected instructions in search results".to_owned(),
                phase: "exploit".to_owned(),
            },
            Event::RequestReceived {
                timestamp: now,
                request_id: serde_json::json!(1),
                method: "tools/call".to_owned(),
            },
            Event::ResponseSent {
                timestamp: now,
                request_id: serde_json::json!("abc"),
                success: true,
                duration_ms: 42,
            },
            Event::SideEffectTriggered {
                timestamp: now,
                effect_type: "notification_flood".to_owned(),
                phase: "exploit".to_owned(),
            },
        ];

        for variant in &variants {
            let json = serde_json::to_string(variant).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(parsed.get("type").is_some(), "missing type tag: {json}");
        }
    }

    #[test]
    fn envelope_flattens_event_fields() {
        let envelope = EventEnvelope {
            sequence: 7,
            event: sample_event(),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Flat structure — sequence, type, and event fields at the same level
        assert_eq!(parsed["sequence"], 7);
        assert_eq!(parsed["type"], "ServerStarted");
        assert_eq!(parsed["server_name"], "test-server");
        assert!(
            parsed.get("event").is_none(),
            "event field should be flattened"
        );
    }

    #[test]
    fn from_file_creates_valid_jsonl_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let emitter = EventEmitter::from_file(&path).unwrap();
        emitter.emit(sample_event());
        emitter.emit(Event::ServerStopped {
            timestamp: Utc::now(),
            reason: StopReason::Completed,
            summary: None,
        });

        assert_eq!(emitter.event_count(), 2);

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = content
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "ServerStarted");
        assert_eq!(lines[1]["type"], "ServerStopped");
    }

    #[test]
    fn stderr_emitter_does_not_panic() {
        let emitter = EventEmitter::stderr();
        emitter.emit(sample_event());
        assert_eq!(emitter.event_count(), 1);
    }

    #[test]
    fn test_run_summary_display() {
        let summary = RunSummary {
            requests_handled: 42,
            responses_sent: 40,
            phase_transitions: 3,
            attacks_triggered: 2,
            uptime_secs: 12.5,
        };
        let display = format!("{summary}");
        assert_eq!(
            display,
            "requests=42 responses=40 transitions=3 attacks=2 uptime=12.5s"
        );
    }

    #[test]
    fn test_run_summary_display_zero_values() {
        let summary = RunSummary {
            requests_handled: 0,
            responses_sent: 0,
            phase_transitions: 0,
            attacks_triggered: 0,
            uptime_secs: 0.0,
        };
        let display = format!("{summary}");
        assert_eq!(
            display,
            "requests=0 responses=0 transitions=0 attacks=0 uptime=0.0s"
        );
    }

    #[test]
    fn test_run_summary_serialize() {
        let summary = RunSummary {
            requests_handled: 10,
            responses_sent: 8,
            phase_transitions: 2,
            attacks_triggered: 1,
            uptime_secs: 5.5,
        };
        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["requests_handled"], 10);
        assert_eq!(json["responses_sent"], 8);
        assert_eq!(json["phase_transitions"], 2);
        assert_eq!(json["attacks_triggered"], 1);
        assert_eq!(json["uptime_secs"], 5.5);
    }

    #[test]
    fn test_event_serialize_server_started() {
        let event = sample_event();
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ServerStarted");
        assert_eq!(json["server_name"], "test-server");
        assert_eq!(json["transport"], "stdio");
    }

    #[test]
    fn test_event_serialize_attack_triggered() {
        let event = Event::AttackTriggered {
            timestamp: DateTime::parse_from_rfc3339("2025-02-04T10:15:30Z")
                .unwrap()
                .with_timezone(&Utc),
            attack_type: "rug_pull".to_owned(),
            details: "Tool description changed".to_owned(),
            phase: "exploit".to_owned(),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "AttackTriggered");
        assert_eq!(json["attack_type"], "rug_pull");
        assert_eq!(json["details"], "Tool description changed");
        assert_eq!(json["phase"], "exploit");
    }

    #[test]
    fn test_stop_reason_debug() {
        // All StopReason variants have Debug
        let variants = [
            StopReason::Completed,
            StopReason::Interrupted,
            StopReason::Terminated,
            StopReason::TerminalPhase,
            StopReason::Error,
        ];
        for variant in &variants {
            let debug_str = format!("{variant:?}");
            assert!(!debug_str.is_empty(), "Debug output should not be empty");
        }
    }

    #[test]
    fn test_trigger_info_serialize() {
        let trigger = TriggerInfo {
            kind: "event".to_owned(),
            event: Some("tools/call".to_owned()),
            count: Some(5),
        };
        let json = serde_json::to_value(&trigger).unwrap();
        assert_eq!(json["kind"], "event");
        assert_eq!(json["event"], "tools/call");
        assert_eq!(json["count"], 5);

        // Also verify skip_serializing_if works for None fields
        let trigger_minimal = TriggerInfo {
            kind: "timer".to_owned(),
            event: None,
            count: None,
        };
        let json_minimal = serde_json::to_value(&trigger_minimal).unwrap();
        assert_eq!(json_minimal["kind"], "timer");
        assert!(
            json_minimal.get("event").is_none(),
            "None event should be skipped"
        );
        assert!(
            json_minimal.get("count").is_none(),
            "None count should be skipped"
        );
    }

    #[test]
    fn test_empty_server_lifecycle_events() {
        // EC-OBS-011: emit ServerStarted and ServerStopped, verify 2 JSONL entries.
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));

        emitter.emit(Event::ServerStarted {
            timestamp: Utc::now(),
            server_name: "lifecycle-test".to_owned(),
            transport: "stdio".to_owned(),
        });
        emitter.emit(Event::ServerStopped {
            timestamp: Utc::now(),
            reason: StopReason::Completed,
            summary: None,
        });

        let contents = tw.contents();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "expected exactly 2 JSONL entries");

        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["type"], "ServerStarted");

        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["type"], "ServerStopped");
    }

    #[test]
    fn test_timestamp_is_utc() {
        // EC-OBS-018: verify that emitted event timestamps are in UTC.
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));

        emitter.emit(Event::ServerStarted {
            timestamp: Utc::now(),
            server_name: "utc-test".to_owned(),
            transport: "stdio".to_owned(),
        });

        let contents = tw.contents();
        let parsed: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        let ts = parsed["timestamp"]
            .as_str()
            .expect("timestamp field should be a string");
        assert!(
            ts.ends_with('Z') || ts.contains("+00:00"),
            "timestamp should be in UTC (ends with Z or +00:00), got: {ts}"
        );
    }

    #[test]
    fn test_metrics_with_no_requests() {
        // EC-OBS-019: recording metrics with zero/no-op values should not panic.
        use crate::observability::metrics::record_request;
        record_request("tools/call");
    }
}
