//! Phase state representation (TJ-SPEC-003 F-001)
//!
//! Lock-free atomic state for tracking current phase, event counts,
//! and phase timing. Designed for concurrent access in HTTP transport.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::time::Instant;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

use crate::config::schema::EntryAction;

/// Maximum number of distinct event types tracked in the `DashMap`.
///
/// Prevents unbounded memory growth from arbitrary event names
/// (e.g., a malicious client sending events with unique names).
const MAX_EVENT_TYPE_CARDINALITY: usize = 10_000;

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
    /// Approximate count of distinct event types in `event_counts`.
    /// Used for cardinality checking without calling `DashMap::len()`
    /// (which would require locking all shards and could deadlock
    /// if called inside an `entry()` lock).
    event_type_count: AtomicUsize,
    /// Timestamp when current phase was entered.
    // std::sync::Mutex is intentional: held briefly for Instant read/write, never
    // across .await points. Per tokio docs this is preferred over tokio::sync::Mutex
    // for short synchronous critical sections.
    phase_entered_at: Mutex<Instant>,
    /// Whether the current phase is terminal (no more transitions)
    is_terminal: AtomicBool,
    /// Total number of phases in the configuration
    num_phases: usize,
    /// Timestamp when the server was started (state was created)
    server_started_at: Instant,
}

impl PhaseState {
    /// Creates a new `PhaseState` starting at phase 0.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn new(num_phases: usize) -> Self {
        let now = Instant::now();
        Self {
            current_phase: AtomicUsize::new(0),
            event_counts: DashMap::new(),
            event_type_count: AtomicUsize::new(0),
            phase_entered_at: Mutex::new(now),
            is_terminal: AtomicBool::new(num_phases == 0),
            num_phases,
            server_started_at: now,
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
    /// Returns the new count after incrementing. If the event is already
    /// tracked, its counter is incremented. New event types are only tracked
    /// if the cardinality limit has not been reached; otherwise the event
    /// is silently dropped and `0` is returned.
    ///
    /// Uses saturating add to handle overflow gracefully.
    ///
    /// Implements: TJ-SPEC-003 F-003
    pub fn increment_event(&self, event_type: &EventType) -> u64 {
        // Fast path: if the event type already exists, increment atomically.
        if let Some(counter) = self.event_counts.get(event_type) {
            let prev = counter.fetch_add(1, Ordering::SeqCst);
            return prev.saturating_add(1);
        }

        // Cardinality guard: reject new event types once we exceed the limit
        // to prevent unbounded DashMap growth from arbitrary event names.
        // Uses a separate atomic counter instead of DashMap::len() because
        // len() locks all shards and would deadlock inside entry().
        // The overshoot under concurrency is bounded to ~num_cpus.
        if self.event_type_count.load(Ordering::SeqCst) >= MAX_EVENT_TYPE_CARDINALITY {
            tracing::warn!(
                event = %event_type,
                limit = MAX_EVENT_TYPE_CARDINALITY,
                "event type cardinality limit reached, dropping event"
            );
            return 0;
        }

        // Use entry() for atomic insert-or-increment to avoid losing
        // increments from concurrent threads inserting the same key.
        match self.event_counts.entry(event_type.clone()) {
            Entry::Occupied(entry) => {
                // Another thread inserted between our get() and entry() — just increment.
                let prev = entry.get().fetch_add(1, Ordering::SeqCst);
                prev.saturating_add(1)
            }
            Entry::Vacant(entry) => {
                self.event_type_count.fetch_add(1, Ordering::SeqCst);
                entry.insert(AtomicU64::new(1));
                1
            }
        }
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

    /// Returns the instant when the server was started (state was created).
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub const fn server_started_at(&self) -> Instant {
        self.server_started_at
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

/// Handle for phase state — either owned (per-connection) or shared (global).
///
/// In `PerConnection` scope, each connection gets its own `PhaseState`.
/// In `Global` scope, all connections share the same `Arc<PhaseState>`.
///
/// Implements: TJ-SPEC-003 F-001
#[derive(Debug)]
pub enum PhaseStateHandle {
    /// Per-connection owned state.
    Owned(PhaseState),
    /// Globally shared state.
    Shared(Arc<PhaseState>),
}

impl PhaseStateHandle {
    /// Returns the current phase index.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn current_phase(&self) -> usize {
        match self {
            Self::Owned(s) => s.current_phase(),
            Self::Shared(s) => s.current_phase(),
        }
    }

    /// Atomically increments the event counter for the given event type.
    ///
    /// Implements: TJ-SPEC-003 F-003
    pub fn increment_event(&self, event: &EventType) -> u64 {
        match self {
            Self::Owned(s) => s.increment_event(event),
            Self::Shared(s) => s.increment_event(event),
        }
    }

    /// Returns the current count for the given event type.
    ///
    /// Implements: TJ-SPEC-003 F-003
    #[must_use]
    pub fn event_count(&self, event: &EventType) -> u64 {
        match self {
            Self::Owned(s) => s.event_count(event),
            Self::Shared(s) => s.event_count(event),
        }
    }

    /// Returns whether the engine is in a terminal state.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        match self {
            Self::Owned(s) => s.is_terminal(),
            Self::Shared(s) => s.is_terminal(),
        }
    }

    /// Returns the instant when the current phase was entered.
    ///
    /// Implements: TJ-SPEC-003 F-001
    #[must_use]
    pub fn phase_entered_at(&self) -> Instant {
        match self {
            Self::Owned(s) => s.phase_entered_at(),
            Self::Shared(s) => s.phase_entered_at(),
        }
    }

    /// Attempts to atomically advance from `from` to `to` phase.
    ///
    /// Implements: TJ-SPEC-003 F-012
    pub fn try_advance(&self, from: usize, to: usize) -> bool {
        match self {
            Self::Owned(s) => s.try_advance(from, to),
            Self::Shared(s) => s.try_advance(from, to),
        }
    }

    /// Marks the current phase as terminal.
    ///
    /// Implements: TJ-SPEC-003 F-001
    pub fn mark_terminal(&self) {
        match self {
            Self::Owned(s) => s.mark_terminal(),
            Self::Shared(s) => s.mark_terminal(),
        }
    }

    /// Resets the phase entry timestamp to now.
    ///
    /// Implements: TJ-SPEC-003 F-001
    pub fn reset_phase_timer(&self) {
        match self {
            Self::Owned(s) => s.reset_phase_timer(),
            Self::Shared(s) => s.reset_phase_timer(),
        }
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
