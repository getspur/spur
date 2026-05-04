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
        let file = match OpenOptions::new()
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
                tracing::warn!(error=%e, "session liveness probe open failed");
                return SessionLivenessProbeResult::Live;
            }
        };

        use fs4::fs_std::FileExt;
        match file.try_lock_exclusive() {
            Ok(true) => SessionLivenessProbeResult::DeadAcquired(DeadSessionGuard {
                file,
                brain_session_id: target.clone(),
            }),
            Ok(false) => SessionLivenessProbeResult::Live,
            Err(e) if is_enotsup_or_enolck(&e) => SessionLivenessProbeResult::FsUnsafe,
            Err(e) => {
                tracing::warn!(error=%e, "try_lock_exclusive failed; treating as Live");
                SessionLivenessProbeResult::Live
            }
        }
    }
}

fn lock_path_for(repo_root: &Path, target: &BrainSessionId) -> PathBuf {
    repo_root
        .join(".spur/sessions")
        .join(format!("{}.lock", target.as_session_id().0))
}

fn is_enotsup_or_enolck(e: &io::Error) -> bool {
    use std::io::ErrorKind;
    if matches!(e.kind(), ErrorKind::Unsupported) {
        return true;
    }

    #[cfg(unix)]
    {
        let raw = e.raw_os_error();
        raw == Some(libc::ENOLCK) || raw == Some(libc::ENOTSUP)
    }

    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;
    use crate::SessionId;
    use fs4::fs_std::FileExt;
    use tempfile::TempDir;

    fn id(s: &str) -> BrainSessionId {
        BrainSessionId::new(SessionId(s.into()))
    }

    fn create_lockfile(td: &TempDir, target: &BrainSessionId) -> PathBuf {
        let dir = td.path().join(".spur/sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.lock", target.as_session_id().0));
        std::fs::write(&path, b"").unwrap();
        path
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

    #[test]
    fn probe_returns_dead_acquired_when_lockfile_unlocked() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        create_lockfile(&td, &target);

        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        match result {
            SessionLivenessProbeResult::DeadAcquired(guard) => {
                assert_eq!(guard.brain_session_id(), &target);
            }
            other => panic!("expected DeadAcquired, got {:?}", other),
        }
    }

    #[test]
    fn probe_returns_live_when_other_holds_lock() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        let lock_path = create_lockfile(&td, &target);

        // Acquire the lock from "another process" (this test holds it).
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        held.try_lock_exclusive().expect("hold lock");

        let result = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(result, SessionLivenessProbeResult::Live));

        // Cleanup: drop releases.
        drop(held);
    }

    #[test]
    fn probe_does_not_truncate_lockfile() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        let lock_path = create_lockfile(&td, &target);
        std::fs::write(&lock_path, b"holder-info-payload").unwrap();
        let before = std::fs::read(&lock_path).unwrap();

        let _result = SessionLivenessProbe::probe(td.path(), &target, &set);
        let after = std::fs::read(&lock_path).unwrap();
        assert_eq!(before, after, "probe must not truncate or modify lockfile");
    }

    #[test]
    fn dead_session_guard_releases_on_drop() {
        let td = TempDir::new().unwrap();
        let set = SelfHeldSet::new();
        let target = id("550e8400-e29b-41d4-a716-446655440000");
        create_lockfile(&td, &target);

        {
            let r = SessionLivenessProbe::probe(td.path(), &target, &set);
            assert!(matches!(r, SessionLivenessProbeResult::DeadAcquired(_)));
            // Guard goes out of scope here; lock should release.
        }

        // Re-probe; should be DeadAcquired again (lock was released).
        let r2 = SessionLivenessProbe::probe(td.path(), &target, &set);
        assert!(matches!(r2, SessionLivenessProbeResult::DeadAcquired(_)));
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
