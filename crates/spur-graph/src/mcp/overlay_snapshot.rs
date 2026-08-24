use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Context as _};

use crate::git::{
    FsmonitorCapabilities, FsmonitorStatusRoute, GitStatusCode, GitStatusObservation,
    PorcelainV2Entry, PorcelainV2EntryKind,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct IndexIdentity {
    path: PathBuf,
    len: u64,
    modified_nanos: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SnapshotIdentity {
    pub(crate) canonical_worktree: PathBuf,
    pub(crate) indexed_graph_content_hash: String,
    pub(crate) indexed_head_oid: Option<String>,
    pub(crate) current_head_oid: String,
    pub(crate) index_identity: IndexIdentity,
    pub(crate) normalized_changed_set_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum OverlayPathState {
    Tracked(String),
    Untracked(String),
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotLifecycleState {
    Cold,
    Validating,
    Valid,
    Retrying,
    ExactFallback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverlaySnapshotSource {
    FsmonitorNative,
    ExactFallback,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SnapshotMeasurements {
    pub(crate) full_index_sweeps: usize,
    pub(crate) hashed_paths: Vec<String>,
    pub(crate) head_lag_diffs: usize,
    pub(crate) retries: usize,
    pub(crate) exact_fallbacks: usize,
    pub(crate) snapshot_reused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverlaySnapshot {
    pub(crate) identity: Option<SnapshotIdentity>,
    pub(crate) path_state: BTreeMap<String, OverlayPathState>,
    pub(crate) source: OverlaySnapshotSource,
    pub(crate) lifecycle: Vec<SnapshotLifecycleState>,
    pub(crate) measurements: SnapshotMeasurements,
}

impl OverlaySnapshot {
    pub(crate) fn changed_oid_hex(&self) -> BTreeMap<String, Option<String>> {
        self.path_state
            .iter()
            .map(|(path, state)| {
                let oid = match state {
                    OverlayPathState::Tracked(oid) | OverlayPathState::Untracked(oid) => {
                        Some(oid.clone())
                    }
                    OverlayPathState::Deleted => None,
                };
                (path.clone(), oid)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SnapshotBase {
    pub(crate) indexed_graph_content_hash: String,
    pub(crate) indexed_head_oid: Option<String>,
    pub(crate) file_oids: BTreeMap<String, String>,
}

impl SnapshotBase {
    pub(crate) fn compatibility(file_oids: BTreeMap<String, String>) -> Self {
        let mut hasher = blake3::Hasher::new();
        for (path, oid) in &file_oids {
            update_len_prefixed(&mut hasher, path.as_bytes());
            update_len_prefixed(&mut hasher, oid.as_bytes());
        }
        Self {
            indexed_graph_content_hash: hasher.finalize().to_hex().to_string(),
            indexed_head_oid: None,
            file_oids,
        }
    }
}

#[derive(Debug, Clone)]
struct CachedSnapshot {
    graph_content_hash: String,
    indexed_head_oid: Option<String>,
    current_head_oid: String,
    index_identity: IndexIdentity,
    tracked_index: BTreeMap<String, String>,
    file_identities: BTreeMap<String, FileIdentity>,
    status_was_clean: bool,
    snapshot: OverlaySnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FileIdentity {
    Missing,
    Present(FileStamp),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified_nanos: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nanos: i64,
}

#[derive(Debug)]
struct SnapshotBuild {
    head_oid: String,
    index_identity: IndexIdentity,
    tracked_index: BTreeMap<String, String>,
    file_identities: BTreeMap<String, FileIdentity>,
    path_state: BTreeMap<String, OverlayPathState>,
    source: OverlaySnapshotSource,
    status_was_clean: bool,
}

static SNAPSHOTS: OnceLock<Mutex<HashMap<PathBuf, CachedSnapshot>>> = OnceLock::new();

fn snapshots() -> &'static Mutex<HashMap<PathBuf, CachedSnapshot>> {
    SNAPSHOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationPhase {
    AfterReadBeforeSecondStat,
    BeforeRevalidate,
}

trait SnapshotRuntime {
    fn capabilities(&self) -> FsmonitorCapabilities;

    fn rev_parse_head(&mut self, worktree: &Path) -> anyhow::Result<String> {
        crate::git::rev_parse_head(worktree)
    }

    fn index_identity(&mut self, worktree: &Path) -> anyhow::Result<IndexIdentity> {
        index_identity(worktree)
    }

    fn status_observation(&mut self, worktree: &Path) -> anyhow::Result<GitStatusObservation> {
        crate::git::status_observation(worktree, self.capabilities())
    }

    fn load_tracked_index(
        &mut self,
        worktree: &Path,
        allowed_extensions: &[&str],
    ) -> anyhow::Result<BTreeMap<String, String>> {
        load_tracked_index(worktree, allowed_extensions)
    }

    fn head_lag_paths(
        &mut self,
        worktree: &Path,
        indexed_head_oid: &str,
        current_head_oid: &str,
        allowed_extensions: &[&str],
    ) -> anyhow::Result<BTreeSet<String>> {
        head_lag_paths(
            worktree,
            indexed_head_oid,
            current_head_oid,
            allowed_extensions,
        )
    }

    fn validation_hook(&mut self, _attempt: usize, _phase: ValidationPhase, _path: &Path) {}
}

struct HookRuntime<F> {
    capabilities: FsmonitorCapabilities,
    hook: F,
}

fn no_validation_hook(_attempt: usize, _phase: ValidationPhase, _path: &Path) {}

impl<F> SnapshotRuntime for HookRuntime<F>
where
    F: FnMut(usize, ValidationPhase, &Path),
{
    fn capabilities(&self) -> FsmonitorCapabilities {
        self.capabilities
    }

    fn validation_hook(&mut self, attempt: usize, phase: ValidationPhase, path: &Path) {
        (self.hook)(attempt, phase, path);
    }
}

#[cfg(test)]
pub(crate) fn snapshot(
    worktree: &Path,
    base: SnapshotBase,
    allowed_extensions: &[&str],
) -> anyhow::Result<OverlaySnapshot> {
    snapshot_with_hook(worktree, base, allowed_extensions, |_, _, _| {})
}

pub(crate) fn snapshot_with_capabilities(
    worktree: &Path,
    base: SnapshotBase,
    allowed_extensions: &[&str],
    capabilities: FsmonitorCapabilities,
) -> anyhow::Result<OverlaySnapshot> {
    let mut runtime = HookRuntime {
        capabilities,
        hook: no_validation_hook,
    };
    snapshot_with_runtime(worktree, base, allowed_extensions, &mut runtime)
}

#[cfg(test)]
fn snapshot_with_hook<F>(
    worktree: &Path,
    base: SnapshotBase,
    allowed_extensions: &[&str],
    hook: F,
) -> anyhow::Result<OverlaySnapshot>
where
    F: FnMut(usize, ValidationPhase, &Path),
{
    let mut runtime = HookRuntime {
        capabilities: FsmonitorCapabilities {
            release_enabled: false,
            built_in_supported: false,
            local_filesystem: true,
            watcher_healthy: false,
        },
        hook,
    };
    snapshot_with_runtime(worktree, base, allowed_extensions, &mut runtime)
}

fn snapshot_with_runtime<R: SnapshotRuntime>(
    worktree: &Path,
    base: SnapshotBase,
    allowed_extensions: &[&str],
    runtime: &mut R,
) -> anyhow::Result<OverlaySnapshot> {
    let canonical_worktree = worktree.canonicalize().map_err(|error| {
        anyhow!(
            "failed to canonicalize `{}` for overlay snapshot: {error}",
            worktree.display()
        )
    })?;
    let cached = snapshots()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&canonical_worktree)
        .cloned();
    let cache_matches_graph = cached.as_ref().is_some_and(|cached| {
        cached.graph_content_hash == base.indexed_graph_content_hash
            && cached.indexed_head_oid == base.indexed_head_oid
    });
    let mut lifecycle = if cache_matches_graph {
        vec![
            SnapshotLifecycleState::Valid,
            SnapshotLifecycleState::Validating,
        ]
    } else {
        vec![
            SnapshotLifecycleState::Cold,
            SnapshotLifecycleState::Validating,
        ]
    };
    let mut measurements = SnapshotMeasurements::default();

    for attempt in 0..=1 {
        if attempt > 0 {
            lifecycle.push(SnapshotLifecycleState::Validating);
        }
        let pre_head_oid = match runtime.rev_parse_head(&canonical_worktree) {
            Ok(head_oid) => head_oid,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        let before_status_index_identity = match runtime.index_identity(&canonical_worktree) {
            Ok(identity) => identity,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        let observation = match runtime.status_observation(&canonical_worktree) {
            Ok(observation) => observation,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        // Status is allowed to refresh Git's index metadata. The consistency
        // window starts after that Git-owned refresh, not before it.
        let observed_index_identity = match runtime.index_identity(&canonical_worktree) {
            Ok(identity) => identity,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };

        if observation.entries.is_empty()
            && cache_matches_graph
            && cached.as_ref().is_some_and(|cached| {
                cached.current_head_oid == pre_head_oid
                    && cached.status_was_clean
                    && (cached.index_identity == before_status_index_identity
                        || cached.index_identity == observed_index_identity)
            })
        {
            runtime.validation_hook(
                attempt,
                ValidationPhase::BeforeRevalidate,
                &canonical_worktree,
            );
            let post_head_oid = match runtime.rev_parse_head(&canonical_worktree) {
                Ok(head_oid) => head_oid,
                Err(_) => {
                    return filesystem_oracle_fallback(
                        &canonical_worktree,
                        &base,
                        allowed_extensions,
                        lifecycle,
                        measurements,
                        runtime,
                    );
                }
            };
            let post_index_identity = match runtime.index_identity(&canonical_worktree) {
                Ok(identity) => identity,
                Err(_) => {
                    return filesystem_oracle_fallback(
                        &canonical_worktree,
                        &base,
                        allowed_extensions,
                        lifecycle,
                        measurements,
                        runtime,
                    );
                }
            };
            if pre_head_oid == post_head_oid && observed_index_identity == post_index_identity {
                let mut refreshed = cached.clone().expect("checked above");
                let mut reused = refreshed.snapshot.clone();
                reused.measurements = SnapshotMeasurements {
                    retries: measurements.retries,
                    snapshot_reused: true,
                    ..SnapshotMeasurements::default()
                };
                lifecycle.push(SnapshotLifecycleState::Valid);
                reused.lifecycle = lifecycle;
                if let Some(identity) = &mut reused.identity {
                    identity.current_head_oid = post_head_oid.clone();
                    identity.index_identity = post_index_identity.clone();
                }
                refreshed.current_head_oid = post_head_oid;
                refreshed.index_identity = post_index_identity;
                refreshed.snapshot = reused.clone();
                snapshots()
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(canonical_worktree.clone(), refreshed);
                return Ok(reused);
            }
        }

        let build = match build_snapshot_once(
            &canonical_worktree,
            &base,
            allowed_extensions,
            cached.as_ref().filter(|_| cache_matches_graph),
            pre_head_oid,
            observed_index_identity,
            &observation,
            &mut measurements,
            false,
            attempt,
            runtime,
        ) {
            Ok(build) => build,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        runtime.validation_hook(
            attempt,
            ValidationPhase::BeforeRevalidate,
            &canonical_worktree,
        );
        let post_head_oid = match runtime.rev_parse_head(&canonical_worktree) {
            Ok(head_oid) => head_oid,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        let post_index_identity = match runtime.index_identity(&canonical_worktree) {
            Ok(identity) => identity,
            Err(_) => {
                return filesystem_oracle_fallback(
                    &canonical_worktree,
                    &base,
                    allowed_extensions,
                    lifecycle,
                    measurements,
                    runtime,
                );
            }
        };
        let files_stable = build.file_identities.iter().all(|(path, identity)| {
            file_identity(&canonical_worktree.join(path)).is_ok_and(|current| &current == identity)
        });
        if build.head_oid == post_head_oid
            && build.index_identity == post_index_identity
            && files_stable
        {
            lifecycle.push(SnapshotLifecycleState::Valid);
            let mut snapshot =
                finish_snapshot(&canonical_worktree, &base, &build, lifecycle, measurements);
            if cached
                .as_ref()
                .is_some_and(|cached| cached.snapshot.identity == snapshot.identity)
                && snapshot.measurements.full_index_sweeps == 0
                && snapshot.measurements.hashed_paths.is_empty()
            {
                snapshot.measurements.snapshot_reused = true;
            }
            store_snapshot(&canonical_worktree, &base, build, &snapshot);
            return Ok(snapshot);
        }

        if attempt == 0 {
            measurements.retries = 1;
            lifecycle.push(SnapshotLifecycleState::Retrying);
            continue;
        }

        return filesystem_oracle_fallback(
            &canonical_worktree,
            &base,
            allowed_extensions,
            lifecycle,
            measurements,
            runtime,
        );
    }

    unreachable!("bounded validation loop returns on every terminal route")
}

#[allow(clippy::too_many_arguments)]
fn build_snapshot_once<R: SnapshotRuntime>(
    worktree: &Path,
    base: &SnapshotBase,
    allowed_extensions: &[&str],
    cached: Option<&CachedSnapshot>,
    head_oid: String,
    index_identity: IndexIdentity,
    observation: &GitStatusObservation,
    measurements: &mut SnapshotMeasurements,
    force_full_index: bool,
    attempt: usize,
    runtime: &mut R,
) -> anyhow::Result<SnapshotBuild> {
    let refresh_index =
        force_full_index || cached.is_none_or(|cached| cached.index_identity != index_identity);
    let tracked_index = if refresh_index {
        measurements.full_index_sweeps += 1;
        runtime.load_tracked_index(worktree, allowed_extensions)?
    } else {
        cached.expect("checked above").tracked_index.clone()
    };

    let mut candidates = BTreeSet::new();
    if let Some(cached) = cached {
        candidates.extend(cached.snapshot.path_state.keys().cloned());
    }
    if refresh_index {
        candidates.extend(
            tracked_index
                .iter()
                .filter(|(path, oid)| base.file_oids.get(*path) != Some(*oid))
                .map(|(path, _)| path.clone()),
        );
        candidates.extend(
            base.file_oids
                .keys()
                .filter(|path| !tracked_index.contains_key(*path))
                .cloned(),
        );
    }

    let mut entries = BTreeMap::<String, &PorcelainV2Entry>::new();
    let mut rename_old_paths = BTreeSet::new();
    for entry in &observation.entries {
        if supported(entry.path.as_str(), allowed_extensions) {
            candidates.insert(entry.path.clone());
            entries.insert(entry.path.clone(), entry);
        }
        if matches!(entry.kind, PorcelainV2EntryKind::Rename { .. }) {
            if let Some(old_path) = entry
                .old_path
                .as_ref()
                .filter(|path| supported(path, allowed_extensions))
            {
                candidates.insert(old_path.clone());
                rename_old_paths.insert(old_path.clone());
            }
        }
    }

    if base
        .indexed_head_oid
        .as_deref()
        .is_some_and(|indexed| indexed != head_oid)
    {
        measurements.head_lag_diffs += 1;
        candidates.extend(runtime.head_lag_paths(
            worktree,
            base.indexed_head_oid.as_deref().expect("checked above"),
            &head_oid,
            allowed_extensions,
        )?);
    }

    let mut path_state = BTreeMap::new();
    let mut file_identities = BTreeMap::new();
    for path in candidates {
        if rename_old_paths.contains(&path) && !entries.contains_key(&path) {
            file_identities.insert(path.clone(), FileIdentity::Missing);
            if base.file_oids.contains_key(&path) {
                path_state.insert(path, OverlayPathState::Deleted);
            }
            continue;
        }

        let entry = entries.get(&path).copied();
        let is_untracked = entry.is_some_and(|entry| {
            matches!(entry.kind, PorcelainV2EntryKind::Untracked)
                || matches!(entry.worktree_status, GitStatusCode::Untracked)
        });
        let observed_change = entry.is_some();
        let current = if observed_change {
            current_worktree_state(
                worktree,
                &path,
                is_untracked,
                cached,
                measurements,
                &mut file_identities,
                attempt,
                runtime,
            )?
        } else if let Some(oid) = tracked_index.get(&path) {
            Some(OverlayPathState::Tracked(oid.clone()))
        } else {
            file_identities.insert(path.clone(), FileIdentity::Missing);
            None
        };

        match current {
            Some(OverlayPathState::Tracked(oid)) => {
                if base.file_oids.get(&path) != Some(&oid) {
                    path_state.insert(path, OverlayPathState::Tracked(oid));
                }
            }
            Some(OverlayPathState::Untracked(oid)) => {
                if base.file_oids.get(&path) != Some(&oid) {
                    path_state.insert(path, OverlayPathState::Untracked(oid));
                }
            }
            Some(OverlayPathState::Deleted) => unreachable!("current state never yields deleted"),
            None => {
                if base.file_oids.contains_key(&path) {
                    path_state.insert(path, OverlayPathState::Deleted);
                }
            }
        }
    }

    Ok(SnapshotBuild {
        head_oid,
        index_identity,
        tracked_index,
        file_identities,
        path_state,
        source: source_from_observation(observation),
        status_was_clean: observation.entries.is_empty(),
    })
}

fn current_worktree_state<R: SnapshotRuntime>(
    worktree: &Path,
    path: &str,
    untracked: bool,
    cached: Option<&CachedSnapshot>,
    measurements: &mut SnapshotMeasurements,
    file_identities: &mut BTreeMap<String, FileIdentity>,
    attempt: usize,
    runtime: &mut R,
) -> anyhow::Result<Option<OverlayPathState>> {
    let absolute = worktree.join(path);
    let before = file_identity(&absolute)?;
    if matches!(before, FileIdentity::Missing) {
        file_identities.insert(path.to_owned(), before);
        return Ok(None);
    }
    if let Some(cached) = cached {
        if cached.file_identities.get(path) == Some(&before) {
            if let Some(state) = cached.snapshot.path_state.get(path) {
                file_identities.insert(path.to_owned(), before);
                return Ok(Some(match state {
                    OverlayPathState::Tracked(oid) if !untracked => {
                        OverlayPathState::Tracked(oid.clone())
                    }
                    OverlayPathState::Tracked(oid) | OverlayPathState::Untracked(oid) => {
                        if untracked {
                            OverlayPathState::Untracked(oid.clone())
                        } else {
                            OverlayPathState::Tracked(oid.clone())
                        }
                    }
                    OverlayPathState::Deleted => return Ok(None),
                }));
            }
        }
    }

    let bytes = fs::read(&absolute).with_context(|| {
        format!(
            "failed to read changed overlay path `{}`",
            absolute.display()
        )
    })?;
    runtime.validation_hook(
        attempt,
        ValidationPhase::AfterReadBeforeSecondStat,
        &absolute,
    );
    let after = file_identity(&absolute)?;
    // The OID belongs to bytes read after `before` and before `after`. Only
    // equal stamps certify that pairing; preserving `before` on mismatch makes
    // outer revalidation reject this attempt instead of blessing old bytes
    // with the newer stamp.
    file_identities.insert(
        path.to_owned(),
        if before == after { after } else { before },
    );
    measurements.hashed_paths.push(path.to_owned());
    let oid = crate::git_blob_oid(&bytes);
    Ok(Some(if untracked {
        OverlayPathState::Untracked(oid)
    } else {
        OverlayPathState::Tracked(oid)
    }))
}

fn finish_snapshot(
    worktree: &Path,
    base: &SnapshotBase,
    build: &SnapshotBuild,
    lifecycle: Vec<SnapshotLifecycleState>,
    measurements: SnapshotMeasurements,
) -> OverlaySnapshot {
    let identity = SnapshotIdentity {
        canonical_worktree: worktree.to_path_buf(),
        indexed_graph_content_hash: base.indexed_graph_content_hash.clone(),
        indexed_head_oid: base.indexed_head_oid.clone(),
        current_head_oid: build.head_oid.clone(),
        index_identity: build.index_identity.clone(),
        normalized_changed_set_fingerprint: normalized_changed_set_fingerprint(
            build.path_state.iter(),
        ),
    };
    OverlaySnapshot {
        identity: Some(identity),
        path_state: build.path_state.clone(),
        source: build.source,
        lifecycle,
        measurements,
    }
}

fn filesystem_oracle_fallback<R: SnapshotRuntime>(
    worktree: &Path,
    base: &SnapshotBase,
    allowed_extensions: &[&str],
    mut lifecycle: Vec<SnapshotLifecycleState>,
    mut measurements: SnapshotMeasurements,
    runtime: &mut R,
) -> anyhow::Result<OverlaySnapshot> {
    snapshots()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(worktree);
    lifecycle.push(SnapshotLifecycleState::ExactFallback);
    measurements.exact_fallbacks = 1;

    let current_oids = super::current_file_oids_via_fs(worktree, allowed_extensions)?;
    let tracked_index = runtime
        .load_tracked_index(worktree, allowed_extensions)
        .unwrap_or_default();
    let changed = super::overlay_changed_oid_hex_from_maps(base.file_oids.clone(), current_oids)?;
    let path_state = changed
        .into_iter()
        .map(|(path, oid)| {
            let state = match oid {
                Some(oid)
                    if tracked_index.contains_key(&path) || base.file_oids.contains_key(&path) =>
                {
                    OverlayPathState::Tracked(oid)
                }
                Some(oid) => OverlayPathState::Untracked(oid),
                None => OverlayPathState::Deleted,
            };
            (path, state)
        })
        .collect();

    lifecycle.push(SnapshotLifecycleState::Valid);
    Ok(OverlaySnapshot {
        // A Git failure or repeated consistency race cannot form the complete
        // stable cache identity. Return the fresh oracle result without making
        // it eligible for snapshot reuse.
        identity: None,
        path_state,
        source: OverlaySnapshotSource::ExactFallback,
        lifecycle,
        measurements,
    })
}

fn store_snapshot(
    worktree: &Path,
    base: &SnapshotBase,
    build: SnapshotBuild,
    snapshot: &OverlaySnapshot,
) {
    snapshots()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            worktree.to_path_buf(),
            CachedSnapshot {
                graph_content_hash: base.indexed_graph_content_hash.clone(),
                indexed_head_oid: base.indexed_head_oid.clone(),
                current_head_oid: build.head_oid,
                index_identity: build.index_identity,
                tracked_index: build.tracked_index,
                file_identities: build.file_identities,
                status_was_clean: build.status_was_clean,
                snapshot: snapshot.clone(),
            },
        );
}

fn load_tracked_index(
    worktree: &Path,
    allowed_extensions: &[&str],
) -> anyhow::Result<BTreeMap<String, String>> {
    Ok(crate::git::ls_files_with_oids(worktree)?
        .into_iter()
        .filter(|entry| !entry.is_gitlink && supported(&entry.path, allowed_extensions))
        .map(|entry| (entry.path, entry.content_oid))
        .collect())
}

fn supported(path: &str, allowed_extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            allowed_extensions
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn head_lag_paths(
    worktree: &Path,
    indexed_head_oid: &str,
    current_head_oid: &str,
    allowed_extensions: &[&str],
) -> anyhow::Result<BTreeSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "diff",
            "--name-status",
            "-z",
            indexed_head_oid,
            current_head_oid,
        ])
        .output()
        .with_context(|| {
            format!(
                "failed to run conditional HEAD-lag diff in `{}`",
                worktree.display()
            )
        })?;
    if !output.status.success() {
        return Err(anyhow!(
            "conditional HEAD-lag diff failed in `{}`: {}",
            worktree.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let records = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut paths = BTreeSet::new();
    let mut index = 0;
    while index < records.len() {
        let status = std::str::from_utf8(records[index])
            .context("HEAD-lag diff emitted non-UTF-8 status")?;
        index += 1;
        let path_count = if status.starts_with('R') || status.starts_with('C') {
            2
        } else {
            1
        };
        for _ in 0..path_count {
            let path = records
                .get(index)
                .ok_or_else(|| anyhow!("malformed HEAD-lag diff after status `{status}`"))?;
            index += 1;
            let path = std::str::from_utf8(path).context("HEAD-lag diff emitted non-UTF-8 path")?;
            if supported(path, allowed_extensions) {
                paths.insert(path.to_owned());
            }
        }
    }
    Ok(paths)
}

fn file_identity(path: &Path) -> anyhow::Result<FileIdentity> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return Ok(FileIdentity::Missing),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => {
            return Ok(FileIdentity::Missing)
        }
        Err(error) => {
            return Err(anyhow!("failed to stat `{}`: {error}", path.display()));
        }
    };
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(FileIdentity::Present(FileStamp {
            len: metadata.len(),
            modified_nanos,
            device: metadata.dev(),
            inode: metadata.ino(),
            ctime: metadata.ctime(),
            ctime_nanos: metadata.ctime_nsec(),
        }))
    }
    #[cfg(not(unix))]
    {
        Ok(FileIdentity::Present(FileStamp {
            len: metadata.len(),
            modified_nanos,
        }))
    }
}

pub(crate) fn normalized_changed_set_fingerprint<'a>(
    entries: impl IntoIterator<Item = (&'a String, &'a OverlayPathState)>,
) -> [u8; 32] {
    let mut normalized = entries.into_iter().collect::<Vec<_>>();
    normalized.sort_by(|left, right| left.0.cmp(right.0).then_with(|| left.1.cmp(right.1)));
    let mut hasher = blake3::Hasher::new();
    for (path, state) in normalized {
        update_len_prefixed(&mut hasher, path.as_bytes());
        match state {
            OverlayPathState::Tracked(oid) => {
                hasher.update(&[0]);
                update_len_prefixed(&mut hasher, oid.as_bytes());
            }
            OverlayPathState::Untracked(oid) => {
                hasher.update(&[1]);
                update_len_prefixed(&mut hasher, oid.as_bytes());
            }
            OverlayPathState::Deleted => {
                hasher.update(&[2]);
            }
        }
    }
    *hasher.finalize().as_bytes()
}

fn update_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
fn exact_status_observation(root: &Path) -> anyhow::Result<GitStatusObservation> {
    crate::git::status_observation(
        root,
        FsmonitorCapabilities {
            release_enabled: false,
            built_in_supported: false,
            local_filesystem: true,
            watcher_healthy: false,
        },
    )
}

fn source_from_observation(observation: &GitStatusObservation) -> OverlaySnapshotSource {
    match observation.source {
        FsmonitorStatusRoute::FsmonitorNative => OverlaySnapshotSource::FsmonitorNative,
        FsmonitorStatusRoute::ExactFallback(_) => OverlaySnapshotSource::ExactFallback,
    }
}

fn index_identity(root: &Path) -> anyhow::Result<IndexIdentity> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
        .output()
        .with_context(|| format!("failed to resolve index path in `{}`", root.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "failed to resolve index path in `{}`: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let path = PathBuf::from(
        std::str::from_utf8(&output.stdout)
            .context("git index path was not UTF-8")?
            .trim_end(),
    );
    let metadata = fs::metadata(&path)
        .with_context(|| format!("failed to stat git index `{}`", path.display()))?;
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(IndexIdentity {
            path,
            len: metadata.len(),
            modified_nanos,
            device: metadata.dev(),
            inode: metadata.ino(),
            ctime: metadata.ctime(),
            ctime_nanos: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Ok(IndexIdentity {
            path,
            len: metadata.len(),
            modified_nanos,
        })
    }
}

#[cfg(test)]
fn snapshot_with_validation_hook<F>(
    worktree: &Path,
    base: SnapshotBase,
    allowed_extensions: &[&str],
    hook: F,
) -> anyhow::Result<OverlaySnapshot>
where
    F: FnMut(usize, ValidationPhase, &Path),
{
    snapshot_with_hook(worktree, base, allowed_extensions, hook)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::git_blob_oid;

    fn init_repo(root: &Path) {
        run_git(root, &["init", "-q"]);
        run_git(root, &["config", "user.email", "snapshot@example.com"]);
        run_git(root, &["config", "user.name", "Snapshot Test"]);
        fs::create_dir_all(root.join("src")).expect("src dir");
    }

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn head(root: &Path) -> String {
        crate::git::rev_parse_head(root).expect("HEAD")
    }

    fn supported_extensions() -> Vec<&'static str> {
        crate::extract::languages::all_supported_extensions()
    }

    fn base(root: &Path, indexed_head_oid: Option<String>) -> SnapshotBase {
        let extensions = supported_extensions();
        let file_oids = crate::git::ls_files_with_oids(root)
            .expect("tracked files")
            .into_iter()
            .filter(|entry| {
                !entry.is_gitlink
                    && super::super::overlay_path_has_supported_extension(&entry.path, &extensions)
            })
            .map(|entry| (entry.path, entry.content_oid))
            .collect::<BTreeMap<_, _>>();
        SnapshotBase {
            indexed_graph_content_hash: format!("test-{}", file_oids.len()),
            indexed_head_oid,
            file_oids,
        }
    }

    fn exact_path_state(root: &Path, base: &SnapshotBase) -> BTreeMap<String, OverlayPathState> {
        let extensions = supported_extensions();
        let observation = exact_status_observation(root).expect("exact status");
        let untracked = observation
            .entries
            .iter()
            .filter(|entry| matches!(entry.kind, PorcelainV2EntryKind::Untracked))
            .map(|entry| entry.path.as_str())
            .collect::<BTreeSet<_>>();
        let current =
            super::super::current_file_oids_via_git(root, &extensions).expect("fresh full scan");
        super::super::overlay_changed_oid_hex_from_maps(base.file_oids.clone(), current)
            .expect("exact delta")
            .into_iter()
            .map(|(path, oid)| {
                let state = match oid {
                    Some(oid) if untracked.contains(path.as_str()) => {
                        OverlayPathState::Untracked(oid)
                    }
                    Some(oid) => OverlayPathState::Tracked(oid),
                    None => OverlayPathState::Deleted,
                };
                (path, state)
            })
            .collect()
    }

    fn exact_changed_oid_hex_via_fs(
        root: &Path,
        base: &SnapshotBase,
    ) -> BTreeMap<String, Option<String>> {
        let canonical = root.canonicalize().expect("canonical worktree");
        let current = super::super::current_file_oids_via_fs(&canonical, &supported_extensions())
            .expect("fresh filesystem oracle");
        super::super::overlay_changed_oid_hex_from_maps(base.file_oids.clone(), current)
            .expect("exact filesystem delta")
    }

    struct InstrumentedRuntime {
        capabilities: FsmonitorCapabilities,
        commands: Vec<&'static str>,
        fail_status: bool,
        synthetic_status_source: bool,
        observed_capabilities: Vec<FsmonitorCapabilities>,
    }

    impl InstrumentedRuntime {
        fn release_disabled() -> Self {
            Self {
                capabilities: FsmonitorCapabilities {
                    release_enabled: false,
                    built_in_supported: true,
                    local_filesystem: true,
                    watcher_healthy: true,
                },
                commands: Vec::new(),
                fail_status: false,
                synthetic_status_source: false,
                observed_capabilities: Vec::new(),
            }
        }
    }

    impl SnapshotRuntime for InstrumentedRuntime {
        fn capabilities(&self) -> FsmonitorCapabilities {
            self.capabilities
        }

        fn rev_parse_head(&mut self, worktree: &Path) -> anyhow::Result<String> {
            self.commands.push("git rev-parse HEAD");
            crate::git::rev_parse_head(worktree)
        }

        fn index_identity(&mut self, worktree: &Path) -> anyhow::Result<IndexIdentity> {
            self.commands.push("git rev-parse --git-path index");
            super::index_identity(worktree)
        }

        fn status_observation(&mut self, worktree: &Path) -> anyhow::Result<GitStatusObservation> {
            self.commands.push("git status --porcelain=v2 -z");
            self.observed_capabilities.push(self.capabilities);
            if self.fail_status {
                return Err(anyhow!("injected git status failure"));
            }
            if self.synthetic_status_source {
                let mut observation = exact_status_observation(worktree)?;
                observation.source = crate::git::fsmonitor_status_route(self.capabilities);
                return Ok(observation);
            }
            crate::git::status_observation(worktree, self.capabilities)
        }

        fn load_tracked_index(
            &mut self,
            worktree: &Path,
            allowed_extensions: &[&str],
        ) -> anyhow::Result<BTreeMap<String, String>> {
            self.commands.push("git ls-files --stage -z");
            super::load_tracked_index(worktree, allowed_extensions)
        }

        fn head_lag_paths(
            &mut self,
            worktree: &Path,
            indexed_head_oid: &str,
            current_head_oid: &str,
            allowed_extensions: &[&str],
        ) -> anyhow::Result<BTreeSet<String>> {
            self.commands.push("git diff --name-status -z");
            super::head_lag_paths(
                worktree,
                indexed_head_oid,
                current_head_oid,
                allowed_extensions,
            )
        }
    }

    #[test]
    fn overlay_snapshot_rename_with_recreated_source_preserves_both_paths() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        let renamed = b"pub fn renamed() {}\n";
        let recreated = b"pub fn recreated() {}\n";
        fs::write(root.join("src/old.rs"), renamed).expect("old source");
        run_git(root, &["add", "src/old.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));

        run_git(root, &["mv", "src/old.rs", "src/new.rs"]);
        fs::write(root.join("src/old.rs"), recreated).expect("recreated source");

        let actual = snapshot(root, base.clone(), &supported_extensions()).expect("snapshot");
        assert_eq!(actual.path_state, exact_path_state(root, &base));
        assert_eq!(
            actual.path_state.get("src/new.rs"),
            Some(&OverlayPathState::Tracked(git_blob_oid(renamed)))
        );
        assert_eq!(
            actual.path_state.get("src/old.rs"),
            Some(&OverlayPathState::Untracked(git_blob_oid(recreated)))
        );
    }

    #[test]
    fn overlay_snapshot_read_stat_race_retries_before_serving_new_stamp() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn v1() {}\n").expect("v1");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");
        fs::write(root.join("src/lib.rs"), "pub fn v2() {}\n").expect("v2");

        let actual = snapshot_with_validation_hook(
            root,
            base.clone(),
            &extensions,
            |attempt, phase, path| {
                if attempt == 0
                    && phase == ValidationPhase::AfterReadBeforeSecondStat
                    && path.ends_with("lib.rs")
                {
                    fs::write(path, "pub fn v3() {}\n").expect("mutate after read");
                }
            },
        )
        .expect("single read/stat race");

        assert_eq!(actual.measurements.retries, 1);
        assert_eq!(actual.measurements.exact_fallbacks, 0);
        assert_eq!(actual.path_state, exact_path_state(root, &base));
        assert_eq!(
            actual.lifecycle,
            vec![
                SnapshotLifecycleState::Valid,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Retrying,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Valid,
            ]
        );
    }

    #[test]
    fn overlay_snapshot_repeated_read_stat_race_uses_fresh_filesystem_oracle() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn v1() {}\n").expect("v1");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");
        fs::write(root.join("src/lib.rs"), "pub fn v2() {}\n").expect("v2");

        let actual = snapshot_with_validation_hook(
            root,
            base.clone(),
            &extensions,
            |attempt, phase, path| {
                if phase == ValidationPhase::AfterReadBeforeSecondStat && path.ends_with("lib.rs") {
                    let body = if attempt == 0 {
                        "pub fn v3() {}\n"
                    } else {
                        "pub fn v4() {}\n"
                    };
                    fs::write(path, body).expect("repeat mutation after read");
                }
            },
        )
        .expect("repeated read/stat race");

        assert_eq!(actual.measurements.retries, 1);
        assert_eq!(actual.measurements.exact_fallbacks, 1);
        assert_eq!(
            actual.changed_oid_hex(),
            exact_changed_oid_hex_via_fs(root, &base)
        );
        assert_eq!(
            actual.lifecycle,
            vec![
                SnapshotLifecycleState::Valid,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Retrying,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::ExactFallback,
                SnapshotLifecycleState::Valid,
            ]
        );
    }

    #[test]
    fn overlay_snapshot_invalid_indexed_head_uses_fresh_filesystem_oracle() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn current() {}\n").expect("source");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let mut base = base(root, None);
        base.indexed_head_oid = Some("0000000000000000000000000000000000000000".to_owned());

        let actual = snapshot(root, base.clone(), &supported_extensions())
            .expect("invalid indexed HEAD must use exact fallback");
        assert_eq!(actual.source, OverlaySnapshotSource::ExactFallback);
        assert_eq!(
            actual.changed_oid_hex(),
            exact_changed_oid_hex_via_fs(root, &base)
        );
        assert_eq!(
            actual.lifecycle,
            vec![
                SnapshotLifecycleState::Cold,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::ExactFallback,
                SnapshotLifecycleState::Valid,
            ]
        );
    }

    #[test]
    fn overlay_snapshot_status_failure_uses_fresh_filesystem_oracle() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn v1() {}\n").expect("v1");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let mut runtime = InstrumentedRuntime::release_disabled();
        snapshot_with_runtime(root, base.clone(), &supported_extensions(), &mut runtime)
            .expect("seed stable cache");
        fs::write(root.join("src/lib.rs"), "pub fn v2() {}\n").expect("v2");
        runtime.fail_status = true;

        let actual =
            snapshot_with_runtime(root, base.clone(), &supported_extensions(), &mut runtime)
                .expect("status failure must use exact fallback");
        assert_eq!(actual.source, OverlaySnapshotSource::ExactFallback);
        assert_eq!(
            actual.identity, None,
            "unstable fallback must not form a cache key"
        );
        assert!(!snapshots()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&root.canonicalize().expect("canonical worktree")));
        assert_eq!(
            actual.changed_oid_hex(),
            exact_changed_oid_hex_via_fs(root, &base)
        );
        assert_eq!(
            actual.lifecycle,
            vec![
                SnapshotLifecycleState::Valid,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::ExactFallback,
                SnapshotLifecycleState::Valid,
            ]
        );

        runtime.fail_status = false;
        let recovered = snapshot_with_runtime(root, base, &supported_extensions(), &mut runtime)
            .expect("recovered Git observation");
        assert!(!recovered.measurements.snapshot_reused);
        assert_eq!(recovered.measurements.full_index_sweeps, 1);
    }

    #[test]
    fn overlay_snapshot_task2_capability_seam_routes_without_enabling_production() {
        for (capabilities, expected) in [
            (
                FsmonitorCapabilities {
                    release_enabled: true,
                    built_in_supported: true,
                    local_filesystem: true,
                    watcher_healthy: true,
                },
                OverlaySnapshotSource::FsmonitorNative,
            ),
            (
                FsmonitorCapabilities {
                    release_enabled: false,
                    built_in_supported: true,
                    local_filesystem: true,
                    watcher_healthy: true,
                },
                OverlaySnapshotSource::ExactFallback,
            ),
            (
                FsmonitorCapabilities {
                    release_enabled: true,
                    built_in_supported: true,
                    local_filesystem: true,
                    watcher_healthy: false,
                },
                OverlaySnapshotSource::ExactFallback,
            ),
        ] {
            let tempdir = tempfile::tempdir().expect("tempdir");
            let root = tempdir.path();
            init_repo(root);
            fs::write(root.join("src/lib.rs"), "pub fn clean() {}\n").expect("source");
            run_git(root, &["add", "src/lib.rs"]);
            run_git(root, &["commit", "-qm", "base"]);
            let base = base(root, Some(head(root)));
            let mut runtime = InstrumentedRuntime {
                capabilities,
                synthetic_status_source: true,
                ..InstrumentedRuntime::release_disabled()
            };

            let actual = snapshot_with_runtime(root, base, &supported_extensions(), &mut runtime)
                .expect("routed snapshot");
            assert_eq!(actual.source, expected);
            assert_eq!(runtime.observed_capabilities, vec![capabilities]);
        }

        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn production() {}\n").expect("source");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let production = snapshot(root, base(root, Some(head(root))), &supported_extensions())
            .expect("production snapshot");
        assert_eq!(production.source, OverlaySnapshotSource::ExactFallback);
    }

    #[test]
    fn overlay_snapshot_warm_command_trace_has_no_ls_files_sweep() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn clean() {}\n").expect("source");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        let mut runtime = InstrumentedRuntime::release_disabled();
        snapshot_with_runtime(root, base.clone(), &extensions, &mut runtime)
            .expect("cold snapshot");
        runtime.commands.clear();

        let warm =
            snapshot_with_runtime(root, base, &extensions, &mut runtime).expect("warm snapshot");
        eprintln!("warm Git command trace: {:?}", runtime.commands);
        assert_eq!(
            runtime.commands,
            vec![
                "git rev-parse HEAD",
                "git rev-parse --git-path index",
                "git status --porcelain=v2 -z",
                "git rev-parse --git-path index",
                "git rev-parse HEAD",
                "git rev-parse --git-path index",
            ]
        );
        assert!(warm.measurements.snapshot_reused);
        assert!(!runtime
            .commands
            .iter()
            .any(|command| command.contains("ls-files")));
    }

    #[test]
    fn overlay_snapshot_hashes_only_staged_unstaged_and_untracked_paths() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        for name in ["clean.rs", "staged.rs", "unstaged.rs"] {
            fs::write(
                root.join("src").join(name),
                format!("pub fn {}() {{}}\n", name.replace('.', "_")),
            )
            .expect("source");
        }
        run_git(root, &["add", "src"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");

        fs::write(root.join("src/staged.rs"), "pub fn staged_v2() {}\n").expect("staged");
        run_git(root, &["add", "src/staged.rs"]);
        fs::write(root.join("src/unstaged.rs"), "pub fn unstaged_v2() {}\n").expect("unstaged");
        fs::write(root.join("src/new.rs"), "pub fn new_file() {}\n").expect("untracked");

        let actual = snapshot(root, base.clone(), &extensions).expect("changed snapshot");
        assert_eq!(actual.path_state, exact_path_state(root, &base));
        assert_eq!(
            actual.measurements.hashed_paths,
            vec!["src/new.rs", "src/staged.rs", "src/unstaged.rs"],
            "only changed supported worktree paths may be content-hashed"
        );
        assert!(
            !actual
                .measurements
                .hashed_paths
                .contains(&"src/clean.rs".to_owned()),
            "clean tracked paths must reuse Git/index OIDs"
        );
    }

    #[test]
    fn overlay_snapshot_emits_delete_and_rename_path_states() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/delete.rs"), "pub fn delete_me() {}\n").expect("delete");
        fs::write(root.join("src/old.rs"), "pub fn renamed() {}\n").expect("rename");
        run_git(root, &["add", "src"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");

        fs::remove_file(root.join("src/delete.rs")).expect("delete file");
        run_git(root, &["mv", "src/old.rs", "src/new.rs"]);

        let actual = snapshot(root, base.clone(), &extensions).expect("changed snapshot");
        assert_eq!(actual.path_state, exact_path_state(root, &base));
        assert_eq!(
            actual.path_state.get("src/delete.rs"),
            Some(&OverlayPathState::Deleted)
        );
        assert_eq!(
            actual.path_state.get("src/old.rs"),
            Some(&OverlayPathState::Deleted)
        );
        assert_eq!(
            actual.path_state.get("src/new.rs"),
            Some(&OverlayPathState::Tracked(git_blob_oid(
                b"pub fn renamed() {}\n"
            )))
        );
    }

    #[test]
    fn overlay_snapshot_runs_conditional_head_lag_delta() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn v1() {}\n").expect("v1");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "v1"]);
        let indexed_head = head(root);
        let base = base(root, Some(indexed_head));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");

        fs::write(root.join("src/lib.rs"), "pub fn v2() {}\n").expect("v2");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "v2"]);
        assert!(crate::git::status_dirty_paths(root)
            .expect("status")
            .is_empty());

        let actual = snapshot(root, base.clone(), &extensions).expect("head-lag snapshot");
        assert_eq!(actual.path_state, exact_path_state(root, &base));
        assert_eq!(actual.measurements.head_lag_diffs, 1);
        assert_eq!(
            actual.path_state.get("src/lib.rs"),
            Some(&OverlayPathState::Tracked(git_blob_oid(
                b"pub fn v2() {}\n"
            )))
        );
    }

    #[test]
    fn overlay_snapshot_fingerprint_is_order_independent() {
        let tracked =
            OverlayPathState::Tracked("1111111111111111111111111111111111111111".to_owned());
        let untracked =
            OverlayPathState::Untracked("2222222222222222222222222222222222222222".to_owned());
        let first = vec![
            ("src/a.rs".to_owned(), tracked.clone()),
            ("src/b.rs".to_owned(), OverlayPathState::Deleted),
            ("src/c.rs".to_owned(), untracked.clone()),
        ];
        let second = vec![
            ("src/c.rs".to_owned(), untracked),
            ("src/a.rs".to_owned(), tracked),
            ("src/b.rs".to_owned(), OverlayPathState::Deleted),
        ];
        assert_eq!(
            normalized_changed_set_fingerprint(first.iter().map(|(path, state)| (path, state))),
            normalized_changed_set_fingerprint(second.iter().map(|(path, state)| (path, state)))
        );
    }

    #[test]
    fn overlay_snapshot_retries_once_then_uses_exact_fallback() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        init_repo(root);
        fs::write(root.join("src/lib.rs"), "pub fn v1() {}\n").expect("v1");
        run_git(root, &["add", "src/lib.rs"]);
        run_git(root, &["commit", "-qm", "base"]);
        let base = base(root, Some(head(root)));
        let extensions = supported_extensions();
        snapshot(root, base.clone(), &extensions).expect("cold snapshot");

        fs::write(root.join("src/lib.rs"), "pub fn v2() {}\n").expect("v2");
        let single = snapshot_with_validation_hook(
            root,
            base.clone(),
            &extensions,
            |attempt, phase, root| {
                if attempt == 0 && phase == ValidationPhase::BeforeRevalidate {
                    fs::write(root.join("src/lib.rs"), "pub fn v3() {}\n").expect("v3");
                    run_git(root, &["add", "src/lib.rs"]);
                    run_git(root, &["commit", "-qm", "validation race"]);
                }
            },
        )
        .expect("single-race snapshot");
        assert_eq!(single.measurements.retries, 1);
        assert_eq!(single.measurements.exact_fallbacks, 0);
        assert_eq!(single.path_state, exact_path_state(root, &base));
        assert_eq!(
            single.lifecycle,
            vec![
                SnapshotLifecycleState::Valid,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Retrying,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Valid,
            ]
        );

        fs::write(root.join("src/lib.rs"), "pub fn v4() {}\n").expect("v4");
        let repeated = snapshot_with_validation_hook(
            root,
            base.clone(),
            &extensions,
            |attempt, phase, root| {
                if phase == ValidationPhase::BeforeRevalidate {
                    let body = if attempt == 0 {
                        "pub fn v5() {}\n"
                    } else {
                        "pub fn v6() {}\n"
                    };
                    fs::write(root.join("src/lib.rs"), body).expect("race mutation");
                    run_git(root, &["add", "src/lib.rs"]);
                }
            },
        )
        .expect("repeated-race snapshot");
        assert_eq!(repeated.measurements.retries, 1);
        assert_eq!(repeated.measurements.exact_fallbacks, 1);
        assert_eq!(repeated.path_state, exact_path_state(root, &base));
        assert_eq!(
            repeated.lifecycle,
            vec![
                SnapshotLifecycleState::Valid,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::Retrying,
                SnapshotLifecycleState::Validating,
                SnapshotLifecycleState::ExactFallback,
                SnapshotLifecycleState::Valid,
            ]
        );
    }
}
