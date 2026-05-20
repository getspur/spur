use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::schema::{CommitIndexArtifact, GRAPH_INDEX_VERSION_TEMPORAL};
use crate::store::cache::COMMIT_INDEX_POINTER_PATH;

#[derive(Debug, thiserror::Error)]
pub enum CommitIndexLoadError {
    #[error("unsupported commit-index pointer schema_version `{found}`; expected `{expected}`")]
    UnsupportedPointerSchemaVersion { found: u32, expected: &'static str },
    #[error("commit-index artifact_relative_path must be relative, got absolute path `{path}`")]
    AbsoluteArtifactRelativePath { path: String },
    #[error("commit-index artifact_relative_path must not contain parent traversal: `{path}`")]
    ParentTraversalArtifactRelativePath { path: String },
    #[error("commit-index artifact_relative_path `{path}` escapes .spur root `{dot_spur}`")]
    ArtifactRelativePathEscapesDotSpur { path: String, dot_spur: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitIndexPointer {
    pub schema_version: u32,
    pub artifact_relative_path: String,
    pub indexed_at: String,
    #[serde(default)]
    pub refs: BTreeMap<String, String>,
}

pub fn load_pointer(worktree: &Path) -> Result<Option<CommitIndexPointer>> {
    let path = worktree.join(COMMIT_INDEX_POINTER_PATH);
    if !path.exists() {
        return Ok(None);
    }

    let text = fs::read_to_string(&path)
        .with_context(|| format!("read commit-index pointer at {}", path.display()))?;
    let pointer: CommitIndexPointer = serde_json::from_str(&text)
        .with_context(|| format!("parse commit-index pointer at {}", path.display()))?;
    let expected_schema_version = current_schema_version()?;
    if pointer.schema_version != expected_schema_version {
        return Err(CommitIndexLoadError::UnsupportedPointerSchemaVersion {
            found: pointer.schema_version,
            expected: GRAPH_INDEX_VERSION_TEMPORAL,
        }
        .into());
    }
    Ok(Some(pointer))
}

fn current_schema_version() -> Result<u32> {
    GRAPH_INDEX_VERSION_TEMPORAL.parse().with_context(|| {
        format!("parse temporal graph index version `{GRAPH_INDEX_VERSION_TEMPORAL}`")
    })
}

pub fn save_pointer(worktree: &Path, pointer: &CommitIndexPointer) -> Result<()> {
    let path = worktree.join(COMMIT_INDEX_POINTER_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(pointer).context("encode commit-index pointer")?;
    fs::write(&path, json).with_context(|| format!("write {}", path.display()))
}

pub fn load_artifact(worktree: &Path, pointer: &CommitIndexPointer) -> Result<CommitIndexArtifact> {
    let path = canonical_artifact_path(worktree, &pointer.artifact_relative_path)?;
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read commit index artifact at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parse commit index artifact at {}", path.display()))
}

fn canonical_artifact_path(worktree: &Path, artifact_relative_path: &str) -> Result<PathBuf> {
    let relative = Path::new(artifact_relative_path);
    if relative.is_absolute() {
        return Err(CommitIndexLoadError::AbsoluteArtifactRelativePath {
            path: artifact_relative_path.to_string(),
        }
        .into());
    }
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(CommitIndexLoadError::ParentTraversalArtifactRelativePath {
            path: artifact_relative_path.to_string(),
        }
        .into());
    }

    let canonical = worktree.join(relative).canonicalize().with_context(|| {
        format!("canonicalize commit-index artifact at {artifact_relative_path}")
    })?;
    let dot_spur = worktree
        .canonicalize()
        .with_context(|| format!("canonicalize worktree root at {}", worktree.display()))?
        .join(".spur");
    if !canonical.starts_with(&dot_spur) {
        return Err(CommitIndexLoadError::ArtifactRelativePathEscapesDotSpur {
            path: canonical.display().to_string(),
            dot_spur: dot_spur.display().to_string(),
        }
        .into());
    }

    Ok(canonical)
}

pub fn save_artifact(
    worktree: &Path,
    relative: &str,
    artifact: &CommitIndexArtifact,
) -> Result<()> {
    let path = worktree.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(artifact).context("encode commit index artifact")?;
    fs::write(&path, json).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CommitArtifact, CommitIndexArtifact, WalkStrategy};
    use tempfile::TempDir;

    #[test]
    fn pointer_round_trips() {
        let dir = TempDir::new().unwrap();
        let pointer = CommitIndexPointer {
            schema_version: current_schema_version().unwrap(),
            artifact_relative_path: "commits/2026-05-20.json".to_string(),
            indexed_at: "2026-05-20T12:00:00Z".to_string(),
            refs: [("main".to_string(), "abc123".to_string())].into(),
        };
        save_pointer(dir.path(), &pointer).unwrap();
        let loaded = load_pointer(dir.path()).unwrap();
        assert_eq!(loaded, Some(pointer));
    }

    #[test]
    fn missing_pointer_returns_none() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_pointer(dir.path()).unwrap(), None);
    }

    #[test]
    fn artifact_round_trips() {
        let a = CommitIndexArtifact {
            schema_version: 1,
            commits: vec![CommitArtifact {
                sha: "abc".into(),
                parents: vec![],
                author_time: 0,
                summary: "init".into(),
            }],
            refs: [("main".into(), "abc".into())].into(),
            indexed_at: "2026-05-20T12:00:00Z".into(),
            walk_strategy: WalkStrategy::Reachable,
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: CommitIndexArtifact = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
    }
}
