use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use arrow_array::{
    FixedSizeListArray, Float32Array, LargeStringArray, RecordBatch, StringArray, UInt32Array,
    UInt64Array, UInt8Array,
};
use arrow_buffer::NullBuffer;
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lancedb::index::{scalar::FtsIndexBuilder, vector::IvfHnswSqIndexBuilder, Index, IndexType};

use crate::content_hash::blake3_hex;
use crate::{
    GraphEdgeArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphSymbolArtifact,
    RelationKind,
};

pub const SECTIONS_DATASET_DIR: &str = "sections.lancedb";
pub const SECTIONS_TABLE: &str = "section_bodies";
const SECTION_VECTOR_DIMENSIONS: usize = 768;
const SECTION_EMBED_MAX_BODY_BYTES: usize = 4096;
const SECTION_EMBED_BATCH_SIZE_DEFAULT: usize = 64;
const SECTION_EMBED_BATCH_SIZE_ENV: &str = "SPUR_GRAPH_SECTION_EMBED_BATCH_SIZE";
const SECTION_EMBED_SKIP_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionEmbeddingOptions {
    pub skip_embeddings: bool,
    pub batch_size: usize,
}

impl SectionEmbeddingOptions {
    pub fn from_env() -> Self {
        let skip_embeddings = matches!(
            std::env::var(SECTION_EMBED_SKIP_ENV),
            Ok(value) if value == "1"
        );
        let batch_size = std::env::var(SECTION_EMBED_BATCH_SIZE_ENV)
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(SECTION_EMBED_BATCH_SIZE_DEFAULT);

        Self {
            skip_embeddings,
            batch_size,
        }
    }
}

impl Default for SectionEmbeddingOptions {
    fn default() -> Self {
        Self {
            skip_embeddings: false,
            batch_size: SECTION_EMBED_BATCH_SIZE_DEFAULT,
        }
    }
}

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
    vector: Option<Vec<f32>>,
}

pub fn write_sections_dataset(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) -> Result<()> {
    write_sections_dataset_with_options(
        artifact,
        worktree_root,
        artifact_dir,
        SectionEmbeddingOptions::from_env(),
    )
}

fn write_sections_dataset_with_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionEmbeddingOptions,
) -> Result<()> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    write_sections_dataset_without_current_runtime(
                        artifact,
                        worktree_root,
                        artifact_dir,
                        options,
                    )
                })
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
    }
    write_sections_dataset_without_current_runtime(artifact, worktree_root, artifact_dir, options)
}

fn write_sections_dataset_without_current_runtime(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionEmbeddingOptions,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create LanceDB runtime")?;
    runtime.block_on(write_sections_dataset_async(
        artifact,
        worktree_root,
        artifact_dir,
        options,
    ))
}

async fn write_sections_dataset_async(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionEmbeddingOptions,
) -> Result<()> {
    let rows = section_rows(artifact, worktree_root)?;
    let vectors = embed_eligible_rows(&rows, options);
    let rows: Vec<SectionRow> = rows
        .into_iter()
        .zip(vectors)
        .map(|(mut row, vector)| {
            row.vector = vector;
            row
        })
        .collect();

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
        ensure_vector_index(&table).await?;
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

async fn ensure_vector_index(table: &lancedb::Table) -> Result<()> {
    let vector_rows = table
        .count_rows(Some("vector IS NOT NULL".to_owned()))
        .await
        .context("failed to count LanceDB section vector rows")?;

    if vector_rows == 0 {
        return Ok(());
    }

    if table
        .list_indices()
        .await
        .context("failed to list LanceDB section indices")?
        .iter()
        .any(|index| {
            is_vector_index_type(&index.index_type) && index.columns.as_slice() == ["vector"]
        })
    {
        return Ok(());
    }

    table
        .create_index(
            &["vector"],
            Index::IvfHnswSq(IvfHnswSqIndexBuilder::default()),
        )
        .execute()
        .await
        .context("failed to create LanceDB vector HNSW index")
}

fn is_vector_index_type(index_type: &IndexType) -> bool {
    matches!(
        index_type,
        IndexType::IvfFlat
            | IndexType::IvfSq
            | IndexType::IvfPq
            | IndexType::IvfRq
            | IndexType::IvfHnswPq
            | IndexType::IvfHnswSq
            | IndexType::IvfHnswFlat
    )
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
        let bytes = match read_file_bytes(worktree_root, path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "section_rows: skipping unreadable file");
                continue;
            }
        };
        let content_hash = blake3_hex(&bytes);
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "section_rows: skipping non-UTF-8 markdown");
                continue;
            }
        };
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
        let bytes = match read_file_bytes(worktree_root, &manifest.path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(path = %manifest.path, error = %e, "section_rows: skipping unreadable file");
                continue;
            }
        };
        let content_hash = blake3_hex(&bytes);
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(e) => {
                tracing::warn!(path = %manifest.path, error = %e, "section_rows: skipping non-UTF-8 markdown");
                continue;
            }
        };
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
            vector: None,
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
        vector: None,
    })
}

fn embed_eligible_rows(
    rows: &[SectionRow],
    options: SectionEmbeddingOptions,
) -> Vec<Option<Vec<f32>>> {
    let result = vec![None; rows.len()];
    if options.skip_embeddings || !rows.iter().any(is_embedding_eligible) {
        return result;
    }

    let model = match TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::NomicEmbedTextV15).with_show_download_progress(false),
    ) {
        Ok(model) => model,
        Err(error) => {
            tracing::warn!(error = %error, "fastembed model unavailable; skipping section embeddings");
            return result;
        }
    };

    embed_eligible_rows_with(rows, options, |texts| {
        model.embed(texts.to_vec(), None).map_err(Into::into)
    })
}

fn embed_eligible_rows_with<F>(
    rows: &[SectionRow],
    options: SectionEmbeddingOptions,
    mut embed_batch: F,
) -> Vec<Option<Vec<f32>>>
where
    F: FnMut(&[&str]) -> Result<Vec<Vec<f32>>>,
{
    let eligible: Vec<(usize, &str)> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| is_embedding_eligible(row))
        .map(|(index, row)| (index, row.body_text.as_str()))
        .collect();

    let mut result = vec![None; rows.len()];
    if options.skip_embeddings {
        return result;
    }
    if eligible.is_empty() {
        return result;
    }

    let batch_size = if options.batch_size == 0 {
        SECTION_EMBED_BATCH_SIZE_DEFAULT
    } else {
        options.batch_size
    };

    for chunk in eligible.chunks(batch_size) {
        let texts: Vec<&str> = chunk.iter().map(|(_, text)| *text).collect();
        let embeddings = match embed_batch(&texts) {
            Ok(embeddings) => embeddings,
            Err(error) => {
                tracing::warn!(error = %error, "fastembed encode failed for section embedding batch; skipping remaining section embeddings");
                return result;
            }
        };

        if embeddings.len() != chunk.len() {
            tracing::warn!(
                expected = chunk.len(),
                actual = embeddings.len(),
                "fastembed returned unexpected section embedding count"
            );
            return result;
        }

        for ((index, _), embedding) in chunk.iter().copied().zip(embeddings) {
            if embedding.len() == SECTION_VECTOR_DIMENSIONS {
                result[index] = Some(embedding);
            } else {
                tracing::warn!(
                    stable_symbol_id = %rows[index].stable_symbol_id,
                    dimensions = embedding.len(),
                    "fastembed returned unexpected section embedding dimensions"
                );
            }
        }
    }

    result
}

fn is_embedding_eligible(row: &SectionRow) -> bool {
    row.heading_level >= 2 && row.body_text.len() <= SECTION_EMBED_MAX_BODY_BYTES
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
    let mut flat_vectors = Vec::with_capacity(rows.len() * SECTION_VECTOR_DIMENSIONS);
    let mut vector_validity = Vec::with_capacity(rows.len());

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
        if let Some(vector) = row
            .vector
            .filter(|vector| vector.len() == SECTION_VECTOR_DIMENSIONS)
        {
            flat_vectors.extend(vector);
            vector_validity.push(true);
        } else {
            flat_vectors.extend(std::iter::repeat_n(0.0f32, SECTION_VECTOR_DIMENSIONS));
            vector_validity.push(false);
        }
    }

    let vector_array = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        SECTION_VECTOR_DIMENSIONS as i32,
        Arc::new(Float32Array::from(flat_vectors)),
        Some(NullBuffer::from(vector_validity)),
    )
    .context("failed to build LanceDB section vector array")?;

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
            Arc::new(vector_array),
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
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                SECTION_VECTOR_DIMENSIONS as i32,
            ),
            true,
        ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn section_row_fixture(heading_level: u8, body_text: String) -> SectionRow {
        SectionRow {
            stable_symbol_id: "symbol".to_owned(),
            file_path: "docs/example.md".to_owned(),
            qualified_name: "docs/example.md::Section".to_owned(),
            heading_level,
            body_text,
            body_byte_start: 0,
            body_byte_end: 0,
            child_count: 0,
            parent_stable_id: None,
            content_hash: "hash".to_owned(),
            vector: None,
        }
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock")
    }

    #[test]
    fn sections_schema_includes_nullable_vector_column() {
        let schema = sections_schema();
        let field = schema.field_with_name("vector").expect("vector field");

        assert!(field.is_nullable());
        match field.data_type() {
            DataType::FixedSizeList(item, 768) => {
                assert_eq!(item.name(), "item");
                assert_eq!(item.data_type(), &DataType::Float32);
                assert!(item.is_nullable());
            }
            data_type => panic!("expected FixedSizeList<Float32, 768>, got {data_type:?}"),
        }
    }

    #[test]
    fn embed_eligible_rows_returns_none_for_h1_and_oversized() {
        let rows = vec![
            section_row_fixture(1, "# Title\n\nBody".to_owned()),
            section_row_fixture(2, format!("## Heading\n\n{}", "x".repeat(4097))),
        ];

        assert_eq!(
            embed_eligible_rows(&rows, SectionEmbeddingOptions::default()),
            vec![None, None]
        );
    }

    #[test]
    fn section_embedding_options_from_env_uses_defaults_for_missing_invalid_and_zero() {
        let _lock = env_lock();
        let _skip = EnvGuard::remove(SECTION_EMBED_SKIP_ENV);
        let batch = EnvGuard::remove(SECTION_EMBED_BATCH_SIZE_ENV);

        assert_eq!(
            SectionEmbeddingOptions::from_env(),
            SectionEmbeddingOptions {
                skip_embeddings: false,
                batch_size: SECTION_EMBED_BATCH_SIZE_DEFAULT,
            }
        );

        std::env::set_var(SECTION_EMBED_BATCH_SIZE_ENV, "not-a-number");
        assert_eq!(
            SectionEmbeddingOptions::from_env().batch_size,
            SECTION_EMBED_BATCH_SIZE_DEFAULT
        );

        std::env::set_var(SECTION_EMBED_BATCH_SIZE_ENV, "0");
        assert_eq!(
            SectionEmbeddingOptions::from_env().batch_size,
            SECTION_EMBED_BATCH_SIZE_DEFAULT
        );

        drop(batch);
    }

    #[test]
    fn section_embedding_options_from_env_accepts_skip_and_valid_batch_size() {
        let _lock = env_lock();
        let _skip = EnvGuard::set(SECTION_EMBED_SKIP_ENV, "1");
        let _batch = EnvGuard::set(SECTION_EMBED_BATCH_SIZE_ENV, "7");

        assert_eq!(
            SectionEmbeddingOptions::from_env(),
            SectionEmbeddingOptions {
                skip_embeddings: true,
                batch_size: 7,
            }
        );
    }

    #[test]
    fn embed_eligible_rows_returns_none_without_calling_embedder_when_skipped() {
        let rows = vec![section_row_fixture(
            2,
            "## Install\n\nInstall body.".to_owned(),
        )];
        let options = SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 1,
        };

        assert_eq!(
            embed_eligible_rows_with(&rows, options, |_| panic!("embedder should not be called")),
            vec![None]
        );
    }

    #[test]
    fn embed_eligible_rows_uses_configured_batch_size() {
        let rows = vec![
            section_row_fixture(2, "## One\n\nBody one.".to_owned()),
            section_row_fixture(1, "# Title\n\nSkipped.".to_owned()),
            section_row_fixture(2, "## Two\n\nBody two.".to_owned()),
            section_row_fixture(2, "## Three\n\nBody three.".to_owned()),
        ];
        let options = SectionEmbeddingOptions {
            skip_embeddings: false,
            batch_size: 2,
        };
        let mut batch_sizes = Vec::new();

        let vectors = embed_eligible_rows_with(&rows, options, |texts| {
            batch_sizes.push(texts.len());
            Ok(vec![vec![0.25; SECTION_VECTOR_DIMENSIONS]; texts.len()])
        });

        assert_eq!(batch_sizes, vec![2, 1]);
        assert!(vectors[0].is_some());
        assert!(vectors[1].is_none());
        assert!(vectors[2].is_some());
        assert!(vectors[3].is_some());
    }
}
