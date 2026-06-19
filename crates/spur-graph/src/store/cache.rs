use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};

use crate::locking::try_lock_exclusive_with_timeout;
use crate::store::build::BuildStats;
use crate::store::lance_sections::{
    write_sections_dataset_with_sidecar_options_and_progress, SectionSidecarOptions,
    SectionSidecarProgressCallback, SectionSidecarProgressEvent, CODE_SYMBOLS_DATASET_DIR,
    SECTIONS_DATASET_DIR,
};
use crate::store::pointer::resolve_artifact_location;
use crate::store::{
    read_artifact_header_parquet, read_artifact_parquet, write_artifact_parquet,
    write_current_pointer, ArtifactStagingDir, GraphArtifactSidecarStatus, WriteOptions,
};
use crate::{git, git::GitCtx, GraphIndexArtifact, GraphIndexPointer, SourceKind};

const CACHE_DIR_NAME: &str = "spur-graph";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const LOCK_FILE_NAME: &str = ".lock";
const WORKTREE_ARTIFACT_PATH: &str = ".spur/graph";
const POINTER_PATH: &str = ".spur/graph-index.pointer.json";
pub const COMMIT_INDEX_POINTER_PATH: &str = ".spur/commit-index.pointer.json";
const POINTER_SCHEMA: &str = "spur-graph-pointer-v1";
// LRU cap for in-process GraphIndexArtifact reuse. Bumped from 4 to 64 when the
// Lance section sidecar landed (s1): write_sections_dataset re-loads the artifact
// per finalization to compute incremental row diffs, and the prior cap evicted
// before adjacent writes could reuse it.
const BASE_ARTIFACT_CACHE_CAP: usize = 64;

#[cfg(test)]
static LOCK_TIMEOUT_MS_OVERRIDE: AtomicU64 = AtomicU64::new(5_000);

static BASE_ARTIFACT_CACHE: OnceLock<Mutex<BaseArtifactCache>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BaseArtifactCacheKey {
    common_dir: PathBuf,
    manifest_version: String,
    graph_content_hash: String,
}

#[derive(Default)]
struct BaseArtifactCache {
    artifacts: HashMap<BaseArtifactCacheKey, Arc<GraphIndexArtifact>>,
    order: VecDeque<BaseArtifactCacheKey>,
}

#[derive(Debug, Clone)]
pub struct BaseArtifactSeed {
    pub base: &'static str,
    pub artifact: Arc<GraphIndexArtifact>,
    pub indexed_commit_oid: Option<String>,
}

pub fn write_with_dedup(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    ctx: &GitCtx,
) -> Result<()> {
    write_with_dedup_with_section_sidecar_options(
        artifact,
        worktree_root,
        ctx,
        SectionSidecarOptions::from_env(),
        None,
    )
}

pub fn write_with_dedup_with_section_sidecar_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    ctx: &GitCtx,
    mut section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<()> {
    // Resolve the current pointer BEFORE writing — it still points to the
    // previous artifact at this moment.  Use it as the carry-forward source.
    // Best-effort: any failure → None, never an error.
    if section_sidecar_options.previous_artifact_dir.is_none() {
        let prev_dir = resolve_artifact_location(worktree_root, None)
            .ok()
            .map(|r| r.path);
        section_sidecar_options.previous_artifact_dir = prev_dir;
    }

    let canonical_dir = canonical_base_dir(&ctx.git_common_dir, &artifact.manifest_version);
    let canonical = canonical_path(
        &ctx.git_common_dir,
        &artifact.manifest_version,
        &artifact.graph_content_hash,
    );
    fs::create_dir_all(&canonical_dir)
        .with_context(|| format!("failed to create `{}`", canonical_dir.display()))?;

    let lock_path = canonical_dir.join(LOCK_FILE_NAME);
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open `{}`", lock_path.display()))?;

    if !try_lock_exclusive_with_timeout(&lock, lock_timeout())? {
        tracing::warn!(
            lock_path = %lock_path.display(),
            "spur-graph: fs2 lock unavailable; writing worktree artifact without canonical pointer"
        );
        let written_dir = write_artifact_to_worktree(
            artifact,
            worktree_root,
            section_sidecar_options,
            section_sidecar_progress,
        )?;
        write_current_pointer(worktree_root, &written_dir)?;
        // No canonical was written, so any prior pointer is stale; remove it.
        remove_if_exists(&worktree_root.join(POINTER_PATH))?;
        return Ok(());
    }

    let write_result = if canonical.join("manifest.json").is_file() {
        repair_canonical_sidecar_if_incomplete(
            worktree_root,
            &canonical,
            section_sidecar_options,
            section_sidecar_progress,
        )?;
        Ok(canonical)
    } else {
        write_canonical_atomically(
            artifact,
            worktree_root,
            &canonical_dir,
            section_sidecar_options,
            section_sidecar_progress,
        )
    };
    let unlock_result = fs2::FileExt::unlock(&lock).context("failed to unlock graph cache lock");
    let written_dir = write_result?;
    unlock_result?;

    write_current_pointer(worktree_root, &written_dir)?;
    write_pointer(artifact, worktree_root, ctx, &written_dir)?;
    Ok(())
}

pub fn lookup_canonical(common_dir: &Path, manifest_version: &str, hash: &str) -> Option<PathBuf> {
    let path = canonical_path(common_dir, manifest_version, hash);
    path.exists().then_some(path)
}

pub fn load_base_artifact_for_worktree(worktree_root: &Path) -> Option<Arc<GraphIndexArtifact>> {
    load_base_seed_for_worktree(worktree_root).map(|seed| seed.artifact)
}

pub fn load_base_seed_for_worktree(worktree_root: &Path) -> Option<BaseArtifactSeed> {
    let worktree_root = match worktree_root.canonicalize() {
        Ok(root) => root,
        Err(error) => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                path = %worktree_root.display(),
                error = %error,
                "spur-graph: skipping base artifact; worktree root is not canonicalizable"
            );
            emit_base_seed_stats("none", BuildStats::default());
            return None;
        }
    };

    if let Some(seed) = load_pointer_artifact(&worktree_root, &worktree_root, "self_pointer") {
        emit_base_seed_selection("self_pointer");
        return Some(seed);
    }

    if let Some(main_root) = main_worktree_root(&worktree_root) {
        if main_root != worktree_root {
            if let Some(seed) = load_pointer_artifact(&worktree_root, &main_root, "main_worktree") {
                emit_base_seed_selection("main_worktree");
                return Some(seed);
            }
        }
    }

    emit_base_seed_stats("none", BuildStats::default());
    None
}

fn canonical_base_dir(common_dir: &Path, manifest_version: &str) -> PathBuf {
    common_dir
        .join(CACHE_DIR_NAME)
        .join(ARTIFACTS_DIR_NAME)
        .join(manifest_version)
}

fn canonical_path(common_dir: &Path, manifest_version: &str, hash: &str) -> PathBuf {
    canonical_base_dir(common_dir, manifest_version).join(format!("{hash}.parquet"))
}

fn write_canonical_atomically(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    canonical_dir: &Path,
    section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<PathBuf> {
    let staging = ArtifactStagingDir::new(canonical_dir, &artifact.graph_content_hash)?;
    write_artifact_parquet(
        artifact,
        staging.path(),
        WriteOptions::default(),
        Vec::new(),
    )?;
    let final_path = staging.commit()?;
    write_sidecar_and_stamp_best_effort(
        artifact,
        worktree_root,
        &final_path,
        section_sidecar_options,
        section_sidecar_progress,
    );
    Ok(final_path)
}

fn repair_canonical_sidecar_if_incomplete(
    worktree_root: &Path,
    canonical: &Path,
    section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<()> {
    let manifest = read_artifact_header_parquet(canonical).with_context(|| {
        format!(
            "failed to read canonical manifest `{}`",
            canonical.display()
        )
    })?;
    if manifest.sidecar_complete {
        return Ok(());
    }

    let artifact = read_artifact_parquet(canonical).with_context(|| {
        format!(
            "failed to load canonical Parquet artifact for sidecar repair `{}`",
            canonical.display()
        )
    })?;
    write_sidecar_and_stamp_best_effort(
        &artifact,
        worktree_root,
        canonical,
        section_sidecar_options,
        section_sidecar_progress,
    );
    Ok(())
}

fn load_pointer_artifact(
    worktree_root: &Path,
    pointer_root: &Path,
    source: &'static str,
) -> Option<BaseArtifactSeed> {
    let pointer = read_graph_index_pointer(pointer_root);
    let resolved = match resolve_artifact_location(pointer_root, None) {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                source,
                path = %pointer_root.display(),
                error = %error,
                "spur-graph: base artifact location unavailable"
            );
            return None;
        }
    };
    let ctx = match git::detect(worktree_root) {
        Some(ctx) => ctx,
        None => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                source,
                worktree = %worktree_root.display(),
                "spur-graph: base artifact unavailable outside git worktree"
            );
            return None;
        }
    };
    let common_dir = ctx
        .git_common_dir
        .canonicalize()
        .unwrap_or_else(|_| ctx.git_common_dir.clone());
    let artifact = read_resolved_base_artifact(common_dir, &resolved.path, source)?;
    let indexed_commit_oid = pointer
        .filter(|pointer| {
            pointer.graph_content_hash == artifact.graph_content_hash
                && pointer.manifest_version == artifact.manifest_version
        })
        .and_then(|pointer| pointer.indexed_commit_oid);
    Some(BaseArtifactSeed {
        base: source,
        artifact,
        indexed_commit_oid,
    })
}

fn read_graph_index_pointer(pointer_root: &Path) -> Option<GraphIndexPointer> {
    let bytes = fs::read(pointer_root.join(POINTER_PATH)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn read_resolved_base_artifact(
    common_dir: PathBuf,
    path: &Path,
    source: &'static str,
) -> Option<Arc<GraphIndexArtifact>> {
    let manifest = match read_artifact_header_parquet(path) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                source,
                path = %path.display(),
                error = %error,
                "spur-graph: base artifact manifest unreadable"
            );
            return None;
        }
    };
    let key = BaseArtifactCacheKey {
        common_dir,
        manifest_version: manifest.manifest_version,
        graph_content_hash: manifest.graph_content_hash,
    };
    read_cached_base_artifact(key, path, source)
}

fn read_cached_base_artifact(
    key: BaseArtifactCacheKey,
    canonical: &Path,
    source: &'static str,
) -> Option<Arc<GraphIndexArtifact>> {
    if let Some(artifact) = base_artifact_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.artifacts.get(&key).cloned())
    {
        return Some(artifact);
    }

    let artifact = match read_artifact_parquet(canonical) {
        Ok(artifact) => artifact,
        Err(error) => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                source,
                path = %canonical.display(),
                error = %error,
                "spur-graph: canonical base artifact unreadable"
            );
            return None;
        }
    };
    if artifact.graph_content_hash != key.graph_content_hash
        || artifact.manifest_version != key.manifest_version
    {
        tracing::debug!(
            target: "spur_graph::base_seed",
            source,
            path = %canonical.display(),
            pointer_hash = %key.graph_content_hash,
            artifact_hash = %artifact.graph_content_hash,
            pointer_manifest_version = %key.manifest_version,
            artifact_manifest_version = %artifact.manifest_version,
            "spur-graph: base artifact does not match pointer"
        );
        return None;
    }

    let artifact = Arc::new(artifact);
    let Ok(mut cache) = base_artifact_cache().lock() else {
        return Some(artifact);
    };
    if let Some(cached) = cache.artifacts.get(&key) {
        return Some(Arc::clone(cached));
    }
    cache.insert(key, Arc::clone(&artifact));
    Some(artifact)
}

fn base_artifact_cache() -> &'static Mutex<BaseArtifactCache> {
    BASE_ARTIFACT_CACHE.get_or_init(|| Mutex::new(BaseArtifactCache::default()))
}

impl BaseArtifactCache {
    fn insert(&mut self, key: BaseArtifactCacheKey, artifact: Arc<GraphIndexArtifact>) {
        self.order.push_back(key.clone());
        self.artifacts.insert(key, artifact);
        while self.order.len() > BASE_ARTIFACT_CACHE_CAP {
            if let Some(evicted) = self.order.pop_front() {
                self.artifacts.remove(&evicted);
            }
        }
    }
}

fn main_worktree_root(worktree_root: &Path) -> Option<PathBuf> {
    let common_dir = match git::rev_parse_common_dir(worktree_root) {
        Ok(common_dir) => common_dir,
        Err(error) => {
            tracing::debug!(
                target: "spur_graph::base_seed",
                worktree = %worktree_root.display(),
                error = %error,
                "spur-graph: unable to resolve git common dir for base artifact"
            );
            return None;
        }
    };
    // Assumes a non-bare main worktree - SPUR does not use bare repos.
    let root = common_dir.parent()?.canonicalize().ok()?;
    Some(root)
}

fn emit_base_seed_selection(base: &'static str) {
    tracing::info!(
        target: "spur_graph::base_seed",
        base,
        "worker base seed source selected"
    );
}

pub fn emit_base_seed_stats(base: &'static str, stats: BuildStats) {
    tracing::info!(
        target: "spur_graph::base_seed",
        base,
        reused_buckets = stats.reused_buckets,
        changed_paths = stats.changed_paths,
        "worker base seed selected"
    );
}

fn write_artifact_to_worktree(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<PathBuf> {
    let worktree_artifact_base = worktree_root.join(WORKTREE_ARTIFACT_PATH);
    let staging = ArtifactStagingDir::new(&worktree_artifact_base, &artifact.graph_content_hash)?;
    write_artifact_parquet(
        artifact,
        staging.path(),
        WriteOptions::default(),
        Vec::new(),
    )?;
    let final_path = staging.commit()?;
    write_sidecar_and_stamp_best_effort(
        artifact,
        worktree_root,
        &final_path,
        section_sidecar_options,
        section_sidecar_progress,
    );
    Ok(final_path)
}

pub fn write_sidecar_and_stamp_best_effort(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) {
    match write_sidecar_overwriting(
        artifact,
        worktree_root,
        artifact_dir,
        section_sidecar_options,
        section_sidecar_progress,
    ) {
        Ok(row_counts) => {
            if let Err(error) = crate::store::stamp_sidecar_status(
                artifact_dir,
                GraphArtifactSidecarStatus {
                    complete: true,
                    row_counts,
                },
            ) {
                tracing::warn!(
                    error = %error,
                    artifact_dir = %artifact_dir.display(),
                    "spur-graph: section sidecar status stamp failed; graph artifact remains usable"
                );
            }
        }
        Err(error) => {
            if let Some(progress) = section_sidecar_progress {
                progress(SectionSidecarProgressEvent::Failed {
                    error: error.to_string(),
                });
            }
            tracing::warn!(
                error = %error,
                artifact_dir = %artifact_dir.display(),
                "spur-graph: section sidecar write failed; graph artifact remains usable"
            );
        }
    }
}

fn write_sidecar_overwriting(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    section_sidecar_options: SectionSidecarOptions,
    section_sidecar_progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<crate::store::GraphArtifactSidecarRowCounts> {
    let staging_dir = temp_path_for(artifact_dir);
    remove_path_if_exists(&staging_dir)?;
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("failed to create `{}`", staging_dir.display()))?;

    let write_result = write_sections_dataset_with_sidecar_options_and_progress(
        artifact,
        worktree_root,
        &staging_dir,
        section_sidecar_options,
        section_sidecar_progress,
    );
    let row_counts = match write_result {
        Ok(row_counts) => row_counts,
        Err(error) => {
            let _ = remove_path_if_exists(&staging_dir);
            return Err(error);
        }
    };

    replace_sidecar_dir(
        &staging_dir.join(SECTIONS_DATASET_DIR),
        &artifact_dir.join(SECTIONS_DATASET_DIR),
    )?;
    replace_sidecar_dir(
        &staging_dir.join(CODE_SYMBOLS_DATASET_DIR),
        &artifact_dir.join(CODE_SYMBOLS_DATASET_DIR),
    )?;
    remove_path_if_exists(&staging_dir)?;
    fsync_dir(artifact_dir);
    if let Some(parent) = artifact_dir.parent() {
        fsync_dir(parent);
    }
    Ok(row_counts)
}

fn replace_sidecar_dir(staged: &Path, final_path: &Path) -> Result<()> {
    if !staged.is_dir() {
        anyhow::bail!("sidecar staging dir missing `{}`", staged.display());
    }

    let backup = temp_path_for(final_path);
    remove_path_if_exists(&backup)?;
    let backup_created = if final_path.exists() {
        fs::rename(final_path, &backup).with_context(|| {
            format!(
                "failed to move existing sidecar `{}` to `{}`",
                final_path.display(),
                backup.display()
            )
        })?;
        true
    } else {
        false
    };

    match fs::rename(staged, final_path) {
        Ok(()) => {
            if backup_created {
                remove_path_if_exists(&backup)?;
            }
            Ok(())
        }
        Err(error) => {
            if backup_created {
                let _ = fs::rename(&backup, final_path);
            }
            Err(error).with_context(|| {
                format!(
                    "failed to install sidecar `{}` at `{}`",
                    staged.display(),
                    final_path.display()
                )
            })
        }
    }
}

fn write_pointer(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    ctx: &GitCtx,
    canonical: &Path,
) -> Result<()> {
    let pointer_path = worktree_root.join(POINTER_PATH);
    if let Some(parent) = pointer_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let pointer = GraphIndexPointer {
        schema: POINTER_SCHEMA.to_owned(),
        graph_content_hash: artifact.graph_content_hash.clone(),
        manifest_version: artifact.manifest_version.clone(),
        source_kind: SourceKind::Git,
        indexed_commit_oid: Some(ctx.head_oid.clone()),
        canonical_artifact_path: canonical.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize graph artifact `{}`",
                canonical.display()
            )
        })?,
    };
    let json =
        serde_json::to_string_pretty(&pointer).context("failed to encode graph index pointer")?;
    let tmp = temp_path_for(&pointer_path);
    fs::write(&tmp, json).with_context(|| format!("failed to write `{}`", tmp.display()))?;
    fs::rename(&tmp, &pointer_path).with_context(|| {
        format!(
            "failed to atomically rename `{}` to `{}`",
            tmp.display(),
            pointer_path.display()
        )
    })?;
    if let Some(parent) = pointer_path.parent() {
        fsync_dir(parent);
    }
    Ok(())
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to remove `{}`", path.display())),
    }
}

fn remove_path_if_exists(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove `{}`", path.display())),
        Ok(_) => {
            fs::remove_file(path).with_context(|| format!("failed to remove `{}`", path.display()))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("failed to stat `{}`", path.display())),
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("graph-index");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.tmp.{}.{unique}", std::process::id()))
}

fn fsync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(test))]
fn lock_timeout() -> Duration {
    Duration::from_secs(5)
}

#[cfg(test)]
fn lock_timeout() -> Duration {
    Duration::from_millis(LOCK_TIMEOUT_MS_OVERRIDE.load(Ordering::SeqCst))
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::process::Command;
    use std::sync::atomic::Ordering;

    use fs2::FileExt as _;
    use tempfile::TempDir;

    use super::{
        lookup_canonical, write_with_dedup, write_with_dedup_with_section_sidecar_options,
    };
    use crate::git::GitCtx;
    use crate::store::lance_sections::{SectionEmbeddingOptions, SectionSidecarOptions};
    use crate::store::{
        read_artifact_header_parquet, read_artifact_parquet, read_current_pointer,
        stamp_sidecar_status, GraphArtifactSidecarRowCounts, GraphArtifactSidecarStatus,
    };
    use crate::{
        GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader,
        GraphIndexPointer, GraphSymbolArtifact, NodeId, SourceKind,
    };

    #[test]
    fn write_creates_canonical_parquet_and_current_pointer() {
        let env = CacheEnv::new();
        let artifact = artifact("hash-a", "src/lib.rs");

        write_with_dedup(&artifact, env.worktree.path(), &env.ctx).unwrap();

        let canonical =
            lookup_canonical(env.common.path(), "manifest-a", "hash-a").expect("canonical path");
        let canonical = fs::canonicalize(canonical).expect("canonicalize artifact path");
        assert!(canonical.is_dir());
        assert_eq!(
            canonical.extension().and_then(|ext| ext.to_str()),
            Some("parquet")
        );
        assert!(canonical.join("nodes.parquet").is_file());
        assert!(canonical.join("edges.parquet").is_file());
        assert!(canonical.join("edges_unresolved.parquet").is_file());
        assert!(canonical.join("files.parquet").is_file());
        assert!(canonical.join("file_manifests.parquet").is_file());
        assert!(canonical.join("tombstones.parquet").is_file());
        assert!(canonical.join("manifest.json").is_file());

        let current = read_current_pointer(env.worktree.path()).expect("read CURRENT");
        assert_eq!(current, canonical);
        assert!(
            !env.worktree.path().join(".spur/graph-index.json").exists(),
            "legacy worktree JSON should not be written after the cutover"
        );

        let pointer_path = env.worktree.path().join(".spur/graph-index.pointer.json");
        let pointer: GraphIndexPointer =
            serde_json::from_slice(&fs::read(pointer_path).unwrap()).unwrap();
        assert_eq!(pointer.schema, "spur-graph-pointer-v1");
        assert_eq!(pointer.graph_content_hash, "hash-a");
        assert_eq!(pointer.manifest_version, "manifest-a");
        assert_eq!(pointer.source_kind, SourceKind::Git);
        assert_eq!(pointer.indexed_commit_oid.as_deref(), Some("head-a"));
        assert_eq!(pointer.canonical_artifact_path, canonical);
    }

    #[test]
    fn second_write_same_hash_keeps_existing_canonical_content() {
        let env = CacheEnv::new();
        let first = artifact("hash-a", "src/lib.rs");
        let second = artifact("hash-a", "src/mutated.rs");

        write_with_dedup(&first, env.worktree.path(), &env.ctx).unwrap();
        write_with_dedup(&second, env.worktree.path(), &env.ctx).unwrap();

        let canonical = lookup_canonical(env.common.path(), "manifest-a", "hash-a").unwrap();
        let canonical = fs::canonicalize(canonical).expect("canonicalize artifact path");
        let canonical_artifact = read_artifact_parquet(&canonical).unwrap();
        assert_eq!(canonical_artifact.files[0].file_path, "src/lib.rs");
        assert!(canonical_artifact
            .files
            .iter()
            .all(|file| file.file_path != "src/mutated.rs"));

        let current = read_current_pointer(env.worktree.path()).expect("read CURRENT");
        assert_eq!(current, canonical);
    }

    #[test]
    fn lock_timeout_writes_worktree_only() {
        let env = CacheEnv::new();
        super::LOCK_TIMEOUT_MS_OVERRIDE.store(25, Ordering::SeqCst);
        let artifact = artifact("hash-a", "src/lib.rs");
        let canonical_dir = env.common.path().join("spur-graph/artifacts/manifest-a");
        fs::create_dir_all(&canonical_dir).unwrap();
        let pointer_path = env.worktree.path().join(".spur/graph-index.pointer.json");
        fs::create_dir_all(pointer_path.parent().unwrap()).unwrap();
        fs::write(&pointer_path, r#"{"stale":true}"#).unwrap();
        let lock_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(canonical_dir.join(".lock"))
            .unwrap();
        lock_file.lock_exclusive().unwrap();

        write_with_dedup(&artifact, env.worktree.path(), &env.ctx).unwrap();

        let current = read_current_pointer(env.worktree.path()).expect("read CURRENT");
        assert!(
            current.is_dir(),
            "fallback should write a worktree parquet dir"
        );
        assert_eq!(
            current.extension().and_then(|ext| ext.to_str()),
            Some("parquet")
        );
        let worktree_graph_dir =
            fs::canonicalize(env.worktree.path().join(".spur/graph")).expect("graph dir");
        assert!(current.starts_with(worktree_graph_dir));
        assert!(lookup_canonical(env.common.path(), "manifest-a", "hash-a").is_none());
        assert!(
            !pointer_path.exists(),
            "lock timeout fallback must not leave a pointer to an unwritten canonical artifact"
        );
        lock_file.unlock().unwrap();
        super::LOCK_TIMEOUT_MS_OVERRIDE.store(5_000, Ordering::SeqCst);
    }

    #[test]
    fn sidecar_success_stamps_manifest_with_row_counts() {
        let env = CacheEnv::new();
        let artifact = artifact_with_sidecar_rows("hash-sidecar");
        write_fixture_sources(env.worktree.path());

        write_with_dedup_with_section_sidecar_options(
            &artifact,
            env.worktree.path(),
            &env.ctx,
            skip_embedding_sidecar_options(),
            None,
        )
        .unwrap();

        let canonical = lookup_canonical(env.common.path(), "manifest-a", "hash-sidecar")
            .expect("canonical path");
        let manifest = read_artifact_header_parquet(&canonical).expect("read manifest");
        assert!(manifest.sidecar_complete);
        assert_eq!(
            manifest.sidecar_row_counts,
            GraphArtifactSidecarRowCounts {
                section_bodies: 1,
                code_symbols: 2,
            }
        );
    }

    #[test]
    fn incomplete_canonical_sidecar_is_rebuilt_on_next_dedup_write() {
        let env = CacheEnv::new();
        let artifact = artifact_with_sidecar_rows("hash-repair");
        write_fixture_sources(env.worktree.path());

        write_with_dedup_with_section_sidecar_options(
            &artifact,
            env.worktree.path(),
            &env.ctx,
            skip_embedding_sidecar_options(),
            None,
        )
        .unwrap();
        let canonical = lookup_canonical(env.common.path(), "manifest-a", "hash-repair").unwrap();
        fs::remove_dir_all(canonical.join("sections.lancedb")).expect("remove sections sidecar");
        fs::remove_dir_all(canonical.join("code_symbols.lance")).expect("remove symbols sidecar");
        stamp_sidecar_status(
            &canonical,
            GraphArtifactSidecarStatus {
                complete: false,
                row_counts: GraphArtifactSidecarRowCounts::default(),
            },
        )
        .expect("mark sidecar incomplete");

        write_with_dedup_with_section_sidecar_options(
            &artifact,
            env.worktree.path(),
            &env.ctx,
            skip_embedding_sidecar_options(),
            None,
        )
        .unwrap();

        let manifest = read_artifact_header_parquet(&canonical).expect("read manifest");
        assert!(manifest.sidecar_complete);
        assert_eq!(manifest.sidecar_row_counts.section_bodies, 1);
        assert_eq!(manifest.sidecar_row_counts.code_symbols, 2);
        assert!(
            canonical.join("sections.lancedb").is_dir(),
            "repair should rebuild the section sidecar"
        );
        assert!(
            canonical.join("code_symbols.lance").is_dir(),
            "repair should rebuild the code-symbol sidecar"
        );
    }

    #[test]
    fn sidecar_failure_keeps_parquet_artifact_and_incomplete_manifest() {
        let output = Command::new(std::env::current_exe().expect("current test binary"))
            .args([
                "--exact",
                "store::cache::tests::sidecar_failure_child",
                "--nocapture",
            ])
            .env("SPUR_GRAPH_CACHE_FAILURE_CHILD", "1")
            .env("SPUR_GRAPH_TEST_FAIL_SECTION_SIDECAR", "1")
            .output()
            .expect("run isolated sidecar failure child test");

        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn sidecar_failure_child() {
        if !matches!(
            std::env::var("SPUR_GRAPH_CACHE_FAILURE_CHILD"),
            Ok(value) if value == "1"
        ) {
            return;
        }

        let env = CacheEnv::new();
        let artifact = artifact_with_sidecar_rows("hash-sidecar-fails");
        write_fixture_sources(env.worktree.path());

        write_with_dedup_with_section_sidecar_options(
            &artifact,
            env.worktree.path(),
            &env.ctx,
            skip_embedding_sidecar_options(),
            None,
        )
        .unwrap();

        let canonical = lookup_canonical(env.common.path(), "manifest-a", "hash-sidecar-fails")
            .expect("canonical path");
        assert!(canonical.join("manifest.json").is_file());
        assert!(canonical.join("nodes.parquet").is_file());
        let manifest = read_artifact_header_parquet(&canonical).expect("read manifest");
        assert!(!manifest.sidecar_complete);
        assert_eq!(
            manifest.sidecar_row_counts,
            GraphArtifactSidecarRowCounts::default()
        );
    }

    struct CacheEnv {
        common: TempDir,
        worktree: TempDir,
        ctx: GitCtx,
    }

    impl CacheEnv {
        fn new() -> Self {
            let common = TempDir::new().unwrap();
            let worktree = TempDir::new().unwrap();
            let ctx = GitCtx {
                worktree_root: worktree.path().to_path_buf(),
                git_common_dir: common.path().to_path_buf(),
                head_oid: "head-a".to_owned(),
            };
            Self {
                common,
                worktree,
                ctx,
            }
        }
    }

    fn artifact(hash: &str, file_path: &str) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "spur-graph-phase2".to_owned(),
                content_hash_blake3: None,
            },
            manifest_version: "manifest-a".to_owned(),
            graph_content_hash: hash.to_owned(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: format!("file:{file_path}"),
                path: file_path.to_owned(),
                content_oid: "oid-a".to_owned(),
                node_ids: Vec::new(),
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: format!("file:{file_path}"),
                file_path: file_path.to_owned(),
            }],
            file_node_ids: vec![NodeId(1)],
            symbols: Vec::new(),
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        }
    }

    fn skip_embedding_sidecar_options() -> SectionSidecarOptions {
        SectionSidecarOptions {
            embedding: SectionEmbeddingOptions {
                skip_embeddings: true,
                batch_size: 64,
            },
            write_batch_size: 512,
            previous_artifact_dir: None,
            delta: None,
        }
    }

    fn write_fixture_sources(worktree: &std::path::Path) {
        fs::create_dir_all(worktree.join("docs")).unwrap();
        fs::create_dir_all(worktree.join("src")).unwrap();
        fs::write(worktree.join("docs/guide.md"), "# Guide\n\nBody text.\n").unwrap();
        fs::write(worktree.join("src/lib.rs"), code_source()).unwrap();
    }

    fn code_source() -> &'static str {
        concat!(
            "/// Parses the request payload into a normalized command shape.\n",
            "/// Keeps enough context for downstream handlers to preserve provenance.\n",
            "fn parse_request() {}\n",
            "\n",
            "struct CommandEnvelope;\n",
            "\n",
            "fn x() {}\n",
        )
    }

    fn artifact_with_sidecar_rows(hash: &str) -> GraphIndexArtifact {
        let mut artifact = artifact(hash, "docs/guide.md");
        artifact.file_manifests.push(GraphFileManifestEntry {
            stable_file_id: "file:src/lib.rs".to_owned(),
            path: "src/lib.rs".to_owned(),
            content_oid: "oid-src".to_owned(),
            node_ids: vec![NodeId(2), NodeId(3)],
        });
        artifact.files.push(GraphFileArtifact {
            stable_file_id: "file:src/lib.rs".to_owned(),
            file_path: "src/lib.rs".to_owned(),
        });
        artifact.file_node_ids.push(NodeId(2));
        let docs_len = "# Guide\n\nBody text.\n".len();
        let source = code_source();
        let parse_start = source.find("fn parse_request").unwrap();
        let parse_end = parse_start + "fn parse_request() {}".len();
        let envelope_start = source.find("struct CommandEnvelope").unwrap();
        let envelope_end = envelope_start + "struct CommandEnvelope;".len();
        let short_start = source.find("fn x").unwrap();
        let short_end = short_start + "fn x() {}".len();
        artifact.symbols = vec![
            GraphSymbolArtifact {
                stable_symbol_id: "section-guide".to_owned(),
                file_path: "docs/guide.md".to_owned(),
                byte_range: [0, docs_len],
                line_range: [1, 3],
                entity_name: "Guide".to_owned(),
                qualified_name: "Guide".to_owned(),
                symbol_kind: "section".to_owned(),
                anchor_hash: "anchor-guide".to_owned(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "fn-parse-request".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                byte_range: [parse_start, parse_end],
                line_range: [3, 3],
                entity_name: "parse_request".to_owned(),
                qualified_name: "parse_request".to_owned(),
                symbol_kind: "function".to_owned(),
                anchor_hash: "anchor-parse".to_owned(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "struct-command-envelope".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                byte_range: [envelope_start, envelope_end],
                line_range: [5, 5],
                entity_name: "CommandEnvelope".to_owned(),
                qualified_name: "CommandEnvelope".to_owned(),
                symbol_kind: "struct".to_owned(),
                anchor_hash: "anchor-envelope".to_owned(),
                enclosing_scope: None,
            },
            GraphSymbolArtifact {
                stable_symbol_id: "fn-x".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                byte_range: [short_start, short_end],
                line_range: [7, 7],
                entity_name: "x".to_owned(),
                qualified_name: "x".to_owned(),
                symbol_kind: "function".to_owned(),
                anchor_hash: "anchor-x".to_owned(),
                enclosing_scope: None,
            },
        ];
        artifact.symbol_node_ids = vec![NodeId(10), NodeId(11), NodeId(12), NodeId(13)];
        artifact
    }
}
