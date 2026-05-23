use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::locking::try_lock_exclusive_with_timeout;
use crate::store::{write_artifact_parquet, write_current_pointer, WriteOptions};
use crate::{git::GitCtx, GraphIndexArtifact, GraphIndexPointer, SourceKind};

const CACHE_DIR_NAME: &str = "spur-graph";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const LOCK_FILE_NAME: &str = ".lock";
const WORKTREE_ARTIFACT_PATH: &str = ".spur/graph";
const POINTER_PATH: &str = ".spur/graph-index.pointer.json";
pub const COMMIT_INDEX_POINTER_PATH: &str = ".spur/commit-index.pointer.json";
const POINTER_SCHEMA: &str = "spur-graph-pointer-v1";

#[cfg(test)]
static LOCK_TIMEOUT_MS_OVERRIDE: AtomicU64 = AtomicU64::new(5_000);

pub fn write_with_dedup(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    ctx: &GitCtx,
) -> Result<()> {
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
        let written_dir = write_artifact_to_worktree(artifact, worktree_root)?;
        write_current_pointer(worktree_root, &written_dir)?;
        // No canonical was written, so any prior pointer is stale; remove it.
        remove_if_exists(&worktree_root.join(POINTER_PATH))?;
        return Ok(());
    }

    let write_result = if canonical.join("manifest.json").is_file() {
        Ok(canonical)
    } else {
        write_canonical_atomically(artifact, &canonical_dir)
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
    canonical_dir: &Path,
) -> Result<PathBuf> {
    write_artifact_parquet(artifact, canonical_dir, WriteOptions::default())
}

fn write_artifact_to_worktree(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
) -> Result<PathBuf> {
    let worktree_artifact_base = worktree_root.join(WORKTREE_ARTIFACT_PATH);
    write_artifact_parquet(artifact, &worktree_artifact_base, WriteOptions::default())
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
        schema: POINTER_SCHEMA.to_string(),
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
    use std::sync::atomic::Ordering;

    use fs2::FileExt;
    use tempfile::TempDir;

    use super::{lookup_canonical, write_with_dedup};
    use crate::git::GitCtx;
    use crate::store::{read_artifact_parquet, read_current_pointer};
    use crate::{
        GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader,
        GraphIndexPointer, NodeId, SourceKind,
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
                head_oid: "head-a".to_string(),
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
                graph_index_version: "spur-graph-phase2".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "manifest-a".to_string(),
            graph_content_hash: hash.to_string(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: format!("file:{file_path}"),
                path: file_path.to_string(),
                content_oid: "oid-a".to_string(),
                node_ids: Vec::new(),
            }],
            files: vec![GraphFileArtifact {
                stable_file_id: format!("file:{file_path}"),
                file_path: file_path.to_string(),
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
}
