//! Phase state representation (TJ-SPEC-003 F-001)
//!
//! Lock-free atomic state for tracking current phase, event counts,
//! and phase timing. Designed for concurrent access in HTTP transport.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use tokio::time::Instant;

use dashmap::DashMap;

use crate::config::schema::EntryAction;

/// Newtype wrapper for event names, used as `DashMap` keys.
///
/// Wraps event names like `"tools/call"` or `"tools/call:calculator"`
/// for type-safe event tracking.
///
/// Implements: TJ-SPEC-003 F-003
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct EventType(pub String);

impl EventType {
    /// Creates a new `EventType` from a string.
    ///
    /// Implements: TJ-SPEC-003 F-003
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Record of a phase transition for downstream processing.
///
/// Implements: TJ-SPEC-003 F-001
#[derive(Debug, Clone)]
pub struct PhaseTransition {
    /// Phase index we transitioned from
    pub from_phase: usize,
    /// Phase index we transitioned to
    pub to_phase: usize,
    /// Human-readable reason the trigger fired
    pub trigger_reason: String,
    /// Entry actions to execute for the new phase
    pub entry_actions: Vec<EntryAction>,
}

/// Lock-free atomic phase state.
///
/// Uses `AtomicUsize` for current phase index, `DashMap<EventType, AtomicU64>`
/// for event counters, and `Mutex<Instant>` for phase entry timestamp.
///
/// Event counters persist across phase transitions.
///
/// Implements: TJ-SPEC-003 F-001
pub struct PhaseState {
    /// Current phase index (0-based), advanced via CAS
    current_phase: AtomicUsize,
    /// Event counts per event type, using atomic increments
    event_counts: DashMap<EventType, AtomicU64>,
    /// Timestamp when current phase was entered
    phase_entered_at: Mutex<Instant>,
    /// Whether the current phase is terminal (no more transitions)
    is_terminal: AtomicBool,
    /// Total number of phases in the configuration
    num_phases: usize,
}

impl PhaseState {
    /// Creates a new `PhaseState` starting at phase 0.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn new(num_phases: usize) -> Self {
        Self {
            current_phase: AtomicUsize::new(0),
            event_counts: DashMap::new(),
            phase_entered_at: Mutex::new(Instant::now()),
            is_terminal: AtomicBool::new(num_phases == 0),
            num_phases,
        }
    }

    /// Returns the current phase index.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn current_phase(&self) -> usize {
        self.current_phase.load(Ordering::SeqCst)
    }

    /// Returns whether the engine is in a terminal state.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.is_terminal.load(Ordering::SeqCst)
    }

    /// Returns the total number of phases.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub const fn num_phases(&self) -> usize {
        self.num_phases
    }

    /// Atomically increments the event counter for the given event type.
    ///
    /// Returns the new count after incrementing.
    /// Uses saturating add to handle overflow gracefully.
    ///
    /// Implements: TJ-SPEC-003 F-003
    pub fn increment_event(&self, event_type: &EventType) -> u64 {
        let prev = self
            .event_counts
            .entry(event_type.clone())
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::SeqCst);
        prev.saturating_add(1)
    }

    /// Returns the current count for the given event type.
    ///
    /// Implements: TJ-SPEC-003 F-003
    #[must_use]
    pub fn event_count(&self, event_type: &EventType) -> u64 {
        self.event_counts
            .get(event_type)
            .map_or(0, |v| v.load(Ordering::SeqCst))
    }

    /// Returns the instant when the current phase was entered.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn phase_entered_at(&self) -> Instant {
        *self
            .phase_entered_at
            .lock()
            .expect("phase_entered_at lock poisoned")
    }

    /// Attempts to atomically advance from `from` to `to` phase.
    ///
    /// Uses compare-and-exchange to ensure exactly-once transition
    /// under concurrent access.
    ///
    /// Returns `true` if the transition succeeded.
    ///
    /// Implements: TJ-SPEC-003 F-012
    pub fn try_advance(&self, from: usize, to: usize) -> bool {
        self.current_phase
            .compare_exchange(from, to, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Marks the current phase as terminal (no further transitions).
    ///
    /// Implements: TJ-SPEC-003 F-001
    pub fn mark_terminal(&self) {
        self.is_terminal.store(true, Ordering::SeqCst);
    }

    /// Resets the phase entry timestamp to now.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    ///
    /// Implements: TJ-SPEC-003 F-001
    pub fn reset_phase_timer(&self) {
        let mut entered = self
            .phase_entered_at
            .lock()
            .expect("phase_entered_at lock poisoned");
        *entered = Instant::now();
    }
}

impl std::fmt::Debug for PhaseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhaseState")
            .field("current_phase", &self.current_phase())
            .field("is_terminal", &self.is_terminal())
            .field("num_phases", &self.num_phases)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_new_state() {
        let state = PhaseState::new(3);
        assert_eq!(state.current_phase(), 0);
        assert!(!state.is_terminal());
        assert_eq!(state.num_phases(), 3);
    }

    #[test]
    fn test_empty_phases_is_terminal() {
        let state = PhaseState::new(0);
        assert!(state.is_terminal());
    }

    #[test]
    fn test_increment_and_read_counters() {
        let state = PhaseState::new(3);
        let event = EventType::new("tools/call");

        assert_eq!(state.event_count(&event), 0);
        assert_eq!(state.increment_event(&event), 1);
        assert_eq!(state.event_count(&event), 1);
        assert_eq!(state.increment_event(&event), 2);
        assert_eq!(state.event_count(&event), 2);
    }

    #[test]
    fn test_independent_event_counters() {
        let state = PhaseState::new(3);
        let call = EventType::new("tools/call");
        let list = EventType::new("tools/list");

        state.increment_event(&call);
        state.increment_event(&call);
        state.increment_event(&list);

        assert_eq!(state.event_count(&call), 2);
        assert_eq!(state.event_count(&list), 1);
    }

    #[test]
    fn test_cas_advance_success() {
        let state = PhaseState::new(3);
        assert!(state.try_advance(0, 1));
        assert_eq!(state.current_phase(), 1);
    }

    #[test]
    fn test_cas_advance_failure() {
        let state = PhaseState::new(3);
        // Try to advance from wrong phase
        assert!(!state.try_advance(1, 2));
        assert_eq!(state.current_phase(), 0);
    }

    #[test]
    fn test_terminal_flag() {
        let state = PhaseState::new(3);
        assert!(!state.is_terminal());
        state.mark_terminal();
        assert!(state.is_terminal());
    }

    #[test]
    fn test_reset_phase_timer() {
        let state = PhaseState::new(3);
        let t1 = state.phase_entered_at();
        // Small sleep to ensure time passes
        std::thread::sleep(std::time::Duration::from_millis(10));
        state.reset_phase_timer();
        let t2 = state.phase_entered_at();
        assert!(t2 > t1);
    }

    #[test]
    fn test_concurrent_increments() {
        let state = Arc::new(PhaseState::new(3));
        let event = EventType::new("tools/call");
        let mut handles = vec![];

        for _ in 0..10 {
            let s = Arc::clone(&state);
            let e = event.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    s.increment_event(&e);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(state.event_count(&event), 1000);
    }

    #[test]
    fn test_concurrent_cas_only_one_wins() {
        let state = Arc::new(PhaseState::new(3));
        let mut handles = vec![];

        for _ in 0..10 {
            let s = Arc::clone(&state);
            handles.push(thread::spawn(move || s.try_advance(0, 1)));
        }

        let results: Vec<bool> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let successes = results.iter().filter(|&&r| r).count();
        assert_eq!(successes, 1);
        assert_eq!(state.current_phase(), 1);
    }

    #[test]
    fn test_event_type_display() {
        let event = EventType::new("tools/call");
        assert_eq!(event.to_string(), "tools/call");
    }

    #[test]
    fn test_debug_output() {
        let state = PhaseState::new(3);
        let debug = format!("{state:?}");
        assert!(debug.contains("PhaseState"));
        assert!(debug.contains("current_phase: 0"));
    }
}
