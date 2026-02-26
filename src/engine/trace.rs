//! Trace capture for protocol events.
//!
//! `SharedTrace` provides a thread-safe, append-only trace buffer
//! that records every protocol event across all actors. Each entry
//! receives a monotonically increasing sequence number for total
//! ordering in the merged trace.
//!
//! See TJ-SPEC-013 §8.4 and TJ-SPEC-015 for merged trace requirements.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::types::Direction;

// ============================================================================
// TraceEntry
// ============================================================================

/// A single entry in the protocol execution trace.
///
/// Each entry records one protocol message (incoming or outgoing)
/// with a globally unique sequence number for total ordering.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Debug, Clone, Serialize)]
pub struct TraceEntry {
    /// Monotonically increasing sequence number (global across actors).
    pub seq: u64,
    /// UTC timestamp when the event was recorded.
    pub timestamp: DateTime<Utc>,
    /// Name of the actor that produced this event.
    pub actor: String,
    /// Name of the phase during which this event occurred.
    pub phase: String,
    /// Direction of the protocol message.
    pub direction: Direction,
    /// Wire method name (e.g., `"tools/call"`).
    pub method: String,
    /// Message content.
    pub content: serde_json::Value,
}

// ============================================================================
// SharedTrace
// ============================================================================

/// Thread-safe, append-only trace buffer for protocol events.
///
/// Cloning a `SharedTrace` produces a handle to the same underlying
/// buffer — all clones share the same entries and sequence counter.
///
/// Implements: TJ-SPEC-013 F-001
#[derive(Clone)]
pub struct SharedTrace {
    entries: Arc<Mutex<Vec<TraceEntry>>>,
    seq_counter: Arc<AtomicU64>,
}

impl SharedTrace {
    /// Creates a new empty trace.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            seq_counter: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Appends a new trace entry with the next sequence number.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    pub fn append(
        &self,
        actor: &str,
        phase: &str,
        direction: Direction,
        method: &str,
        content: &serde_json::Value,
    ) {
        let mut entries = self
            .entries
            .lock()
            .expect("trace mutex should not be poisoned");

        // Generate seq inside the lock so entries are always in seq order.
        let seq = self.seq_counter.fetch_add(1, Ordering::Relaxed);
        entries.push(TraceEntry {
            seq,
            timestamp: Utc::now(),
            actor: actor.to_string(),
            phase: phase.to_string(),
            direction,
            method: method.to_string(),
            content: content.clone(),
        });
    }

    /// Returns a snapshot of all trace entries.
    ///
    /// The returned `Vec` is independent of the trace — subsequent
    /// appends do not affect it.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn snapshot(&self) -> Vec<TraceEntry> {
        let entries = self
            .entries
            .lock()
            .expect("trace mutex should not be poisoned");
        entries.clone()
    }

    /// Returns the number of entries in the trace.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    #[must_use]
    pub fn len(&self) -> usize {
        let entries = self
            .entries
            .lock()
            .expect("trace mutex should not be poisoned");
        entries.len()
    }

    /// Returns `true` if the trace contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SharedTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for SharedTrace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedTrace")
            .field("entries_count", &self.len())
            .finish()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trace_is_empty() {
        let trace = SharedTrace::new();
        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
        assert!(trace.snapshot().is_empty());
    }

    #[test]
    fn append_increments_length() {
        let trace = SharedTrace::new();
        trace.append(
            "actor1",
            "phase1",
            Direction::Incoming,
            "tools/call",
            &serde_json::json!({}),
        );
        assert_eq!(trace.len(), 1);
        assert!(!trace.is_empty());
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let trace = SharedTrace::new();
        for i in 0..5 {
            trace.append(
                "actor1",
                "phase1",
                Direction::Incoming,
                &format!("method_{i}"),
                &serde_json::json!({}),
            );
        }

        let entries = trace.snapshot();
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.seq, i as u64);
        }
    }

    #[test]
    fn snapshot_is_independent() {
        let trace = SharedTrace::new();
        trace.append(
            "actor1",
            "phase1",
            Direction::Incoming,
            "tools/call",
            &serde_json::json!({}),
        );

        let snap = trace.snapshot();
        assert_eq!(snap.len(), 1);

        // Append more after snapshot
        trace.append(
            "actor1",
            "phase1",
            Direction::Outgoing,
            "tools/call",
            &serde_json::json!({}),
        );

        // Original snapshot unchanged
        assert_eq!(snap.len(), 1);
        // Trace has both
        assert_eq!(trace.len(), 2);
    }

    #[test]
    fn cloned_trace_shares_entries() {
        let trace = SharedTrace::new();
        let trace2 = trace.clone();

        trace.append(
            "actor1",
            "phase1",
            Direction::Incoming,
            "tools/call",
            &serde_json::json!({}),
        );

        assert_eq!(trace2.len(), 1);
    }

    #[test]
    fn concurrent_appends() {
        let trace = SharedTrace::new();
        let handles: Vec<_> = (0..10)
            .map(|i| {
                let trace = trace.clone();
                std::thread::spawn(move || {
                    for j in 0..10 {
                        trace.append(
                            &format!("actor_{i}"),
                            "phase1",
                            Direction::Incoming,
                            &format!("method_{j}"),
                            &serde_json::json!({}),
                        );
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(trace.len(), 100);

        // Verify all sequence numbers are unique
        let entries = trace.snapshot();
        let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        seqs.dedup();
        assert_eq!(seqs.len(), 100);
    }

    #[test]
    fn trace_entry_fields_captured() {
        let trace = SharedTrace::new();
        let content = serde_json::json!({"name": "calculator", "arguments": {}});
        trace.append(
            "mcp_poison",
            "trust_building",
            Direction::Incoming,
            "tools/call",
            &content,
        );

        let entries = trace.snapshot();
        let entry = &entries[0];
        assert_eq!(entry.actor, "mcp_poison");
        assert_eq!(entry.phase, "trust_building");
        assert_eq!(entry.direction, Direction::Incoming);
        assert_eq!(entry.method, "tools/call");
        assert_eq!(entry.content, content);
    }

    #[test]
    fn default_trace_is_empty() {
        let trace = SharedTrace::default();
        assert!(trace.is_empty());
    }
}
