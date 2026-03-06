//! Thread-safe cross-actor extractor storage.
//!
//! `ExtractorStore` provides shared extractor state between actors,
//! enabling cross-actor interpolation and `await_extractors` synchronization.
//!
//! See TJ-SPEC-015 §4 for the extractor store specification.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::watch;

/// Thread-safe cross-actor extractor storage.
///
/// Stores extractor values keyed by `(actor_name, extractor_name)`.
/// Used by the `PhaseLoop` to publish captured values and by other
/// actors to read them for cross-actor interpolation.
///
/// A monotonically increasing version counter is broadcast via a
/// `watch` channel so that waiters (e.g. `await_extractors`) are
/// notified immediately when any value is set, avoiding polling.
///
/// Implements: TJ-SPEC-015 F-001
#[derive(Clone)]
pub struct ExtractorStore {
    store: Arc<DashMap<(String, String), String>>,
    version_tx: Arc<watch::Sender<u64>>,
}

impl Default for ExtractorStore {
    fn default() -> Self {
        let (version_tx, _) = watch::channel(0u64);
        Self {
            store: Arc::new(DashMap::new()),
            version_tx: Arc::new(version_tx),
        }
    }
}

impl ExtractorStore {
    /// Creates a new empty extractor store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an extractor value for a given actor.
    ///
    /// The insert and version bump are performed inside a single
    /// `send_modify` closure so that subscribers always see the new
    /// value when they wake.
    pub fn set(&self, actor: &str, name: &str, value: String) {
        let store = Arc::clone(&self.store);
        let key = (actor.to_string(), name.to_string());
        self.version_tx.send_modify(move |v| {
            store.insert(key, value);
            *v += 1;
        });
    }

    /// Returns a receiver that is notified whenever a value is set.
    ///
    /// Each call to [`set`] increments an internal version counter.
    /// Callers can `changed().await` on the returned receiver to
    /// wake immediately when new extractor data is available.
    ///
    /// Implements: TJ-SPEC-015 F-001
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version_tx.subscribe()
    }

    /// Gets an extractor value for a given actor.
    #[must_use]
    pub fn get(&self, actor: &str, name: &str) -> Option<String> {
        self.store
            .get(&(actor.to_string(), name.to_string()))
            .map(|v| v.value().clone())
    }

    /// Returns all extractors as qualified `actor_name.extractor_name` keys.
    ///
    /// Used to build the interpolation extractors map per SDK §5.5.
    #[must_use]
    pub fn all_qualified(&self) -> HashMap<String, String> {
        self.store
            .iter()
            .map(|entry| {
                let (actor, name) = entry.key();
                (format!("{actor}.{name}"), entry.value().clone())
            })
            .collect()
    }
}

impl std::fmt::Debug for ExtractorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractorStore")
            .field("entries", &self.store.len())
            .finish_non_exhaustive()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_store_set_and_get() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "abc123".to_string());
        assert_eq!(store.get("actor1", "token"), Some("abc123".to_string()));
    }

    #[test]
    fn extractor_store_get_missing() {
        let store = ExtractorStore::new();
        assert_eq!(store.get("actor1", "token"), None);
    }

    #[test]
    fn extractor_store_overwrite() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "old".to_string());
        store.set("actor1", "token", "new".to_string());
        assert_eq!(store.get("actor1", "token"), Some("new".to_string()));
    }

    #[test]
    fn extractor_store_all_qualified() {
        let store = ExtractorStore::new();
        store.set("actor1", "token", "abc".to_string());
        store.set("actor2", "session", "xyz".to_string());

        let qualified = store.all_qualified();
        assert_eq!(qualified.get("actor1.token"), Some(&"abc".to_string()));
        assert_eq!(qualified.get("actor2.session"), Some(&"xyz".to_string()));
        assert_eq!(qualified.len(), 2);
    }

    #[test]
    fn extractor_store_clone_shares_data() {
        let store = ExtractorStore::new();
        let store2 = store.clone();

        store.set("actor1", "token", "abc".to_string());
        assert_eq!(store2.get("actor1", "token"), Some("abc".to_string()));
    }

    // ---- Property Tests ----

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            #[test]
            fn prop_no_lost_writes(
                entries in prop::collection::vec(
                    ("[a-z]{1,8}", "[a-z]{1,8}", "[a-zA-Z0-9]{1,20}"),
                    1..50,
                )
            ) {
                let store = ExtractorStore::new();
                let mut expected = std::collections::HashSet::new();

                for (actor, name, value) in &entries {
                    store.set(actor, name, value.clone());
                    expected.insert((actor.clone(), name.clone()));
                }

                // All distinct (actor, name) pairs should be readable
                for (actor, name) in &expected {
                    prop_assert!(store.get(actor, name).is_some(),
                        "lost write for ({}, {})", actor, name);
                }
            }

            #[test]
            fn prop_version_monotonic(
                writes in prop::collection::vec(
                    ("[a-z]{1,5}", "[a-z]{1,5}", "[a-z]{1,10}"),
                    1..20,
                )
            ) {
                let store = ExtractorStore::new();
                let rx = store.subscribe();

                for (actor, name, value) in &writes {
                    store.set(actor, name, value.clone());
                }

                let version = *rx.borrow();
                prop_assert!(version >= writes.len() as u64,
                    "version {} should be >= write count {}", version, writes.len());
            }

            #[test]
            fn prop_qualified_key_format(
                entries in prop::collection::vec(
                    ("[a-z]{1,8}", "[a-z]{1,8}", "[a-z]{1,10}"),
                    1..20,
                )
            ) {
                let store = ExtractorStore::new();
                for (actor, name, value) in &entries {
                    store.set(actor, name, value.clone());
                }

                let qualified = store.all_qualified();
                for key in qualified.keys() {
                    // Every key must contain exactly one '.'
                    let dot_count = key.chars().filter(|c| *c == '.').count();
                    prop_assert_eq!(dot_count, 1,
                        "qualified key '{}' should have exactly one dot", key);

                    let parts: Vec<&str> = key.splitn(2, '.').collect();
                    prop_assert_eq!(parts.len(), 2);
                    prop_assert!(!parts[0].is_empty(), "actor part should not be empty");
                    prop_assert!(!parts[1].is_empty(), "name part should not be empty");
                }
            }

            #[test]
            fn prop_last_writer_wins(
                actor in "[a-z]{1,5}",
                name in "[a-z]{1,5}",
                values in prop::collection::vec("[a-z]{1,10}", 2..10),
            ) {
                let store = ExtractorStore::new();
                for value in &values {
                    store.set(&actor, &name, value.clone());
                }

                let result = store.get(&actor, &name).unwrap();
                // Result should be one of the values we wrote
                prop_assert!(values.contains(&result),
                    "get() returned '{}' which was never written", result);
            }
        }
    }

    #[tokio::test]
    async fn subscribe_notifies_on_set() {
        let store = ExtractorStore::new();
        let mut rx = store.subscribe();

        // Spawn a task that sets a value after a brief yield.
        let store2 = store.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            store2.set("actor1", "token", "hello".to_string());
        });

        // The receiver should be notified without polling.
        tokio::time::timeout(std::time::Duration::from_secs(1), rx.changed())
            .await
            .expect("timed out waiting for notification")
            .expect("watch sender dropped");

        assert_eq!(store.get("actor1", "token"), Some("hello".to_string()));
    }
}
