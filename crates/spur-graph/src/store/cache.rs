use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs2::FileExt;

use crate::store::json::write_artifact;
use crate::{git::GitCtx, GraphIndexArtifact, GraphIndexPointer, SourceKind};

const CACHE_DIR_NAME: &str = "spur-graph";
const ARTIFACTS_DIR_NAME: &str = "artifacts";
const LOCK_FILE_NAME: &str = ".lock";
const WORKTREE_ARTIFACT_PATH: &str = ".spur/graph-index.json";
const POINTER_PATH: &str = ".spur/graph-index.pointer.json";
const POINTER_SCHEMA: &str = "spur-graph-pointer-v1";
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

#[cfg(test)]
static LOCK_TIMEOUT_MS_OVERRIDE: AtomicU64 = AtomicU64::new(5_000);

pub fn write_with_dedup(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    ctx: &GitCtx,
) -> Result<()> {
    let canonical = canonical_path(
        &ctx.git_common_dir,
        &artifact.manifest_version,
        &artifact.graph_content_hash,
    );
    let canonical_dir = canonical
        .parent()
        .context("canonical graph artifact path has no parent")?;
    fs::create_dir_all(canonical_dir)
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
        write_artifact_to_worktree(artifact, worktree_root)?;
        // No canonical was written, so any prior pointer is stale; remove it.
        remove_if_exists(&worktree_root.join(POINTER_PATH))?;
        return Ok(());
    }

    let write_result = if canonical.exists() {
        Ok(())
    } else {
        write_canonical_atomically(artifact, &canonical)
    };
    let unlock_result = fs2::FileExt::unlock(&lock).context("failed to unlock graph cache lock");
    write_result?;
    unlock_result?;

    install_worktree_artifact(&canonical, worktree_root)?;
    write_pointer(artifact, worktree_root, ctx, &canonical)?;
    Ok(())
}

pub fn lookup_canonical(common_dir: &Path, manifest_version: &str, hash: &str) -> Option<PathBuf> {
    let path = canonical_path(common_dir, manifest_version, hash);
    path.exists().then_some(path)
}

fn canonical_path(common_dir: &Path, manifest_version: &str, hash: &str) -> PathBuf {
    common_dir
        .join(CACHE_DIR_NAME)
        .join(ARTIFACTS_DIR_NAME)
        .join(manifest_version)
        .join(format!("{hash}.json"))
}

fn try_lock_exclusive_with_timeout(file: &File, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(err) if is_lock_contended(&err) => {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                thread::sleep(
                    LOCK_RETRY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(err) => return Err(err).context("failed to acquire graph cache lock"),
        }
    }
}

fn is_lock_contended(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
    )
}

fn write_canonical_atomically(artifact: &GraphIndexArtifact, canonical: &Path) -> Result<()> {
    let parent = canonical
        .parent()
        .context("canonical graph artifact path has no parent")?;
    let tmp = temp_path_for(canonical);
    write_artifact(artifact, &tmp)?;
    fs::rename(&tmp, canonical).with_context(|| {
        format!(
            "failed to atomically rename `{}` to `{}`",
            tmp.display(),
            canonical.display()
        )
    })?;
    fsync_dir(parent);
    Ok(())
}

fn write_artifact_to_worktree(artifact: &GraphIndexArtifact, worktree_root: &Path) -> Result<()> {
    let worktree_artifact = worktree_root.join(WORKTREE_ARTIFACT_PATH);
    write_artifact(artifact, &worktree_artifact)
}

fn install_worktree_artifact(canonical: &Path, worktree_root: &Path) -> Result<()> {
    let worktree_artifact = worktree_root.join(WORKTREE_ARTIFACT_PATH);
    if let Some(parent) = worktree_artifact.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    if same_inode(canonical, &worktree_artifact).unwrap_or(false) {
        return Ok(());
    }
    remove_if_exists(&worktree_artifact)?;
    link_or_copy(canonical, &worktree_artifact)
}

fn link_or_copy(source: &Path, dest: &Path) -> Result<()> {
    link_or_copy_with_hardlink(source, dest, |source, dest| fs::hard_link(source, dest))
}

fn link_or_copy_with_hardlink<F>(source: &Path, dest: &Path, hardlink: F) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    match hardlink(source, dest) {
        Ok(()) => Ok(()),
        Err(err) if is_hardlink_fallback(&err) => {
            fs::copy(source, dest).with_context(|| {
                format!(
                    "failed to copy canonical graph artifact `{}` to `{}`",
                    source.display(),
                    dest.display()
                )
            })?;
            Ok(())
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to hardlink canonical graph artifact `{}` to `{}`",
                source.display(),
                dest.display()
            )
        }),
    }
}

fn is_hardlink_fallback(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::CrossesDevices | io::ErrorKind::Unsupported
    )
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
        canonical_artifact_path: canonical.to_path_buf(),
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

#[cfg(unix)]
fn same_inode(left: &Path, right: &Path) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;

    let left = fs::metadata(left)?;
    let right = fs::metadata(right)?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(not(unix))]
fn same_inode(left: &Path, right: &Path) -> io::Result<bool> {
    Ok(fs::canonicalize(left)? == fs::canonicalize(right)?)
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
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use tempfile::TempDir;

    use super::{lookup_canonical, write_with_dedup};
    use crate::git::GitCtx;
    use crate::{
        GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader,
        GraphIndexPointer, SourceKind,
    };

    #[test]
    fn write_creates_canonical_and_worktree_hardlink() {
        let env = CacheEnv::new();
        let artifact = artifact("hash-a", "src/lib.rs");

        write_with_dedup(&artifact, env.worktree.path(), &env.ctx).unwrap();

        let canonical =
            lookup_canonical(env.common.path(), "manifest-a", "hash-a").expect("canonical path");
        assert!(canonical.exists());
        let worktree_artifact = env.worktree.path().join(".spur/graph-index.json");
        assert!(worktree_artifact.exists());

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&canonical).unwrap().ino(),
            fs::metadata(&worktree_artifact).unwrap().ino()
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
        let canonical_json = fs::read_to_string(&canonical).unwrap();
        assert!(canonical_json.contains("src/lib.rs"));
        assert!(!canonical_json.contains("src/mutated.rs"));

        let worktree_json =
            fs::read_to_string(env.worktree.path().join(".spur/graph-index.json")).unwrap();
        assert!(worktree_json.contains("src/lib.rs"));
        assert!(!worktree_json.contains("src/mutated.rs"));
    }

    #[test]
    fn cross_fs_hardlink_error_falls_back_to_copy() {
        let env = CacheEnv::new();
        let source = env.common.path().join("source.json");
        let dest = env.worktree.path().join("dest.json");
        fs::write(&source, r#"{"ok":true}"#).unwrap();

        super::link_or_copy_with_hardlink(&source, &dest, |_source, _dest| {
            Err(std::io::Error::from(std::io::ErrorKind::CrossesDevices))
        })
        .unwrap();

        assert_eq!(fs::read_to_string(dest).unwrap(), r#"{"ok":true}"#);
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

        assert!(env.worktree.path().join(".spur/graph-index.json").exists());
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
            symbols: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
