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
        /// Human-readable stop reason.
        reason: String,
    },

    /// A new phase has been entered.
    PhaseEntered {
        /// When the transition occurred.
        timestamp: DateTime<Utc>,
        /// Name of the phase that was entered.
        phase_name: String,
        /// Zero-based index of the phase.
        phase_index: usize,
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
        let file = std::fs::File::create(path)?;
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
            reason: "done".to_owned(),
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
                reason: "shutdown".to_owned(),
            },
            Event::PhaseEntered {
                timestamp: now,
                phase_name: "exploit".to_owned(),
                phase_index: 2,
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
}
