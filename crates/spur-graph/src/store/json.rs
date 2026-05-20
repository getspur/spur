use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::str;

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::content_hash::{compute_graph_content_hash, git_blob_oid};
use crate::discovery::discover_files;
use crate::extract::GraphFacts;
use crate::extract::{build_facts_for_paths, languages::all_supported_extensions};
use crate::validation::compute_anchor_hash;
use crate::{
    git, DirtyEntry, GitCtx, GraphEdgeArtifact, GraphFileArtifact, GraphFileManifestEntry,
    GraphIndexArtifact, GraphIndexHeader, GraphNode, GraphSymbolArtifact, GraphTombstoneEntry,
    NodeKind, RelationKind, SourceSpan,
};

pub const PHASE1_GRAPH_INDEX_VERSION: &str = "spur-graph-phase2";
pub const SCHEMA_VERSION: &str = "spur-graph-schema-v5";
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentFileEntry {
    path: String,
    content_oid: String,
    extractable: bool,
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
    let worktree_root = worktree_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", worktree_root.display()))?;
    let current_entries = discover_current_entries(&worktree_root)?;
    let buckets = buckets_from_facts(facts, &worktree_root, &current_entries)?;
    Ok(compose_artifact(
        buckets,
        &current_entries,
        current_manifest_version(),
        Vec::new(),
    ))
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

    let current_entries = discover_current_entries(&root)?;
    let prev_buckets = buckets_from_artifact(prev);
    let prev_content_oids: BTreeMap<_, _> = prev
        .file_manifests
        .iter()
        .map(|entry| (entry.path.as_str(), entry.content_oid.as_str()))
        .collect();

    let mut buckets = BTreeMap::new();
    let mut changed_paths = Vec::new();
    for current in current_entries.values() {
        if prev_content_oids
            .get(current.path.as_str())
            .is_some_and(|content_oid| *content_oid == current.content_oid)
        {
            if let Some(bucket) = prev_buckets.get(&current.path) {
                buckets.insert(current.path.clone(), bucket.clone());
                continue;
            }
        }
        if current.extractable {
            changed_paths.push(root.join(&current.path));
        }
    }

    if !changed_paths.is_empty() {
        let changed_facts = build_facts_for_paths(&root, &changed_paths)?;
        let changed_buckets = buckets_from_facts(&changed_facts, &root, &current_entries)?;
        buckets.extend(changed_buckets);
    }

    add_missing_manifest_buckets(&mut buckets, &current_entries);

    let tombstones = tombstones_from_removed_paths(prev, &current_entries);
    let artifact = compose_artifact(buckets, &current_entries, manifest_version, tombstones);
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

fn buckets_from_facts(
    facts: &GraphFacts,
    worktree_root: &Path,
    current_entries: &BTreeMap<String, CurrentFileEntry>,
) -> anyhow::Result<BTreeMap<String, FileBucket>> {
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
    let parent_by_target = parent_by_target(facts);

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
                if !current_entries.contains_key(&node.label) {
                    continue;
                }
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
                            content_oid: content_oid_for(current_entries, &node.label),
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
                if !current_entries.contains_key(&file_path) {
                    continue;
                }
                let anchor_hash = anchor_hash(worktree_root, &file_path, span);
                let symbol = GraphSymbolArtifact {
                    stable_symbol_id: node.stable_key.clone(),
                    file_path: file_path.clone(),
                    byte_range: [span.start_byte as usize, span.end_byte as usize],
                    line_range: [span.start_line as usize, span.end_line as usize],
                    entity_name: symbol_entity_name(&node.label),
                    qualified_name: qualified_name(&parent_by_target, &nodes_by_id, node),
                    symbol_kind: symbol_kind(node.kind).to_string(),
                    anchor_hash,
                    enclosing_scope: enclosing_scope(&parent_by_target, &nodes_by_id, node),
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
                            content_oid: content_oid_for(current_entries, &file_path),
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
        if !current_entries.contains_key(&source_file_path) {
            continue;
        }
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
                    content_oid: content_oid_for(current_entries, &source_file_path),
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

    Ok(buckets)
}

fn discover_current_entries(root: &Path) -> anyhow::Result<BTreeMap<String, CurrentFileEntry>> {
    let allowed_extensions = all_supported_extensions();
    if let Some(ctx) = git::detect(root) {
        discover_git_entries(root, &ctx, &allowed_extensions)
    } else {
        discover_fs_entries(root, &allowed_extensions)
    }
}

fn discover_git_entries(
    root: &Path,
    _ctx: &GitCtx,
    allowed_extensions: &[&str],
) -> anyhow::Result<BTreeMap<String, CurrentFileEntry>> {
    let dirty_entries = git::status_dirty_paths(root)?;
    let dirty_paths: BTreeMap<String, DirtyEntry> = dirty_entries
        .into_iter()
        .filter(|entry| is_supported_path(&entry.path, allowed_extensions))
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let mut entries = BTreeMap::new();

    for tracked in git::ls_files_with_oids(root)? {
        if !is_supported_path(&tracked.path, allowed_extensions) {
            continue;
        }

        let content_oid = if tracked.is_gitlink {
            tracked.content_oid
        } else if dirty_paths.contains_key(&tracked.path) {
            let Some(content_oid) = read_worktree_content_oid(root, &tracked.path)? else {
                continue;
            };
            content_oid
        } else {
            tracked.content_oid
        };
        entries.insert(
            tracked.path.clone(),
            CurrentFileEntry {
                path: tracked.path,
                content_oid,
                extractable: !tracked.is_gitlink,
            },
        );
    }

    for dirty in dirty_paths.values() {
        if entries.contains_key(&dirty.path) {
            continue;
        }
        let Some(content_oid) = read_worktree_content_oid(root, &dirty.path)? else {
            continue;
        };
        entries.insert(
            dirty.path.clone(),
            CurrentFileEntry {
                path: dirty.path.clone(),
                content_oid,
                extractable: true,
            },
        );
    }

    Ok(entries)
}

fn discover_fs_entries(
    root: &Path,
    allowed_extensions: &[&str],
) -> anyhow::Result<BTreeMap<String, CurrentFileEntry>> {
    let mut entries = BTreeMap::new();
    for path in discover_files(root, allowed_extensions)? {
        let relative_path = relative_path(root, &path)?;
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read `{}`", path.display()))?;
        entries.insert(
            relative_path.clone(),
            CurrentFileEntry {
                path: relative_path,
                content_oid: git_blob_oid(&bytes),
                extractable: true,
            },
        );
    }
    Ok(entries)
}

fn read_worktree_content_oid(root: &Path, path: &str) -> anyhow::Result<Option<String>> {
    match fs::read(root.join(path)) {
        Ok(bytes) => Ok(Some(git_blob_oid(&bytes))),
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::IsADirectory) => Ok(None),
        Err(err) => {
            Err(err).with_context(|| format!("failed to read `{}`", root.join(path).display()))
        }
    }
}

fn is_supported_path(path: &str, allowed_extensions: &[&str]) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            allowed_extensions
                .iter()
                .any(|allowed| extension.eq_ignore_ascii_case(allowed))
        })
}

fn relative_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path.strip_prefix(root).with_context(|| {
        format!(
            "failed to make `{}` relative to `{}`",
            path.display(),
            root.display()
        )
    })?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn content_oid_for(current_entries: &BTreeMap<String, CurrentFileEntry>, path: &str) -> String {
    current_entries
        .get(path)
        .map(|entry| entry.content_oid.clone())
        .unwrap_or_default()
}

fn buckets_from_artifact(artifact: &GraphIndexArtifact) -> BTreeMap<String, FileBucket> {
    let mut buckets = BTreeMap::new();
    for manifest in &artifact.file_manifests {
        let stable_file_id = manifest.stable_file_id.clone();
        let path = manifest.path.clone();
        buckets.insert(
            path.clone(),
            FileBucket {
                file: artifact
                    .files
                    .iter()
                    .find(|file| file.file_path == path)
                    .cloned()
                    .unwrap_or_else(|| GraphFileArtifact {
                        stable_file_id: stable_file_id.clone(),
                        file_path: path.clone(),
                    }),
                manifest: manifest.clone(),
                symbols: artifact
                    .symbols
                    .iter()
                    .filter(|symbol| symbol.file_path == path)
                    .cloned()
                    .collect(),
                edges: artifact
                    .edges
                    .iter()
                    .filter(|edge| {
                        edge.source_stable_symbol_id == stable_file_id
                            || artifact.symbols.iter().any(|symbol| {
                                symbol.file_path == path
                                    && symbol.stable_symbol_id == edge.source_stable_symbol_id
                            })
                    })
                    .cloned()
                    .collect(),
            },
        );
    }
    buckets
}

fn add_missing_manifest_buckets(
    buckets: &mut BTreeMap<String, FileBucket>,
    current_entries: &BTreeMap<String, CurrentFileEntry>,
) {
    for current in current_entries.values() {
        buckets
            .entry(current.path.clone())
            .or_insert_with(|| empty_bucket(&current.path, &current.content_oid));
    }
}

fn empty_bucket(path: &str, content_oid: &str) -> FileBucket {
    let stable_file_id = stable_file_id_from_path(path);
    FileBucket {
        file: GraphFileArtifact {
            stable_file_id: stable_file_id.clone(),
            file_path: path.to_string(),
        },
        manifest: GraphFileManifestEntry {
            stable_file_id,
            path: path.to_string(),
            content_oid: content_oid.to_string(),
            node_ids: Vec::new(),
        },
        symbols: Vec::new(),
        edges: Vec::new(),
    }
}

fn tombstones_from_removed_paths(
    prev: &GraphIndexArtifact,
    current_entries: &BTreeMap<String, CurrentFileEntry>,
) -> Vec<GraphTombstoneEntry> {
    let mut tombstones: Vec<_> = prev
        .file_manifests
        .iter()
        .filter(|entry| !current_entries.contains_key(&entry.path))
        .map(|entry| GraphTombstoneEntry {
            path: entry.path.clone(),
            stable_file_id: entry.stable_file_id.clone(),
        })
        .collect();
    tombstones.sort_by(|a, b| a.path.cmp(&b.path));
    tombstones
}

fn compose_artifact(
    mut buckets: BTreeMap<String, FileBucket>,
    current_entries: &BTreeMap<String, CurrentFileEntry>,
    manifest_version: String,
    tombstones: Vec<GraphTombstoneEntry>,
) -> GraphIndexArtifact {
    add_missing_manifest_buckets(&mut buckets, current_entries);
    rebind_cross_file_edges(&mut buckets);
    let graph_content_hash = compute_graph_content_hash(
        current_entries
            .values()
            .map(|entry| (entry.path.as_str(), entry.content_oid.as_str())),
    );
    rebuild_from_buckets(buckets, manifest_version, graph_content_hash, tombstones)
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
    graph_content_hash: String,
    tombstones: Vec<GraphTombstoneEntry>,
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
        graph_content_hash,
        file_manifests: manifests,
        files,
        symbols,
        edges,
        tombstones,
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

fn parent_by_target(facts: &GraphFacts) -> HashMap<crate::NodeId, crate::NodeId> {
    let mut parent_by_target = HashMap::new();
    for edge in &facts.edges {
        if edge.relation != RelationKind::Contains {
            continue;
        }
        let Some(target_node_id) = edge.target_node_id else {
            continue;
        };
        parent_by_target
            .entry(target_node_id)
            .or_insert(edge.source_node_id);
    }
    parent_by_target
}

fn containing_parent<'a>(
    parent_by_target: &HashMap<crate::NodeId, crate::NodeId>,
    nodes_by_id: &HashMap<crate::NodeId, &'a GraphNode>,
    node: &GraphNode,
) -> Option<&'a GraphNode> {
    parent_by_target
        .get(&node.node_id)
        .and_then(|parent_id| nodes_by_id.get(parent_id).copied())
}

fn qualified_name(
    parent_by_target: &HashMap<crate::NodeId, crate::NodeId>,
    nodes_by_id: &HashMap<crate::NodeId, &GraphNode>,
    node: &GraphNode,
) -> String {
    let mut segments = vec![symbol_entity_name(&node.label)];
    let mut current = node;
    let mut seen = HashSet::new();
    seen.insert(node.node_id);

    while let Some(parent) = containing_parent(parent_by_target, nodes_by_id, current) {
        if !seen.insert(parent.node_id) {
            break;
        }
        if parent.kind == NodeKind::File {
            break;
        }

        segments.push(qualified_scope_segment(parent));
        current = parent;
    }

    segments.reverse();
    segments.join("::")
}

fn qualified_scope_segment(node: &GraphNode) -> String {
    match node.kind {
        NodeKind::Impl => format!("impl {}", symbol_entity_name(&node.label)),
        _ => symbol_entity_name(&node.label),
    }
}

fn enclosing_scope(
    parent_by_target: &HashMap<crate::NodeId, crate::NodeId>,
    nodes_by_id: &HashMap<crate::NodeId, &GraphNode>,
    node: &GraphNode,
) -> Option<String> {
    containing_parent(parent_by_target, nodes_by_id, node).and_then(|parent| match parent.kind {
        NodeKind::File => None,
        NodeKind::Impl => Some(format!("impl {}", parent.label)),
        _ => Some(parent.label.clone()),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use super::{artifact_from_facts, artifact_from_facts_incremental, BuildMode};
    use crate::content_hash::{compute_graph_content_hash, git_blob_oid};
    use crate::extract::build_facts;

    #[test]
    fn incremental_reuses_bucket_when_content_oid_is_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
        fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").expect("write b.rs");

        let mut prev =
            artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("artifact");
        let b_content_oid = prev
            .file_manifests
            .iter()
            .find(|entry| entry.path == "src/b.rs")
            .expect("b manifest")
            .content_oid
            .clone();
        assert!(
            !b_content_oid.is_empty(),
            "full build must stamp content_oid"
        );

        let marker = "reused-bucket-marker".to_string();
        prev.symbols
            .iter_mut()
            .find(|symbol| symbol.file_path == "src/b.rs")
            .expect("b symbol")
            .enclosing_scope = Some(marker.clone());

        fs::write(root.join("src/a.rs"), "pub fn alpha_changed() {}\n").expect("rewrite a.rs");

        let (next, mode) = artifact_from_facts_incremental(&prev, root).expect("incremental");
        assert_eq!(mode, BuildMode::Incremental);
        assert_eq!(
            next.file_manifests
                .iter()
                .find(|entry| entry.path == "src/b.rs")
                .expect("next b manifest")
                .content_oid,
            b_content_oid
        );
        assert_eq!(
            next.symbols
                .iter()
                .find(|symbol| symbol.file_path == "src/b.rs")
                .expect("next b symbol")
                .enclosing_scope
                .as_deref(),
            Some(marker.as_str())
        );
        assert!(next
            .symbols
            .iter()
            .any(|symbol| symbol.file_path == "src/a.rs" && symbol.entity_name == "alpha_changed"));
    }

    #[test]
    fn incremental_uses_git_blob_oid_for_dirty_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        init_repo(root);
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
        git(root, &["add", "src/a.rs"]);
        git(root, &["commit", "-m", "add a"]);

        let prev =
            artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("artifact");

        let dirty_bytes = b"pub fn alpha_dirty() {}\n";
        fs::write(root.join("src/a.rs"), dirty_bytes).expect("dirty a.rs");

        let (next, mode) = artifact_from_facts_incremental(&prev, root).expect("incremental");
        assert_eq!(mode, BuildMode::Incremental);
        let expected_oid = git_blob_oid(dirty_bytes);
        assert_eq!(
            next.file_manifests
                .iter()
                .find(|entry| entry.path == "src/a.rs")
                .expect("dirty manifest")
                .content_oid,
            expected_oid
        );
        assert_eq!(
            next.graph_content_hash,
            compute_graph_content_hash([("src/a.rs", expected_oid.as_str())])
        );
    }

    #[test]
    fn incremental_emits_tombstone_for_removed_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
        fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").expect("write b.rs");

        let prev =
            artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("artifact");
        let removed_stable_file_id = prev
            .file_manifests
            .iter()
            .find(|entry| entry.path == "src/a.rs")
            .expect("a manifest")
            .stable_file_id
            .clone();

        fs::remove_file(root.join("src/a.rs")).expect("remove a.rs");
        let (next, mode) = artifact_from_facts_incremental(&prev, root).expect("incremental");

        assert_eq!(mode, BuildMode::Incremental);
        assert_eq!(next.tombstones.len(), 1);
        assert_eq!(next.tombstones[0].path, "src/a.rs");
        assert_eq!(next.tombstones[0].stable_file_id, removed_stable_file_id);
        assert!(!next
            .file_manifests
            .iter()
            .any(|entry| entry.path == "src/a.rs"));
    }

    fn init_repo(root: &Path) {
        git(root, &["init"]);
        git(root, &["config", "user.name", "Spur Test"]);
        git(root, &["config", "user.email", "spur@example.invalid"]);
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
