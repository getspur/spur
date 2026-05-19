use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::str;

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::extract::GraphFacts;
use crate::validation::compute_anchor_hash;
use crate::{
    GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
    GraphIndexHeader, GraphNode, GraphSymbolArtifact, GraphTombstoneEntry, NodeKind, RelationKind,
    SourceSpan,
};

pub const PHASE1_GRAPH_INDEX_VERSION: &str = "spur-graph-phase2";
pub const SCHEMA_VERSION: &str = "spur-graph-schema-v4";
pub const EXTRACTOR_VERSION: &str = "2026-05-16-persisted-edges-v3";

const TAG_QUERY_BYTES: &[&[u8]] = &[
    include_bytes!("../../queries/markdown/tags.scm"),
    include_bytes!("../../queries/python/tags.scm"),
    include_bytes!("../../queries/rust/tags.scm"),
    include_bytes!("../../queries/typescript/tags.scm"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone)]
struct FileBucket {
    file: GraphFileArtifact,
    manifest: GraphFileManifestEntry,
    symbols: Vec<GraphSymbolArtifact>,
    edges: Vec<GraphEdgeArtifact>,
}

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

pub fn current_manifest_version() -> String {
    let mut hasher = Sha256::new();
    hasher.update(SCHEMA_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(EXTRACTOR_VERSION.as_bytes());
    hasher.update([0]);
    for bytes in TAG_QUERY_BYTES {
        hasher.update(bytes);
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

pub fn artifact_from_facts(
    facts: &GraphFacts,
    worktree_root: &Path,
) -> anyhow::Result<GraphIndexArtifact> {
    build_artifact_from_facts_and_stats(facts, worktree_root, None)
}

pub fn artifact_from_facts_incremental(
    prev: &GraphIndexArtifact,
    root: &Path,
) -> anyhow::Result<(GraphIndexArtifact, BuildMode)> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let manifest_version = current_manifest_version();
    if prev.manifest_version != manifest_version {
        tracing::info!(
            expected = %manifest_version,
            found = %prev.manifest_version,
            "spur-graph: full rebuild selected: manifest_version changed"
        );
        let facts = crate::build_facts(&root)?.0;
        return Ok((artifact_from_facts(&facts, &root)?, BuildMode::Full));
    }

    // TODO(graph-incr-rewrite): replace this conservative rebuild with content_oid
    // manifest diffing once git/fs discovery is wired into the builder.
    let facts = crate::build_facts(&root)?.0;
    let mut artifact = artifact_from_facts(&facts, &root)?;
    artifact.manifest_version = manifest_version;
    Ok((artifact, BuildMode::Incremental))
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

fn build_artifact_from_facts_and_stats(
    facts: &GraphFacts,
    worktree_root: &Path,
    _known_stats: Option<&BTreeMap<String, (u128, u64)>>,
) -> anyhow::Result<GraphIndexArtifact> {
    let spans_by_id: HashMap<_, _> = facts
        .spans
        .iter()
        .map(|span| (span.span_id, span))
        .collect();
    let nodes_by_id: HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect();

    let mut buckets: BTreeMap<String, FileBucket> = BTreeMap::new();
    for node in &facts.nodes {
        let Some(span_id) = node.source_span_id else {
            continue;
        };
        let Some(span) = spans_by_id.get(&span_id).copied() else {
            continue;
        };

        match node.kind {
            NodeKind::File => {
                let stable_file_id = node.stable_key.clone();
                buckets
                    .entry(node.label.clone())
                    .or_insert_with(|| FileBucket {
                        file: GraphFileArtifact {
                            stable_file_id: stable_file_id.clone(),
                            file_path: node.label.clone(),
                        },
                        manifest: GraphFileManifestEntry {
                            stable_file_id,
                            path: node.label.clone(),
                            // TODO(graph-incr-rewrite): populate from git/fs content discovery.
                            content_oid: String::new(),
                            node_ids: vec![node.node_id],
                        },
                        symbols: Vec::new(),
                        edges: Vec::new(),
                    });
            }
            NodeKind::Module
            | NodeKind::Function
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Impl
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::Method
            | NodeKind::TypeAlias
            | NodeKind::Section => {
                let file_path = file_path_for_span(facts, span).unwrap_or_default();
                let anchor_hash = anchor_hash(worktree_root, &file_path, span);
                let symbol = GraphSymbolArtifact {
                    stable_symbol_id: node.stable_key.clone(),
                    file_path: file_path.clone(),
                    byte_range: [span.start_byte as usize, span.end_byte as usize],
                    line_range: [span.start_line as usize, span.end_line as usize],
                    entity_name: symbol_entity_name(&node.label),
                    symbol_kind: symbol_kind(node.kind).to_string(),
                    anchor_hash,
                    enclosing_scope: enclosing_scope(facts, &nodes_by_id, node),
                };
                let entry = buckets.entry(file_path.clone()).or_insert_with(|| {
                    let stable_file_id = stable_file_id_from_path(&file_path);
                    FileBucket {
                        file: GraphFileArtifact {
                            stable_file_id: stable_file_id.clone(),
                            file_path: file_path.clone(),
                        },
                        manifest: GraphFileManifestEntry {
                            stable_file_id,
                            path: file_path.clone(),
                            // TODO(graph-incr-rewrite): populate from git/fs content discovery.
                            content_oid: String::new(),
                            node_ids: Vec::new(),
                        },
                        symbols: Vec::new(),
                        edges: Vec::new(),
                    }
                });
                entry.manifest.node_ids.push(node.node_id);
                entry.symbols.push(symbol);
            }
            _ => {}
        }
    }

    for edge in &facts.edges {
        let Some(source_node) = nodes_by_id.get(&edge.source_node_id).copied() else {
            tracing::warn!(
                source_node_id = edge.source_node_id.get(),
                "spur-graph: dropping edge with unknown source node"
            );
            continue;
        };
        let Some(source_file_path) = node_file_path(facts, source_node, &spans_by_id) else {
            tracing::warn!(
                source_stable_key = %source_node.stable_key,
                source_node_id = edge.source_node_id.get(),
                relation = %relation_discriminator(edge.relation),
                "spur-graph: dropping edge with unknown source file path"
            );
            continue;
        };
        let target_stable_symbol_id = edge
            .target_node_id
            .and_then(|target_node_id| nodes_by_id.get(&target_node_id))
            .map(|node| node.stable_key.clone());
        let entry = buckets.entry(source_file_path.clone()).or_insert_with(|| {
            let stable_file_id = stable_file_id_from_path(&source_file_path);
            FileBucket {
                file: GraphFileArtifact {
                    stable_file_id: stable_file_id.clone(),
                    file_path: source_file_path.clone(),
                },
                manifest: GraphFileManifestEntry {
                    stable_file_id,
                    path: source_file_path.clone(),
                    // TODO(graph-incr-rewrite): populate from git/fs content discovery.
                    content_oid: String::new(),
                    node_ids: Vec::new(),
                },
                symbols: Vec::new(),
                edges: Vec::new(),
            }
        });
        entry.edges.push(GraphEdgeArtifact {
            source_stable_symbol_id: source_node.stable_key.clone(),
            target_stable_symbol_id,
            target_label: edge.target_label.clone(),
            relation: edge.relation,
            confidence: edge.confidence,
            confidence_score: edge.confidence_score,
        });
    }

    for bucket in buckets.values_mut() {
        bucket.manifest.node_ids.sort_by_key(|id| id.get());
        bucket.symbols.sort_by(|a, b| {
            a.byte_range
                .cmp(&b.byte_range)
                .then(a.entity_name.cmp(&b.entity_name))
                .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
        });
        bucket
            .edges
            .sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));
    }

    rebind_cross_file_edges(&mut buckets);
    Ok(rebuild_from_buckets(buckets, current_manifest_version()))
}

fn rebind_cross_file_edges(buckets: &mut BTreeMap<String, FileBucket>) {
    let mut symbols_by_entity_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for bucket in buckets.values() {
        for symbol in &bucket.symbols {
            symbols_by_entity_name
                .entry(symbol.entity_name.clone())
                .or_default()
                .push(symbol.stable_symbol_id.clone());
        }
    }

    for bucket in buckets.values_mut() {
        for edge in &mut bucket.edges {
            let Some(target_label) = edge.target_label.as_deref() else {
                continue;
            };
            if edge.relation == RelationKind::Links {
                continue;
            }
            let Some(matches) = symbols_by_entity_name.get(target_label) else {
                edge.target_stable_symbol_id = None;
                continue;
            };
            let resolved = matches
                .iter()
                .map(String::as_str)
                .min()
                .expect("matches is non-empty");
            if matches.len() > 1 {
                tracing::warn!(
                    target_label = target_label,
                    candidates = matches.len(),
                    resolved_stable_symbol_id = resolved,
                    "spur-graph: ambiguous cross-file target_label; resolved to lexicographically smallest stable_symbol_id"
                );
            }
            edge.target_stable_symbol_id = Some(resolved.to_string());
        }
    }
}

fn rebuild_from_buckets(
    mut buckets: BTreeMap<String, FileBucket>,
    manifest_version: String,
) -> GraphIndexArtifact {
    let mut files = Vec::new();
    let mut symbols = Vec::new();
    let mut manifests = Vec::new();
    let mut edges = Vec::new();

    for bucket in buckets.values_mut() {
        bucket.symbols.sort_by(|a, b| {
            a.byte_range
                .cmp(&b.byte_range)
                .then(a.entity_name.cmp(&b.entity_name))
                .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
        });
        bucket.manifest.node_ids.sort_by_key(|id| id.get());
        bucket
            .edges
            .sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));
        files.push(bucket.file.clone());
        symbols.extend(bucket.symbols.clone());
        manifests.push(bucket.manifest.clone());
        edges.extend(bucket.edges.clone());
    }

    files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    symbols.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.byte_range.cmp(&b.byte_range))
            .then(a.entity_name.cmp(&b.entity_name))
            .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
    });
    manifests.sort_by(|a, b| a.path.cmp(&b.path));
    edges.sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));

    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: PHASE1_GRAPH_INDEX_VERSION.to_string(),
            // Content hash is stamped at write time, after body serialization input is finalized.
            content_hash_blake3: None,
        },
        manifest_version,
        // TODO(graph-incr-rewrite): populate from computed manifest entries.
        graph_content_hash: String::new(),
        file_manifests: manifests,
        files,
        symbols,
        edges,
        // TODO(graph-incr-rewrite): populate from value-level manifest deletions.
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
    }
}

fn edge_sort_key(edge: &GraphEdgeArtifact) -> (&str, &str, &str, &str) {
    (
        edge.source_stable_symbol_id.as_str(),
        edge.target_stable_symbol_id.as_deref().unwrap_or(""),
        relation_discriminator(edge.relation),
        edge.target_label.as_deref().unwrap_or(""),
    )
}

fn relation_discriminator(relation: RelationKind) -> &'static str {
    match relation {
        RelationKind::Imports => "imports",
        RelationKind::Calls => "calls",
        RelationKind::Contains => "contains",
        RelationKind::Implements => "implements",
        RelationKind::Defines => "defines",
        RelationKind::References => "references",
        RelationKind::Uses => "uses",
        RelationKind::Extends => "extends",
        RelationKind::Links => "links",
    }
}

fn node_file_path(
    facts: &GraphFacts,
    node: &GraphNode,
    spans_by_id: &HashMap<crate::SpanId, &SourceSpan>,
) -> Option<String> {
    if node.kind == NodeKind::File {
        return Some(node.label.clone());
    }
    let span_id = node.source_span_id?;
    let span = spans_by_id.get(&span_id).copied()?;
    file_path_for_span(facts, span)
}

fn file_path_for_span(facts: &GraphFacts, span: &SourceSpan) -> Option<String> {
    facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.file_id == Some(span.file_id))
        .map(|node| node.label.clone())
}

fn stable_file_id_from_path(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    format!(
        "{:016x}",
        u64::from_be_bytes(hasher.finalize()[..8].try_into().unwrap())
    )
}

fn anchor_hash(root: &Path, file_path: &str, span: &SourceSpan) -> String {
    let full_path = root.join(file_path);
    let bytes = match fs::read(&full_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(
                file_path,
                full_path = %full_path.display(),
                error = %error,
                "spur-graph anchor hash fallback to sentinel: source read failed"
            );
            return "0".to_string();
        }
    };
    let start = span.start_byte as usize;
    let end = span.end_byte as usize;
    let Some(bytes) = bytes.get(start..end) else {
        tracing::warn!(
            file_path,
            full_path = %full_path.display(),
            span_start = start,
            span_end = end,
            "spur-graph anchor hash fallback to sentinel: byte range mismatch"
        );
        return "0".to_string();
    };
    let slice = match str::from_utf8(bytes) {
        Ok(slice) => slice,
        Err(_) => {
            tracing::warn!(
                file_path,
                full_path = %full_path.display(),
                span_start = start,
                span_end = end,
                "spur-graph anchor hash fallback to sentinel: UTF-8 boundary mismatch"
            );
            return "0".to_string();
        }
    };
    compute_anchor_hash(slice).to_string()
}

fn symbol_kind(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::Function => "function",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Struct => "struct",
        NodeKind::Impl => "impl",
        NodeKind::Trait => "trait",
        NodeKind::Enum => "enum",
        NodeKind::Method => "method",
        NodeKind::TypeAlias => "type_alias",
        NodeKind::Section => "section",
        _ => "symbol",
    }
}

fn symbol_entity_name(label: &str) -> String {
    label.strip_prefix("impl ").unwrap_or(label).to_string()
}

fn enclosing_scope(
    facts: &GraphFacts,
    nodes_by_id: &HashMap<crate::NodeId, &GraphNode>,
    node: &GraphNode,
) -> Option<String> {
    facts
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::Contains && edge.target_node_id == Some(node.node_id)
        })
        .and_then(|edge| nodes_by_id.get(&edge.source_node_id).copied())
        .and_then(|parent| match parent.kind {
            NodeKind::File => None,
            NodeKind::Impl => Some(format!("impl {}", parent.label)),
            _ => Some(parent.label.clone()),
        })
}
