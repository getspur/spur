//! Probe whether a brain session is held by any live process, without
//! mutating the lockfile state observable by other processes.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use crate::BrainSessionId;

#[derive(Debug, Clone, Default)]
pub struct SelfHeldSet {
    inner: Arc<RwLock<HashSet<BrainSessionId>>>,
}

impl SelfHeldSet {
    pub fn new() -> Self {
        Self { inner: Arc::new(RwLock::new(HashSet::new())) }
    }

    pub fn insert(&self, id: BrainSessionId) {
        self.inner.write().expect("SelfHeldSet poisoned").insert(id);
    }

    pub fn remove(&self, id: &BrainSessionId) -> bool {
        self.inner.write().expect("SelfHeldSet poisoned").remove(id)
    }

    pub fn contains(&self, id: &BrainSessionId) -> bool {
        self.inner.read().expect("SelfHeldSet poisoned").contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionId;

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    #[test]
    fn self_held_set_insert_and_contains() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        assert!(!set.contains(&a));
        set.insert(a.clone());
        assert!(set.contains(&a));
    }

    #[test]
    fn self_held_set_remove_returns_true_when_present() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(a.clone());
        assert!(set.remove(&a));
        assert!(!set.contains(&a));
    }

    #[test]
    fn self_held_set_remove_returns_false_when_absent() {
        let set = SelfHeldSet::new();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        assert!(!set.remove(&a));
    }

    #[test]
    fn self_held_set_clones_share_state() {
        let set = SelfHeldSet::new();
        let clone = set.clone();
        let a = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(a.clone());
        assert!(clone.contains(&a));
    }
}
