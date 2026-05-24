use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::ScopedJoinHandle;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context};
use arrow_array::{
    Array, Float32Array, Int32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use base64::Engine as _;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::data_type::{ByteArray, ByteArrayType, FloatType, Int32Type, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::{SerializedColumnWriter, SerializedFileWriter};
use parquet::schema::parser::parse_message_type;
use parquet::schema::types::ColumnPath;

use crate::store::build::{EXTRACTOR_VERSION, SCHEMA_VERSION};
use crate::{
    ChangeKind, CommitArtifact, Confidence, EdgeEndpoint, GitPath, GraphEdgeArtifact,
    GraphEdgeKind, GraphFileArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader,
    GraphSymbolArtifact, GraphTombstoneEntry, NodeId, RelationKind, RenamePrev, SnapshotKey,
    SymbolSnapshotArtifact, TemporalEdgeArtifact,
};

pub const PARQUET_ROW_GROUP_SIZE: usize = 16_384;
const ENCLOSING_SCOPE_DICTIONARY: bool = true;
const EDGES_BY_DST_PRESENT: bool = true;
const STALE_PARQUET_TEMP_DIR_AGE: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    pub emit_edges_by_dst: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            emit_edges_by_dst: EDGES_BY_DST_PRESENT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphArtifactManifest {
    pub graph_index_version: String,
    pub schema_version: String,
    pub manifest_version: String,
    pub graph_content_hash: String,
    pub indexed_commit_oid: Option<String>,
    pub extractor_version: String,
    pub complete: bool,
    pub row_counts: GraphArtifactRowCounts,
    pub parquet_writer: GraphArtifactParquetWriter,
    pub edges_by_dst_present: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphArtifactRowCounts {
    pub nodes: usize,
    pub edges: usize,
    pub edges_by_dst: Option<usize>,
    pub edges_unresolved: usize,
    pub files: usize,
    pub file_manifests: usize,
    pub tombstones: usize,
    #[serde(default)]
    pub commits: usize,
    #[serde(default)]
    pub symbol_snapshots: usize,
    #[serde(default)]
    pub temporal_edges: usize,
    #[serde(default)]
    pub diagnostics: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphArtifactParquetWriter {
    pub compression: String,
    pub row_group_size: usize,
}

#[derive(Debug, Clone)]
struct NodeRow {
    node_id: NodeId,
    symbol: GraphSymbolArtifact,
}

#[derive(Debug, Clone)]
struct FileRow {
    node_id: NodeId,
    file: GraphFileArtifact,
}

#[derive(Debug, Clone)]
struct ResolvedEdgeRow {
    edge: GraphEdgeArtifact,
    src_id: NodeId,
    dst_id: NodeId,
}

#[derive(Debug, Clone)]
struct UnresolvedEdgeRow {
    edge: GraphEdgeArtifact,
    src_id: NodeId,
}

#[derive(Debug, Clone)]
struct CommitRow {
    sha: String,
    parents: Vec<String>,
    author_time: i64,
    summary: String,
}

#[derive(Debug, Clone)]
struct SymbolSnapshotRow {
    key_stable_symbol_id: String,
    key_commit: String,
    file_path_b64: String,
    entity_name: String,
    symbol_kind: String,
    enclosing_scope: Option<String>,
    byte_range_start: i64,
    byte_range_end: i64,
    line_range_start: i64,
    line_range_end: i64,
    anchor_hash: String,
    tokens: Vec<String>,
}

#[derive(Debug, Clone)]
struct TemporalEdgeRow {
    source_kind: String,
    source_path_b64: Option<String>,
    source_stable_symbol_id: Option<String>,
    source_commit: Option<String>,
    target_kind: String,
    target_path_b64: Option<String>,
    target_stable_symbol_id: Option<String>,
    target_commit: Option<String>,
    relation: String,
    parent: Option<String>,
    change_kind: Option<String>,
    rename_prev_path_b64: Option<String>,
    rename_prev_stable_symbol_id: Option<String>,
    rename_prev_commit: Option<String>,
}

enum ColumnData {
    RequiredString(Vec<String>),
    OptionalString(Vec<Option<String>>),
    RequiredI64(Vec<i64>),
    RequiredI32(Vec<i32>),
    RequiredF32(Vec<f32>),
    RequiredListI64(Vec<Vec<i64>>),
    RequiredListString(Vec<Vec<String>>),
}

impl ColumnData {
    fn len(&self) -> usize {
        match self {
            Self::RequiredString(values) => values.len(),
            Self::OptionalString(values) => values.len(),
            Self::RequiredI64(values) => values.len(),
            Self::RequiredI32(values) => values.len(),
            Self::RequiredF32(values) => values.len(),
            Self::RequiredListI64(values) => values.len(),
            Self::RequiredListString(values) => values.len(),
        }
    }
}

pub fn write_artifact_parquet(
    artifact: &GraphIndexArtifact,
    base_dir: &Path,
    options: WriteOptions,
) -> anyhow::Result<PathBuf> {
    let artifact_hash = &artifact.graph_content_hash;
    fs::create_dir_all(base_dir)
        .with_context(|| format!("failed to create `{}`", base_dir.display()))?;
    sweep_stale_parquet_temp_dirs(base_dir);

    let final_dir = base_dir.join(format!("{artifact_hash}.parquet"));
    let temp_dir = parquet_temp_dir(base_dir, artifact_hash);
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)
            .with_context(|| format!("failed to remove stale temp dir `{}`", temp_dir.display()))?;
    }
    fs::create_dir(&temp_dir)
        .with_context(|| format!("failed to create `{}`", temp_dir.display()))?;

    let mut files = file_rows(artifact)?;
    let mut nodes = node_rows(artifact)?;
    files.sort_by(|a, b| a.file.file_path.cmp(&b.file.file_path));
    nodes.sort_by(|a, b| {
        a.symbol
            .file_path
            .cmp(&b.symbol.file_path)
            .then(a.symbol.stable_symbol_id.cmp(&b.symbol.stable_symbol_id))
    });

    let endpoint_ids = endpoint_id_map(&files, &nodes)?;
    let (mut resolved_edges, mut unresolved_edges) = edge_rows(artifact, &endpoint_ids)?;
    resolved_edges.sort_by(|a, b| {
        a.src_id
            .get()
            .cmp(&b.src_id.get())
            .then(a.dst_id.get().cmp(&b.dst_id.get()))
    });
    unresolved_edges.sort_by(|a, b| a.src_id.get().cmp(&b.src_id.get()));

    let mut file_manifests = artifact.file_manifests.clone();
    file_manifests.sort_by(|a, b| a.path.cmp(&b.path));
    let mut tombstones = artifact.tombstones.clone();
    tombstones.sort_by(|a, b| a.path.cmp(&b.path));

    let mut edges_by_dst = None;
    if options.emit_edges_by_dst {
        let mut by_dst = resolved_edges.clone();
        by_dst.sort_by(|a, b| {
            a.dst_id
                .get()
                .cmp(&b.dst_id.get())
                .then(a.src_id.get().cmp(&b.src_id.get()))
        });
        edges_by_dst = Some(by_dst);
    }

    let nodes_path = temp_dir.join("nodes.parquet");
    let edges_path = temp_dir.join("edges.parquet");
    let edges_by_dst_path = temp_dir.join("edges_by_dst.parquet");
    let unresolved_edges_path = temp_dir.join("edges_unresolved.parquet");
    let files_path = temp_dir.join("files.parquet");
    let file_manifests_path = temp_dir.join("file_manifests.parquet");
    let tombstones_path = temp_dir.join("tombstones.parquet");
    let commits_path = temp_dir.join("commits.parquet");
    let symbol_snapshots_path = temp_dir.join("symbol_snapshots.parquet");
    let temporal_edges_path = temp_dir.join("temporal_edges.parquet");
    let diagnostics_path = temp_dir.join("diagnostics.parquet");

    std::thread::scope(|scope| {
        let nodes_handle = scope.spawn(|| write_nodes(&nodes_path, &nodes));
        let edges_handle = scope.spawn(|| write_edges(&edges_path, &resolved_edges));
        let unresolved_edges_handle =
            scope.spawn(|| write_unresolved_edges(&unresolved_edges_path, &unresolved_edges));
        let files_handle = scope.spawn(|| write_files(&files_path, &files));
        let file_manifests_handle =
            scope.spawn(|| write_file_manifests(&file_manifests_path, &file_manifests));
        let tombstones_handle = scope.spawn(|| write_tombstones(&tombstones_path, &tombstones));
        let commits_handle = (!artifact.commits.is_empty())
            .then(|| scope.spawn(|| write_commits(&commits_path, &artifact.commits, &options)));
        let symbol_snapshots_handle = (!artifact.symbol_snapshots.is_empty()).then(|| {
            scope.spawn(|| {
                write_symbol_snapshots(&symbol_snapshots_path, &artifact.symbol_snapshots, &options)
            })
        });
        let temporal_edges_handle = (!artifact.temporal_edges.is_empty()).then(|| {
            scope.spawn(|| {
                write_temporal_edges(&temporal_edges_path, &artifact.temporal_edges, &options)
            })
        });
        let diagnostics_handle = (!artifact.diagnostics.is_empty())
            .then(|| scope.spawn(|| write_diagnostics(&diagnostics_path, &artifact.diagnostics)));
        let edges_by_dst_handle = edges_by_dst
            .as_ref()
            .map(|rows| scope.spawn(|| write_edges(&edges_by_dst_path, rows)));

        join_scoped(nodes_handle, "write nodes.parquet")?;
        join_scoped(edges_handle, "write edges.parquet")?;
        join_scoped(unresolved_edges_handle, "write edges_unresolved.parquet")?;
        join_scoped(files_handle, "write files.parquet")?;
        join_scoped(file_manifests_handle, "write file_manifests.parquet")?;
        join_scoped(tombstones_handle, "write tombstones.parquet")?;
        if let Some(handle) = commits_handle {
            join_scoped(handle, "write commits.parquet")?;
        }
        if let Some(handle) = symbol_snapshots_handle {
            join_scoped(handle, "write symbol_snapshots.parquet")?;
        }
        if let Some(handle) = temporal_edges_handle {
            join_scoped(handle, "write temporal_edges.parquet")?;
        }
        if let Some(handle) = diagnostics_handle {
            join_scoped(handle, "write diagnostics.parquet")?;
        }
        if let Some(handle) = edges_by_dst_handle {
            join_scoped(handle, "write edges_by_dst.parquet")?;
        }
        Ok::<_, anyhow::Error>(())
    })?;

    let manifest = GraphArtifactManifest {
        graph_index_version: artifact.header.graph_index_version.clone(),
        schema_version: SCHEMA_VERSION.to_string(),
        manifest_version: artifact.manifest_version.clone(),
        graph_content_hash: artifact.graph_content_hash.clone(),
        indexed_commit_oid: None,
        extractor_version: EXTRACTOR_VERSION.to_string(),
        complete: true,
        row_counts: GraphArtifactRowCounts {
            nodes: nodes.len(),
            edges: resolved_edges.len(),
            edges_by_dst: options.emit_edges_by_dst.then_some(resolved_edges.len()),
            edges_unresolved: unresolved_edges.len(),
            files: files.len(),
            file_manifests: file_manifests.len(),
            tombstones: tombstones.len(),
            commits: artifact.commits.len(),
            symbol_snapshots: artifact.symbol_snapshots.len(),
            temporal_edges: artifact.temporal_edges.len(),
            diagnostics: artifact.diagnostics.len(),
        },
        parquet_writer: GraphArtifactParquetWriter {
            compression: "zstd-3".to_string(),
            row_group_size: PARQUET_ROW_GROUP_SIZE,
        },
        edges_by_dst_present: options.emit_edges_by_dst,
    };
    let manifest_json =
        serde_json::to_string_pretty(&manifest).context("failed to encode Parquet manifest")?;
    write_manifest(&temp_dir, &manifest_json)?;
    fsync_dir(&temp_dir)?;

    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)
            .with_context(|| format!("failed to remove `{}`", final_dir.display()))?;
    }
    fs::rename(&temp_dir, &final_dir).with_context(|| {
        format!(
            "failed to atomically rename `{}` to `{}`",
            temp_dir.display(),
            final_dir.display()
        )
    })?;
    fsync_dir(base_dir)?;

    Ok(final_dir)
}

pub fn read_artifact_header_parquet(dir: &Path) -> anyhow::Result<GraphArtifactManifest> {
    let manifest_path = dir.join("manifest.json");
    let content = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read `{}`", manifest_path.display()))?;
    let manifest: GraphArtifactManifest = serde_json::from_str(&content)
        .with_context(|| format!("invalid Parquet manifest `{}`", manifest_path.display()))?;
    Ok(manifest)
}

pub fn read_artifact_parquet(dir: &Path) -> anyhow::Result<GraphIndexArtifact> {
    let manifest = read_artifact_header_parquet(dir)?;
    if !manifest.complete {
        bail!(
            "refusing to load incomplete Parquet artifact `{}`",
            dir.display()
        );
    }
    let legacy_snapshot_ids = manifest.schema_version == "spur-graph-schema-v5"
        || manifest.graph_index_version != crate::schema::GRAPH_INDEX_VERSION_TEMPORAL;
    let nodes_path = dir.join("nodes.parquet");
    let edges_path = dir.join("edges.parquet");
    let unresolved_edges_path = dir.join("edges_unresolved.parquet");
    let files_path = dir.join("files.parquet");
    let file_manifests_path = dir.join("file_manifests.parquet");
    let tombstones_path = dir.join("tombstones.parquet");
    let commits_path = dir.join("commits.parquet");
    let symbol_snapshots_path = dir.join("symbol_snapshots.parquet");
    let temporal_edges_path = dir.join("temporal_edges.parquet");
    let diagnostics_path = dir.join("diagnostics.parquet");
    let row_counts = manifest.row_counts.clone();

    let (files, file_node_ids) = read_files(&files_path, row_counts.files)?;
    let file_manifests = read_file_manifests(&file_manifests_path, row_counts.file_manifests)?;
    let tombstones = read_tombstones(&tombstones_path, row_counts.tombstones)?;

    let (
        (symbols, symbol_node_ids),
        mut edges,
        unresolved_edges,
        commits,
        symbol_snapshots,
        temporal_edges,
        diagnostics,
    ) = std::thread::scope(|scope| {
        let nodes = scope.spawn(|| read_nodes(&nodes_path, row_counts.nodes));
        let edges = scope.spawn(|| read_edges(&edges_path, row_counts.edges));
        let unresolved_edges = scope
            .spawn(|| read_unresolved_edges(&unresolved_edges_path, row_counts.edges_unresolved));
        let commits = scope.spawn(|| read_commits(&commits_path, row_counts.commits));
        let symbol_snapshots = scope
            .spawn(|| read_symbol_snapshots(&symbol_snapshots_path, row_counts.symbol_snapshots));
        let temporal_edges =
            scope.spawn(|| read_temporal_edges(&temporal_edges_path, row_counts.temporal_edges));
        let diagnostics =
            scope.spawn(|| read_diagnostics(&diagnostics_path, row_counts.diagnostics));

        Ok::<_, anyhow::Error>((
            join_scoped(nodes, "read nodes.parquet")?,
            join_scoped(edges, "read edges.parquet")?,
            join_scoped(unresolved_edges, "read edges_unresolved.parquet")?,
            join_scoped(commits, "read commits.parquet")?,
            join_scoped(symbol_snapshots, "read symbol_snapshots.parquet")?,
            join_scoped(temporal_edges, "read temporal_edges.parquet")?,
            join_scoped(diagnostics, "read diagnostics.parquet")?,
        ))
    })?;

    edges.extend(unresolved_edges);

    let mut artifact = GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: manifest.graph_index_version,
            content_hash_blake3: None,
        },
        manifest_version: manifest.manifest_version,
        graph_content_hash: manifest.graph_content_hash,
        file_manifests,
        files,
        file_node_ids,
        symbols,
        symbol_node_ids,
        edges,
        tombstones,
        diagnostics,
        commits,
        symbol_snapshots,
        temporal_edges,
    };
    if legacy_snapshot_ids {
        crate::schema::rehash_legacy_snapshot_ids(&mut artifact);
    }
    Ok(artifact)
}

fn join_scoped<T>(
    handle: ScopedJoinHandle<'_, anyhow::Result<T>>,
    label: &str,
) -> anyhow::Result<T> {
    handle
        .join()
        .map_err(|_| anyhow!("Parquet worker thread panicked during {label}"))?
}

fn file_rows(artifact: &GraphIndexArtifact) -> anyhow::Result<Vec<FileRow>> {
    if artifact.file_node_ids.len() != artifact.files.len() {
        bail!(
            "artifact has {} files but {} file node ids",
            artifact.files.len(),
            artifact.file_node_ids.len()
        );
    }
    artifact
        .files
        .iter()
        .cloned()
        .zip(artifact.file_node_ids.iter().copied())
        .map(|(file, node_id)| Ok(FileRow { file, node_id }))
        .collect()
}

fn node_rows(artifact: &GraphIndexArtifact) -> anyhow::Result<Vec<NodeRow>> {
    if artifact.symbol_node_ids.len() != artifact.symbols.len() {
        bail!(
            "artifact has {} symbols but {} symbol node ids",
            artifact.symbols.len(),
            artifact.symbol_node_ids.len()
        );
    }
    artifact
        .symbols
        .iter()
        .cloned()
        .zip(artifact.symbol_node_ids.iter().copied())
        .map(|(symbol, node_id)| Ok(NodeRow { symbol, node_id }))
        .collect()
}

fn endpoint_id_map(
    files: &[FileRow],
    nodes: &[NodeRow],
) -> anyhow::Result<HashMap<String, NodeId>> {
    let mut endpoint_ids: HashMap<String, NodeId> = HashMap::new();
    let mut endpoint_metadata: HashMap<String, (String, String, String, [usize; 2])> =
        HashMap::new();
    for row in files {
        if let Some(existing) = endpoint_ids.insert(row.file.stable_file_id.clone(), row.node_id) {
            let prev = endpoint_metadata
                .get(&row.file.stable_file_id)
                .cloned()
                .unwrap_or_default();
            bail!(
                "stable endpoint id `{stable_id}` maps to both NodeId({prev_id}) (file path={prev_path}) and NodeId({new_id}) (file path={new_path})",
                stable_id = row.file.stable_file_id,
                prev_id = existing.get(),
                prev_path = prev.2,
                new_id = row.node_id.get(),
                new_path = row.file.file_path,
            );
        }
        endpoint_metadata.insert(
            row.file.stable_file_id.clone(),
            (
                "file".into(),
                row.file.file_path.clone(),
                row.file.file_path.clone(),
                [0, 0],
            ),
        );
    }
    for row in nodes {
        if let Some(existing) =
            endpoint_ids.insert(row.symbol.stable_symbol_id.clone(), row.node_id)
        {
            let prev = endpoint_metadata
                .get(&row.symbol.stable_symbol_id)
                .cloned()
                .unwrap_or_default();
            bail!(
                "stable endpoint id `{stable_id}` maps to both NodeId({prev_id}) (kind={prev_kind} qn={prev_qn:?} path={prev_path} range={prev_range:?}) and NodeId({new_id}) (kind={new_kind} qn={new_qn:?} path={new_path} range={new_range:?})",
                stable_id = row.symbol.stable_symbol_id,
                prev_id = existing.get(),
                prev_kind = prev.0,
                prev_qn = prev.1,
                prev_path = prev.2,
                prev_range = prev.3,
                new_id = row.node_id.get(),
                new_kind = row.symbol.symbol_kind,
                new_qn = row.symbol.qualified_name,
                new_path = row.symbol.file_path,
                new_range = row.symbol.byte_range,
            );
        }
        endpoint_metadata.insert(
            row.symbol.stable_symbol_id.clone(),
            (
                row.symbol.symbol_kind.clone(),
                row.symbol.qualified_name.clone(),
                row.symbol.file_path.clone(),
                row.symbol.byte_range,
            ),
        );
    }
    Ok(endpoint_ids)
}

fn edge_rows(
    artifact: &GraphIndexArtifact,
    endpoint_ids: &HashMap<String, NodeId>,
) -> anyhow::Result<(Vec<ResolvedEdgeRow>, Vec<UnresolvedEdgeRow>)> {
    let mut resolved = Vec::new();
    let mut unresolved = Vec::new();
    for edge in &artifact.edges {
        let src_id = *endpoint_ids
            .get(&edge.source_stable_symbol_id)
            .ok_or_else(|| anyhow!("missing source endpoint `{}`", edge.source_stable_symbol_id))?;
        if let Some(target_stable_id) = &edge.target_stable_symbol_id {
            let dst_id = *endpoint_ids
                .get(target_stable_id)
                .ok_or_else(|| anyhow!("missing target endpoint `{target_stable_id}`"))?;
            resolved.push(ResolvedEdgeRow {
                edge: edge.clone(),
                src_id,
                dst_id,
            });
        } else {
            unresolved.push(UnresolvedEdgeRow {
                edge: edge.clone(),
                src_id,
            });
        }
    }
    Ok((resolved, unresolved))
}

fn write_nodes(path: &Path, rows: &[NodeRow]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary stable_symbol_id (STRING);
          required int64 node_id;
          required binary file_path (STRING);
          required int64 byte_range_start;
          required int64 byte_range_end;
          required int32 line_start;
          required int32 line_end;
          required binary entity_name (STRING);
          required binary qualified_name (STRING);
          required binary symbol_kind (STRING);
          required binary anchor_hash (STRING);
          optional binary enclosing_scope (STRING);
        }
        "#,
        &[
            "file_path",
            "entity_name",
            "qualified_name",
            "symbol_kind",
            if ENCLOSING_SCOPE_DICTIONARY {
                "enclosing_scope"
            } else {
                ""
            },
        ],
        vec![
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| node_id_to_i64(row.node_id))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.file_path.clone())
                    .collect(),
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| usize_to_i64(row.symbol.byte_range[0], "byte_range_start"))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| usize_to_i64(row.symbol.byte_range[1], "byte_range_end"))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredI32(
                rows.iter()
                    .map(|row| usize_to_i32(row.symbol.line_range[0], "line_start"))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredI32(
                rows.iter()
                    .map(|row| usize_to_i32(row.symbol.line_range[1], "line_end"))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.entity_name.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.qualified_name.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.symbol_kind.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.symbol.anchor_hash.clone())
                    .collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.symbol.enclosing_scope.clone())
                    .collect(),
            ),
        ],
    )
}

fn write_edges(path: &Path, rows: &[ResolvedEdgeRow]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary source_stable_id (STRING);
          required binary target_stable_id (STRING);
          required int64 src_id;
          required int64 dst_id;
          optional binary target_label (STRING);
          required binary relation (STRING);
          required binary confidence (STRING);
          required float confidence_score;
          optional binary edge_kind (STRING);
          optional binary bind_method (STRING);
        }
        "#,
        &[
            "source_stable_id",
            "target_stable_id",
            "target_label",
            "relation",
            "confidence",
            "edge_kind",
            "bind_method",
        ],
        vec![
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.edge.source_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| {
                        row.edge
                            .target_stable_symbol_id
                            .clone()
                            .expect("resolved edge has target")
                    })
                    .collect(),
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| node_id_to_i64(row.src_id))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| node_id_to_i64(row.dst_id))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.target_label.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| relation_to_str(row.edge.relation).to_string())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| confidence_to_str(row.edge.confidence).to_string())
                    .collect(),
            ),
            ColumnData::RequiredF32(rows.iter().map(|row| row.edge.confidence_score).collect()),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.edge_kind.map(edge_kind_to_str).map(str::to_string))
                    .collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.bind_method.clone())
                    .collect(),
            ),
        ],
    )
}

fn write_unresolved_edges(path: &Path, rows: &[UnresolvedEdgeRow]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary source_stable_id (STRING);
          required int64 src_id;
          optional binary target_label (STRING);
          required binary relation (STRING);
          required binary confidence (STRING);
          required float confidence_score;
          optional binary edge_kind (STRING);
          optional binary bind_method (STRING);
        }
        "#,
        &[
            "source_stable_id",
            "target_label",
            "relation",
            "confidence",
            "edge_kind",
            "bind_method",
        ],
        vec![
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.edge.source_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| node_id_to_i64(row.src_id))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.target_label.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| relation_to_str(row.edge.relation).to_string())
                    .collect(),
            ),
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| confidence_to_str(row.edge.confidence).to_string())
                    .collect(),
            ),
            ColumnData::RequiredF32(rows.iter().map(|row| row.edge.confidence_score).collect()),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.edge_kind.map(edge_kind_to_str).map(str::to_string))
                    .collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.edge.bind_method.clone())
                    .collect(),
            ),
        ],
    )
}

fn write_files(path: &Path, rows: &[FileRow]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary stable_file_id (STRING);
          required int64 node_id;
          required binary file_path (STRING);
        }
        "#,
        &["file_path"],
        vec![
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.file.stable_file_id.clone())
                    .collect(),
            ),
            ColumnData::RequiredI64(
                rows.iter()
                    .map(|row| node_id_to_i64(row.node_id))
                    .collect::<anyhow::Result<_>>()?,
            ),
            ColumnData::RequiredString(rows.iter().map(|row| row.file.file_path.clone()).collect()),
        ],
    )
}

fn write_file_manifests(path: &Path, rows: &[GraphFileManifestEntry]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary stable_file_id (STRING);
          required binary path (STRING);
          required binary content_oid (STRING);
          required group node_ids (LIST) {
            repeated group list {
              required int64 element;
            }
          }
        }
        "#,
        &["path"],
        vec![
            ColumnData::RequiredString(rows.iter().map(|row| row.stable_file_id.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.path.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.content_oid.clone()).collect()),
            ColumnData::RequiredListI64(
                rows.iter()
                    .map(|row| {
                        row.node_ids
                            .iter()
                            .copied()
                            .map(node_id_to_i64)
                            .collect::<anyhow::Result<Vec<_>>>()
                    })
                    .collect::<anyhow::Result<_>>()?,
            ),
        ],
    )
}

fn write_tombstones(path: &Path, rows: &[GraphTombstoneEntry]) -> anyhow::Result<()> {
    write_table(
        path,
        r#"
        message schema {
          required binary path (STRING);
          required binary stable_file_id (STRING);
        }
        "#,
        &["path"],
        vec![
            ColumnData::RequiredString(rows.iter().map(|row| row.path.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.stable_file_id.clone()).collect()),
        ],
    )
}

fn write_commits(
    path: &Path,
    rows: &[CommitArtifact],
    _options: &WriteOptions,
) -> anyhow::Result<usize> {
    let rows: Vec<CommitRow> = rows
        .iter()
        .map(|row| CommitRow {
            sha: row.sha.clone(),
            parents: row.parents.clone(),
            author_time: row.author_time,
            summary: row.summary.clone(),
        })
        .collect();
    write_table(
        path,
        r#"
        message schema {
          required binary sha (STRING);
          optional group parents (LIST) {
            repeated group list {
              required binary element (STRING);
            }
          }
          required int64 author_time;
          required binary summary (STRING);
        }
        "#,
        &["sha", "summary"],
        vec![
            ColumnData::RequiredString(rows.iter().map(|row| row.sha.clone()).collect()),
            ColumnData::RequiredListString(rows.iter().map(|row| row.parents.clone()).collect()),
            ColumnData::RequiredI64(rows.iter().map(|row| row.author_time).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.summary.clone()).collect()),
        ],
    )?;
    Ok(rows.len())
}

fn write_symbol_snapshots(
    path: &Path,
    rows: &[SymbolSnapshotArtifact],
    _options: &WriteOptions,
) -> anyhow::Result<usize> {
    let rows = rows
        .iter()
        .map(|row| {
            Ok(SymbolSnapshotRow {
                key_stable_symbol_id: row.key.stable_symbol_id.clone(),
                key_commit: row.key.commit.clone(),
                file_path_b64: git_path_to_b64(&row.file_path),
                entity_name: row.entity_name.clone(),
                symbol_kind: row.symbol_kind.clone(),
                enclosing_scope: row.enclosing_scope.clone(),
                byte_range_start: usize_to_i64(row.byte_range[0], "byte_range_start")?,
                byte_range_end: usize_to_i64(row.byte_range[1], "byte_range_end")?,
                line_range_start: usize_to_i64(row.line_range[0], "line_range_start")?,
                line_range_end: usize_to_i64(row.line_range[1], "line_range_end")?,
                anchor_hash: row.anchor_hash.clone(),
                tokens: row.tokens.clone(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    write_table(
        path,
        r#"
        message schema {
          required binary key_stable_symbol_id (STRING);
          required binary key_commit (STRING);
          required binary file_path_b64 (STRING);
          required binary entity_name (STRING);
          required binary symbol_kind (STRING);
          optional binary enclosing_scope (STRING);
          required int64 byte_range_start;
          required int64 byte_range_end;
          required int64 line_range_start;
          required int64 line_range_end;
          required binary anchor_hash (STRING);
          optional group tokens (LIST) {
            repeated group list {
              required binary element (STRING);
            }
          }
        }
        "#,
        &[
            "key_stable_symbol_id",
            "key_commit",
            "file_path_b64",
            "entity_name",
            "symbol_kind",
            if ENCLOSING_SCOPE_DICTIONARY {
                "enclosing_scope"
            } else {
                ""
            },
            "anchor_hash",
        ],
        vec![
            ColumnData::RequiredString(
                rows.iter()
                    .map(|row| row.key_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::RequiredString(rows.iter().map(|row| row.key_commit.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.file_path_b64.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.entity_name.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.symbol_kind.clone()).collect()),
            ColumnData::OptionalString(
                rows.iter().map(|row| row.enclosing_scope.clone()).collect(),
            ),
            ColumnData::RequiredI64(rows.iter().map(|row| row.byte_range_start).collect()),
            ColumnData::RequiredI64(rows.iter().map(|row| row.byte_range_end).collect()),
            ColumnData::RequiredI64(rows.iter().map(|row| row.line_range_start).collect()),
            ColumnData::RequiredI64(rows.iter().map(|row| row.line_range_end).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.anchor_hash.clone()).collect()),
            ColumnData::RequiredListString(rows.iter().map(|row| row.tokens.clone()).collect()),
        ],
    )?;
    Ok(rows.len())
}

fn write_temporal_edges(
    path: &Path,
    rows: &[TemporalEdgeArtifact],
    _options: &WriteOptions,
) -> anyhow::Result<usize> {
    let rows: Vec<TemporalEdgeRow> = rows
        .iter()
        .map(|row| {
            let (source_path_b64, source_stable_symbol_id, source_commit) =
                endpoint_columns(&row.source);
            let (target_path_b64, target_stable_symbol_id, target_commit) =
                endpoint_columns(&row.target);
            let (rename_prev_path_b64, rename_prev_stable_symbol_id, rename_prev_commit) =
                change_kind_rename_prev_columns(row.change_kind.as_ref());
            TemporalEdgeRow {
                source_kind: endpoint_kind_to_str(&row.source).to_string(),
                source_path_b64,
                source_stable_symbol_id,
                source_commit,
                target_kind: endpoint_kind_to_str(&row.target).to_string(),
                target_path_b64,
                target_stable_symbol_id,
                target_commit,
                relation: relation_to_str(row.relation).to_string(),
                parent: row.parent.clone(),
                change_kind: row
                    .change_kind
                    .as_ref()
                    .map(change_kind_to_str)
                    .map(str::to_string),
                rename_prev_path_b64,
                rename_prev_stable_symbol_id,
                rename_prev_commit,
            }
        })
        .collect();
    write_table(
        path,
        r#"
        message schema {
          required binary source_kind (STRING);
          optional binary source_path_b64 (STRING);
          optional binary source_stable_symbol_id (STRING);
          optional binary source_commit (STRING);
          required binary target_kind (STRING);
          optional binary target_path_b64 (STRING);
          optional binary target_stable_symbol_id (STRING);
          optional binary target_commit (STRING);
          required binary relation (STRING);
          optional binary parent (STRING);
          optional binary change_kind (STRING);
          optional binary rename_prev_path_b64 (STRING);
          optional binary rename_prev_stable_symbol_id (STRING);
          optional binary rename_prev_commit (STRING);
        }
        "#,
        &["source_kind", "target_kind", "relation", "change_kind"],
        vec![
            ColumnData::RequiredString(rows.iter().map(|row| row.source_kind.clone()).collect()),
            ColumnData::OptionalString(
                rows.iter().map(|row| row.source_path_b64.clone()).collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.source_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::OptionalString(rows.iter().map(|row| row.source_commit.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.target_kind.clone()).collect()),
            ColumnData::OptionalString(
                rows.iter().map(|row| row.target_path_b64.clone()).collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.target_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::OptionalString(rows.iter().map(|row| row.target_commit.clone()).collect()),
            ColumnData::RequiredString(rows.iter().map(|row| row.relation.clone()).collect()),
            ColumnData::OptionalString(rows.iter().map(|row| row.parent.clone()).collect()),
            ColumnData::OptionalString(rows.iter().map(|row| row.change_kind.clone()).collect()),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.rename_prev_path_b64.clone())
                    .collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.rename_prev_stable_symbol_id.clone())
                    .collect(),
            ),
            ColumnData::OptionalString(
                rows.iter()
                    .map(|row| row.rename_prev_commit.clone())
                    .collect(),
            ),
        ],
    )?;
    Ok(rows.len())
}

fn write_diagnostics(path: &Path, rows: &[String]) -> anyhow::Result<usize> {
    write_table(
        path,
        r#"
        message schema {
          required binary message (STRING);
        }
        "#,
        &["message"],
        vec![ColumnData::RequiredString(rows.to_vec())],
    )?;
    Ok(rows.len())
}

fn write_table(
    path: &Path,
    schema: &str,
    dictionary_columns: &[&str],
    columns: Vec<ColumnData>,
) -> anyhow::Result<()> {
    let row_count = columns.first().map(ColumnData::len).unwrap_or(0);
    for column in &columns {
        if column.len() != row_count {
            bail!("column length mismatch while writing `{}`", path.display());
        }
    }

    let schema = Arc::new(parse_message_type(schema).context("failed to parse Parquet schema")?);
    let file =
        File::create(path).with_context(|| format!("failed to create `{}`", path.display()))?;
    let props = Arc::new(writer_properties(dictionary_columns)?);
    let mut writer =
        SerializedFileWriter::new(file, schema, props).context("failed to create writer")?;

    for start in (0..row_count).step_by(PARQUET_ROW_GROUP_SIZE) {
        let end = (start + PARQUET_ROW_GROUP_SIZE).min(row_count);
        let mut row_group = writer
            .next_row_group()
            .context("failed to start Parquet row group")?;
        for column in &columns {
            let mut column_writer = row_group
                .next_column()
                .context("failed to create Parquet column writer")?
                .ok_or_else(|| anyhow!("Parquet schema has fewer columns than data"))?;
            write_column(column, start, end, &mut column_writer)?;
            column_writer
                .close()
                .context("failed to close Parquet column writer")?;
        }
        if row_group
            .next_column()
            .context("failed to inspect Parquet column writer")?
            .is_some()
        {
            bail!(
                "Parquet schema has more columns than data for `{}`",
                path.display()
            );
        }
        row_group.close().context("failed to close row group")?;
    }
    writer.close().context("failed to close Parquet writer")?;
    sync_file(path)?;
    Ok(())
}

fn write_manifest(dir: &Path, manifest_json: &str) -> anyhow::Result<()> {
    let manifest_path = dir.join("manifest.json");
    let mut file = File::create(&manifest_path)
        .with_context(|| format!("failed to create `{}`", manifest_path.display()))?;
    file.write_all(manifest_json.as_bytes())
        .with_context(|| format!("failed to write `{}`", manifest_path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync `{}`", manifest_path.display()))?;
    Ok(())
}

fn sync_file(path: &Path) -> anyhow::Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open `{}` for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to fsync `{}`", path.display()))
}

fn fsync_dir(path: &Path) -> anyhow::Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory `{}` for fsync", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to fsync directory `{}`", path.display()))
}

fn parquet_temp_dir(base_dir: &Path, artifact_hash: &str) -> PathBuf {
    base_dir.join(format!(
        "{artifact_hash}.parquet.tmp.{}",
        std::process::id()
    ))
}

fn sweep_stale_parquet_temp_dirs(base_dir: &Path) {
    let entries = match fs::read_dir(base_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                path = %base_dir.display(),
                error = %err,
                "spur-graph: failed to scan Parquet artifact temp dirs"
            );
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(
                    path = %base_dir.display(),
                    error = %err,
                    "spur-graph: failed to inspect Parquet artifact temp dir entry"
                );
                continue;
            }
        };
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "spur-graph: failed to stat Parquet artifact temp dir entry"
                );
                continue;
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.contains(".parquet.tmp.") {
            continue;
        }
        if !parquet_temp_dir_is_stale(&path) {
            continue;
        }
        if let Err(err) = fs::remove_dir_all(&path) {
            tracing::warn!(
                path = %path.display(),
                error = %err,
                "spur-graph: failed to remove stale Parquet artifact temp dir"
            );
        }
    }
}

fn parquet_temp_dir_is_stale(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map(|age| age >= STALE_PARQUET_TEMP_DIR_AGE)
        .unwrap_or(false)
}

fn writer_properties(dictionary_columns: &[&str]) -> anyhow::Result<WriterProperties> {
    let mut builder = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).context("invalid zstd compression level")?,
        ))
        .set_max_row_group_size(PARQUET_ROW_GROUP_SIZE)
        .set_dictionary_enabled(false);

    for column in dictionary_columns
        .iter()
        .copied()
        .filter(|column| !column.is_empty())
    {
        builder = builder.set_column_dictionary_enabled(ColumnPath::from(column), true);
    }
    Ok(builder.build())
}

fn write_column(
    column: &ColumnData,
    start: usize,
    end: usize,
    writer: &mut SerializedColumnWriter<'_>,
) -> anyhow::Result<()> {
    match column {
        ColumnData::RequiredString(values) => {
            let values = values[start..end]
                .iter()
                .map(|value| ByteArray::from(value.as_bytes().to_vec()))
                .collect::<Vec<_>>();
            writer
                .typed::<ByteArrayType>()
                .write_batch(&values, None, None)?;
        }
        ColumnData::OptionalString(values) => {
            let mut encoded = Vec::new();
            let mut def_levels = Vec::with_capacity(end - start);
            for value in &values[start..end] {
                if let Some(value) = value {
                    encoded.push(ByteArray::from(value.as_bytes().to_vec()));
                    def_levels.push(1);
                } else {
                    def_levels.push(0);
                }
            }
            writer
                .typed::<ByteArrayType>()
                .write_batch(&encoded, Some(&def_levels), None)?;
        }
        ColumnData::RequiredI64(values) => {
            writer
                .typed::<Int64Type>()
                .write_batch(&values[start..end], None, None)?;
        }
        ColumnData::RequiredI32(values) => {
            writer
                .typed::<Int32Type>()
                .write_batch(&values[start..end], None, None)?;
        }
        ColumnData::RequiredF32(values) => {
            writer
                .typed::<FloatType>()
                .write_batch(&values[start..end], None, None)?;
        }
        ColumnData::RequiredListI64(values) => {
            let mut encoded = Vec::new();
            let mut def_levels = Vec::new();
            let mut rep_levels = Vec::new();
            for list in &values[start..end] {
                if list.is_empty() {
                    def_levels.push(0);
                    rep_levels.push(0);
                } else {
                    for (index, value) in list.iter().enumerate() {
                        encoded.push(*value);
                        def_levels.push(1);
                        rep_levels.push(if index == 0 { 0 } else { 1 });
                    }
                }
            }
            writer.typed::<Int64Type>().write_batch(
                &encoded,
                Some(&def_levels),
                Some(&rep_levels),
            )?;
        }
        ColumnData::RequiredListString(values) => {
            let mut encoded = Vec::new();
            let mut def_levels = Vec::new();
            let mut rep_levels = Vec::new();
            for list in &values[start..end] {
                if list.is_empty() {
                    def_levels.push(1);
                    rep_levels.push(0);
                } else {
                    for (index, value) in list.iter().enumerate() {
                        encoded.push(ByteArray::from(value.as_bytes().to_vec()));
                        def_levels.push(2);
                        rep_levels.push(if index == 0 { 0 } else { 1 });
                    }
                }
            }
            writer.typed::<ByteArrayType>().write_batch(
                &encoded,
                Some(&def_levels),
                Some(&rep_levels),
            )?;
        }
    }
    Ok(())
}

fn read_nodes(
    path: &Path,
    row_count: usize,
) -> anyhow::Result<(Vec<GraphSymbolArtifact>, Vec<NodeId>)> {
    let mut symbols = Vec::with_capacity(row_count);
    let mut node_ids = Vec::with_capacity(symbols.capacity());
    for batch in read_record_batches(path)? {
        let stable_symbol_id = string_array(&batch, 0, "stable_symbol_id")?;
        let node_id = i64_array(&batch, 1, "node_id")?;
        let file_path = string_array(&batch, 2, "file_path")?;
        let byte_range_start = i64_array(&batch, 3, "byte_range_start")?;
        let byte_range_end = i64_array(&batch, 4, "byte_range_end")?;
        let line_start = i32_array(&batch, 5, "line_start")?;
        let line_end = i32_array(&batch, 6, "line_end")?;
        let entity_name = string_array(&batch, 7, "entity_name")?;
        let qualified_name = string_array(&batch, 8, "qualified_name")?;
        let symbol_kind = string_array(&batch, 9, "symbol_kind")?;
        let anchor_hash = string_array(&batch, 10, "anchor_hash")?;
        let enclosing_scope = string_array(&batch, 11, "enclosing_scope")?;

        for row in 0..batch.num_rows() {
            symbols.push(GraphSymbolArtifact {
                stable_symbol_id: required_string_value(stable_symbol_id, row, "stable_symbol_id")?,
                file_path: required_string_value(file_path, row, "file_path")?,
                byte_range: [
                    i64_to_usize(byte_range_start.value(row), "byte_range_start")?,
                    i64_to_usize(byte_range_end.value(row), "byte_range_end")?,
                ],
                line_range: [
                    i32_to_usize(line_start.value(row), "line_start")?,
                    i32_to_usize(line_end.value(row), "line_end")?,
                ],
                entity_name: required_string_value(entity_name, row, "entity_name")?,
                qualified_name: required_string_value(qualified_name, row, "qualified_name")?,
                symbol_kind: required_string_value(symbol_kind, row, "symbol_kind")?,
                anchor_hash: required_string_value(anchor_hash, row, "anchor_hash")?,
                enclosing_scope: optional_string_value(enclosing_scope, row),
            });
            node_ids.push(i64_to_node_id(node_id.value(row), "node_id")?);
        }
    }
    Ok((symbols, node_ids))
}

fn read_edges(path: &Path, row_count: usize) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
    let mut edges = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let source_stable_id = string_array(&batch, 0, "source_stable_id")?;
        let target_stable_id = string_array(&batch, 1, "target_stable_id")?;
        let target_label = string_array(&batch, 4, "target_label")?;
        let relation = string_array(&batch, 5, "relation")?;
        let confidence = string_array(&batch, 6, "confidence")?;
        let confidence_score = f32_array(&batch, 7, "confidence_score")?;
        let edge_kind = string_array(&batch, 8, "edge_kind")?;
        let bind_method = optional_string_array_by_name(&batch, "bind_method")?;

        for row in 0..batch.num_rows() {
            edges.push(GraphEdgeArtifact {
                source_stable_symbol_id: required_string_value(
                    source_stable_id,
                    row,
                    "source_stable_id",
                )?,
                target_stable_symbol_id: Some(required_string_value(
                    target_stable_id,
                    row,
                    "target_stable_id",
                )?),
                target_label: optional_string_value(target_label, row),
                relation: relation_from_str(&required_string_value(relation, row, "relation")?)?,
                confidence: confidence_from_str(&required_string_value(
                    confidence,
                    row,
                    "confidence",
                )?)?,
                confidence_score: confidence_score.value(row),
                change_kind: None,
                edge_kind: optional_string_value(edge_kind, row)
                    .as_deref()
                    .map(edge_kind_from_str)
                    .transpose()?,
                bind_method: bind_method.and_then(|values| optional_string_value(values, row)),
            });
        }
    }
    Ok(edges)
}

fn read_unresolved_edges(path: &Path, row_count: usize) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
    let mut edges = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let source_stable_id = string_array(&batch, 0, "source_stable_id")?;
        let target_label = string_array(&batch, 2, "target_label")?;
        let relation = string_array(&batch, 3, "relation")?;
        let confidence = string_array(&batch, 4, "confidence")?;
        let confidence_score = f32_array(&batch, 5, "confidence_score")?;
        let edge_kind = string_array(&batch, 6, "edge_kind")?;
        let bind_method = optional_string_array_by_name(&batch, "bind_method")?;

        for row in 0..batch.num_rows() {
            edges.push(GraphEdgeArtifact {
                source_stable_symbol_id: required_string_value(
                    source_stable_id,
                    row,
                    "source_stable_id",
                )?,
                target_stable_symbol_id: None,
                target_label: optional_string_value(target_label, row),
                relation: relation_from_str(&required_string_value(relation, row, "relation")?)?,
                confidence: confidence_from_str(&required_string_value(
                    confidence,
                    row,
                    "confidence",
                )?)?,
                confidence_score: confidence_score.value(row),
                change_kind: None,
                edge_kind: optional_string_value(edge_kind, row)
                    .as_deref()
                    .map(edge_kind_from_str)
                    .transpose()?,
                bind_method: bind_method.and_then(|values| optional_string_value(values, row)),
            });
        }
    }
    Ok(edges)
}

fn read_files(
    path: &Path,
    row_count: usize,
) -> anyhow::Result<(Vec<GraphFileArtifact>, Vec<NodeId>)> {
    let mut files = Vec::with_capacity(row_count);
    let mut node_ids = Vec::with_capacity(files.capacity());
    for batch in read_record_batches(path)? {
        let stable_file_id = string_array(&batch, 0, "stable_file_id")?;
        let node_id = i64_array(&batch, 1, "node_id")?;
        let file_path = string_array(&batch, 2, "file_path")?;
        for row in 0..batch.num_rows() {
            files.push(GraphFileArtifact {
                stable_file_id: required_string_value(stable_file_id, row, "stable_file_id")?,
                file_path: required_string_value(file_path, row, "file_path")?,
            });
            node_ids.push(i64_to_node_id(node_id.value(row), "node_id")?);
        }
    }
    Ok((files, node_ids))
}

fn read_file_manifests(
    path: &Path,
    row_count: usize,
) -> anyhow::Result<Vec<GraphFileManifestEntry>> {
    let mut manifests = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let stable_file_id = string_array(&batch, 0, "stable_file_id")?;
        let path_col = string_array(&batch, 1, "path")?;
        let content_oid = string_array(&batch, 2, "content_oid")?;
        let node_ids = list_array(&batch, 3, "node_ids")?;
        for row in 0..batch.num_rows() {
            manifests.push(GraphFileManifestEntry {
                stable_file_id: required_string_value(stable_file_id, row, "stable_file_id")?,
                path: required_string_value(path_col, row, "path")?,
                content_oid: required_string_value(content_oid, row, "content_oid")?,
                node_ids: required_node_id_list_value(node_ids, row, "node_ids")?,
            });
        }
    }
    Ok(manifests)
}

fn read_tombstones(path: &Path, row_count: usize) -> anyhow::Result<Vec<GraphTombstoneEntry>> {
    if row_count == 0 {
        return Ok(Vec::new());
    }
    let mut tombstones = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let path_col = string_array(&batch, 0, "path")?;
        let stable_file_id = string_array(&batch, 1, "stable_file_id")?;
        for row in 0..batch.num_rows() {
            tombstones.push(GraphTombstoneEntry {
                path: required_string_value(path_col, row, "path")?,
                stable_file_id: required_string_value(stable_file_id, row, "stable_file_id")?,
            });
        }
    }
    Ok(tombstones)
}

fn read_commits(path: &Path, row_count: usize) -> anyhow::Result<Vec<CommitArtifact>> {
    if row_count == 0 || !path.exists() {
        return Ok(Vec::new());
    }
    let mut commits = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let sha = string_array(&batch, 0, "sha")?;
        let parents = list_array(&batch, 1, "parents")?;
        let author_time = i64_array(&batch, 2, "author_time")?;
        let summary = string_array(&batch, 3, "summary")?;
        for row in 0..batch.num_rows() {
            commits.push(CommitArtifact {
                sha: required_string_value(sha, row, "sha")?,
                parents: required_string_list_value(parents, row, "parents")?,
                author_time: author_time.value(row),
                summary: required_string_value(summary, row, "summary")?,
            });
        }
    }
    Ok(commits)
}

fn read_symbol_snapshots(
    path: &Path,
    row_count: usize,
) -> anyhow::Result<Vec<SymbolSnapshotArtifact>> {
    if row_count == 0 || !path.exists() {
        return Ok(Vec::new());
    }
    let mut snapshots = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let key_stable_symbol_id = string_array(&batch, 0, "key_stable_symbol_id")?;
        let key_commit = string_array(&batch, 1, "key_commit")?;
        let file_path_b64 = string_array(&batch, 2, "file_path_b64")?;
        let entity_name = string_array(&batch, 3, "entity_name")?;
        let symbol_kind = string_array(&batch, 4, "symbol_kind")?;
        let enclosing_scope = string_array(&batch, 5, "enclosing_scope")?;
        let byte_range_start = i64_array(&batch, 6, "byte_range_start")?;
        let byte_range_end = i64_array(&batch, 7, "byte_range_end")?;
        let line_range_start = i64_array(&batch, 8, "line_range_start")?;
        let line_range_end = i64_array(&batch, 9, "line_range_end")?;
        let anchor_hash = string_array(&batch, 10, "anchor_hash")?;
        let tokens = list_array(&batch, 11, "tokens")?;
        for row in 0..batch.num_rows() {
            let file_path_b64 = required_string_value(file_path_b64, row, "file_path_b64")?;
            let file_path = git_path_from_b64(&file_path_b64)
                .with_context(|| format!("row {row}: invalid file_path_b64"))?;
            snapshots.push(SymbolSnapshotArtifact {
                key: SnapshotKey {
                    stable_symbol_id: required_string_value(
                        key_stable_symbol_id,
                        row,
                        "key_stable_symbol_id",
                    )?,
                    commit: required_string_value(key_commit, row, "key_commit")?,
                },
                file_path,
                entity_name: required_string_value(entity_name, row, "entity_name")?,
                symbol_kind: required_string_value(symbol_kind, row, "symbol_kind")?,
                enclosing_scope: optional_string_value(enclosing_scope, row),
                byte_range: [
                    i64_to_usize(byte_range_start.value(row), "byte_range_start")?,
                    i64_to_usize(byte_range_end.value(row), "byte_range_end")?,
                ],
                line_range: [
                    i64_to_usize(line_range_start.value(row), "line_range_start")?,
                    i64_to_usize(line_range_end.value(row), "line_range_end")?,
                ],
                anchor_hash: required_string_value(anchor_hash, row, "anchor_hash")?,
                tokens: required_string_list_value(tokens, row, "tokens")?,
            });
        }
    }
    Ok(snapshots)
}

fn read_temporal_edges(path: &Path, row_count: usize) -> anyhow::Result<Vec<TemporalEdgeArtifact>> {
    if row_count == 0 || !path.exists() {
        return Ok(Vec::new());
    }
    let mut edges = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let source_kind = string_array(&batch, 0, "source_kind")?;
        let source_path_b64 = string_array(&batch, 1, "source_path_b64")?;
        let source_stable_symbol_id = string_array(&batch, 2, "source_stable_symbol_id")?;
        let source_commit = string_array(&batch, 3, "source_commit")?;
        let target_kind = string_array(&batch, 4, "target_kind")?;
        let target_path_b64 = string_array(&batch, 5, "target_path_b64")?;
        let target_stable_symbol_id = string_array(&batch, 6, "target_stable_symbol_id")?;
        let target_commit = string_array(&batch, 7, "target_commit")?;
        let relation = string_array(&batch, 8, "relation")?;
        let parent = string_array(&batch, 9, "parent")?;
        let change_kind = string_array(&batch, 10, "change_kind")?;
        let rename_prev_path_b64 = string_array(&batch, 11, "rename_prev_path_b64")?;
        let rename_prev_stable_symbol_id =
            string_array(&batch, 12, "rename_prev_stable_symbol_id")?;
        let rename_prev_commit = string_array(&batch, 13, "rename_prev_commit")?;

        for row in 0..batch.num_rows() {
            let source_kind = required_string_value(source_kind, row, "source_kind")?;
            let target_kind = required_string_value(target_kind, row, "target_kind")?;
            let relation_value = required_string_value(relation, row, "relation")?;
            edges.push(TemporalEdgeArtifact {
                source: endpoint_from_columns(
                    row,
                    "source",
                    &source_kind,
                    optional_string_value(source_path_b64, row),
                    optional_string_value(source_stable_symbol_id, row),
                    optional_string_value(source_commit, row),
                )?,
                target: endpoint_from_columns(
                    row,
                    "target",
                    &target_kind,
                    optional_string_value(target_path_b64, row),
                    optional_string_value(target_stable_symbol_id, row),
                    optional_string_value(target_commit, row),
                )?,
                relation: relation_from_str(&relation_value)
                    .with_context(|| format!("row {row}: invalid relation `{relation_value}`"))?,
                parent: optional_string_value(parent, row),
                change_kind: change_kind_from_columns(
                    row,
                    optional_string_value(change_kind, row),
                    optional_string_value(rename_prev_path_b64, row),
                    optional_string_value(rename_prev_stable_symbol_id, row),
                    optional_string_value(rename_prev_commit, row),
                )?,
            });
        }
    }
    Ok(edges)
}

fn read_diagnostics(path: &Path, row_count: usize) -> anyhow::Result<Vec<String>> {
    if row_count == 0 || !path.exists() {
        return Ok(Vec::new());
    }
    let mut diagnostics = Vec::with_capacity(row_count);
    for batch in read_record_batches(path)? {
        let message = string_array(&batch, 0, "message")?;
        for row in 0..batch.num_rows() {
            diagnostics.push(required_string_value(message, row, "message")?);
        }
    }
    Ok(diagnostics)
}

fn read_record_batches(path: &Path) -> anyhow::Result<Vec<RecordBatch>> {
    let file = File::open(path).with_context(|| format!("failed to open `{}`", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    builder
        .with_batch_size(PARQUET_ROW_GROUP_SIZE)
        .build()
        .with_context(|| format!("failed to build Arrow reader for `{}`", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to decode `{}`", path.display()))
}

fn string_array<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> anyhow::Result<&'a StringArray> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("expected string column `{name}`"))
}

fn optional_string_array_by_name<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> anyhow::Result<Option<&'a StringArray>> {
    let schema = batch.schema();
    let Ok(index) = schema.index_of(name) else {
        return Ok(None);
    };
    string_array(batch, index, name).map(Some)
}

fn i64_array<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> anyhow::Result<&'a Int64Array> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("expected int64 column `{name}`"))
}

fn i32_array<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> anyhow::Result<&'a Int32Array> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("expected int32 column `{name}`"))
}

fn f32_array<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> anyhow::Result<&'a Float32Array> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| anyhow!("expected float32 column `{name}`"))
}

fn list_array<'a>(
    batch: &'a RecordBatch,
    index: usize,
    name: &str,
) -> anyhow::Result<&'a ListArray> {
    batch
        .column(index)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| anyhow!("expected list column `{name}`"))
}

fn required_string_value(values: &StringArray, index: usize, name: &str) -> anyhow::Result<String> {
    if values.is_null(index) {
        bail!("missing required string column `{name}`");
    }
    Ok(values.value(index).to_string())
}

fn optional_string_value(values: &StringArray, index: usize) -> Option<String> {
    (!values.is_null(index)).then(|| values.value(index).to_string())
}

fn required_node_id_list_value(
    values: &ListArray,
    index: usize,
    name: &str,
) -> anyhow::Result<Vec<NodeId>> {
    if values.is_null(index) {
        return Ok(Vec::new());
    }
    let item_values = values.value(index);
    let node_ids = item_values
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("expected int64 elements in list column `{name}`"))?;
    (0..node_ids.len())
        .map(|item_index| i64_to_node_id(node_ids.value(item_index), name))
        .collect()
}

fn required_string_list_value(
    values: &ListArray,
    index: usize,
    name: &str,
) -> anyhow::Result<Vec<String>> {
    if values.is_null(index) {
        return Ok(Vec::new());
    }
    let item_values = values.value(index);
    let strings = item_values
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("expected string elements in list column `{name}`"))?;
    let mut out = Vec::with_capacity(strings.len());
    for item_index in 0..strings.len() {
        if strings.is_null(item_index) {
            bail!("missing string element {item_index} in list column `{name}`");
        }
        out.push(strings.value(item_index).to_string());
    }
    Ok(out)
}

fn node_id_to_i64(node_id: NodeId) -> anyhow::Result<i64> {
    i64::try_from(node_id.get()).context("NodeId does not fit in Parquet Int64")
}

fn i64_to_node_id(value: i64, column: &str) -> anyhow::Result<NodeId> {
    Ok(NodeId(u64::try_from(value).with_context(|| {
        format!("negative NodeId in `{column}`")
    })?))
}

fn usize_to_i64(value: usize, column: &str) -> anyhow::Result<i64> {
    i64::try_from(value).with_context(|| format!("`{column}` does not fit in Int64"))
}

fn usize_to_i32(value: usize, column: &str) -> anyhow::Result<i32> {
    i32::try_from(value).with_context(|| format!("`{column}` does not fit in Int32"))
}

fn i64_to_usize(value: i64, column: &str) -> anyhow::Result<usize> {
    usize::try_from(value).with_context(|| format!("negative value in `{column}`"))
}

fn i32_to_usize(value: i32, column: &str) -> anyhow::Result<usize> {
    usize::try_from(value).with_context(|| format!("negative value in `{column}`"))
}

fn relation_to_str(relation: RelationKind) -> &'static str {
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

fn relation_from_str(value: &str) -> anyhow::Result<RelationKind> {
    match value {
        "imports" => Ok(RelationKind::Imports),
        "calls" => Ok(RelationKind::Calls),
        "contains" => Ok(RelationKind::Contains),
        "implements" => Ok(RelationKind::Implements),
        "defines" => Ok(RelationKind::Defines),
        "references" => Ok(RelationKind::References),
        "uses" => Ok(RelationKind::Uses),
        "extends" => Ok(RelationKind::Extends),
        "links" => Ok(RelationKind::Links),
        "touches" => Ok(RelationKind::Touches),
        _ => bail!("unknown relation `{value}`"),
    }
}

fn confidence_to_str(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::SyntaxExact => "syntax_exact",
        Confidence::Heuristic => "heuristic",
        Confidence::Unknown => "unknown",
    }
}

fn confidence_from_str(value: &str) -> anyhow::Result<Confidence> {
    match value {
        "syntax_exact" => Ok(Confidence::SyntaxExact),
        "heuristic" => Ok(Confidence::Heuristic),
        "unknown" => Ok(Confidence::Unknown),
        _ => bail!("unknown confidence `{value}`"),
    }
}

fn edge_kind_to_str(edge_kind: GraphEdgeKind) -> &'static str {
    match edge_kind {
        GraphEdgeKind::Calls => "calls",
        GraphEdgeKind::CallsDyn => "calls_dyn",
        GraphEdgeKind::ReferencesHof => "references_hof",
        GraphEdgeKind::ReferencesOther => "references_other",
    }
}

fn edge_kind_from_str(value: &str) -> anyhow::Result<GraphEdgeKind> {
    match value {
        "calls" => Ok(GraphEdgeKind::Calls),
        "calls_dyn" => Ok(GraphEdgeKind::CallsDyn),
        "references_hof" => Ok(GraphEdgeKind::ReferencesHof),
        "references_other" => Ok(GraphEdgeKind::ReferencesOther),
        _ => bail!("unknown edge kind `{value}`"),
    }
}

type EndpointColumns = (Option<String>, Option<String>, Option<String>);

fn endpoint_columns(endpoint: &EdgeEndpoint) -> EndpointColumns {
    match endpoint {
        EdgeEndpoint::File { path } => (Some(git_path_to_b64(path)), None, None),
        EdgeEndpoint::Symbol { stable_symbol_id } => (None, Some(stable_symbol_id.clone()), None),
        EdgeEndpoint::Snapshot { key } => (
            None,
            Some(key.stable_symbol_id.clone()),
            Some(key.commit.clone()),
        ),
        EdgeEndpoint::Commit { sha } => (None, None, Some(sha.clone())),
    }
}

fn change_kind_rename_prev_columns(change_kind: Option<&ChangeKind>) -> EndpointColumns {
    match change_kind {
        Some(ChangeKind::RenamedFrom(RenamePrev::File(path))) => {
            (Some(git_path_to_b64(path)), None, None)
        }
        Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(key))) => (
            None,
            Some(key.stable_symbol_id.clone()),
            Some(key.commit.clone()),
        ),
        Some(ChangeKind::Added | ChangeKind::Modified | ChangeKind::Deleted) | None => {
            (None, None, None)
        }
    }
}

fn endpoint_from_columns(
    row: usize,
    prefix: &str,
    kind: &str,
    path_b64: Option<String>,
    stable_symbol_id: Option<String>,
    commit: Option<String>,
) -> anyhow::Result<EdgeEndpoint> {
    match endpoint_kind_from_str(kind)
        .with_context(|| format!("row {row}: invalid {prefix}_kind `{kind}`"))?
    {
        EdgeEndpointKind::File => {
            let context = format!("{prefix}_kind=file");
            require_column(
                row,
                path_b64.is_some(),
                &context,
                &format!("{prefix}_path_b64"),
            )?;
            reject_column(
                row,
                stable_symbol_id.is_some(),
                &context,
                &format!("{prefix}_stable_symbol_id"),
            )?;
            reject_column(row, commit.is_some(), &context, &format!("{prefix}_commit"))?;
            let path_b64 = path_b64.expect("validated path is present");
            Ok(EdgeEndpoint::File {
                path: git_path_from_b64(&path_b64)
                    .with_context(|| format!("row {row}: invalid {prefix}_path_b64"))?,
            })
        }
        EdgeEndpointKind::Symbol => {
            let context = format!("{prefix}_kind=symbol");
            reject_column(
                row,
                path_b64.is_some(),
                &context,
                &format!("{prefix}_path_b64"),
            )?;
            require_column(
                row,
                stable_symbol_id.is_some(),
                &context,
                &format!("{prefix}_stable_symbol_id"),
            )?;
            reject_column(row, commit.is_some(), &context, &format!("{prefix}_commit"))?;
            Ok(EdgeEndpoint::Symbol {
                stable_symbol_id: stable_symbol_id.expect("validated stable symbol id is present"),
            })
        }
        EdgeEndpointKind::Snapshot => {
            let context = format!("{prefix}_kind=snapshot");
            reject_column(
                row,
                path_b64.is_some(),
                &context,
                &format!("{prefix}_path_b64"),
            )?;
            require_column(
                row,
                stable_symbol_id.is_some(),
                &context,
                &format!("{prefix}_stable_symbol_id"),
            )?;
            require_column(row, commit.is_some(), &context, &format!("{prefix}_commit"))?;
            Ok(EdgeEndpoint::Snapshot {
                key: SnapshotKey {
                    stable_symbol_id: stable_symbol_id
                        .expect("validated stable symbol id is present"),
                    commit: commit.expect("validated commit is present"),
                },
            })
        }
        EdgeEndpointKind::Commit => {
            let context = format!("{prefix}_kind=commit");
            reject_column(
                row,
                path_b64.is_some(),
                &context,
                &format!("{prefix}_path_b64"),
            )?;
            reject_column(
                row,
                stable_symbol_id.is_some(),
                &context,
                &format!("{prefix}_stable_symbol_id"),
            )?;
            require_column(row, commit.is_some(), &context, &format!("{prefix}_commit"))?;
            Ok(EdgeEndpoint::Commit {
                sha: commit.expect("validated commit is present"),
            })
        }
    }
}

fn change_kind_from_columns(
    row: usize,
    change_kind: Option<String>,
    rename_prev_path_b64: Option<String>,
    rename_prev_stable_symbol_id: Option<String>,
    rename_prev_commit: Option<String>,
) -> anyhow::Result<Option<ChangeKind>> {
    let Some(change_kind) = change_kind else {
        reject_column(
            row,
            rename_prev_path_b64.is_some(),
            "change_kind absent",
            "rename_prev_path_b64",
        )?;
        reject_column(
            row,
            rename_prev_stable_symbol_id.is_some(),
            "change_kind absent",
            "rename_prev_stable_symbol_id",
        )?;
        reject_column(
            row,
            rename_prev_commit.is_some(),
            "change_kind absent",
            "rename_prev_commit",
        )?;
        return Ok(None);
    };

    Ok(Some(
        match change_kind_base_from_str(&change_kind)
            .with_context(|| format!("row {row}: invalid change_kind `{change_kind}`"))?
        {
            ChangeKindBase::Added => {
                reject_rename_prev_columns(
                    row,
                    rename_prev_path_b64.as_ref(),
                    rename_prev_stable_symbol_id.as_ref(),
                    rename_prev_commit.as_ref(),
                    &change_kind,
                )?;
                ChangeKind::Added
            }
            ChangeKindBase::Modified => {
                reject_rename_prev_columns(
                    row,
                    rename_prev_path_b64.as_ref(),
                    rename_prev_stable_symbol_id.as_ref(),
                    rename_prev_commit.as_ref(),
                    &change_kind,
                )?;
                ChangeKind::Modified
            }
            ChangeKindBase::Deleted => {
                reject_rename_prev_columns(
                    row,
                    rename_prev_path_b64.as_ref(),
                    rename_prev_stable_symbol_id.as_ref(),
                    rename_prev_commit.as_ref(),
                    &change_kind,
                )?;
                ChangeKind::Deleted
            }
            ChangeKindBase::RenamedFromFile => {
                let context = format!("change_kind `{change_kind}`");
                require_column(
                    row,
                    rename_prev_path_b64.is_some(),
                    &context,
                    "rename_prev_path_b64",
                )?;
                reject_column(
                    row,
                    rename_prev_stable_symbol_id.is_some(),
                    &context,
                    "rename_prev_stable_symbol_id",
                )?;
                reject_column(
                    row,
                    rename_prev_commit.is_some(),
                    &context,
                    "rename_prev_commit",
                )?;
                let path_b64 = rename_prev_path_b64.expect("validated path is present");
                ChangeKind::RenamedFrom(RenamePrev::File(
                    git_path_from_b64(&path_b64)
                        .with_context(|| format!("row {row}: invalid rename_prev_path_b64"))?,
                ))
            }
            ChangeKindBase::RenamedFromSymbol => {
                let context = format!("change_kind `{change_kind}`");
                reject_column(
                    row,
                    rename_prev_path_b64.is_some(),
                    &context,
                    "rename_prev_path_b64",
                )?;
                require_column(
                    row,
                    rename_prev_stable_symbol_id.is_some(),
                    &context,
                    "rename_prev_stable_symbol_id",
                )?;
                require_column(
                    row,
                    rename_prev_commit.is_some(),
                    &context,
                    "rename_prev_commit",
                )?;
                ChangeKind::RenamedFrom(RenamePrev::Symbol(SnapshotKey {
                    stable_symbol_id: rename_prev_stable_symbol_id
                        .expect("validated stable symbol id is present"),
                    commit: rename_prev_commit.expect("validated commit is present"),
                }))
            }
        },
    ))
}

fn reject_rename_prev_columns(
    row: usize,
    rename_prev_path_b64: Option<&String>,
    rename_prev_stable_symbol_id: Option<&String>,
    rename_prev_commit: Option<&String>,
    change_kind: &str,
) -> anyhow::Result<()> {
    if rename_prev_path_b64.is_some()
        || rename_prev_stable_symbol_id.is_some()
        || rename_prev_commit.is_some()
    {
        bail!("row {row}: change_kind `{change_kind}` must not include rename_prev columns");
    }
    Ok(())
}

fn require_column(row: usize, present: bool, context: &str, column: &str) -> anyhow::Result<()> {
    if !present {
        bail!("row {row}: {context} requires `{column}`");
    }
    Ok(())
}

fn reject_column(row: usize, present: bool, context: &str, column: &str) -> anyhow::Result<()> {
    if present {
        bail!("row {row}: {context} must not include `{column}`");
    }
    Ok(())
}

#[allow(dead_code)]
fn git_path_to_b64(path: &GitPath) -> String {
    base64::engine::general_purpose::STANDARD_NO_PAD.encode(path.as_bytes())
}

#[allow(dead_code)]
fn git_path_from_b64(value: &str) -> anyhow::Result<GitPath> {
    let bytes = match b64_decode_lenient(value) {
        Ok(bytes) => bytes,
        Err(err) => bail!("invalid GitPath base64 `{value}`: {err}"),
    };
    Ok(GitPath::from_bytes(bytes))
}

fn b64_decode_lenient(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    let standard = value.replace('-', "+").replace('_', "/");
    let padding = (4 - standard.len() % 4) % 4;
    let mut padded = String::with_capacity(standard.len() + padding);
    padded.push_str(&standard);
    padded.extend(std::iter::repeat_n('=', padding));
    base64::engine::general_purpose::STANDARD.decode(padded)
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeEndpointKind {
    File,
    Symbol,
    Snapshot,
    Commit,
}

#[allow(dead_code)]
fn endpoint_kind_to_str(endpoint: &EdgeEndpoint) -> &'static str {
    match endpoint {
        EdgeEndpoint::File { .. } => "file",
        EdgeEndpoint::Symbol { .. } => "symbol",
        EdgeEndpoint::Snapshot { .. } => "snapshot",
        EdgeEndpoint::Commit { .. } => "commit",
    }
}

#[allow(dead_code)]
fn endpoint_kind_from_str(value: &str) -> anyhow::Result<EdgeEndpointKind> {
    match value {
        "file" => Ok(EdgeEndpointKind::File),
        "symbol" => Ok(EdgeEndpointKind::Symbol),
        "snapshot" => Ok(EdgeEndpointKind::Snapshot),
        "commit" => Ok(EdgeEndpointKind::Commit),
        _ => bail!("unknown endpoint kind `{value}`"),
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeKindBase {
    Added,
    Modified,
    Deleted,
    RenamedFromFile,
    RenamedFromSymbol,
}

#[allow(dead_code)]
fn change_kind_to_str(change: &ChangeKind) -> &'static str {
    match change {
        ChangeKind::Added => "added",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::RenamedFrom(RenamePrev::File(_)) => "renamed_from_file",
        ChangeKind::RenamedFrom(RenamePrev::Symbol(_)) => "renamed_from_symbol",
    }
}

#[allow(dead_code)]
fn change_kind_base_from_str(value: &str) -> anyhow::Result<ChangeKindBase> {
    match value {
        "added" => Ok(ChangeKindBase::Added),
        "modified" => Ok(ChangeKindBase::Modified),
        "deleted" => Ok(ChangeKindBase::Deleted),
        "renamed_from_file" => Ok(ChangeKindBase::RenamedFromFile),
        "renamed_from_symbol" => Ok(ChangeKindBase::RenamedFromSymbol),
        _ => bail!("unknown change kind `{value}`"),
    }
}

#[cfg(test)]
mod helpers_test {
    use super::*;
    use crate::{ChangeKind, EdgeEndpoint, GitPath, RenamePrev, SnapshotKey};

    #[test]
    fn git_path_b64_round_trips_path_bytes() {
        let path = GitPath::from_bytes(b"src/non-utf8-\xff.rs".to_vec());

        let encoded = git_path_to_b64(&path);
        let decoded = git_path_from_b64(&encoded).expect("decode git path");

        assert_eq!(decoded, path);
    }

    #[test]
    fn endpoint_kind_round_trips_all_discriminators() {
        let snapshot_key = SnapshotKey {
            stable_symbol_id: "graph://symbol/sample".to_string(),
            commit: "abc123".to_string(),
        };
        let endpoints = [
            (
                EdgeEndpoint::File {
                    path: GitPath::from_bytes(b"src/lib.rs".to_vec()),
                },
                EdgeEndpointKind::File,
            ),
            (
                EdgeEndpoint::Symbol {
                    stable_symbol_id: "graph://symbol/sample".to_string(),
                },
                EdgeEndpointKind::Symbol,
            ),
            (
                EdgeEndpoint::Snapshot { key: snapshot_key },
                EdgeEndpointKind::Snapshot,
            ),
            (
                EdgeEndpoint::Commit {
                    sha: "abc123".to_string(),
                },
                EdgeEndpointKind::Commit,
            ),
        ];

        for (endpoint, expected) in endpoints {
            let encoded = endpoint_kind_to_str(&endpoint);
            let decoded = endpoint_kind_from_str(encoded).expect("decode endpoint kind");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn change_kind_round_trips_simple_base_discriminators() {
        let changes = [
            (ChangeKind::Added, ChangeKindBase::Added),
            (ChangeKind::Modified, ChangeKindBase::Modified),
            (ChangeKind::Deleted, ChangeKindBase::Deleted),
        ];

        for (change, expected) in changes {
            let encoded = change_kind_to_str(&change);
            let decoded = change_kind_base_from_str(encoded).expect("decode change kind");
            assert_eq!(decoded, expected);
        }
    }

    #[test]
    fn change_kind_round_trips_renamed_from_file_base() {
        let change = ChangeKind::RenamedFrom(RenamePrev::File(GitPath::from_bytes(
            b"src/old.rs".to_vec(),
        )));

        let encoded = change_kind_to_str(&change);
        let decoded = change_kind_base_from_str(encoded).expect("decode change kind");

        assert_eq!(decoded, ChangeKindBase::RenamedFromFile);
    }

    #[test]
    fn change_kind_round_trips_renamed_from_symbol_base() {
        let change = ChangeKind::RenamedFrom(RenamePrev::Symbol(SnapshotKey {
            stable_symbol_id: "graph://symbol/old".to_string(),
            commit: "abc123".to_string(),
        }));

        let encoded = change_kind_to_str(&change);
        let decoded = change_kind_base_from_str(encoded).expect("decode change kind");

        assert_eq!(decoded, ChangeKindBase::RenamedFromSymbol);
    }
}

#[cfg(test)]
mod parquet_temporal_test {
    use super::*;
    use crate::{
        ChangeKind, CommitArtifact, EdgeEndpoint, RenamePrev, SnapshotKey, SymbolSnapshotArtifact,
        TemporalEdgeArtifact,
    };

    #[test]
    fn commits_round_trip() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let rows = vec![
            CommitArtifact {
                sha: "c1".to_string(),
                parents: Vec::new(),
                author_time: 1_700_000_001,
                summary: "initial import".to_string(),
            },
            CommitArtifact {
                sha: "c2".to_string(),
                parents: vec!["c1".to_string(), "merge-parent".to_string()],
                author_time: 1_700_000_123,
                summary: "merge branch".to_string(),
            },
        ];

        let path = tempdir.path().join("commits.parquet");
        let written = write_commits(&path, &rows, &WriteOptions::default())?;
        let decoded = read_commits(&path, rows.len())?;

        assert_eq!(written, rows.len());
        assert_eq!(decoded, rows);
        Ok(())
    }

    #[test]
    fn symbol_snapshots_round_trip() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let rows = vec![
            SymbolSnapshotArtifact {
                key: snapshot_key("sym-a", "c1"),
                file_path: GitPath::from_bytes(b"src/lib.rs".to_vec()),
                entity_name: "alpha".to_string(),
                symbol_kind: "function".to_string(),
                enclosing_scope: Some("mod root".to_string()),
                byte_range: [10, 42],
                line_range: [2, 5],
                anchor_hash: "anchor-a".to_string(),
                tokens: vec!["alpha".to_string(), "body".to_string()],
            },
            SymbolSnapshotArtifact {
                key: snapshot_key("sym-b", "c2"),
                file_path: GitPath::from_bytes(b"src/non-utf8-\xff.rs".to_vec()),
                entity_name: "Beta".to_string(),
                symbol_kind: "struct".to_string(),
                enclosing_scope: None,
                byte_range: [100, 180],
                line_range: [9, 17],
                anchor_hash: "anchor-b".to_string(),
                tokens: Vec::new(),
            },
        ];

        let path = tempdir.path().join("symbol_snapshots.parquet");
        let written = write_symbol_snapshots(&path, &rows, &WriteOptions::default())?;
        let decoded = read_symbol_snapshots(&path, rows.len())?;

        assert_eq!(written, rows.len());
        assert_eq!(decoded, rows);
        Ok(())
    }

    #[test]
    fn temporal_edges_round_trip() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let old_symbol = snapshot_key("sym-old", "c1");
        let new_symbol = snapshot_key("sym-new", "c2");
        let rows = vec![
            TemporalEdgeArtifact {
                source: EdgeEndpoint::Snapshot {
                    key: old_symbol.clone(),
                },
                target: EdgeEndpoint::Snapshot {
                    key: new_symbol.clone(),
                },
                relation: RelationKind::Touches,
                parent: Some("c1".to_string()),
                change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(old_symbol))),
            },
            TemporalEdgeArtifact {
                source: EdgeEndpoint::Commit {
                    sha: "c3".to_string(),
                },
                target: EdgeEndpoint::File {
                    path: GitPath::from_bytes(b"src/new-name.rs".to_vec()),
                },
                relation: RelationKind::Touches,
                parent: Some("c2".to_string()),
                change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::File(
                    GitPath::from_bytes(b"src/old-name.rs".to_vec()),
                ))),
            },
        ];

        let path = tempdir.path().join("temporal_edges.parquet");
        let written = write_temporal_edges(&path, &rows, &WriteOptions::default())?;
        let decoded = read_temporal_edges(&path, rows.len())?;

        assert_eq!(written, rows.len());
        assert_eq!(decoded, rows);
        Ok(())
    }

    #[test]
    fn temporal_collections_round_trip() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut artifact = empty_artifact("temporal-collections-round-trip");
        let old_symbol = snapshot_key("sym-old", "c1");
        let new_symbol = snapshot_key("sym-new", "c2");

        artifact.commits = vec![
            CommitArtifact {
                sha: "c1".to_string(),
                parents: Vec::new(),
                author_time: 1_700_000_001,
                summary: "initial import".to_string(),
            },
            CommitArtifact {
                sha: "c2".to_string(),
                parents: vec!["c1".to_string()],
                author_time: 1_700_000_123,
                summary: "rename alpha to beta".to_string(),
            },
        ];
        artifact.symbol_snapshots = vec![
            SymbolSnapshotArtifact {
                key: old_symbol.clone(),
                file_path: GitPath::from_bytes(b"src/old.rs".to_vec()),
                entity_name: "alpha".to_string(),
                symbol_kind: "function".to_string(),
                enclosing_scope: Some("mod root".to_string()),
                byte_range: [10, 42],
                line_range: [2, 5],
                anchor_hash: "anchor-old".to_string(),
                tokens: vec!["alpha".to_string(), "body".to_string()],
            },
            SymbolSnapshotArtifact {
                key: new_symbol.clone(),
                file_path: GitPath::from_bytes(b"src/new.rs".to_vec()),
                entity_name: "beta".to_string(),
                symbol_kind: "function".to_string(),
                enclosing_scope: Some("mod root".to_string()),
                byte_range: [100, 150],
                line_range: [9, 14],
                anchor_hash: "anchor-new".to_string(),
                tokens: vec!["beta".to_string(), "body".to_string()],
            },
        ];
        artifact.temporal_edges = vec![
            TemporalEdgeArtifact {
                source: EdgeEndpoint::Commit {
                    sha: "c2".to_string(),
                },
                target: EdgeEndpoint::Snapshot {
                    key: new_symbol.clone(),
                },
                relation: RelationKind::Touches,
                parent: Some("c1".to_string()),
                change_kind: Some(ChangeKind::Added),
            },
            TemporalEdgeArtifact {
                source: EdgeEndpoint::Snapshot {
                    key: old_symbol.clone(),
                },
                target: EdgeEndpoint::Snapshot {
                    key: new_symbol.clone(),
                },
                relation: RelationKind::Touches,
                parent: Some("c1".to_string()),
                change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(old_symbol))),
            },
        ];

        let dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())?;
        let manifest = read_artifact_header_parquet(&dir)?;
        let decoded = read_artifact_parquet(&dir)?;

        assert_eq!(manifest.row_counts.commits, artifact.commits.len());
        assert_eq!(
            manifest.row_counts.symbol_snapshots,
            artifact.symbol_snapshots.len()
        );
        assert_eq!(
            manifest.row_counts.temporal_edges,
            artifact.temporal_edges.len()
        );
        assert_eq!(decoded.commits, artifact.commits);
        assert_eq!(decoded.symbol_snapshots, artifact.symbol_snapshots);
        assert_eq!(decoded.temporal_edges, artifact.temporal_edges);
        Ok(())
    }

    #[test]
    fn diagnostics_round_trip() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let mut artifact = empty_artifact("diagnostics-round-trip");
        artifact.diagnostics = vec![
            "parse_failed path=src/lib.rs sha=abc123".to_string(),
            "tree_sitter_error path=src/main.rs line=42".to_string(),
            "ambiguous_rename stable_symbol_id=sym-main".to_string(),
        ];

        let dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())?;
        let manifest = read_artifact_header_parquet(&dir)?;
        let decoded = read_artifact_parquet(&dir)?;

        assert_eq!(manifest.row_counts.diagnostics, artifact.diagnostics.len());
        assert_eq!(decoded.diagnostics, artifact.diagnostics);
        Ok(())
    }

    #[test]
    fn back_compat_loads_artifact_without_temporal_tables() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let dir = write_artifact_parquet(
            &empty_artifact("back-compat-without-temporal-tables"),
            tempdir.path(),
            WriteOptions::default(),
        )?;

        assert!(!dir.join("commits.parquet").exists());
        assert!(!dir.join("symbol_snapshots.parquet").exists());
        assert!(!dir.join("temporal_edges.parquet").exists());
        assert!(!dir.join("diagnostics.parquet").exists());

        let manifest_path = dir.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path)?)?;
        let row_counts = manifest
            .get_mut("row_counts")
            .and_then(serde_json::Value::as_object_mut)
            .expect("row_counts must be an object");
        row_counts.remove("commits");
        row_counts.remove("symbol_snapshots");
        row_counts.remove("temporal_edges");
        row_counts.remove("diagnostics");
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;

        let manifest = read_artifact_header_parquet(&dir)?;
        let decoded = read_artifact_parquet(&dir)?;

        assert_eq!(manifest.row_counts.commits, 0);
        assert_eq!(manifest.row_counts.symbol_snapshots, 0);
        assert_eq!(manifest.row_counts.temporal_edges, 0);
        assert_eq!(manifest.row_counts.diagnostics, 0);
        assert!(decoded.commits.is_empty());
        assert!(decoded.symbol_snapshots.is_empty());
        assert!(decoded.temporal_edges.is_empty());
        assert!(decoded.diagnostics.is_empty());
        Ok(())
    }

    fn snapshot_key(stable_symbol_id: &str, commit: &str) -> SnapshotKey {
        SnapshotKey {
            stable_symbol_id: stable_symbol_id.to_string(),
            commit: commit.to_string(),
        }
    }

    fn empty_artifact(graph_content_hash: &str) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: crate::schema::GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test-manifest".to_string(),
            graph_content_hash: graph_content_hash.to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            file_node_ids: Vec::new(),
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
