use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::str;
use std::time::Instant;

use anyhow::Context;
use sha2::{Digest, Sha256};

use crate::content_hash::{compute_graph_content_hash, git_blob_oid};
use crate::discovery::discover_files;
use crate::extract::GraphFacts;
use crate::extract::{build_facts_for_paths, languages::all_supported_extensions};
use crate::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use crate::validation::compute_anchor_hash;
use crate::{
    git, graph_edge_kind_or_default, DirtyEntry, GitCtx, GraphEdgeArtifact, GraphEdgeKind,
    GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphNode,
    GraphSymbolArtifact, GraphTombstoneEntry, NodeId, NodeKind, RelationKind, SourceSpan,
};

pub const SCHEMA_VERSION: &str = "spur-graph-schema-v6";
pub const EXTRACTOR_VERSION: &str = "2026-05-21-mcp-tool-registrations-v1";

#[derive(Debug, Clone, Copy)]
struct ManifestQueryBytes<'a> {
    language: &'a str,
    query: &'a str,
    bytes: &'a [u8],
}

const MANIFEST_QUERY_BYTES: &[ManifestQueryBytes<'static>] = &[
    ManifestQueryBytes {
        language: "cpp",
        query: "tags",
        bytes: include_bytes!("../../queries/cpp/tags.scm"),
    },
    ManifestQueryBytes {
        language: "cpp",
        query: "spur-edges",
        bytes: include_bytes!("../../queries/cpp/spur-edges.scm"),
    },
    ManifestQueryBytes {
        language: "markdown",
        query: "tags",
        bytes: include_bytes!("../../queries/markdown/tags.scm"),
    },
    ManifestQueryBytes {
        language: "markdown",
        query: "symbols",
        bytes: include_bytes!("../../queries/markdown/symbols.scm"),
    },
    ManifestQueryBytes {
        language: "markdown",
        query: "spur-edges",
        bytes: include_bytes!("../../queries/markdown/spur-edges.scm"),
    },
    ManifestQueryBytes {
        language: "markdown",
        query: "inline-spur-edges",
        bytes: include_bytes!("../../queries/markdown/inline-spur-edges.scm"),
    },
    ManifestQueryBytes {
        language: "python",
        query: "tags",
        bytes: include_bytes!("../../queries/python/tags.scm"),
    },
    ManifestQueryBytes {
        language: "python",
        query: "symbols",
        bytes: include_bytes!("../../queries/python/symbols.scm"),
    },
    ManifestQueryBytes {
        language: "python",
        query: "spur-edges",
        bytes: include_bytes!("../../queries/python/spur-edges.scm"),
    },
    ManifestQueryBytes {
        language: "rust",
        query: "tags",
        bytes: include_bytes!("../../queries/rust/tags.scm"),
    },
    ManifestQueryBytes {
        language: "rust",
        query: "symbols",
        bytes: include_bytes!("../../queries/rust/symbols.scm"),
    },
    ManifestQueryBytes {
        language: "rust",
        query: "spur-edges",
        bytes: include_bytes!("../../queries/rust/spur-edges.scm"),
    },
    ManifestQueryBytes {
        language: "typescript",
        query: "tags",
        bytes: include_bytes!("../../queries/typescript/tags.scm"),
    },
    ManifestQueryBytes {
        language: "typescript",
        query: "symbols",
        bytes: include_bytes!("../../queries/typescript/symbols.scm"),
    },
    ManifestQueryBytes {
        language: "typescript",
        query: "spur-edges",
        bytes: include_bytes!("../../queries/typescript/spur-edges.scm"),
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone)]
struct FileBucket {
    file: GraphFileArtifact,
    file_node_id: Option<NodeId>,
    manifest: GraphFileManifestEntry,
    symbols: Vec<GraphSymbolArtifact>,
    symbol_node_ids: Vec<NodeId>,
    edges: Vec<GraphEdgeArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CurrentFileEntry {
    path: String,
    content_oid: String,
    extractable: bool,
}

pub fn current_manifest_version() -> String {
    manifest_version_from_query_bytes(SCHEMA_VERSION, EXTRACTOR_VERSION, MANIFEST_QUERY_BYTES)
}

fn manifest_version_from_query_bytes(
    schema_version: &str,
    extractor_version: &str,
    query_bytes: &[ManifestQueryBytes<'_>],
) -> String {
    let mut hasher = Sha256::new();
    update_manifest_hash_field(&mut hasher, schema_version.as_bytes());
    update_manifest_hash_field(&mut hasher, extractor_version.as_bytes());
    let mut query_bytes_by_language = query_bytes.iter().collect::<Vec<_>>();
    query_bytes_by_language.sort_by_key(|query| (query.language, query.query));
    for query in query_bytes_by_language {
        update_manifest_hash_field(&mut hasher, query.language.as_bytes());
        update_manifest_hash_field(&mut hasher, query.query.as_bytes());
        update_manifest_hash_field(&mut hasher, query.bytes);
    }
    format!("{:x}", hasher.finalize())
}

fn update_manifest_hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub fn artifact_from_facts(
    facts: &GraphFacts,
    worktree_root: &Path,
) -> anyhow::Result<GraphIndexArtifact> {
    let build_started = Instant::now();
    let worktree_root = worktree_root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", worktree_root.display()))?;
    let discover_started = Instant::now();
    let discover_span = tracing::info_span!(
        target: "spur_graph::build::discover",
        "discover_current_entries",
        root = %worktree_root.display(),
        files = tracing::field::Empty
    );
    let current_entries = {
        let _entered = discover_span.enter();
        let result = discover_current_entries(&worktree_root);
        match &result {
            Ok(entries) => {
                discover_span.record("files", entries.len());
                tracing::info!(
                    target: "spur_graph::build::discover",
                    files = entries.len(),
                    elapsed_ms = elapsed_ms(discover_started),
                    "spur-graph build phase completed"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "spur_graph::build::discover",
                    error = %error,
                    elapsed_ms = elapsed_ms(discover_started),
                    "spur-graph build phase failed"
                );
            }
        }
        result?
    };
    let buckets = buckets_from_facts(facts, &worktree_root, &current_entries)?;
    let compose_started = Instant::now();
    let compose_span = tracing::info_span!(
        target: "spur_graph::build::compose",
        "compose_artifact",
        files = current_entries.len(),
        buckets = buckets.len()
    );
    let artifact = {
        let _entered = compose_span.enter();
        let artifact = compose_artifact(
            buckets,
            &current_entries,
            current_manifest_version(),
            Vec::new(),
        );
        tracing::info!(
            target: "spur_graph::build::compose",
            files = artifact.files.len(),
            symbols = artifact.symbols.len(),
            edges = artifact.edges.len(),
            tombstones = artifact.tombstones.len(),
            elapsed_ms = elapsed_ms(compose_started),
            "spur-graph build phase completed"
        );
        artifact
    };
    tracing::info!(
        target: "spur_graph::build",
        mode = "Full",
        files = artifact.files.len(),
        symbols = artifact.symbols.len(),
        edges = artifact.edges.len(),
        changed = 0_u64,
        elapsed_ms = elapsed_ms(build_started),
        "spur-graph build completed"
    );
    Ok(artifact)
}

pub fn artifact_from_facts_incremental(
    prev: &GraphIndexArtifact,
    root: &Path,
) -> anyhow::Result<(GraphIndexArtifact, BuildMode)> {
    let build_started = Instant::now();
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
        let extract_started = Instant::now();
        let extract_span = tracing::info_span!(
            target: "spur_graph::build::extract_full",
            "build_facts",
            root = %root.display()
        );
        let facts = {
            let _entered = extract_span.enter();
            let result = crate::build_facts(&root);
            match &result {
                Ok((facts, file_counts)) => {
                    tracing::info!(
                        target: "spur_graph::build::extract_full",
                        files = file_counts.values().sum::<usize>(),
                        nodes = facts.nodes.len(),
                        edges = facts.edges.len(),
                        elapsed_ms = elapsed_ms(extract_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::extract_full",
                        error = %error,
                        elapsed_ms = elapsed_ms(extract_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?.0
        };
        let artifact_started = Instant::now();
        let artifact_span = tracing::info_span!(
            target: "spur_graph::build::artifact",
            "artifact_from_facts",
            root = %root.display()
        );
        let artifact = {
            let _entered = artifact_span.enter();
            let result = artifact_from_facts(&facts, &root);
            match &result {
                Ok(artifact) => {
                    tracing::info!(
                        target: "spur_graph::build::artifact",
                        files = artifact.files.len(),
                        symbols = artifact.symbols.len(),
                        edges = artifact.edges.len(),
                        elapsed_ms = elapsed_ms(artifact_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::artifact",
                        error = %error,
                        elapsed_ms = elapsed_ms(artifact_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?
        };
        return Ok((artifact, BuildMode::Full));
    }

    let discover_started = Instant::now();
    let discover_span = tracing::info_span!(
        target: "spur_graph::build::discover",
        "discover_current_entries",
        root = %root.display(),
        files = tracing::field::Empty
    );
    let current_entries = {
        let _entered = discover_span.enter();
        let result = discover_current_entries(&root);
        match &result {
            Ok(entries) => {
                discover_span.record("files", entries.len());
                tracing::info!(
                    target: "spur_graph::build::discover",
                    files = entries.len(),
                    elapsed_ms = elapsed_ms(discover_started),
                    "spur-graph build phase completed"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "spur_graph::build::discover",
                    error = %error,
                    elapsed_ms = elapsed_ms(discover_started),
                    "spur-graph build phase failed"
                );
            }
        }
        result?
    };
    let rebucket_started = Instant::now();
    let rebucket_span = tracing::info_span!(
        target: "spur_graph::build::rebucket",
        "buckets_from_artifact",
        prev_files = prev.file_manifests.len(),
        prev_symbols = prev.symbols.len(),
        prev_edges = prev.edges.len()
    );
    let prev_buckets = {
        let _entered = rebucket_span.enter();
        let buckets = buckets_from_artifact(prev);
        tracing::info!(
            target: "spur_graph::build::rebucket",
            buckets = buckets.len(),
            elapsed_ms = elapsed_ms(rebucket_started),
            "spur-graph build phase completed"
        );
        buckets
    };

    let changed_started = Instant::now();
    let changed_span = tracing::info_span!(
        target: "spur_graph::build::changed_paths",
        "compute_changed_paths",
        files = current_entries.len(),
        prev_files = prev.file_manifests.len(),
        changed_paths = tracing::field::Empty
    );
    let (mut buckets, changed_paths) = {
        let _entered = changed_span.enter();
        let prev_content_oids: BTreeMap<_, _> = prev
            .file_manifests
            .iter()
            .map(|entry| (entry.path.as_str(), entry.content_oid.as_str()))
            .collect();

        let mut buckets = BTreeMap::new();
        let mut changed_paths = Vec::new();
        let mut reused = 0_usize;
        for current in current_entries.values() {
            if prev_content_oids
                .get(current.path.as_str())
                .is_some_and(|content_oid| *content_oid == current.content_oid)
            {
                if let Some(bucket) = prev_buckets.get(&current.path) {
                    buckets.insert(current.path.clone(), bucket.clone());
                    reused += 1;
                    continue;
                }
            }
            if current.extractable {
                changed_paths.push(root.join(&current.path));
            }
        }
        changed_span.record("changed_paths", changed_paths.len());
        tracing::info!(
            target: "spur_graph::build::changed_paths",
            changed_paths = changed_paths.len(),
            reused,
            elapsed_ms = elapsed_ms(changed_started),
            "spur-graph build phase completed"
        );
        (buckets, changed_paths)
    };
    let changed_count = changed_paths.len();

    if !changed_paths.is_empty() {
        let extract_started = Instant::now();
        let extract_span = tracing::info_span!(
            target: "spur_graph::build::extract_changed",
            "build_facts_for_paths",
            changed_paths = changed_paths.len()
        );
        let changed_facts = {
            let _entered = extract_span.enter();
            let result = build_facts_for_paths(&root, &changed_paths);
            match &result {
                Ok(facts) => {
                    tracing::info!(
                        target: "spur_graph::build::extract_changed",
                        changed_paths = changed_paths.len(),
                        nodes = facts.nodes.len(),
                        edges = facts.edges.len(),
                        elapsed_ms = elapsed_ms(extract_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::extract_changed",
                        changed_paths = changed_paths.len(),
                        error = %error,
                        elapsed_ms = elapsed_ms(extract_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?
        };
        let changed_buckets = buckets_from_facts(&changed_facts, &root, &current_entries)?;
        buckets.extend(changed_buckets);
    }

    let compose_started = Instant::now();
    let compose_span = tracing::info_span!(
        target: "spur_graph::build::compose",
        "compose_artifact",
        files = current_entries.len(),
        buckets = buckets.len()
    );
    let artifact = {
        let _entered = compose_span.enter();
        add_missing_manifest_buckets(&mut buckets, &current_entries);

        let tombstones = tombstones_from_removed_paths(prev, &current_entries);
        let artifact = compose_artifact(buckets, &current_entries, manifest_version, tombstones);
        tracing::info!(
            target: "spur_graph::build::compose",
            files = artifact.files.len(),
            symbols = artifact.symbols.len(),
            edges = artifact.edges.len(),
            tombstones = artifact.tombstones.len(),
            elapsed_ms = elapsed_ms(compose_started),
            "spur-graph build phase completed"
        );
        artifact
    };
    tracing::info!(
        target: "spur_graph::build",
        mode = "Incremental",
        files = artifact.files.len(),
        symbols = artifact.symbols.len(),
        edges = artifact.edges.len(),
        changed = changed_count,
        elapsed_ms = elapsed_ms(build_started),
        "spur-graph build completed"
    );
    Ok((artifact, BuildMode::Incremental))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
                let entry = buckets
                    .entry(node.label.clone())
                    .or_insert_with(|| FileBucket {
                        file: GraphFileArtifact {
                            stable_file_id: stable_file_id.clone(),
                            file_path: node.label.clone(),
                        },
                        file_node_id: Some(node.node_id),
                        manifest: GraphFileManifestEntry {
                            stable_file_id: stable_file_id.clone(),
                            path: node.label.clone(),
                            content_oid: content_oid_for(current_entries, &node.label),
                            node_ids: vec![node.node_id],
                        },
                        symbols: Vec::new(),
                        symbol_node_ids: Vec::new(),
                        edges: Vec::new(),
                    });
                entry.file = GraphFileArtifact {
                    stable_file_id: stable_file_id.clone(),
                    file_path: node.label.clone(),
                };
                entry.file_node_id = Some(node.node_id);
                entry.manifest.stable_file_id = stable_file_id;
                if !entry.manifest.node_ids.contains(&node.node_id) {
                    entry.manifest.node_ids.push(node.node_id);
                }
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
            | NodeKind::Field
            | NodeKind::Constant
            | NodeKind::TypeAlias
            | NodeKind::Macro
            | NodeKind::Section
            | NodeKind::McpTool => {
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
                        file_node_id: None,
                        manifest: GraphFileManifestEntry {
                            stable_file_id,
                            path: file_path.clone(),
                            content_oid: content_oid_for(current_entries, &file_path),
                            node_ids: Vec::new(),
                        },
                        symbols: Vec::new(),
                        symbol_node_ids: Vec::new(),
                        edges: Vec::new(),
                    }
                });
                entry.manifest.node_ids.push(node.node_id);
                entry.symbol_node_ids.push(node.node_id);
                entry.symbols.push(symbol);
            }
            NodeKind::Commit => {
                // Commit nodes belong in artifact.commits, not artifact.symbols.
                continue;
            }
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
                file_node_id: None,
                manifest: GraphFileManifestEntry {
                    stable_file_id,
                    path: source_file_path.clone(),
                    content_oid: content_oid_for(current_entries, &source_file_path),
                    node_ids: Vec::new(),
                },
                symbols: Vec::new(),
                symbol_node_ids: Vec::new(),
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
            change_kind: None,
            edge_kind: Some(graph_edge_kind_or_default(edge.relation, edge.edge_kind)),
        });
    }

    for bucket in buckets.values_mut() {
        bucket.manifest.node_ids.sort_by_key(|id| id.get());
        sort_bucket_symbols(bucket);
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
    let mut manifest_by_path = BTreeMap::new();
    for manifest in &artifact.file_manifests {
        manifest_by_path.insert(manifest.path.as_str(), manifest);
    }

    let mut file_by_path = HashMap::new();
    let mut file_node_id_by_path = HashMap::new();
    for file in &artifact.files {
        file_by_path.entry(file.file_path.as_str()).or_insert(file);
    }
    if artifact.file_node_ids.len() == artifact.files.len() {
        for (file, node_id) in artifact.files.iter().zip(&artifact.file_node_ids) {
            file_node_id_by_path
                .entry(file.file_path.as_str())
                .or_insert(*node_id);
        }
    }

    let mut symbols_by_path: HashMap<&str, Vec<&GraphSymbolArtifact>> = HashMap::new();
    let mut symbol_node_id_by_stable_id = HashMap::new();
    if artifact.symbol_node_ids.len() == artifact.symbols.len() {
        for (symbol, node_id) in artifact.symbols.iter().zip(&artifact.symbol_node_ids) {
            symbol_node_id_by_stable_id.insert(symbol.stable_symbol_id.as_str(), *node_id);
        }
    }
    let mut source_paths: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut seen_source_paths = HashSet::new();
    for manifest in manifest_by_path.values() {
        insert_source_path(
            &mut source_paths,
            &mut seen_source_paths,
            manifest.stable_file_id.as_str(),
            manifest.path.as_str(),
        );
    }
    for symbol in &artifact.symbols {
        let path = symbol.file_path.as_str();
        symbols_by_path.entry(path).or_default().push(symbol);
        if manifest_by_path.contains_key(path) {
            insert_source_path(
                &mut source_paths,
                &mut seen_source_paths,
                symbol.stable_symbol_id.as_str(),
                path,
            );
        }
    }

    let mut edges_by_path: HashMap<&str, Vec<&GraphEdgeArtifact>> = HashMap::new();
    for edge in &artifact.edges {
        if let Some(paths) = source_paths.get(edge.source_stable_symbol_id.as_str()) {
            for path in paths {
                edges_by_path.entry(path).or_default().push(edge);
            }
        }
    }

    let mut buckets = BTreeMap::new();
    for (path, manifest) in manifest_by_path {
        buckets.insert(
            path.to_string(),
            FileBucket {
                file: file_by_path.get(path).copied().cloned().unwrap_or_else(|| {
                    GraphFileArtifact {
                        stable_file_id: manifest.stable_file_id.clone(),
                        file_path: manifest.path.clone(),
                    }
                }),
                file_node_id: file_node_id_by_path.get(path).copied(),
                manifest: manifest.clone(),
                symbols: symbols_by_path
                    .get(path)
                    .map(|symbols| symbols.iter().copied().cloned().collect())
                    .unwrap_or_default(),
                symbol_node_ids: symbols_by_path
                    .get(path)
                    .map(|symbols| {
                        symbols
                            .iter()
                            .filter_map(|symbol| {
                                symbol_node_id_by_stable_id
                                    .get(symbol.stable_symbol_id.as_str())
                                    .copied()
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                edges: edges_by_path
                    .get(path)
                    .map(|edges| edges.iter().copied().cloned().collect())
                    .unwrap_or_default(),
            },
        );
    }
    buckets
}

fn insert_source_path<'a>(
    source_paths: &mut HashMap<&'a str, Vec<&'a str>>,
    seen_source_paths: &mut HashSet<(&'a str, &'a str)>,
    source_id: &'a str,
    path: &'a str,
) {
    if seen_source_paths.insert((source_id, path)) {
        source_paths.entry(source_id).or_default().push(path);
    }
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
        file_node_id: None,
        manifest: GraphFileManifestEntry {
            stable_file_id,
            path: path.to_string(),
            content_oid: content_oid.to_string(),
            node_ids: Vec::new(),
        },
        symbols: Vec::new(),
        symbol_node_ids: Vec::new(),
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

    let mut ambiguous_unresolved = 0usize;
    for bucket in buckets.values_mut() {
        for edge in &mut bucket.edges {
            let Some(target_label) = edge.target_label.as_deref() else {
                continue;
            };
            let skip_rebind =
                edge.edge_kind == Some(GraphEdgeKind::CallsDyn) || target_label.contains("::");
            if edge.target_stable_symbol_id.is_some() && skip_rebind {
                continue;
            }
            if edge.relation == RelationKind::Links {
                continue;
            }
            let Some(matches) = symbols_by_entity_name.get(target_label) else {
                edge.target_stable_symbol_id = None;
                continue;
            };
            if matches.len() > 1 {
                ambiguous_unresolved += 1;
                tracing::debug!(
                    target_label = target_label,
                    candidates = matches.len(),
                    "spur-graph: ambiguous cross-file target_label; leaving unresolved"
                );
                edge.target_stable_symbol_id = None;
            } else {
                let resolved = matches.first().expect("matches is non-empty");
                edge.target_stable_symbol_id = Some(resolved.clone());
            }
        }
    }
    if ambiguous_unresolved > 0 {
        tracing::info!(
            ambiguous_unresolved,
            "spur-graph: left ambiguous cross-file edges unresolved"
        );
    }
}

fn rebuild_from_buckets(
    mut buckets: BTreeMap<String, FileBucket>,
    manifest_version: String,
    graph_content_hash: String,
    tombstones: Vec<GraphTombstoneEntry>,
) -> GraphIndexArtifact {
    let mut files = Vec::new();
    let mut file_node_ids = Vec::new();
    let mut symbols = Vec::new();
    let mut symbol_node_ids = Vec::new();
    let mut manifests = Vec::new();
    let mut edges = Vec::new();

    for bucket in buckets.values_mut() {
        sort_bucket_symbols(bucket);
        bucket.manifest.node_ids.sort_by_key(|id| id.get());
        bucket
            .edges
            .sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));
        files.push((bucket.file.clone(), bucket.file_node_id));
        if bucket.symbol_node_ids.len() == bucket.symbols.len() {
            symbols.extend(
                bucket
                    .symbols
                    .iter()
                    .cloned()
                    .zip(bucket.symbol_node_ids.iter().copied().map(Some)),
            );
        } else {
            symbols.extend(bucket.symbols.iter().cloned().map(|symbol| (symbol, None)));
        }
        manifests.push(bucket.manifest.clone());
        edges.extend(bucket.edges.clone());
    }

    files.sort_by(|a, b| a.0.file_path.cmp(&b.0.file_path));
    symbols.sort_by(|a, b| {
        a.0.file_path
            .cmp(&b.0.file_path)
            .then(a.0.byte_range.cmp(&b.0.byte_range))
            .then(a.0.entity_name.cmp(&b.0.entity_name))
            .then(a.0.stable_symbol_id.cmp(&b.0.stable_symbol_id))
    });
    manifests.sort_by(|a, b| a.path.cmp(&b.path));
    edges.sort_by(|a, b| edge_sort_key(a).cmp(&edge_sort_key(b)));
    let mut next_synthetic_file_node_id = max_existing_node_id(&files, &symbols, &manifests)
        .and_then(|id| id.checked_add(1))
        .unwrap_or(1);
    let files = files
        .into_iter()
        .map(|(file, node_id)| {
            let node_id = node_id.unwrap_or_else(|| {
                let synthetic = NodeId(next_synthetic_file_node_id);
                next_synthetic_file_node_id = next_synthetic_file_node_id
                    .checked_add(1)
                    .expect("exhausted synthetic file NodeId space");
                synthetic
            });
            file_node_ids.push(node_id);
            file
        })
        .collect();
    let symbols = symbols
        .into_iter()
        .map(|(symbol, node_id)| {
            if let Some(node_id) = node_id {
                symbol_node_ids.push(node_id);
            }
            symbol
        })
        .collect();

    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
            // Content hash is stamped at write time, after body serialization input is finalized.
            content_hash_blake3: None,
        },
        manifest_version,
        graph_content_hash,
        file_manifests: manifests,
        files,
        file_node_ids,
        symbols,
        symbol_node_ids,
        edges,
        tombstones,
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn max_existing_node_id(
    files: &[(GraphFileArtifact, Option<NodeId>)],
    symbols: &[(GraphSymbolArtifact, Option<NodeId>)],
    manifests: &[GraphFileManifestEntry],
) -> Option<u64> {
    files
        .iter()
        .filter_map(|(_, node_id)| node_id.map(NodeId::get))
        .chain(
            symbols
                .iter()
                .filter_map(|(_, node_id)| node_id.map(NodeId::get)),
        )
        .chain(
            manifests
                .iter()
                .flat_map(|manifest| manifest.node_ids.iter().map(|id| id.get())),
        )
        .max()
}

fn sort_bucket_symbols(bucket: &mut FileBucket) {
    if bucket.symbol_node_ids.len() == bucket.symbols.len() {
        let mut symbols: Vec<_> = bucket
            .symbols
            .drain(..)
            .zip(bucket.symbol_node_ids.drain(..))
            .collect();
        symbols.sort_by(|a, b| {
            a.0.byte_range
                .cmp(&b.0.byte_range)
                .then(a.0.entity_name.cmp(&b.0.entity_name))
                .then(a.0.stable_symbol_id.cmp(&b.0.stable_symbol_id))
        });
        for (symbol, node_id) in symbols {
            bucket.symbols.push(symbol);
            bucket.symbol_node_ids.push(node_id);
        }
    } else {
        bucket.symbols.sort_by(|a, b| {
            a.byte_range
                .cmp(&b.byte_range)
                .then(a.entity_name.cmp(&b.entity_name))
                .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
        });
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
        RelationKind::Touches => "touches",
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
    kind.discriminator()
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
    if node.kind == NodeKind::McpTool {
        return symbol_entity_name(&node.label);
    }

    let mut segments = vec![qualified_scope_segment(node)];
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
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use super::{
        artifact_from_facts, artifact_from_facts_incremental, buckets_from_artifact,
        compose_artifact, empty_bucket, manifest_version_from_query_bytes, BuildMode,
        CurrentFileEntry, ManifestQueryBytes, GRAPH_INDEX_VERSION_TEMPORAL,
    };
    use crate::content_hash::{compute_graph_content_hash, git_blob_oid};
    use crate::extract::{build_facts, build_facts_for_paths, GraphFacts};
    use crate::{
        graph_edge_kind_or_default, Confidence, FileId, GraphEdgeArtifact, GraphFileArtifact,
        GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphNode,
        GraphSymbolArtifact, NodeId, NodeKind, RelationKind, RunId, SourceSpan, SpanId,
    };
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Record};
    use tracing::{Event, Id, Metadata, Subscriber};

    #[test]
    fn manifest_version_changes_when_spur_edges_query_bytes_change() {
        let base = [
            ManifestQueryBytes {
                language: "rust",
                query: "tags",
                bytes: b"rust tags",
            },
            ManifestQueryBytes {
                language: "rust",
                query: "spur-edges",
                bytes: b"rust edges v1",
            },
            ManifestQueryBytes {
                language: "typescript",
                query: "tags",
                bytes: b"typescript tags",
            },
            ManifestQueryBytes {
                language: "typescript",
                query: "spur-edges",
                bytes: b"typescript edges",
            },
        ];
        let changed = [
            ManifestQueryBytes {
                language: "rust",
                query: "tags",
                bytes: b"rust tags",
            },
            ManifestQueryBytes {
                language: "rust",
                query: "spur-edges",
                bytes: b"rust edges v2",
            },
            ManifestQueryBytes {
                language: "typescript",
                query: "tags",
                bytes: b"typescript tags",
            },
            ManifestQueryBytes {
                language: "typescript",
                query: "spur-edges",
                bytes: b"typescript edges",
            },
        ];

        assert_ne!(
            manifest_version_from_query_bytes("schema", "extractor", &base),
            manifest_version_from_query_bytes("schema", "extractor", &changed)
        );
    }

    #[test]
    fn manifest_version_is_deterministic_for_identical_query_inputs() {
        let inputs = [
            ManifestQueryBytes {
                language: "typescript",
                query: "spur-edges",
                bytes: b"typescript edges",
            },
            ManifestQueryBytes {
                language: "rust",
                query: "tags",
                bytes: b"rust tags",
            },
        ];

        let first = manifest_version_from_query_bytes("schema", "extractor", &inputs);
        let second = manifest_version_from_query_bytes("schema", "extractor", &inputs);

        assert_eq!(first, second);

        let reordered = [inputs[1], inputs[0]];
        assert_eq!(
            first,
            manifest_version_from_query_bytes("schema", "extractor", &reordered)
        );
    }

    #[test]
    fn buckets_from_artifact_rebuckets_edges_by_file_or_symbol_source() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test-manifest".to_string(),
            graph_content_hash: "test-hash".to_string(),
            file_manifests: vec![
                manifest("file:a", "src/a.rs"),
                manifest("file:b", "src/b.rs"),
            ],
            files: vec![file("file:a", "src/a.rs"), file("file:b", "src/b.rs")],
            file_node_ids: Vec::new(),
            symbols: vec![
                symbol("sym:a1", "src/a.rs"),
                symbol("sym:a2", "src/a.rs"),
                symbol("sym:b1", "src/b.rs"),
            ],
            symbol_node_ids: Vec::new(),
            edges: vec![
                edge("file:a", Some("sym:a1"), RelationKind::Contains),
                edge("sym:a2", Some("sym:b1"), RelationKind::Calls),
                edge("sym:b1", Some("sym:a1"), RelationKind::References),
                edge("unknown", Some("sym:a1"), RelationKind::Uses),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        };

        let actual = buckets_from_artifact(&artifact);
        assert_eq!(
            actual.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["src/a.rs", "src/b.rs"]
        );

        let a_bucket = actual.get("src/a.rs").expect("a bucket");
        assert_eq!(a_bucket.file, file("file:a", "src/a.rs"));
        assert_eq!(a_bucket.manifest, manifest("file:a", "src/a.rs"));
        assert_eq!(
            a_bucket.symbols,
            vec![symbol("sym:a1", "src/a.rs"), symbol("sym:a2", "src/a.rs")]
        );
        assert_eq!(
            a_bucket.edges,
            vec![
                edge("file:a", Some("sym:a1"), RelationKind::Contains),
                edge("sym:a2", Some("sym:b1"), RelationKind::Calls),
            ]
        );

        let b_bucket = actual.get("src/b.rs").expect("b bucket");
        assert_eq!(b_bucket.file, file("file:b", "src/b.rs"));
        assert_eq!(b_bucket.manifest, manifest("file:b", "src/b.rs"));
        assert_eq!(b_bucket.symbols, vec![symbol("sym:b1", "src/b.rs")]);
        assert_eq!(
            b_bucket.edges,
            vec![edge("sym:b1", Some("sym:a1"), RelationKind::References)]
        );
    }

    #[test]
    fn compose_artifact_assigns_file_node_ids_for_manifest_only_files() {
        let mut buckets = BTreeMap::new();
        let mut lib_bucket = empty_bucket("src/lib.rs", "oid-lib");
        lib_bucket.file_node_id = Some(NodeId(10));
        buckets.insert("src/lib.rs".to_string(), lib_bucket);

        let current_entries = BTreeMap::from([
            (
                "README.txt".to_string(),
                CurrentFileEntry {
                    path: "README.txt".to_string(),
                    content_oid: "oid-readme".to_string(),
                    extractable: false,
                },
            ),
            (
                "src/lib.rs".to_string(),
                CurrentFileEntry {
                    path: "src/lib.rs".to_string(),
                    content_oid: "oid-lib".to_string(),
                    extractable: true,
                },
            ),
        ]);

        let artifact = compose_artifact(
            buckets,
            &current_entries,
            "test-manifest".to_string(),
            Vec::new(),
        );

        assert_eq!(artifact.files.len(), 2);
        assert_eq!(artifact.file_node_ids.len(), artifact.files.len());
        let readme_index = artifact
            .files
            .iter()
            .position(|file| file.file_path == "README.txt")
            .expect("README file");
        assert_ne!(artifact.file_node_ids[readme_index], NodeId(10));
        let readme_manifest = artifact
            .file_manifests
            .iter()
            .find(|entry| entry.path == "README.txt")
            .expect("README manifest");
        assert!(readme_manifest.node_ids.is_empty());
    }

    #[test]
    fn mcp_tool_symbols_persist_through_artifact_build() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(
            root.join("src/lib.rs"),
            r#"
struct ToolDefinition {
    name: String,
}

fn submit_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan".into(),
        description: "".into(),
        input_schema: json!({}),
    }
}
"#,
        )
        .expect("write lib.rs");

        let facts = build_facts_for_paths(root, &[PathBuf::from("src/lib.rs")]).expect("extract");
        let artifact = artifact_from_facts(&facts, root).expect("artifact");

        assert!(
            artifact
                .symbols
                .iter()
                .any(|symbol| symbol.symbol_kind == "mcp_tool"),
            "artifact should contain at least one persisted MCP tool symbol"
        );
    }

    #[test]
    fn commit_nodes_do_not_persist_as_symbols() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/lib.rs"), "pub fn alpha() {}\n").expect("write lib.rs");
        let facts = GraphFacts {
            nodes: vec![
                GraphNode {
                    node_id: NodeId(1),
                    stable_key: "file:src/lib.rs".to_string(),
                    label: "src/lib.rs".to_string(),
                    kind: NodeKind::File,
                    file_id: Some(FileId(1)),
                    source_span_id: Some(SpanId(1)),
                    first_seen_run_id: RunId(1),
                },
                GraphNode {
                    node_id: NodeId(2),
                    stable_key: "commit:abc123".to_string(),
                    label: "abc123".to_string(),
                    kind: NodeKind::Commit,
                    file_id: None,
                    source_span_id: Some(SpanId(1)),
                    first_seen_run_id: RunId(1),
                },
            ],
            edges: Vec::new(),
            spans: vec![SourceSpan {
                span_id: SpanId(1),
                file_id: FileId(1),
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                end_line: 0,
            }],
        };

        let artifact = artifact_from_facts(&facts, root).expect("artifact");

        assert!(
            !artifact
                .symbols
                .iter()
                .any(|symbol| symbol.symbol_kind == "commit"),
            "commit nodes belong in artifact.commits, not artifact.symbols"
        );
    }

    #[test]
    fn full_artifact_build_emits_phase_tracing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
        let facts = build_facts(root).expect("extract").0;
        let subscriber = RecordingSubscriber::default();
        let _guard = tracing::subscriber::set_default(subscriber.clone());
        tracing::callsite::rebuild_interest_cache();

        let artifact = artifact_from_facts(&facts, root).expect("artifact");

        assert!(!artifact.files.is_empty());
        subscriber
            .assert_span_targets(["spur_graph::build::discover", "spur_graph::build::compose"]);
        subscriber.assert_summary("Full", "0");
    }

    #[test]
    fn incremental_artifact_build_emits_phase_tracing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(root.join("src/a.rs"), "pub fn alpha() {}\n").expect("write a.rs");
        fs::write(root.join("src/b.rs"), "pub fn beta() {}\n").expect("write b.rs");
        let subscriber = RecordingSubscriber::default();
        let _guard = tracing::subscriber::set_default(subscriber.clone());
        tracing::callsite::rebuild_interest_cache();
        let prev =
            artifact_from_facts(&build_facts(root).expect("extract").0, root).expect("artifact");
        subscriber.clear();
        fs::write(root.join("src/a.rs"), "pub fn alpha_changed() {}\n").expect("rewrite a.rs");

        let (_next, mode) = artifact_from_facts_incremental(&prev, root).expect("incremental");

        assert_eq!(mode, BuildMode::Incremental);
        subscriber.assert_span_targets([
            "spur_graph::build::discover",
            "spur_graph::build::rebucket",
            "spur_graph::build::changed_paths",
            "spur_graph::build::extract_changed",
            "spur_graph::build::compose",
        ]);
        subscriber.assert_summary("Incremental", "1");
    }

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

    #[derive(Clone, Default)]
    struct RecordingSubscriber {
        inner: Arc<RecordingInner>,
    }

    #[derive(Default)]
    struct RecordingInner {
        next_id: AtomicU64,
        spans: Mutex<Vec<TraceRecord>>,
        events: Mutex<Vec<TraceRecord>>,
    }

    #[derive(Debug, Clone)]
    struct TraceRecord {
        target: &'static str,
        name: &'static str,
        fields: BTreeMap<String, String>,
    }

    impl RecordingSubscriber {
        fn clear(&self) {
            self.inner.spans.lock().expect("spans").clear();
            self.inner.events.lock().expect("events").clear();
        }

        fn assert_span_targets<const N: usize>(&self, expected_targets: [&str; N]) {
            let actual = self
                .inner
                .spans
                .lock()
                .expect("spans")
                .iter()
                .map(|record| record.target)
                .collect::<Vec<_>>();
            for expected in expected_targets {
                assert!(
                    actual.contains(&expected),
                    "missing span target {expected}; actual targets: {actual:?}"
                );
            }
        }

        fn assert_summary(&self, expected_mode: &str, expected_changed: &str) {
            let events = self.inner.events.lock().expect("events");
            let summary = events
                .iter()
                .find(|record| {
                    record.target == "spur_graph::build"
                        && record.name.starts_with("event")
                        && record.fields.get("mode").map(String::as_str) == Some(expected_mode)
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing {expected_mode} summary event; actual events: {:?}",
                        *events
                    )
                });
            assert_eq!(
                summary.fields.get("changed").map(String::as_str),
                Some(expected_changed)
            );
            assert!(summary.fields.contains_key("elapsed_ms"));
            assert!(summary.fields.contains_key("files"));
            assert!(summary.fields.contains_key("symbols"));
            assert!(summary.fields.contains_key("edges"));
        }
    }

    impl Subscriber for RecordingSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, attrs: &Attributes<'_>) -> Id {
            let mut visitor = FieldVisitor::default();
            attrs.record(&mut visitor);
            let metadata = attrs.metadata();
            self.inner.spans.lock().expect("spans").push(TraceRecord {
                target: metadata.target(),
                name: metadata.name(),
                fields: visitor.fields,
            });
            Id::from_u64(self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            let metadata = event.metadata();
            self.inner.events.lock().expect("events").push(TraceRecord {
                target: metadata.target(),
                name: metadata.name(),
                fields: visitor.fields,
            });
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[derive(Default)]
    struct FieldVisitor {
        fields: BTreeMap<String, String>,
    }

    impl Visit for FieldVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), value.to_string());
        }

        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.fields
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    fn file(stable_file_id: &str, path: &str) -> GraphFileArtifact {
        GraphFileArtifact {
            stable_file_id: stable_file_id.to_string(),
            file_path: path.to_string(),
        }
    }

    fn manifest(stable_file_id: &str, path: &str) -> GraphFileManifestEntry {
        GraphFileManifestEntry {
            stable_file_id: stable_file_id.to_string(),
            path: path.to_string(),
            content_oid: format!("oid:{path}"),
            node_ids: Vec::new(),
        }
    }

    fn symbol(stable_symbol_id: &str, path: &str) -> GraphSymbolArtifact {
        GraphSymbolArtifact {
            stable_symbol_id: stable_symbol_id.to_string(),
            file_path: path.to_string(),
            byte_range: [0, 8],
            line_range: [1, 1],
            entity_name: stable_symbol_id.to_string(),
            qualified_name: stable_symbol_id.to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: format!("hash:{stable_symbol_id}"),
            enclosing_scope: None,
        }
    }

    fn edge(
        source_stable_symbol_id: &str,
        target_stable_symbol_id: Option<&str>,
        relation: RelationKind,
    ) -> GraphEdgeArtifact {
        GraphEdgeArtifact {
            source_stable_symbol_id: source_stable_symbol_id.to_string(),
            target_stable_symbol_id: target_stable_symbol_id.map(str::to_string),
            target_label: None,
            relation,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            change_kind: None,
            edge_kind: Some(graph_edge_kind_or_default(relation, None)),
        }
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
