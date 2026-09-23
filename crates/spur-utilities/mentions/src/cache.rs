//! Root-scoped snapshot cache (2026-08-27 mentions spec §4).
//!
//! Cache identity is
//! `CacheKey = canonical_root + source_key + profile_fingerprint + source_token`:
//! a snapshot produced under one traversal profile, root, or source data
//! revision can never be reused under another. Publication is atomic
//! (readers see either the previous complete generation or the new one),
//! failed builds never replace a valid previous snapshot, and TTL is
//! accounted on the injected [`crate::Clock`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::entry::SourceSnapshot;

/// Cache identity: canonical root + source key + profile fingerprint +
/// source token (the source's data revision, e.g. a code-graph artifact
/// identity; `0` means "TTL governs").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheKey {
    /// Canonicalized workspace root.
    pub canonical_root: PathBuf,
    /// [`crate::MentionSource::key`] of the owning source.
    pub source_key: Box<str>,
    /// [`crate::FilesystemProfile::fingerprint`] of the traversal profile.
    pub profile_fingerprint: u64,
    /// Source-supplied data revision token.
    pub source_token: u64,
}

impl CacheKey {
    /// Assemble a key from its four identity components.
    pub fn new(
        canonical_root: PathBuf,
        source_key: &str,
        profile_fingerprint: u64,
        source_token: u64,
    ) -> Self {
        Self {
            canonical_root,
            source_key: source_key.into(),
            profile_fingerprint,
            source_token,
        }
    }
}

/// One published snapshot plus its build time.
pub(crate) struct CacheEntry {
    pub snapshot: Arc<SourceSnapshot>,
    pub cached_at: Instant,
}

/// TTL-scoped snapshot storage. No process-global state: engines own their
/// cache and die with it.
#[derive(Default)]
pub(crate) struct SnapshotCache {
    entries: HashMap<CacheKey, CacheEntry>,
}

impl SnapshotCache {
    /// Fresh-hit lookup: `Some` only when an entry exists under exactly
    /// this key and its age (on the injected clock) is within `ttl`
    /// (inclusive; today's TUI rebuilds only when `elapsed > ttl`).
    pub(crate) fn get_fresh(
        &self,
        key: &CacheKey,
        now: Instant,
        ttl: Duration,
    ) -> Option<Arc<SourceSnapshot>> {
        let entry = self.entries.get(key)?;
        let age = now
            .checked_duration_since(entry.cached_at)
            .unwrap_or(Duration::ZERO);
        if age > ttl {
            return None;
        }
        Some(Arc::clone(&entry.snapshot))
    }

    /// Retention lookup: the previous snapshot under this key regardless of
    /// freshness. Serves failed-build retention (a failed rebuild keeps
    /// serving the previous complete snapshot instead of an empty picker).
    pub(crate) fn get_any(&self, key: &CacheKey) -> Option<Arc<SourceSnapshot>> {
        self.entries
            .get(key)
            .map(|entry| Arc::clone(&entry.snapshot))
    }

    /// Atomically publish a freshly built snapshot under `key`. The insert
    /// is one `HashMap` step: readers observe either the previous complete
    /// generation or the new one, never a partial snapshot.
    pub(crate) fn publish(
        &mut self,
        key: CacheKey,
        snapshot: SourceSnapshot,
        now: Instant,
    ) -> Arc<SourceSnapshot> {
        let snapshot = Arc::new(snapshot);
        self.entries.insert(
            key,
            CacheEntry {
                snapshot: Arc::clone(&snapshot),
                cached_at: now,
            },
        );
        snapshot
    }

    /// Drop every snapshot for `canonical_root` (explicit root
    /// invalidation; the next query rebuilds).
    pub(crate) fn invalidate_root(&mut self, canonical_root: &Path) {
        self.entries
            .retain(|key, _| key.canonical_root != canonical_root);
    }

    /// Drop every snapshot for one source key (all roots): source-swap
    /// invalidation, the per-source counterpart of [`SnapshotCache::invalidate_root`].
    pub(crate) fn invalidate_source(&mut self, source_key: &str) {
        self.entries
            .retain(|key, _| key.source_key.as_ref() != source_key);
    }

    /// Drop every snapshot.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(token: u64) -> CacheKey {
        CacheKey::new(PathBuf::from("/ws"), "file", PROFILE_FP, token)
    }

    const PROFILE_FP: u64 = 7;

    fn snapshot(generation: u64) -> SourceSnapshot {
        SourceSnapshot::new(Vec::new(), generation)
    }

    #[test]
    fn fresh_hit_within_ttl_and_miss_past_it() {
        let mut cache = SnapshotCache::default();
        let now = Instant::now();
        cache.publish(key(0), snapshot(1), now);

        assert!(cache
            .get_fresh(&key(0), now, Duration::from_secs(600))
            .is_some());
        // Exactly at the TTL boundary the entry is still fresh
        // (exclusive rebuild semantics).
        assert!(cache
            .get_fresh(
                &key(0),
                now + Duration::from_secs(600),
                Duration::from_secs(600)
            )
            .is_some());
        assert!(cache
            .get_fresh(
                &key(0),
                now + Duration::from_secs(600) + Duration::from_nanos(1),
                Duration::from_secs(600)
            )
            .is_none());
        // A different token is a different key: miss.
        assert!(cache
            .get_fresh(&key(1), now, Duration::from_secs(600))
            .is_none());
    }

    #[test]
    fn retention_lookup_serves_expired_entries() {
        let mut cache = SnapshotCache::default();
        let now = Instant::now();
        cache.publish(key(0), snapshot(1), now);

        let later = now + Duration::from_secs(9_000);
        assert!(cache
            .get_fresh(&key(0), later, Duration::from_secs(600))
            .is_none());
        assert!(cache.get_any(&key(0)).is_some());
    }

    #[test]
    fn invalidate_root_drops_only_that_root() {
        let mut cache = SnapshotCache::default();
        let now = Instant::now();
        cache.publish(key(0), snapshot(1), now);
        let other = CacheKey::new(PathBuf::from("/other"), "file", 7, 0);
        cache.publish(other, snapshot(1), now);

        cache.invalidate_root(Path::new("/ws"));
        assert!(cache.get_any(&key(0)).is_none());
        assert!(cache
            .get_any(&CacheKey::new(PathBuf::from("/other"), "file", 7, 0))
            .is_some());
    }
}
