use std::fs;
use std::fs::File;
use std::io;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};

use crate::{GraphIndexArtifact, GraphIndexPointer};

use super::parquet::read_artifact_header_parquet;

const CURRENT_PATH: &str = ".spur/graph/CURRENT";
const POINTER_PATH: &str = ".spur/graph-index.pointer.json";
const LEGACY_ARTIFACT_PATH: &str = ".spur/graph-index.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedArtifact {
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub cache_key: ArtifactCacheKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactFormat {
    LegacyJson,
    Parquet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactCacheKey {
    LegacyJson { path: PathBuf, mtime: SystemTime },
    Parquet { graph_content_hash: String },
}

pub fn resolve_artifact_location(
    worktree_root: &Path,
    explicit_override: Option<&Path>,
) -> anyhow::Result<ResolvedArtifact> {
    let worktree_root = worktree_root.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize worktree root `{}`",
            worktree_root.display()
        )
    })?;

    if let Some(path) = explicit_override {
        let path = absolutize(&worktree_root, path);
        tracing::debug!(
            priority = 1,
            source = "explicit_override",
            path = %path.display(),
            "spur-graph: considering artifact location"
        );
        if let Some(resolved) = resolve_path(&path, 1, "explicit_override") {
            return Ok(resolved);
        }
    } else {
        tracing::debug!(
            priority = 1,
            source = "explicit_override",
            "spur-graph: skipping artifact location; no explicit override provided"
        );
    }

    let current_path = worktree_root.join(CURRENT_PATH);
    tracing::debug!(
        priority = 2,
        source = "current",
        path = %current_path.display(),
        "spur-graph: considering artifact location"
    );
    match fs::symlink_metadata(&current_path) {
        Ok(_) => match read_current_pointer(&worktree_root) {
            Ok(path) => {
                if let Some(resolved) = resolve_parquet_path(&path, 2, "current") {
                    return Ok(resolved);
                }
            }
            Err(err) => {
                tracing::warn!(
                    priority = 2,
                    source = "current",
                    path = %current_path.display(),
                    error = %err,
                    "spur-graph: skipping invalid CURRENT pointer"
                );
            }
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(
                priority = 2,
                source = "current",
                path = %current_path.display(),
                "spur-graph: skipping artifact location; CURRENT does not exist"
            );
        }
        Err(err) => {
            tracing::warn!(
                priority = 2,
                source = "current",
                path = %current_path.display(),
                error = %err,
                "spur-graph: skipping artifact location; failed to inspect CURRENT"
            );
        }
    }

    let pointer_path = worktree_root.join(POINTER_PATH);
    tracing::debug!(
        priority = 3,
        source = "pointer_file",
        path = %pointer_path.display(),
        "spur-graph: considering artifact location"
    );
    match read_pointer_file(&pointer_path) {
        Ok(Some(pointer)) => {
            let path = absolutize(&worktree_root, &pointer.canonical_artifact_path);
            if let Some(resolved) = resolve_path(&path, 3, "pointer_file") {
                return Ok(resolved);
            }
        }
        Ok(None) => {
            tracing::debug!(
                priority = 3,
                source = "pointer_file",
                path = %pointer_path.display(),
                "spur-graph: skipping artifact location; pointer file does not exist"
            );
        }
        Err(err) => {
            tracing::warn!(
                priority = 3,
                source = "pointer_file",
                path = %pointer_path.display(),
                error = %err,
                "spur-graph: skipping invalid pointer file"
            );
        }
    }

    let legacy_path = worktree_root.join(LEGACY_ARTIFACT_PATH);
    tracing::debug!(
        priority = 4,
        source = "legacy_worktree_json",
        path = %legacy_path.display(),
        "spur-graph: considering artifact location"
    );
    if let Some(resolved) = resolve_legacy_json_path(&legacy_path, 4, "legacy_worktree_json") {
        return Ok(resolved);
    }

    Err(anyhow!(
        "no valid spur graph artifact found under `{}`",
        worktree_root.display()
    ))
}

pub fn write_current_pointer(worktree_root: &Path, hash_dir: &Path) -> anyhow::Result<()> {
    let current_path = worktree_root.join(CURRENT_PATH);
    let current_dir = current_path
        .parent()
        .ok_or_else(|| anyhow!("CURRENT path has no parent"))?;
    fs::create_dir_all(current_dir)
        .with_context(|| format!("failed to create `{}`", current_dir.display()))?;

    let target = hash_dir
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", hash_dir.display()))?;
    let tmp = temp_path_for(&current_path);
    remove_current_tmp(&tmp)?;
    write_current_target(&tmp, &target)?;
    fs::rename(&tmp, &current_path).with_context(|| {
        format!(
            "failed to atomically rename `{}` to `{}`",
            tmp.display(),
            current_path.display()
        )
    })?;
    fsync_dir(current_dir);
    Ok(())
}

pub fn read_current_pointer(worktree_root: &Path) -> anyhow::Result<PathBuf> {
    let current_path = worktree_root.join(CURRENT_PATH);
    let target = read_current_target(&current_path)
        .with_context(|| format!("failed to read `{}`", current_path.display()))?;
    let absolute = absolutize(current_path.parent().unwrap_or(worktree_root), &target);
    absolute.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize CURRENT target `{}`",
            absolute.display()
        )
    })
}

fn resolve_path(path: &Path, priority: u8, source: &'static str) -> Option<ResolvedArtifact> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => resolve_parquet_path(path, priority, source),
        Ok(metadata) if metadata.is_file() => resolve_legacy_json_path(path, priority, source),
        Ok(_) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                "spur-graph: skipping artifact location; path is neither file nor directory"
            );
            None
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(
                priority,
                source,
                path = %path.display(),
                "spur-graph: skipping artifact location; path does not exist"
            );
            None
        }
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                error = %err,
                "spur-graph: skipping artifact location; failed to inspect path"
            );
            None
        }
    }
}

fn resolve_parquet_path(
    path: &Path,
    priority: u8,
    source: &'static str,
) -> Option<ResolvedArtifact> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(
                priority,
                source,
                path = %path.display(),
                "spur-graph: skipping Parquet artifact; directory does not exist"
            );
            return None;
        }
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                error = %err,
                "spur-graph: skipping Parquet artifact; failed to inspect directory"
            );
            return None;
        }
    };
    if !metadata.is_dir() {
        tracing::warn!(
            priority,
            source,
            path = %path.display(),
            "spur-graph: skipping Parquet artifact; path is not a directory"
        );
        return None;
    }

    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                error = %err,
                "spur-graph: skipping Parquet artifact; failed to canonicalize path"
            );
            return None;
        }
    };
    let manifest = match read_artifact_header_parquet(&canonical) {
        Ok(manifest) => manifest,
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %canonical.display(),
                error = %err,
                "spur-graph: skipping Parquet artifact; manifest is invalid"
            );
            return None;
        }
    };
    if !manifest.complete {
        tracing::warn!(
            priority,
            source,
            path = %canonical.display(),
            "spur-graph: skipping Parquet artifact; manifest is incomplete"
        );
        return None;
    }

    tracing::debug!(
        priority,
        source,
        path = %canonical.display(),
        graph_content_hash = %manifest.graph_content_hash,
        "spur-graph: selected Parquet artifact"
    );
    Some(ResolvedArtifact {
        path: canonical,
        format: ArtifactFormat::Parquet,
        cache_key: ArtifactCacheKey::Parquet {
            graph_content_hash: manifest.graph_content_hash,
        },
    })
}

fn resolve_legacy_json_path(
    path: &Path,
    priority: u8,
    source: &'static str,
) -> Option<ResolvedArtifact> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(
                priority,
                source,
                path = %path.display(),
                "spur-graph: skipping legacy JSON artifact; file does not exist"
            );
            return None;
        }
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                error = %err,
                "spur-graph: skipping legacy JSON artifact; failed to inspect file"
            );
            return None;
        }
    };
    if !metadata.is_file() {
        tracing::warn!(
            priority,
            source,
            path = %path.display(),
            "spur-graph: skipping legacy JSON artifact; path is not a file"
        );
        return None;
    }
    if let Err(err) = parse_legacy_json(path) {
        tracing::warn!(
            priority,
            source,
            path = %path.display(),
            error = %err,
            "spur-graph: skipping legacy JSON artifact; JSON is invalid"
        );
        return None;
    }

    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %path.display(),
                error = %err,
                "spur-graph: skipping legacy JSON artifact; failed to canonicalize path"
            );
            return None;
        }
    };
    let mtime = match metadata.modified() {
        Ok(mtime) => mtime,
        Err(err) => {
            tracing::warn!(
                priority,
                source,
                path = %canonical.display(),
                error = %err,
                "spur-graph: skipping legacy JSON artifact; failed to read mtime"
            );
            return None;
        }
    };

    tracing::warn!(
        priority,
        source,
        path = %canonical.display(),
        "spur-graph: loading legacy JSON graph artifact; JSON artifacts are deprecated and will be removed after the Parquet cutover"
    );
    tracing::debug!(
        priority,
        source,
        path = %canonical.display(),
        "spur-graph: selected legacy JSON artifact"
    );
    Some(ResolvedArtifact {
        path: canonical.clone(),
        format: ArtifactFormat::LegacyJson,
        cache_key: ArtifactCacheKey::LegacyJson {
            path: canonical,
            mtime,
        },
    })
}

fn parse_legacy_json(path: &Path) -> anyhow::Result<()> {
    let file = File::open(path)
        .with_context(|| format!("failed to read graph index artifact `{}`", path.display()))?;
    let reader = BufReader::new(file);
    let _: GraphIndexArtifact = serde_json::from_reader(reader)
        .map_err(|err| anyhow!("invalid graph index JSON in `{}`: {err}", path.display()))?;
    Ok(())
}

fn read_pointer_file(path: &Path) -> anyhow::Result<Option<GraphIndexPointer>> {
    match File::open(path) {
        Ok(file) => {
            let pointer = serde_json::from_reader(BufReader::new(file))
                .with_context(|| format!("invalid graph index pointer `{}`", path.display()))?;
            Ok(Some(pointer))
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err)
            .with_context(|| format!("failed to read graph index pointer `{}`", path.display())),
    }
}

fn absolutize(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(unix)]
fn write_current_target(path: &Path, target: &Path) -> anyhow::Result<()> {
    std::os::unix::fs::symlink(target, path).with_context(|| {
        format!(
            "failed to create CURRENT symlink `{}` -> `{}`",
            path.display(),
            target.display()
        )
    })
}

#[cfg(not(unix))]
fn write_current_target(path: &Path, target: &Path) -> anyhow::Result<()> {
    fs::write(path, target.display().to_string())
        .with_context(|| format!("failed to write `{}`", path.display()))
}

fn read_current_target(path: &Path) -> anyhow::Result<PathBuf> {
    match fs::read_link(path) {
        Ok(target) => Ok(target),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::InvalidInput | io::ErrorKind::Unsupported
            ) =>
        {
            let content = fs::read_to_string(path)
                .with_context(|| format!("failed to read `{}`", path.display()))?;
            Ok(PathBuf::from(content.trim()))
        }
        Err(err) => {
            Err(err).with_context(|| format!("failed to read symlink `{}`", path.display()))
        }
    }
}

fn remove_current_tmp(path: &Path) -> anyhow::Result<()> {
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
        .unwrap_or("CURRENT");
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.with_file_name(format!(".{file_name}.tmp.{}.{unique}", std::process::id()))
}

fn fsync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}
