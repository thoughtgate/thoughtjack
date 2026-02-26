//! Structured event stream for `ThoughtJack` (TJ-SPEC-008 F-011 / F-012).
//!
//! Discrete, typed events emitted during scenario execution. Events are
//! serialized as newline-delimited JSON (JSONL) and include a monotonically
//! increasing sequence number for ordering guarantees.

use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use serde::Serialize;

// ---------------------------------------------------------------------------
// Event variants (TJ-SPEC-008 Appendix A)
// ---------------------------------------------------------------------------

/// A discrete event emitted during `ThoughtJack` operation.
///
/// Each variant is tagged with `"type"` when serialized to JSON so consumers
/// can dispatch on the event kind.
///
/// Implements: TJ-SPEC-008 F-011
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ThoughtJackEvent {
    // --- Engine (TJ-SPEC-013) ---
    /// A new phase has been entered.
    PhaseEntered {
        /// Actor name.
        actor: String,
        /// Name of the phase that was entered.
        phase_name: String,
        /// Zero-based index of the phase.
        phase_index: usize,
    },

    /// A phase transition occurred.
    PhaseAdvanced {
        /// Actor name.
        actor: String,
        /// Source phase name.
        from: String,
        /// Destination phase name.
        to: String,
        /// Trigger description.
        trigger: String,
    },

    /// Terminal phase reached for an actor.
    PhaseTerminal {
        /// Actor name.
        actor: String,
        /// Name of the terminal phase.
        phase_name: String,
    },

    /// An extractor captured a value from a protocol event.
    ExtractorCaptured {
        /// Actor name.
        actor: String,
        /// Extractor name.
        name: String,
        /// Preview of the captured value (truncated).
        value_preview: String,
    },

    /// A synthesize (LLM generation) call completed.
    SynthesizeGenerated {
        /// Actor name.
        actor: String,
        /// Protocol for which the response was generated.
        protocol: String,
    },

    /// Synthesize validation was bypassed (`--raw-synthesize`).
    SynthesizeValidationBypassed {
        /// Actor name.
        actor: String,
    },

    /// An entry action was executed on phase entry.
    EntryActionExecuted {
        /// Actor name.
        actor: String,
        /// Type of entry action.
        action_type: String,
    },

    // --- Orchestration (TJ-SPEC-015) ---
    /// The orchestrator started.
    OrchestratorStarted {
        /// Total number of actors.
        actor_count: usize,
        /// Number of server-mode actors.
        server_count: usize,
        /// Number of client-mode actors.
        client_count: usize,
    },

    /// An actor was initialized.
    ActorInit {
        /// Actor name.
        actor_name: String,
        /// Actor mode (e.g., `mcp_server`, `a2a_client`).
        mode: String,
    },

    /// A server-mode actor is ready to accept connections.
    ActorReady {
        /// Actor name.
        actor_name: String,
        /// Bind address (for server-mode actors).
        bind_address: String,
    },

    /// All server actors are ready; client actors may start.
    ReadinessGateOpen {
        /// Number of server actors that became ready.
        server_count: usize,
        /// Time elapsed waiting for readiness in milliseconds.
        elapsed_ms: u64,
    },

    /// Readiness gate timed out; some servers not ready.
    ReadinessGateTimeout {
        /// Actors that did not become ready.
        not_ready: Vec<String>,
    },

    /// An actor started executing its phase loop.
    ActorStarted {
        /// Actor name.
        actor_name: String,
        /// Number of phases in this actor.
        phase_count: usize,
    },

    /// An actor completed execution.
    ActorCompleted {
        /// Actor name.
        actor_name: String,
        /// Completion reason.
        reason: String,
        /// Number of phases completed.
        phases_completed: usize,
    },

    /// An actor encountered an error.
    ActorError {
        /// Actor name.
        actor_name: String,
        /// Error description.
        error: String,
    },

    /// An actor is waiting for cross-actor extractors.
    AwaitExtractorsWaiting {
        /// Actor name.
        actor: String,
        /// Current phase index.
        phase_index: usize,
        /// Extractor names being awaited.
        awaiting: Vec<String>,
    },

    /// Cross-actor extractors resolved.
    AwaitExtractorsResolved {
        /// Actor name.
        actor: String,
        /// Phase index where resolution occurred.
        phase_index: usize,
    },

    /// Timed out waiting for cross-actor extractors.
    AwaitExtractorsTimeout {
        /// Actor name.
        actor: String,
        /// Phase index.
        phase_index: usize,
        /// Extractor names still missing.
        missing: Vec<String>,
    },

    /// The orchestrator is shutting down.
    OrchestratorShutdown {
        /// Shutdown reason.
        reason: String,
    },

    /// The orchestrator completed all actors.
    OrchestratorCompleted {
        /// Summary description.
        summary: String,
    },

    // --- Verdict (TJ-SPEC-014) ---
    /// Grace period started after final phase.
    GracePeriodStarted {
        /// Duration in seconds.
        duration_seconds: u64,
    },

    /// Grace period expired normally.
    GracePeriodExpired {
        /// Messages captured during grace period.
        messages_captured: usize,
    },

    /// Grace period terminated early.
    GracePeriodEarlyTermination {
        /// Reason for early termination.
        reason: String,
    },

    /// An indicator was evaluated.
    IndicatorEvaluated {
        /// Indicator ID.
        indicator_id: String,
        /// Evaluation method (cel, pattern, semantic).
        method: String,
        /// Evaluation result.
        result: String,
        /// Evaluation duration in milliseconds.
        duration_ms: u64,
    },

    /// An indicator was skipped.
    IndicatorSkipped {
        /// Indicator ID.
        indicator_id: String,
        /// Reason for skipping.
        reason: String,
    },

    /// An LLM call was made for semantic evaluation.
    SemanticLlmCall {
        /// Model name.
        model: String,
        /// Indicator ID being evaluated.
        indicator_id: String,
        /// LLM call latency in milliseconds.
        latency_ms: u64,
    },

    /// Verdict was computed.
    VerdictComputed {
        /// Verdict result (exploited, `not_exploited`, partial, error).
        result: String,
        /// Number of indicators that matched.
        matched: usize,
        /// Total number of indicators evaluated.
        total: usize,
    },

    // --- Protocol (TJ-SPEC-013, 016, 017, 018) ---
    /// A protocol message was received from the agent.
    ProtocolMessageReceived {
        /// Actor name.
        actor: String,
        /// Method or event name.
        method: String,
        /// Protocol identifier (mcp, a2a, `ag_ui`).
        protocol: String,
    },

    /// A protocol message was sent to the agent.
    ProtocolMessageSent {
        /// Actor name.
        actor: String,
        /// Method or event name.
        method: String,
        /// Protocol identifier.
        protocol: String,
        /// Send duration in milliseconds.
        duration_ms: u64,
    },

    /// A protocol notification (non-request message).
    ProtocolNotification {
        /// Actor name.
        actor: String,
        /// Method name.
        method: String,
        /// Direction (incoming/outgoing).
        direction: String,
    },

    /// A transport-level error occurred.
    ProtocolTransportError {
        /// Actor name.
        actor: String,
        /// Error description.
        error: String,
    },

    /// A server-mode driver handled an interleaved server request.
    ProtocolInterleave {
        /// Actor name.
        actor: String,
        /// Server request method that was interleaved.
        server_method: String,
    },

    // --- Legacy (v0.2 compatibility) ---
    /// The server has started (v0.2 mode).
    ServerStarted {
        /// Configured server name.
        server_name: String,
        /// Transport type (e.g., "stdio", "http").
        transport: String,
    },

    /// The server has stopped (v0.2 mode).
    ServerStopped {
        /// Why the server stopped.
        reason: String,
        /// Uptime in seconds.
        uptime_seconds: u64,
    },

    /// A transport connection was established.
    TransportConnected {
        /// Connection identifier.
        connection_id: String,
    },

    /// A transport connection was disconnected.
    TransportDisconnected {
        /// Connection identifier.
        connection_id: String,
        /// Disconnection reason.
        reason: String,
    },

    // --- General ---
    /// A general error event.
    Error {
        /// Error type/category.
        error_type: String,
        /// Error message.
        message: String,
        /// Error context.
        context: String,
    },
}

// ---------------------------------------------------------------------------
// Envelope (adds sequence number + timestamp via serde flatten)
// ---------------------------------------------------------------------------

/// Wraps a [`ThoughtJackEvent`] with a monotonically increasing sequence number
/// and a UTC timestamp.
#[derive(Debug, Serialize)]
struct EventEnvelope {
    /// Zero-based, monotonically increasing sequence counter.
    sequence: u64,
    /// When the event was emitted.
    timestamp: DateTime<Utc>,
    /// The wrapped event (flattened into the same JSON object).
    #[serde(flatten)]
    event: ThoughtJackEvent,
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Thread-safe, buffered JSONL event writer.
///
/// Each call to [`emit`](Self::emit) atomically increments the sequence
/// counter, serializes the event as a single JSON line, and flushes the
/// underlying writer. Serialization or I/O failures are silently dropped
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
    pub fn emit(&self, event: ThoughtJackEvent) {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
        let envelope = EventEnvelope {
            sequence: seq,
            timestamp: Utc::now(),
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

    fn sample_event() -> ThoughtJackEvent {
        ThoughtJackEvent::ServerStarted {
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
        emitter.emit(ThoughtJackEvent::ServerStopped {
            reason: "completed".to_owned(),
            uptime_seconds: 42,
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
    fn all_event_categories_serialize_to_valid_json() {
        let variants: Vec<ThoughtJackEvent> = vec![
            // Engine
            ThoughtJackEvent::PhaseEntered {
                actor: "a".to_owned(),
                phase_name: "p".to_owned(),
                phase_index: 0,
            },
            ThoughtJackEvent::PhaseAdvanced {
                actor: "a".to_owned(),
                from: "p1".to_owned(),
                to: "p2".to_owned(),
                trigger: "t".to_owned(),
            },
            ThoughtJackEvent::PhaseTerminal {
                actor: "a".to_owned(),
                phase_name: "p".to_owned(),
            },
            ThoughtJackEvent::ExtractorCaptured {
                actor: "a".to_owned(),
                name: "x".to_owned(),
                value_preview: "v".to_owned(),
            },
            ThoughtJackEvent::SynthesizeGenerated {
                actor: "a".to_owned(),
                protocol: "mcp".to_owned(),
            },
            ThoughtJackEvent::SynthesizeValidationBypassed {
                actor: "a".to_owned(),
            },
            ThoughtJackEvent::EntryActionExecuted {
                actor: "a".to_owned(),
                action_type: "notification".to_owned(),
            },
            // Orchestration
            ThoughtJackEvent::OrchestratorStarted {
                actor_count: 2,
                server_count: 1,
                client_count: 1,
            },
            ThoughtJackEvent::ActorInit {
                actor_name: "a".to_owned(),
                mode: "mcp_server".to_owned(),
            },
            ThoughtJackEvent::ActorReady {
                actor_name: "a".to_owned(),
                bind_address: ":3000".to_owned(),
            },
            ThoughtJackEvent::ReadinessGateOpen {
                server_count: 1,
                elapsed_ms: 100,
            },
            ThoughtJackEvent::ReadinessGateTimeout {
                not_ready: vec!["a".to_owned()],
            },
            ThoughtJackEvent::ActorStarted {
                actor_name: "a".to_owned(),
                phase_count: 3,
            },
            ThoughtJackEvent::ActorCompleted {
                actor_name: "a".to_owned(),
                reason: "terminal".to_owned(),
                phases_completed: 3,
            },
            ThoughtJackEvent::ActorError {
                actor_name: "a".to_owned(),
                error: "boom".to_owned(),
            },
            ThoughtJackEvent::AwaitExtractorsWaiting {
                actor: "a".to_owned(),
                phase_index: 1,
                awaiting: vec!["x".to_owned()],
            },
            ThoughtJackEvent::AwaitExtractorsResolved {
                actor: "a".to_owned(),
                phase_index: 1,
            },
            ThoughtJackEvent::AwaitExtractorsTimeout {
                actor: "a".to_owned(),
                phase_index: 1,
                missing: vec!["x".to_owned()],
            },
            ThoughtJackEvent::OrchestratorShutdown {
                reason: "cancel".to_owned(),
            },
            ThoughtJackEvent::OrchestratorCompleted {
                summary: "done".to_owned(),
            },
            // Verdict
            ThoughtJackEvent::GracePeriodStarted {
                duration_seconds: 30,
            },
            ThoughtJackEvent::GracePeriodExpired {
                messages_captured: 5,
            },
            ThoughtJackEvent::GracePeriodEarlyTermination {
                reason: "eof".to_owned(),
            },
            ThoughtJackEvent::IndicatorEvaluated {
                indicator_id: "i1".to_owned(),
                method: "cel".to_owned(),
                result: "matched".to_owned(),
                duration_ms: 10,
            },
            ThoughtJackEvent::IndicatorSkipped {
                indicator_id: "i2".to_owned(),
                reason: "no trace".to_owned(),
            },
            ThoughtJackEvent::SemanticLlmCall {
                model: "gpt-4".to_owned(),
                indicator_id: "i3".to_owned(),
                latency_ms: 500,
            },
            ThoughtJackEvent::VerdictComputed {
                result: "exploited".to_owned(),
                matched: 2,
                total: 3,
            },
            // Protocol
            ThoughtJackEvent::ProtocolMessageReceived {
                actor: "a".to_owned(),
                method: "tools/call".to_owned(),
                protocol: "mcp".to_owned(),
            },
            ThoughtJackEvent::ProtocolMessageSent {
                actor: "a".to_owned(),
                method: "tools/call".to_owned(),
                protocol: "mcp".to_owned(),
                duration_ms: 5,
            },
            ThoughtJackEvent::ProtocolNotification {
                actor: "a".to_owned(),
                method: "notify".to_owned(),
                direction: "outgoing".to_owned(),
            },
            ThoughtJackEvent::ProtocolTransportError {
                actor: "a".to_owned(),
                error: "timeout".to_owned(),
            },
            ThoughtJackEvent::ProtocolInterleave {
                actor: "a".to_owned(),
                server_method: "sampling/createMessage".to_owned(),
            },
            // Legacy
            ThoughtJackEvent::ServerStarted {
                server_name: "s".to_owned(),
                transport: "stdio".to_owned(),
            },
            ThoughtJackEvent::ServerStopped {
                reason: "completed".to_owned(),
                uptime_seconds: 60,
            },
            ThoughtJackEvent::TransportConnected {
                connection_id: "1".to_owned(),
            },
            ThoughtJackEvent::TransportDisconnected {
                connection_id: "1".to_owned(),
                reason: "eof".to_owned(),
            },
            // General
            ThoughtJackEvent::Error {
                error_type: "io".to_owned(),
                message: "disk full".to_owned(),
                context: "writing trace".to_owned(),
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
            timestamp: DateTime::parse_from_rfc3339("2025-02-04T10:15:30Z")
                .unwrap()
                .with_timezone(&Utc),
            event: sample_event(),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Flat structure — sequence, timestamp, type, and event fields at the same level
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
        emitter.emit(ThoughtJackEvent::ServerStopped {
            reason: "completed".to_owned(),
            uptime_seconds: 10,
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
    fn test_timestamp_is_utc() {
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));
        emitter.emit(sample_event());

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
    fn test_empty_server_lifecycle_events() {
        let tw = TestWriter::new();
        let emitter = EventEmitter::new(Box::new(tw.clone()));

        emitter.emit(ThoughtJackEvent::ServerStarted {
            server_name: "lifecycle-test".to_owned(),
            transport: "stdio".to_owned(),
        });
        emitter.emit(ThoughtJackEvent::ServerStopped {
            reason: "completed".to_owned(),
            uptime_seconds: 0,
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
    fn test_metrics_with_no_requests() {
        // EC-OBS-019: recording metrics with zero/no-op values should not panic.
        use crate::observability::metrics::record_request;
        record_request("tools/call");
    }
}
