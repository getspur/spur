//! The neutral mention completion engine (2026-08-27 mentions spec §§2-5).
//!
//! [`MentionEngine::query`] canonicalizes the root, resolves one snapshot
//! per registered source through the root-scoped TTL cache (identity:
//! `canonical_root + source_key + profile_fingerprint + source_token`),
//! ranks the concatenated candidates with the bounded top-K selection
//! (byte-identical to the deterministic full-sort reference, pinned by the
//! sud-m5 property tests), and returns owned rows plus non-fatal
//! diagnostics.
//!
//! Failure policy (spec error table): a *required* source whose build
//! fails fails the whole query with a typed [`MentionError`]; an
//! *optional* source degrades to a [`QueryDiagnostic`] and keeps serving
//! its previously retained snapshot. Failed builds never replace a valid
//! previous snapshot, and publication is atomic.
//!
//! The engine is `Send` (asserted by test): frontends choose their own
//! ownership wrapper — the TUI keeps `Rc<RefCell<…>>` façades on the UI
//! thread, the notebook uses `Arc<Mutex<MentionEngine>>` — and there is no
//! process-global cache state.
//!
//! Tracing spans are redacted by construction: they record a root *hash*,
//! the profile fingerprint, counts, and durations — never paths, URIs, or
//! query text.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use nucleo_matcher::{Config, Matcher};

use crate::cache::{CacheKey, SnapshotCache};
use crate::clock::Clock;
use crate::entry::{
    MentionEntry, MentionSource, SourceBuildError, SourceBuildErrorKind, SourceContext,
    SourceSnapshot,
};
use crate::error::{MentionError, QueryDiagnostic, RootError, SourceBuildFailure, TraversalError};
use crate::profile::FilesystemProfile;
use crate::rank::{score_all, select_top_k, RankOptions};
use std::path::PathBuf;

/// Initial compatibility TTL, matching the TUI registry snapshot cache
/// it replaced (600s; the old facade's per-kind TTL constants were both
/// this value). Cache capacity and eviction are
/// configuration; benchmarks may justify changing this later.
pub const DEFAULT_SNAPSHOT_TTL: Duration = Duration::from_secs(600);

/// Tracing target for engine spans.
const SPAN_TARGET: &str = "spur_mentions";

/// Options for one [`MentionEngine::query`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryOptions {
    /// Filesystem traversal profile forwarded to every source build and
    /// fingerprinted into every cache key.
    pub filesystem_profile: FilesystemProfile,
    /// Ranking options (limit, tier policy).
    pub rank: RankOptions,
}

impl QueryOptions {
    /// Defaults: TUI-compatible filesystem profile, unlimited default-tier
    /// ranking.
    pub const fn new() -> Self {
        Self {
            filesystem_profile: FilesystemProfile::tui(),
            rank: RankOptions::new(),
        }
    }
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of one query: ranked, limit-truncated rows plus the non-fatal
/// degradations observed while assembling them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryResult {
    /// Ranked rows in display order (bounded top-K selection over the
    /// full-sort comparator, then truncate).
    pub entries: Vec<MentionEntry>,
    /// One entry per degraded optional source.
    pub diagnostics: Vec<QueryDiagnostic>,
}

/// Outcome of one [`MentionEngine::resolve_snapshot`] call (the per-source
/// seam frontends with deferred builds compose on; the fused
/// [`MentionEngine::query`] path never surfaces it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotResolution {
    /// Fresh cache hit: the snapshot is within TTL under the current
    /// profile fingerprint and source token.
    Fresh(Arc<SourceSnapshot>),
    /// The source was rebuilt and published now.
    Built(Arc<SourceSnapshot>),
    /// A rebuild failed; the previously retained snapshot keeps serving
    /// (even an expired one), exactly like the fused query's optional
    /// degradation policy.
    Retained(Arc<SourceSnapshot>),
    /// The rebuild failed and nothing was retained under this identity.
    Failed(SourceBuildFailure),
}

/// Per-slot outcome of the shared resolve loop behind both
/// [`MentionEngine::query`] and [`MentionEngine::resolve_snapshot`].
enum SlotOutcome {
    Fresh(Arc<SourceSnapshot>),
    Built(Arc<SourceSnapshot>),
    Failed {
        key: CacheKey,
        error: SourceBuildError,
    },
}

/// One registered source and its failure policy.
struct SourceSlot {
    source: Box<dyn MentionSource>,
    required: bool,
    /// Last generation stamped for this source; monotonic across every
    /// publish (rebuilds, root switches, profile switches, cache clears).
    last_generation: u64,
}

/// Neutral mention completion engine.
pub struct MentionEngine {
    sources: Vec<SourceSlot>,
    cache: SnapshotCache,
    clock: Arc<dyn Clock>,
    ttl: Duration,
    matcher: Matcher,
}

impl MentionEngine {
    /// Create an engine measuring snapshot age on `clock` with the
    /// default 600s TTL.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_ttl(clock, DEFAULT_SNAPSHOT_TTL)
    }

    /// Create an engine with an explicit snapshot TTL.
    pub fn with_ttl(clock: Arc<dyn Clock>, ttl: Duration) -> Self {
        Self {
            sources: Vec::new(),
            cache: SnapshotCache::default(),
            clock,
            ttl,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Register a source. `required` sources fail the whole query when
    /// their build fails; optional ones degrade to diagnostics.
    ///
    /// One source per [`MentionSource::key`]: registering a key again
    /// replaces the earlier source (its cached snapshots stay until
    /// invalidated or expired) and keeps the key's generation counter, so
    /// generations stay monotonic across source swaps.
    pub fn register_source(&mut self, source: Box<dyn MentionSource>, required: bool) {
        let key = source.key().to_owned();
        let last_generation = self
            .sources
            .iter()
            .find(|slot| slot.source.key() == key)
            .map_or(0, |slot| slot.last_generation);
        self.sources.retain(|slot| slot.source.key() != key);
        self.sources.push(SourceSlot {
            source,
            required,
            last_generation,
        });
    }

    /// Configured snapshot TTL.
    pub fn cache_ttl(&self) -> Duration {
        self.ttl
    }

    /// Last generation this source published (`None` before the first
    /// successful build). Monotonic by construction: the engine owns the
    /// per-source counter and re-stamps every published snapshot, so
    /// generations never go backwards regardless of root, profile, or
    /// cache lifecycle.
    pub fn snapshot_generation(&self, source_key: &str) -> Option<u64> {
        self.sources
            .iter()
            .find(|slot| slot.source.key() == source_key)
            .map(|slot| slot.last_generation)
            .filter(|generation| *generation > 0)
    }

    /// Drop every cached snapshot (all roots, all sources). Per-source
    /// generation counters keep advancing.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Drop every cached snapshot for one source key (all roots): the
    /// per-source counterpart of [`MentionEngine::invalidate_root`] for
    /// source-swap invalidation. Per-source generation counters keep
    /// advancing.
    pub fn invalidate_source(&mut self, source_key: &str) {
        self.cache.invalidate_source(source_key);
    }

    /// Fresh-cache lookup for one source without building: `Some` only
    /// when a snapshot exists under exactly this root/profile/token
    /// identity and is within TTL.
    ///
    /// Cache identity uses the canonical form of `root` when resolvable
    /// (alternate spellings of one directory share a snapshot) and the path
    /// as given otherwise. Unlike [`MentionEngine::query`], the root is
    /// neither validated nor canonicalized for its own sake: frontends
    /// that derive row data (file URIs, relative displays) from the root
    /// keep byte-identical output regardless of how they spell the path.
    pub fn cached_snapshot(
        &self,
        root: &Path,
        source_key: &str,
        options: &QueryOptions,
    ) -> Option<Arc<SourceSnapshot>> {
        let slot = self
            .sources
            .iter()
            .find(|slot| slot.source.key() == source_key)?;
        let key = CacheKey::new(
            identity_root(root),
            slot.source.key(),
            options.filesystem_profile.fingerprint(),
            slot.source.source_token(),
        );
        self.cache.get_fresh(&key, self.clock.now(), self.ttl)
    }

    /// Resolve one source's snapshot: fresh cache hit, or build and publish
    /// now. The per-source seam behind frontends whose query pipelines
    /// cannot use the fused [`MentionEngine::query`] (deferred off-thread
    /// builds, frontend-specific ranking over borrowed rows).
    ///
    /// Builds receive `root` exactly as passed — never a canonicalized
    /// substitute — while the cache identity still uses the canonical form
    /// (see [`MentionEngine::cached_snapshot`]). Required/optional policy is
    /// the caller's: this seam always degrades on failure
    /// ([`SnapshotResolution::Retained`] / [`Failed`]); only the fused query
    /// hard-fails on required sources.
    pub fn resolve_snapshot(
        &mut self,
        root: &Path,
        source_key: &str,
        options: &QueryOptions,
    ) -> SnapshotResolution {
        let Some(pos) = self.slot_index(source_key) else {
            return SnapshotResolution::Failed(SourceBuildFailure::new(
                source_key,
                "source is not registered with this engine",
            ));
        };
        match self.resolve_slot(pos, root, options) {
            SlotOutcome::Fresh(snapshot) => SnapshotResolution::Fresh(snapshot),
            SlotOutcome::Built(snapshot) => SnapshotResolution::Built(snapshot),
            SlotOutcome::Failed { key, error } => match self.cache.get_any(&key) {
                Some(retained) => SnapshotResolution::Retained(retained),
                None => SnapshotResolution::Failed(SourceBuildFailure::new(
                    key.source_key.into_string(),
                    error.message,
                )),
            },
        }
    }

    /// Publish an externally built snapshot under the caller's root/profile
    /// identity: stamps the source's next generation and stores atomically.
    /// For deferred builds whose data was produced outside the engine (e.g.
    /// a frontend worker thread); the fused query never calls it.
    ///
    /// Returns `None` when no source is registered under `source_key`.
    pub fn publish_snapshot(
        &mut self,
        root: &Path,
        source_key: &str,
        options: &QueryOptions,
        snapshot: SourceSnapshot,
    ) -> Option<Arc<SourceSnapshot>> {
        let pos = self.slot_index(source_key)?;
        let slot = &mut self.sources[pos];
        slot.last_generation += 1;
        let mut snapshot = snapshot;
        snapshot.generation = slot.last_generation;
        let key = CacheKey::new(
            identity_root(root),
            slot.source.key(),
            options.filesystem_profile.fingerprint(),
            slot.source.source_token(),
        );
        Some(self.cache.publish(key, snapshot, self.clock.now()))
    }

    fn slot_index(&self, source_key: &str) -> Option<usize> {
        self.sources
            .iter()
            .position(|slot| slot.source.key() == source_key)
    }

    /// Shared per-slot resolve loop: fresh-hit lookup, build + publish on
    /// miss. `root` is passed to the source verbatim; only the cache key
    /// uses the canonical identity form.
    fn resolve_slot(&mut self, pos: usize, root: &Path, options: &QueryOptions) -> SlotOutcome {
        let slot = &mut self.sources[pos];
        let key = CacheKey::new(
            identity_root(root),
            slot.source.key(),
            options.filesystem_profile.fingerprint(),
            slot.source.source_token(),
        );
        if let Some(snapshot) = self.cache.get_fresh(&key, self.clock.now(), self.ttl) {
            tracing::debug!(
                target: SPAN_TARGET,
                source = key.source_key.as_ref(),
                cache_hit = true,
                "mention source snapshot resolved"
            );
            return SlotOutcome::Fresh(snapshot);
        }
        let context = SourceContext {
            filesystem_profile: options.filesystem_profile,
        };
        match slot.source.build(root, &context) {
            Ok(mut snapshot) => {
                slot.last_generation += 1;
                snapshot.generation = slot.last_generation;
                tracing::debug!(
                    target: SPAN_TARGET,
                    source = key.source_key.as_ref(),
                    cache_hit = false,
                    generation = slot.last_generation,
                    "mention source snapshot built"
                );
                SlotOutcome::Built(self.cache.publish(key, snapshot, self.clock.now()))
            }
            Err(error) => SlotOutcome::Failed { key, error },
        }
    }

    /// Explicitly invalidate every snapshot for `root` (canonicalized),
    /// independent of the TTL.
    pub fn invalidate_root(&mut self, root: &Path) {
        if let Ok(canonical) = root.canonicalize() {
            self.cache.invalidate_root(&canonical);
        }
    }

    /// Run one completion query: resolve or rebuild each source snapshot,
    /// then rank the concatenated candidates with the bounded top-K
    /// selection — `select_nth_unstable_by` plus a prefix sort when the
    /// match count exceeds the limit, byte-identical to the full-sort
    /// reference (`tests/rank_top_k.rs`) and capped at
    /// `options.rank.limit`.
    pub fn query(
        &mut self,
        root: &Path,
        query: &str,
        options: &QueryOptions,
    ) -> Result<QueryResult, MentionError> {
        let span = tracing::debug_span!(
            target: SPAN_TARGET,
            "mention_engine_query",
            root_hash = tracing::field::Empty,
            profile_fingerprint = tracing::field::Empty,
            query_len = query.len(),
            limit = tracing::field::Empty,
            sources = self.sources.len(),
            candidates = tracing::field::Empty,
            matched = tracing::field::Empty,
            cache_hits = tracing::field::Empty,
            cache_misses = tracing::field::Empty,
            build_us = tracing::field::Empty,
            score_us = tracing::field::Empty,
            select_us = tracing::field::Empty,
        );
        let _guard = span.enter();

        let canonical_root = root.canonicalize().map_err(|error| {
            MentionError::Root(RootError::new(format!(
                "canonical root resolution failed: {}",
                error.kind()
            )))
        })?;
        if !canonical_root.is_dir() {
            return Err(MentionError::Root(RootError::new(
                "canonical root is not a directory",
            )));
        }
        let root_hash_hex = format!("{:016x}", root_hash(&canonical_root));
        span.record("root_hash", root_hash_hex.as_str());
        span.record(
            "profile_fingerprint",
            options.filesystem_profile.fingerprint(),
        );
        span.record(
            "limit",
            options
                .rank
                .limit
                .map(|limit| i64::try_from(limit).unwrap_or(i64::MAX))
                .unwrap_or(-1),
        );

        let mut diagnostics = Vec::new();
        let mut snapshots: Vec<Arc<SourceSnapshot>> = Vec::with_capacity(self.sources.len());
        let mut cache_hits = 0u32;
        let mut cache_misses = 0u32;

        let build_started = self.clock.now();
        for pos in 0..self.sources.len() {
            match self.resolve_slot(pos, &canonical_root, options) {
                SlotOutcome::Fresh(snapshot) => {
                    cache_hits += 1;
                    snapshots.push(snapshot);
                }
                SlotOutcome::Built(snapshot) => {
                    cache_misses += 1;
                    snapshots.push(snapshot);
                }
                // Failed builds never replace a valid previous snapshot:
                // the cache entry is left untouched.
                SlotOutcome::Failed { key, error } => {
                    cache_misses += 1;
                    if self.sources[pos].required {
                        return Err(match error.kind {
                            SourceBuildErrorKind::Traversal => MentionError::Traversal(
                                TraversalError::new(key.source_key.into_string(), error.message),
                            ),
                            SourceBuildErrorKind::Other => {
                                MentionError::SourceBuild(SourceBuildFailure::new(
                                    key.source_key.into_string(),
                                    error.message,
                                ))
                            }
                        });
                    }
                    // Optional degradation: keep serving the retained
                    // snapshot (even an expired one) rather than an empty
                    // picker.
                    if let Some(retained) = self.cache.get_any(&key) {
                        snapshots.push(retained);
                    }
                    diagnostics.push(QueryDiagnostic {
                        source_key: key.source_key.into_string(),
                        message: error.message,
                    });
                }
            }
        }
        let build_finished = self.clock.now();
        span.record(
            "build_us",
            micros(build_finished.duration_since(build_started)),
        );
        span.record("cache_hits", cache_hits);
        span.record("cache_misses", cache_misses);

        let mut refs: Vec<&MentionEntry> = Vec::new();
        for snapshot in &snapshots {
            refs.extend(snapshot.entries.iter());
        }
        span.record("candidates", refs.len());

        // Same scoring pass the reference `rank_full_sort` runs; the select
        // phase is the sud-m5 bounded top-K (select_nth_unstable_by +
        // prefix sort when matches exceed the limit), split here so the
        // score and select phases time separately.
        let score_started = self.clock.now();
        let mut scored = score_all(&refs, query, &options.rank.tiers, &mut self.matcher);
        let score_finished = self.clock.now();
        let matched = scored.len();
        let kept = select_top_k(&mut scored, options.rank.limit);
        scored.truncate(kept);
        let select_finished = self.clock.now();
        span.record(
            "score_us",
            micros(score_finished.duration_since(score_started)),
        );
        span.record(
            "select_us",
            micros(select_finished.duration_since(score_finished)),
        );
        span.record("matched", matched);

        let entries: Vec<MentionEntry> = scored
            .into_iter()
            .map(|ranked| ranked.entry.clone())
            .collect();

        Ok(QueryResult {
            entries,
            diagnostics,
        })
    }
}

/// Cache-identity root: the canonical form when resolvable, the path as
/// given otherwise. Build inputs stay verbatim (see the per-source seams:
/// [`MentionEngine::resolve_snapshot`] and friends); only cache identity
/// is normalized so alternate spellings of one directory share a snapshot.
fn identity_root(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// Redacted root identity for spans: a 64-bit hash of the canonical path.
/// The path itself never enters tracing.
fn root_hash(path: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_hash_is_stable_distinct_and_redacted() {
        let a = Path::new("/ws/alpha");
        let b = Path::new("/ws/beta");

        assert_eq!(root_hash(a), root_hash(a));
        assert_ne!(root_hash(a), root_hash(b));

        // Redaction: the rendered form is hex and carries no path bytes.
        let rendered = format!("{:016x}", root_hash(a));
        assert!(rendered.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!rendered.contains('/'));
    }

    #[test]
    fn micros_saturates() {
        assert_eq!(micros(Duration::from_micros(5)), 5);
        assert_eq!(micros(Duration::ZERO), 0);
    }
}
