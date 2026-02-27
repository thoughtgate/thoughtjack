//! Thread-safe cross-actor extractor storage.
//!
//! `ExtractorStore` provides shared extractor state between actors,
//! enabling cross-actor interpolation and `await_extractors` synchronization.
//!
//! See TJ-SPEC-015 §4 for the extractor store specification.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;

/// Thread-safe cross-actor extractor storage.
///
/// Stores extractor values keyed by `(actor_name, extractor_name)`.
/// Used by the `PhaseLoop` to publish captured values and by other
/// actors to read them for cross-actor interpolation.
///
/// Implements: TJ-SPEC-015 F-001
#[derive(Clone, Default)]
pub struct ExtractorStore {
    store: Arc<DashMap<(String, String), String>>,
}

impl ExtractorStore {
    /// Creates a new empty extractor store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets an extractor value for a given actor.
    pub fn set(&self, actor: &str, name: &str, value: String) {
        self.store
            .insert((actor.to_string(), name.to_string()), value);
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
}
