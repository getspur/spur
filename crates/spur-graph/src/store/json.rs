use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::discovery::discover_files;
use crate::extract::{build_facts_for_paths, GraphFacts};
use crate::validation::compute_anchor_hash;
use crate::{
    GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact,
    GraphIndexHeader, GraphNode, GraphSymbolArtifact, NodeKind, RelationKind, SourceSpan,
};

pub const PHASE1_GRAPH_INDEX_VERSION: &str = "spur-graph-phase2";
pub const SCHEMA_VERSION: &str = "spur-graph-schema-v3";
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

    let allowed_extensions = crate::extract::languages::all_supported_extensions();
    let discovered_paths = discover_files(&root, &allowed_extensions)?;
    let path_set: HashSet<String> = discovered_paths
        .iter()
        .filter_map(|p| p.strip_prefix(&root).ok())
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    let mut discovered_meta = BTreeMap::new();
    for path in discovered_paths {
        let relative = path
            .strip_prefix(&root)
            .expect("discovered path is rooted")
            .to_string_lossy()
            .replace('\\', "/");
        let meta = fs::metadata(&path)
            .with_context(|| format!("failed to read metadata for `{}`", path.display()))?;
        let mtime_nanos = meta
            .modified()
            .ok()
            .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|dur| dur.as_nanos())
            .unwrap_or(0);
        discovered_meta.insert(relative, (mtime_nanos, meta.len()));
    }

    let prev_manifest_by_path: HashMap<_, _> = prev
        .file_manifests
        .iter()
        .map(|m| (m.path.as_str(), m))
        .collect();
    let changed_paths: Vec<String> = discovered_meta
        .iter()
        .filter_map(|(path, (mtime_nanos, size_bytes))| {
            let changed = prev_manifest_by_path
                .get(path.as_str())
                .map(|m| m.mtime_nanos != *mtime_nanos || m.size_bytes != *size_bytes)
                .unwrap_or(true);
            changed.then_some(path.clone())
        })
        .collect();

    let mut cached_buckets = buckets_from_artifact(prev);
    cached_buckets.retain(|path, _| path_set.contains(path));

    if changed_paths.is_empty() {
        return Ok((
            rebuild_from_buckets(cached_buckets, manifest_version),
            BuildMode::Incremental,
        ));
    }

    let changed_full_paths: Vec<PathBuf> = changed_paths.iter().map(|p| root.join(p)).collect();
    let changed_facts = build_facts_for_paths(&root, &changed_full_paths)?;
    let changed_artifact =
        build_artifact_from_facts_and_stats(&changed_facts, &root, Some(&discovered_meta))?;
    for (path, bucket) in buckets_from_artifact(&changed_artifact) {
        cached_buckets.insert(path, bucket);
    }

    Ok((
        rebuild_from_buckets(cached_buckets, manifest_version),
        BuildMode::Incremental,
    ))
}

pub fn write_artifact(artifact: &GraphIndexArtifact, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(artifact).context("failed to encode graph artifact")?;
    fs::write(path, json).with_context(|| format!("failed to write `{}`", path.display()))
}

fn build_artifact_from_facts_and_stats(
    facts: &GraphFacts,
    worktree_root: &Path,
    known_stats: Option<&BTreeMap<String, (u128, u64)>>,
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
                let stats = if let Some(stats_map) = known_stats {
                    stats_map.get(&node.label).copied().unwrap_or((0, 0))
                } else {
                    file_stats(worktree_root, &node.label)
                };
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
                            mtime_nanos: stats.0,
                            size_bytes: stats.1,
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
                    let stats = if let Some(stats_map) = known_stats {
                        stats_map.get(&file_path).copied().unwrap_or((0, 0))
                    } else {
                        file_stats(worktree_root, &file_path)
                    };
                    FileBucket {
                        file: GraphFileArtifact {
                            stable_file_id: stable_file_id.clone(),
                            file_path: file_path.clone(),
                        },
                        manifest: GraphFileManifestEntry {
                            stable_file_id,
                            path: file_path.clone(),
                            mtime_nanos: stats.0,
                            size_bytes: stats.1,
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
        let target_stable_symbol_id = nodes_by_id
            .get(&edge.target_node_id)
            .map(|node| node.stable_key.clone());
        let entry = buckets.entry(source_file_path.clone()).or_insert_with(|| {
            let stable_file_id = stable_file_id_from_path(&source_file_path);
            let stats = if let Some(stats_map) = known_stats {
                stats_map.get(&source_file_path).copied().unwrap_or((0, 0))
            } else {
                file_stats(worktree_root, &source_file_path)
            };
            FileBucket {
                file: GraphFileArtifact {
                    stable_file_id: stable_file_id.clone(),
                    file_path: source_file_path.clone(),
                },
                manifest: GraphFileManifestEntry {
                    stable_file_id,
                    path: source_file_path.clone(),
                    mtime_nanos: stats.0,
                    size_bytes: stats.1,
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

    Ok(rebuild_from_buckets(buckets, current_manifest_version()))
}

fn buckets_from_artifact(artifact: &GraphIndexArtifact) -> BTreeMap<String, FileBucket> {
    let mut manifests_by_path: HashMap<_, _> = artifact
        .file_manifests
        .iter()
        .cloned()
        .map(|m| (m.path.clone(), m))
        .collect();
    let mut files_by_path: HashMap<_, _> = artifact
        .files
        .iter()
        .cloned()
        .map(|f| (f.file_path.clone(), f))
        .collect();

    let mut by_path: BTreeMap<String, FileBucket> = BTreeMap::new();
    for symbol in &artifact.symbols {
        by_path
            .entry(symbol.file_path.clone())
            .or_insert_with(|| FileBucket {
                file: files_by_path.remove(&symbol.file_path).unwrap_or_else(|| {
                    GraphFileArtifact {
                        stable_file_id: stable_file_id_from_path(&symbol.file_path),
                        file_path: symbol.file_path.clone(),
                    }
                }),
                manifest: manifests_by_path
                    .remove(&symbol.file_path)
                    .unwrap_or_else(|| GraphFileManifestEntry {
                        stable_file_id: stable_file_id_from_path(&symbol.file_path),
                        path: symbol.file_path.clone(),
                        mtime_nanos: 0,
                        size_bytes: 0,
                        node_ids: Vec::new(),
                    }),
                symbols: Vec::new(),
                edges: Vec::new(),
            })
            .symbols
            .push(symbol.clone());
    }

    let symbol_file_by_stable_id: HashMap<_, _> = artifact
        .symbols
        .iter()
        .map(|symbol| (symbol.stable_symbol_id.clone(), symbol.file_path.clone()))
        .collect();
    let file_path_by_stable_file_id: HashMap<_, _> = artifact
        .files
        .iter()
        .map(|file| (file.stable_file_id.clone(), file.file_path.clone()))
        .collect();
    let stable_file_id_by_path: HashMap<_, _> = artifact
        .files
        .iter()
        .map(|file| (file.file_path.clone(), file.stable_file_id.clone()))
        .collect();
    for edge in &artifact.edges {
        let source_path = symbol_file_by_stable_id
            .get(&edge.source_stable_symbol_id)
            .cloned()
            .or_else(|| {
                file_path_by_stable_file_id
                    .get(&edge.source_stable_symbol_id)
                    .cloned()
            });
        let Some(source_path) = source_path else {
            tracing::warn!(
                source_stable_symbol_id = %edge.source_stable_symbol_id,
                "spur-graph: dropping artifact edge with unknown source stable id"
            );
            continue;
        };
        by_path
            .entry(source_path.clone())
            .or_insert_with(|| FileBucket {
                file: GraphFileArtifact {
                    stable_file_id: stable_file_id_by_path
                        .get(&source_path)
                        .cloned()
                        .unwrap_or_else(|| stable_file_id_from_path(&source_path)),
                    file_path: source_path.clone(),
                },
                manifest: GraphFileManifestEntry {
                    stable_file_id: stable_file_id_by_path
                        .get(&source_path)
                        .cloned()
                        .unwrap_or_else(|| stable_file_id_from_path(&source_path)),
                    path: source_path.clone(),
                    mtime_nanos: 0,
                    size_bytes: 0,
                    node_ids: Vec::new(),
                },
                symbols: Vec::new(),
                edges: Vec::new(),
            })
            .edges
            .push(edge.clone());
    }

    for (path, file) in files_by_path {
        by_path.entry(path.clone()).or_insert_with(|| FileBucket {
            manifest: manifests_by_path
                .remove(&path)
                .unwrap_or_else(|| GraphFileManifestEntry {
                    stable_file_id: file.stable_file_id.clone(),
                    path: path.clone(),
                    mtime_nanos: 0,
                    size_bytes: 0,
                    node_ids: Vec::new(),
                }),
            file,
            symbols: Vec::new(),
            edges: Vec::new(),
        });
    }

    by_path
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
        },
        manifest_version,
        file_manifests: manifests,
        files,
        symbols,
        edges,
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

fn file_stats(root: &Path, file_path: &str) -> (u128, u64) {
    let full_path = root.join(file_path);
    let Ok(meta) = fs::metadata(&full_path) else {
        return (0, 0);
    };
    let mtime_nanos = meta
        .modified()
        .ok()
        .and_then(|mtime| mtime.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|dur| dur.as_nanos())
        .unwrap_or(0);
    (mtime_nanos, meta.len())
}

fn anchor_hash(root: &Path, file_path: &str, span: &SourceSpan) -> String {
    let full_path = root.join(file_path);
    let content = match fs::read_to_string(&full_path) {
        Ok(content) => content,
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
    let slice = match content.get(start..end) {
        Some(slice) => slice,
        None => {
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
        .find(|edge| edge.relation == RelationKind::Contains && edge.target_node_id == node.node_id)
        .and_then(|edge| nodes_by_id.get(&edge.source_node_id).copied())
        .and_then(|parent| match parent.kind {
            NodeKind::File => None,
            NodeKind::Impl => Some(format!("impl {}", parent.label)),
            _ => Some(parent.label.clone()),
        })
}
