use std::path::{Path, PathBuf};
use std::sync::Arc;

use spur_graph::store::lance_sections::{
    write_sections_dataset_skipping_embeddings,
    write_sections_dataset_skipping_embeddings_with_delta, SidecarDelta, SECTIONS_DATASET_DIR,
};
use spur_graph::store::read_artifact_header_parquet;
use spur_graph::temporal::{resolve_symbol_at_indexed, symbol_history, Resolution, TemporalIndex};
use spur_graph::{
    load_artifact, resolve_artifact_location, resolve_worktree_root_from, CommitIndexArtifact,
    GraphIndexArtifact,
};
use uuid::Uuid;

use crate::mcp::McpHandlerError;

pub(super) struct DocArtifactSource {
    artifact_dir: PathBuf,
    artifact: Option<Arc<GraphIndexArtifact>>,
    _temp_dir: Option<OverlayDocTempDir>,
}

impl DocArtifactSource {
    fn resolved(artifact_dir: PathBuf) -> Self {
        Self {
            artifact_dir,
            artifact: None,
            _temp_dir: None,
        }
    }

    fn overlay(
        artifact_dir: PathBuf,
        artifact: Arc<GraphIndexArtifact>,
        temp_dir: OverlayDocTempDir,
    ) -> Self {
        Self {
            artifact_dir,
            artifact: Some(artifact),
            _temp_dir: Some(temp_dir),
        }
    }

    pub(super) fn artifact_dir(&self) -> &Path {
        &self.artifact_dir
    }

    pub(super) fn artifact(&self) -> Option<&Arc<GraphIndexArtifact>> {
        self.artifact.as_ref()
    }
}

struct OverlayDocTempDir {
    path: PathBuf,
}

impl OverlayDocTempDir {
    fn new() -> Result<Self, McpHandlerError> {
        let path = std::env::temp_dir().join(format!("spur-doc-overlay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to create temporary doc_navigate overlay directory `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for OverlayDocTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub(super) async fn open_doc_artifact_for_request(
    worktree: &Path,
) -> Result<DocArtifactSource, McpHandlerError> {
    if let Ok(resolved) = resolve_artifact_location(worktree, None) {
        return Ok(DocArtifactSource::resolved(resolved.path));
    }

    let base_delta = spur_graph::mcp::markdown_overlay_base_delta_for_worktree(worktree).await;
    if let Some(delta) = &base_delta {
        if base_sidecar_usable(&delta.base_artifact_dir) {
            if delta.changed_markdown_paths.is_empty() && delta.deleted_markdown_paths.is_empty() {
                return Ok(DocArtifactSource::resolved(delta.base_artifact_dir.clone()));
            }

            if let Ok(artifact) =
                spur_graph::mcp::overlaid_graph_artifact_from_base_seed_for_worktree(
                    worktree.to_path_buf(),
                    spur_graph::mcp::shared_rebuild_coordinator(),
                )
                .await
            {
                let temp_dir = OverlayDocTempDir::new()?;
                let artifact_dir = temp_dir.path().join("artifact");
                let sidecar_delta = SidecarDelta::new(
                    delta.changed_markdown_paths.clone(),
                    delta.deleted_markdown_paths.clone(),
                );
                match write_sections_dataset_skipping_embeddings_with_delta(
                    &artifact,
                    worktree,
                    &artifact_dir,
                    &delta.base_artifact_dir,
                    sidecar_delta,
                ) {
                    Ok(()) => {
                        return Ok(DocArtifactSource::overlay(artifact_dir, artifact, temp_dir));
                    }
                    Err(error) => {
                        tracing::debug!(
                            error = %error,
                            worktree = %worktree.display(),
                            base_artifact_dir = %delta.base_artifact_dir.display(),
                            "doc_navigate overlay delta sidecar failed; falling back to full sidecar write"
                        );
                    }
                }
            }
        }
    }

    let artifact = spur_graph::mcp::overlaid_graph_artifact_from_base_seed_for_worktree(
        worktree.to_path_buf(),
        spur_graph::mcp::shared_rebuild_coordinator(),
    )
    .await?;
    let temp_dir = OverlayDocTempDir::new()?;
    let artifact_dir = temp_dir.path().join("artifact");
    write_sections_dataset_skipping_embeddings(&artifact, worktree, &artifact_dir).map_err(
        |error| {
            McpHandlerError::Internal(format!(
                "failed to build doc_navigate overlay sections in {}: {error}",
                worktree.display()
            ))
        },
    )?;
    Ok(DocArtifactSource::overlay(artifact_dir, artifact, temp_dir))
}

fn base_sidecar_usable(artifact_dir: &Path) -> bool {
    let manifest_marks_complete =
        read_artifact_header_parquet(artifact_dir).is_ok_and(|manifest| manifest.sidecar_complete);
    if !manifest_marks_complete {
        tracing::debug!(
            artifact_dir = %artifact_dir.display(),
            "base graph manifest does not mark section sidecar complete; checking sidecar directory"
        );
    }
    artifact_dir.join(SECTIONS_DATASET_DIR).is_dir()
}

pub(super) fn resolve_root_for_as_of(
    artifact_dir: &Path,
    worktree: &Path,
    root: &str,
    as_of: Option<&str>,
    artifact: Option<&Arc<GraphIndexArtifact>>,
) -> Result<String, McpHandlerError> {
    let Some(as_of) = as_of else {
        return Ok(root.to_owned());
    };
    let artifact = match artifact {
        Some(artifact) => Arc::clone(artifact),
        None => Arc::new(load_artifact(artifact_dir).map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to load graph artifact `{}`: {error}",
                artifact_dir.display()
            ))
        })?),
    };
    let commits = load_commit_index(worktree)?;
    resolve_symbol_as_of(&artifact, &commits, root, as_of)
}

fn resolve_symbol_as_of(
    artifact: &Arc<GraphIndexArtifact>,
    commits: &CommitIndexArtifact,
    symbol_id: &str,
    as_of: &str,
) -> Result<String, McpHandlerError> {
    if !commits.commits.iter().any(|commit| commit.sha == as_of) {
        return Err(McpHandlerError::InvalidParams(format!(
            "as_of commit `{as_of}` is not indexed"
        )));
    }

    let temporal_index = TemporalIndex::new(Arc::clone(artifact));
    let history = symbol_history(&temporal_index, commits, symbol_id);
    if history.is_empty() {
        return Err(McpHandlerError::NotFound(format!(
            "symbol {symbol_id} has no temporal history in graph artifact"
        )));
    }

    for (_, _, key) in history {
        match resolve_symbol_at_indexed(
            &temporal_index,
            commits,
            &key.stable_symbol_id,
            &key.commit,
            as_of,
        ) {
            Resolution::Found { value, .. } => return Ok(value),
            Resolution::Deleted { last_seen } => {
                return Err(McpHandlerError::NotFound(format!(
                    "symbol {symbol_id} was deleted before `{as_of}`; last seen at {}",
                    last_seen.commit
                )));
            }
            Resolution::Ambiguous { candidates } => {
                let candidates = candidates.join(", ");
                return Err(McpHandlerError::InvalidParams(format!(
                    "symbol {symbol_id} is ambiguous at `{as_of}`; candidates: {candidates}"
                )));
            }
            Resolution::Unknown { .. } => {}
        }
    }

    Err(McpHandlerError::NotFound(format!(
        "symbol {symbol_id} not present at commit `{as_of}`"
    )))
}

fn load_commit_index(worktree: &Path) -> Result<CommitIndexArtifact, McpHandlerError> {
    let pointer = spur_graph::store::commit_index::load_pointer(worktree).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index pointer in {}: {error}",
            worktree.display()
        ))
    })?;
    let pointer = pointer.ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "commit index not found; run `spur graph build --history` in {}",
            worktree.display()
        ))
    })?;
    spur_graph::store::commit_index::load_artifact(worktree, &pointer).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to load commit index artifact in {}: {error}",
            worktree.display()
        ))
    })
}

pub(super) fn current_worktree() -> Result<PathBuf, McpHandlerError> {
    if let Some(worktree) = spur_graph::mcp::scoped_worktree_root() {
        return Ok(worktree);
    }
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}
