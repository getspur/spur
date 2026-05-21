use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::{
    GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
    GraphSymbolArtifact, GraphTombstoneEntry,
};

#[derive(serde::Serialize)]
struct GraphArtifactBodyForHash<'a> {
    files: &'a [GraphFileArtifact],
    symbols: &'a [GraphSymbolArtifact],
    edges: &'a [GraphEdgeArtifact],
    file_manifests: &'a [GraphFileManifestEntry],
    graph_content_hash: &'a str,
    manifest_version: &'a str,
    tombstones: &'a [GraphTombstoneEntry],
}

pub fn write_artifact(artifact: &GraphIndexArtifact, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let mut artifact_with_hash = artifact.clone();
    artifact_with_hash.header.content_hash_blake3 = Some(
        artifact_content_hash_blake3_hex(artifact)
            .context("failed to compute graph artifact content hash")?,
    );
    let json = serde_json::to_string_pretty(&artifact_with_hash)
        .context("failed to encode graph artifact")?;
    fs::write(path, json).with_context(|| format!("failed to write `{}`", path.display()))
}

fn artifact_content_hash_blake3_hex(artifact: &GraphIndexArtifact) -> anyhow::Result<String> {
    let body = GraphArtifactBodyForHash {
        files: &artifact.files,
        symbols: &artifact.symbols,
        edges: &artifact.edges,
        file_manifests: &artifact.file_manifests,
        graph_content_hash: &artifact.graph_content_hash,
        manifest_version: &artifact.manifest_version,
        tombstones: &artifact.tombstones,
    };
    let canonical_json = serde_json::to_vec(&body)
        .context("failed to encode graph artifact body for content hash")?;
    Ok(blake3::hash(&canonical_json).to_hex().to_string())
}
