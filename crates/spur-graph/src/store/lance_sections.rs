use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use arrow_array::{
    LargeStringArray, RecordBatch, StringArray, UInt32Array, UInt64Array, UInt8Array,
};
use arrow_schema::{DataType, Field, Schema};
use lancedb::index::{scalar::FtsIndexBuilder, Index, IndexType};

use crate::content_hash::blake3_hex;
use crate::{
    GraphEdgeArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphSymbolArtifact,
    RelationKind,
};

pub const SECTIONS_DATASET_DIR: &str = "sections.lancedb";
pub const SECTIONS_TABLE: &str = "section_bodies";

#[derive(Debug)]
struct SectionRow {
    stable_symbol_id: String,
    file_path: String,
    qualified_name: String,
    heading_level: u8,
    body_text: String,
    body_byte_start: u64,
    body_byte_end: u64,
    child_count: u32,
    parent_stable_id: Option<String>,
    content_hash: String,
}

pub fn write_sections_dataset(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) -> Result<()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    write_sections_dataset_without_current_runtime(
                        artifact,
                        worktree_root,
                        artifact_dir,
                    )
                })
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
    }
    write_sections_dataset_without_current_runtime(artifact, worktree_root, artifact_dir)
}

fn write_sections_dataset_without_current_runtime(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create LanceDB runtime")?;
    runtime.block_on(write_sections_dataset_async(
        artifact,
        worktree_root,
        artifact_dir,
    ))
}

async fn write_sections_dataset_async(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) -> Result<()> {
    let rows = section_rows(artifact, worktree_root)?;
    fs::create_dir_all(artifact_dir)
        .with_context(|| format!("failed to create `{}`", artifact_dir.display()))?;
    let dataset_dir = artifact_dir.join(SECTIONS_DATASET_DIR);
    fs::create_dir_all(&dataset_dir)
        .with_context(|| format!("failed to create `{}`", dataset_dir.display()))?;

    let db = lancedb::connect(dataset_dir.to_string_lossy().as_ref())
        .execute()
        .await
        .context("failed to connect to sections.lancedb")?;
    let schema = sections_schema();
    let existing = db.open_table(SECTIONS_TABLE).execute().await.ok();
    let rows = filter_rows_for_new_file_versions(existing.as_ref(), rows).await?;
    let batch = rows_to_batch(rows, schema.clone())?;

    let (table, dataset_changed) = if let Some(table) = existing {
        if batch.num_rows() > 0 {
            table
                .add(batch)
                .execute()
                .await
                .context("failed to append LanceDB section rows")?;
            (table, true)
        } else {
            (table, false)
        }
    } else {
        let table = db
            .create_table(SECTIONS_TABLE, batch)
            .execute()
            .await
            .context("failed to create LanceDB sections table")?;
        (table, true)
    };

    if dataset_changed {
        ensure_body_text_fts_index(&table).await?;
    }

    Ok(())
}

async fn ensure_body_text_fts_index(table: &lancedb::Table) -> Result<()> {
    if table
        .list_indices()
        .await
        .context("failed to list LanceDB section indices")?
        .iter()
        .any(|index| {
            index.index_type == IndexType::FTS && index.columns.as_slice() == ["body_text"]
        })
    {
        return Ok(());
    }

    table
        .create_index(&["body_text"], Index::FTS(FtsIndexBuilder::default()))
        .execute()
        .await
        .context("failed to create LanceDB body_text FTS index")
}

async fn filter_rows_for_new_file_versions(
    table: Option<&lancedb::Table>,
    rows: Vec<SectionRow>,
) -> Result<Vec<SectionRow>> {
    let Some(table) = table else {
        return Ok(rows);
    };
    let mut seen = HashSet::new();
    let mut known_unchanged = HashSet::new();
    for row in &rows {
        if !seen.insert((row.file_path.clone(), row.content_hash.clone())) {
            continue;
        }
        let filter = format!(
            "file_path = '{}' AND content_hash = '{}'",
            sql_string_literal(row.file_path.as_str()),
            sql_string_literal(row.content_hash.as_str())
        );
        if table
            .count_rows(Some(filter))
            .await
            .context("failed to check existing LanceDB section rows")?
            > 0
        {
            known_unchanged.insert((row.file_path.clone(), row.content_hash.clone()));
        }
    }
    Ok(rows
        .into_iter()
        .filter(|row| !known_unchanged.contains(&(row.file_path.clone(), row.content_hash.clone())))
        .collect())
}

fn section_rows(artifact: &GraphIndexArtifact, worktree_root: &Path) -> Result<Vec<SectionRow>> {
    let section_ids: BTreeSet<&str> = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.symbol_kind == "section")
        .map(|symbol| symbol.stable_symbol_id.as_str())
        .collect();
    let child_count_by_parent = child_count_by_parent(&artifact.edges, &section_ids);
    let parent_by_child = parent_by_child(&artifact.edges, &section_ids);
    let manifest_by_path: BTreeMap<&str, &GraphFileManifestEntry> = artifact
        .file_manifests
        .iter()
        .map(|manifest| (manifest.path.as_str(), manifest))
        .collect();

    let mut sections_by_path: BTreeMap<&str, Vec<&GraphSymbolArtifact>> = BTreeMap::new();
    for symbol in &artifact.symbols {
        if symbol.symbol_kind == "section" {
            sections_by_path
                .entry(symbol.file_path.as_str())
                .or_default()
                .push(symbol);
        }
    }

    let mut rows = Vec::new();
    for (path, sections) in &sections_by_path {
        let bytes = read_file_bytes(worktree_root, path)?;
        let content_hash = blake3_hex(&bytes);
        let source = std::str::from_utf8(&bytes)
            .with_context(|| format!("markdown source `{}` is not UTF-8", path))?;
        for section in sections {
            rows.push(section_row(
                section,
                source,
                content_hash.as_str(),
                &child_count_by_parent,
                &parent_by_child,
            )?);
        }
    }

    for manifest in manifest_by_path.values() {
        if !is_markdown_path(&manifest.path)
            || sections_by_path.contains_key(manifest.path.as_str())
        {
            continue;
        }
        let bytes = read_file_bytes(worktree_root, &manifest.path)?;
        let content_hash = blake3_hex(&bytes);
        let source = std::str::from_utf8(&bytes)
            .with_context(|| format!("markdown source `{}` is not UTF-8", manifest.path))?;
        rows.push(SectionRow {
            stable_symbol_id: manifest.stable_file_id.clone(),
            file_path: manifest.path.clone(),
            qualified_name: manifest.path.clone(),
            heading_level: 0,
            body_text: source.to_owned(),
            body_byte_start: 0,
            body_byte_end: bytes.len() as u64,
            child_count: 0,
            parent_stable_id: None,
            content_hash,
        });
    }

    rows.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.body_byte_start.cmp(&b.body_byte_start))
            .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
    });
    Ok(rows)
}

fn section_row(
    section: &GraphSymbolArtifact,
    source: &str,
    content_hash: &str,
    child_count_by_parent: &HashMap<&str, u32>,
    parent_by_child: &HashMap<&str, String>,
) -> Result<SectionRow> {
    let start = section.byte_range[0];
    let end = section.byte_range[1];
    let body_text = source
        .get(start..end)
        .with_context(|| {
            format!(
                "section byte range {}..{} is not a UTF-8 boundary in `{}`",
                start, end, section.file_path
            )
        })?
        .to_owned();
    Ok(SectionRow {
        stable_symbol_id: section.stable_symbol_id.clone(),
        file_path: section.file_path.clone(),
        qualified_name: section.qualified_name.clone(),
        heading_level: heading_level(&body_text),
        body_text,
        body_byte_start: start as u64,
        body_byte_end: end as u64,
        child_count: child_count_by_parent
            .get(section.stable_symbol_id.as_str())
            .copied()
            .unwrap_or(0),
        parent_stable_id: parent_by_child
            .get(section.stable_symbol_id.as_str())
            .cloned(),
        content_hash: content_hash.to_owned(),
    })
}

fn child_count_by_parent<'a>(
    edges: &'a [GraphEdgeArtifact],
    section_ids: &BTreeSet<&'a str>,
) -> HashMap<&'a str, u32> {
    let mut counts = HashMap::new();
    for edge in edges {
        if edge.relation != RelationKind::Contains {
            continue;
        }
        if !section_ids.contains(edge.source_stable_symbol_id.as_str()) {
            continue;
        }
        if edge
            .target_stable_symbol_id
            .as_deref()
            .is_some_and(|target| section_ids.contains(target))
        {
            *counts
                .entry(edge.source_stable_symbol_id.as_str())
                .or_insert(0) += 1;
        }
    }
    counts
}

fn parent_by_child<'a>(
    edges: &'a [GraphEdgeArtifact],
    section_ids: &BTreeSet<&'a str>,
) -> HashMap<&'a str, String> {
    let mut parents = HashMap::new();
    for edge in edges {
        if edge.relation != RelationKind::Contains
            || !section_ids.contains(edge.source_stable_symbol_id.as_str())
        {
            continue;
        }
        if let Some(target) = edge.target_stable_symbol_id.as_deref() {
            if section_ids.contains(target) {
                parents.insert(target, edge.source_stable_symbol_id.clone());
            }
        }
    }
    parents
}

fn rows_to_batch(rows: Vec<SectionRow>, schema: Arc<Schema>) -> Result<RecordBatch> {
    let mut stable_symbol_ids = Vec::with_capacity(rows.len());
    let mut file_paths = Vec::with_capacity(rows.len());
    let mut qualified_names = Vec::with_capacity(rows.len());
    let mut heading_levels = Vec::with_capacity(rows.len());
    let mut body_texts = Vec::with_capacity(rows.len());
    let mut body_byte_starts = Vec::with_capacity(rows.len());
    let mut body_byte_ends = Vec::with_capacity(rows.len());
    let mut child_counts = Vec::with_capacity(rows.len());
    let mut parent_stable_ids = Vec::with_capacity(rows.len());
    let mut content_hashes = Vec::with_capacity(rows.len());

    for row in rows {
        stable_symbol_ids.push(row.stable_symbol_id);
        file_paths.push(row.file_path);
        qualified_names.push(row.qualified_name);
        heading_levels.push(row.heading_level);
        body_texts.push(row.body_text);
        body_byte_starts.push(row.body_byte_start);
        body_byte_ends.push(row.body_byte_end);
        child_counts.push(row.child_count);
        parent_stable_ids.push(row.parent_stable_id);
        content_hashes.push(row.content_hash);
    }

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(stable_symbol_ids)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(qualified_names)),
            Arc::new(UInt8Array::from(heading_levels)),
            Arc::new(LargeStringArray::from(body_texts)),
            Arc::new(UInt64Array::from(body_byte_starts)),
            Arc::new(UInt64Array::from(body_byte_ends)),
            Arc::new(UInt32Array::from(child_counts)),
            Arc::new(StringArray::from(parent_stable_ids)),
            Arc::new(StringArray::from(content_hashes)),
        ],
    )
    .context("failed to build LanceDB sections batch")
}

fn sections_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("stable_symbol_id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("qualified_name", DataType::Utf8, false),
        Field::new("heading_level", DataType::UInt8, false),
        Field::new("body_text", DataType::LargeUtf8, false),
        Field::new("body_byte_start", DataType::UInt64, false),
        Field::new("body_byte_end", DataType::UInt64, false),
        Field::new("child_count", DataType::UInt32, false),
        Field::new("parent_stable_id", DataType::Utf8, true),
        Field::new("content_hash", DataType::Utf8, false),
    ]))
}

fn read_file_bytes(worktree_root: &Path, path: &str) -> Result<Vec<u8>> {
    fs::read(worktree_root.join(path))
        .with_context(|| format!("failed to read `{}`", worktree_root.join(path).display()))
}

fn heading_level(body_text: &str) -> u8 {
    body_text
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == b'#')
        .count()
        .try_into()
        .unwrap_or(u8::MAX)
}

fn is_markdown_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
        })
}

fn sql_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}
