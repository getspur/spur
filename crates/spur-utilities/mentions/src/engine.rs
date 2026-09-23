//! The neutral mention completion engine (2026-08-27 mentions spec §§2-5).
//!
//! [`MentionEngine::query`] canonicalizes the root, resolves one snapshot
//! per registered source through the root-scoped TTL cache (identity:
//! `canonical_root + source_key + profile_fingerprint + source_token`),
//! ranks the concatenated candidates with the deterministic full-sort
//! reference, and returns owned rows plus non-fatal diagnostics.
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
    MentionEntry, MentionSource, SourceBuildErrorKind, SourceContext, SourceSnapshot,
};
use crate::error::{MentionError, QueryDiagnostic, RootError, SourceBuildFailure, TraversalError};
use crate::profile::FilesystemProfile;
use crate::rank::{score_all, sort_ranked, RankOptions};

/// Initial compatibility TTL, matching today's TUI registry
/// (`GLOBAL_SOURCE_CACHE_TTL`). Cache capacity and eviction are
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
    /// Ranked rows in display order (full-sort comparator, then truncate).
    pub entries: Vec<MentionEntry>,
    /// One entry per degraded optional source.
    pub diagnostics: Vec<QueryDiagnostic>,
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
    pub fn register_source(&mut self, source: Box<dyn MentionSource>, required: bool) {
        self.sources.push(SourceSlot {
            source,
            required,
            last_generation: 0,
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

    /// Explicitly invalidate every snapshot for `root` (canonicalized),
    /// independent of the TTL.
    pub fn invalidate_root(&mut self, root: &Path) {
        if let Ok(canonical) = root.canonicalize() {
            self.cache.invalidate_root(&canonical);
        }
    }

    /// Run one completion query: resolve or rebuild each source snapshot,
    /// rank the concatenated candidates with the full-sort reference, and
    /// truncate to `options.rank.limit`.
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

        let context = SourceContext {
            filesystem_profile: options.filesystem_profile,
        };
        let mut diagnostics = Vec::new();
        let mut snapshots: Vec<Arc<SourceSnapshot>> = Vec::with_capacity(self.sources.len());
        let mut cache_hits = 0u32;
        let mut cache_misses = 0u32;

        let build_started = self.clock.now();
        for slot in &mut self.sources {
            let key = CacheKey::new(
                canonical_root.clone(),
                slot.source.key(),
                options.filesystem_profile.fingerprint(),
                slot.source.source_token(),
            );
            if let Some(snapshot) = self.cache.get_fresh(&key, self.clock.now(), self.ttl) {
                cache_hits += 1;
                tracing::debug!(
                    target: SPAN_TARGET,
                    source = key.source_key.as_ref(),
                    cache_hit = true,
                    "mention source snapshot resolved"
                );
                snapshots.push(snapshot);
                continue;
            }
            cache_misses += 1;
            match slot.source.build(&canonical_root, &context) {
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
                    let published = self.cache.publish(key, snapshot, self.clock.now());
                    snapshots.push(published);
                }
                Err(error) => {
                    // Failed builds never replace a valid previous
                    // snapshot: the cache entry is left untouched.
                    if slot.required {
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

        // Same primitives the reference `rank_full_sort` composes; split
        // here so the score and select phases time separately.
        let score_started = self.clock.now();
        let mut scored = score_all(&refs, query, &options.rank.tiers, &mut self.matcher);
        let score_finished = self.clock.now();
        sort_ranked(&mut scored);
        let select_finished = self.clock.now();
        span.record(
            "score_us",
            micros(score_finished.duration_since(score_started)),
        );
        span.record(
            "select_us",
            micros(select_finished.duration_since(score_finished)),
        );
        span.record("matched", scored.len());

        if let Some(limit) = options.rank.limit {
            scored.truncate(limit);
        }
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
