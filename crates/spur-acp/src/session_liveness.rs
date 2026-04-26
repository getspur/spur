//! Probe whether a brain session is held by any live process, without
//! mutating the lockfile state observable by other processes.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::BrainSessionId;

#[derive(Debug, Clone, Default)]
pub struct SelfHeldSet {
    inner: Arc<RwLock<HashSet<BrainSessionId>>>,
}

impl SelfHeldSet {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    pub fn insert(&self, id: BrainSessionId) {
        self.inner.write().expect("SelfHeldSet poisoned").insert(id);
    }

    pub fn remove(&self, id: &BrainSessionId) -> bool {
        self.inner.write().expect("SelfHeldSet poisoned").remove(id)
    }

    pub fn contains(&self, id: &BrainSessionId) -> bool {
        self.inner
            .read()
            .expect("SelfHeldSet poisoned")
            .contains(id)
    }
}

#[derive(Debug)]
pub enum SessionLivenessProbeResult {
    Live,
    DeadAcquired(DeadSessionGuard),
    Self_,
    Missing,
    FsUnsafe,
}

#[derive(Debug)]
pub struct DeadSessionGuard {
    #[allow(dead_code)]
    file: File,
    brain_session_id: BrainSessionId,
}

impl DeadSessionGuard {
    pub fn brain_session_id(&self) -> &BrainSessionId {
        &self.brain_session_id
    }
}

pub struct SessionLivenessProbe;

impl SessionLivenessProbe {
    pub fn probe(
        repo_root: &Path,
        target: &BrainSessionId,
        held_by_self: &SelfHeldSet,
    ) -> SessionLivenessProbeResult {
        if held_by_self.contains(target) {
            return SessionLivenessProbeResult::Self_;
        }
        let lock_path = lock_path_for(repo_root, target);
        let _file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .truncate(false)
            .open(&lock_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return SessionLivenessProbeResult::Missing;
            }
            Err(e) => {
                tracing::warn!(error=%e, path=%lock_path.display(),
                    "session liveness probe open failed; treating as Live");
                return SessionLivenessProbeResult::Live;
            }
        };
        // flock branch added in next task
        unimplemented!("flock variant in Task 9")
    }
}

fn lock_path_for(repo_root: &Path, target: &BrainSessionId) -> PathBuf {
    repo_root
        .join(".spur/sessions")
        .join(format!("{}.lock", target.as_session_id().0))
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::SessionId;
    use tempfile::TempDir;

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    #[test]
    fn probe_returns_self_for_held_session() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        set.insert(target.clone());
        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(result, SessionLivenessProbeResult::Self_));
    }

    #[test]
    fn probe_returns_missing_when_lockfile_absent() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(result, SessionLivenessProbeResult::Missing));
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
