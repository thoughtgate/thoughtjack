//! Response sequence tracking (TJ-SPEC-009 F-005).
//!
//! Tracks call counts per item and resolves sequence indices based on
//! exhaustion behavior.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::config::schema::{ExhaustedBehavior, StateScope};

/// Tracks call counts for tools, resources, and prompts.
///
/// Thread-safe via `DashMap` + `AtomicU64`. Key format is driven by
/// `StateScope` — per-connection keys include the connection ID while
/// global keys use a fixed prefix.
///
/// Implements: TJ-SPEC-009 F-005
pub struct CallTracker {
    counters: DashMap<String, AtomicU64>,
}

impl CallTracker {
    /// Creates a new call tracker.
    ///
    /// Implements: TJ-SPEC-009 F-005
    #[must_use]
    pub fn new() -> Self {
        Self {
            counters: DashMap::new(),
        }
    }

    /// Increments the call count for the given key and returns the new count (1-indexed).
    ///
    /// Counter saturates at `u64::MAX` (EC-DYN-013).
    ///
    /// Implements: TJ-SPEC-009 F-005
    #[must_use]
    pub fn increment(&self, key: &str) -> u64 {
        let entry = self
            .counters
            .entry(key.to_string())
            .or_insert_with(|| AtomicU64::new(0));
        // Saturating increment — load, add, store
        let prev = entry.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_add(1))
        });
        // Drop the DashMap ref before returning
        drop(entry);
        // fetch_update always succeeds with Some, so unwrap is safe
        prev.unwrap_or(0).saturating_add(1)
    }

    /// Returns the current call count for the given key (0 if not called).
    ///
    /// Implements: TJ-SPEC-009 F-005
    #[must_use]
    pub fn get(&self, key: &str) -> u64 {
        self.counters
            .get(key)
            .map_or(0, |c| c.load(Ordering::Relaxed))
    }

    /// Builds a tracker key from the given parameters.
    ///
    /// Per-connection scope: `"{connection_id}:{item_type}:{item_name}"`
    /// Global scope: `"global:{item_type}:{item_name}"`
    ///
    /// Implements: TJ-SPEC-009 F-005
    #[must_use]
    pub fn make_key(
        connection_id: u64,
        scope: StateScope,
        item_type: &str,
        item_name: &str,
    ) -> String {
        match scope {
            StateScope::PerConnection => {
                format!("{connection_id}:{item_type}:{item_name}")
            }
            StateScope::Global => {
                format!("global:{item_type}:{item_name}")
            }
        }
    }
}

impl Default for CallTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Error when a sequence is exhausted with `ExhaustedBehavior::Error`.
///
/// Implements: TJ-SPEC-009 F-005
#[derive(Debug)]
pub struct SequenceExhausted;

/// Resolves the sequence index for a given call count.
///
/// `call_count` is 1-indexed (first call = 1).
/// Returns the 0-based index into the sequence array.
///
/// # Errors
///
/// Returns `SequenceExhausted` when `on_exhausted` is `Error` and the
/// sequence is exhausted.
///
/// Implements: TJ-SPEC-009 F-005
pub const fn resolve_sequence_index(
    len: usize,
    call_count: u64,
    on_exhausted: ExhaustedBehavior,
) -> Result<usize, SequenceExhausted> {
    if len == 0 {
        return Err(SequenceExhausted);
    }

    // Truncation is acceptable: sequence lengths are bounded by config size
    #[allow(clippy::cast_possible_truncation)]
    let index = call_count.saturating_sub(1) as usize;

    if index < len {
        Ok(index)
    } else {
        match on_exhausted {
            ExhaustedBehavior::Cycle => Ok(index % len),
            ExhaustedBehavior::Last => Ok(len - 1),
            ExhaustedBehavior::Error => Err(SequenceExhausted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_increment_returns_one_indexed() {
        let tracker = CallTracker::new();
        assert_eq!(tracker.increment("key1"), 1);
        assert_eq!(tracker.increment("key1"), 2);
        assert_eq!(tracker.increment("key1"), 3);
    }

    #[test]
    fn test_get_returns_current() {
        let tracker = CallTracker::new();
        assert_eq!(tracker.get("key1"), 0);
        let _ = tracker.increment("key1");
        assert_eq!(tracker.get("key1"), 1);
    }

    #[test]
    fn test_independent_keys() {
        let tracker = CallTracker::new();
        let _ = tracker.increment("key1");
        let _ = tracker.increment("key1");
        let _ = tracker.increment("key2");
        assert_eq!(tracker.get("key1"), 2);
        assert_eq!(tracker.get("key2"), 1);
    }

    #[test]
    fn test_make_key_per_connection() {
        let key = CallTracker::make_key(42, StateScope::PerConnection, "tool", "calc");
        assert_eq!(key, "42:tool:calc");
    }

    #[test]
    fn test_make_key_global() {
        let key = CallTracker::make_key(42, StateScope::Global, "tool", "calc");
        assert_eq!(key, "global:tool:calc");
    }

    #[test]
    fn test_sequence_within_range() {
        assert_eq!(
            resolve_sequence_index(3, 1, ExhaustedBehavior::Last).unwrap(),
            0
        );
        assert_eq!(
            resolve_sequence_index(3, 2, ExhaustedBehavior::Last).unwrap(),
            1
        );
        assert_eq!(
            resolve_sequence_index(3, 3, ExhaustedBehavior::Last).unwrap(),
            2
        );
    }

    #[test]
    fn test_sequence_cycle() {
        assert_eq!(
            resolve_sequence_index(3, 4, ExhaustedBehavior::Cycle).unwrap(),
            0
        );
        assert_eq!(
            resolve_sequence_index(3, 5, ExhaustedBehavior::Cycle).unwrap(),
            1
        );
        assert_eq!(
            resolve_sequence_index(3, 6, ExhaustedBehavior::Cycle).unwrap(),
            2
        );
        assert_eq!(
            resolve_sequence_index(3, 7, ExhaustedBehavior::Cycle).unwrap(),
            0
        );
    }

    #[test]
    fn test_sequence_last() {
        assert_eq!(
            resolve_sequence_index(3, 4, ExhaustedBehavior::Last).unwrap(),
            2
        );
        assert_eq!(
            resolve_sequence_index(3, 100, ExhaustedBehavior::Last).unwrap(),
            2
        );
    }

    #[test]
    fn test_sequence_error() {
        assert!(resolve_sequence_index(3, 4, ExhaustedBehavior::Error).is_err());
    }

    // EC-DYN-012: empty sequence
    #[test]
    fn test_empty_sequence() {
        assert!(resolve_sequence_index(0, 1, ExhaustedBehavior::Last).is_err());
    }

    // EC-DYN-013: counter overflow saturation
    #[test]
    fn test_counter_saturation() {
        let tracker = CallTracker::new();
        // Manually set to near max
        tracker
            .counters
            .insert("key".to_string(), AtomicU64::new(u64::MAX - 1));
        assert_eq!(tracker.increment("key"), u64::MAX);
        // Should saturate
        assert_eq!(tracker.increment("key"), u64::MAX);
    }

    // EC-DYN-015: concurrent requests to same tool
    #[test]
    fn test_concurrent_increment() {
        use std::sync::Arc;
        use std::thread;

        let tracker = Arc::new(CallTracker::new());
        let threads: Vec<_> = (0..10)
            .map(|_| {
                let tracker = Arc::clone(&tracker);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = tracker.increment("concurrent_key");
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        // 10 threads × 100 increments = 1000
        assert_eq!(tracker.get("concurrent_key"), 1000);
    }

    // Sequence with call_count = 0 (first call is 1, so 0 means "before first call")
    #[test]
    fn test_sequence_call_count_zero() {
        // call_count 0 underflows to index wrapping via saturating_sub(1)
        let result = resolve_sequence_index(3, 0, ExhaustedBehavior::Last).unwrap();
        // saturating_sub(1) on 0 gives 0, wrapping to 0 with u64::MAX
        // Actually 0u64.saturating_sub(1) = 0, so index = 0
        assert_eq!(result, 0);
    }
}
