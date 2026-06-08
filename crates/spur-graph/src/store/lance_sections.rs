use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use arrow_array::{
    Array as _, FixedSizeListArray, Float32Array, LargeStringArray, RecordBatch, StringArray,
    UInt32Array, UInt64Array, UInt8Array,
};
use arrow_buffer::NullBuffer;
use arrow_schema::{DataType, Field, Schema};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use lancedb::index::{scalar::FtsIndexBuilder, vector::IvfHnswSqIndexBuilder, Index, IndexType};
use lancedb::query::{ExecutableQuery as _, QueryBase as _, Select};

use crate::content_hash::blake3_hex;
use crate::{
    GraphEdgeArtifact, GraphFileManifestEntry, GraphIndexArtifact, GraphSymbolArtifact,
    RelationKind,
};

pub const EMBED_MODEL_NAME: &str = "BGESmallENV15";
pub const EMBED_MODEL_APPROX_SIZE_MB: usize = 130;
pub const SECTIONS_DATASET_DIR: &str = "sections.lancedb";
pub const SECTIONS_TABLE: &str = "section_bodies";
pub const CODE_SYMBOLS_DATASET_DIR: &str = "code_symbols.lance";
pub const CODE_SYMBOLS_TABLE: &str = "code_symbols";
const SECTION_VECTOR_DIMENSIONS: usize = 384;
const SECTION_EMBED_MAX_BODY_BYTES: usize = 4096;
const SECTION_EMBED_BATCH_SIZE_DEFAULT: usize = 64;
const SECTION_EMBED_BATCH_SIZE_ENV: &str = "SPUR_GRAPH_SECTION_EMBED_BATCH_SIZE";
pub const SECTION_EMBED_SKIP_ENV: &str = "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS";
const SECTION_WRITE_BATCH_SIZE_DEFAULT: usize = 512;
const SECTION_WRITE_BATCH_SIZE_ENV: &str = "SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE";
// Integration tests spawn the debug-built CLI; keep this hook out of release builds.
#[cfg(debug_assertions)]
const SECTION_SIDECAR_TEST_FAIL_ENV: &str = "SPUR_GRAPH_TEST_FAIL_SECTION_SIDECAR";

pub type SectionSidecarProgressCallback<'a> = dyn Fn(SectionSidecarProgressEvent) + Sync + 'a;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionSidecarProgressEvent {
    Started {
        total_rows: usize,
        markdown_files: usize,
        embeddings_enabled: bool,
        embedding_batch_size: usize,
        write_batch_size: usize,
    },
    BatchStarted {
        batch_index: usize,
        batch_rows: usize,
        embedding_eligible_rows: usize,
        embeddings_available: bool,
        processed_rows: usize,
        total_rows: usize,
    },
    EmbeddingChunkStarted {
        batch_index: usize,
        batch_rows: usize,
        chunk_index: usize,
        chunk_count: usize,
        chunk_rows: usize,
        completed_eligible_rows: usize,
        embedding_eligible_rows: usize,
        processed_rows: usize,
        total_rows: usize,
    },
    BatchWritten {
        batch_index: usize,
        written_rows: usize,
        skipped_existing_rows: usize,
        processed_rows: usize,
        total_rows: usize,
    },
    ModelDownloading {
        model_name: &'static str,
        approximate_size_mb: usize,
    },
    Indexing {
        label: &'static str,
    },
    Finished {
        total_rows: usize,
        written_rows: usize,
        skipped_existing_rows: usize,
    },
    Failed {
        error: String,
    },
}

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

    pub fn from_env_with_skip_override(skip_embeddings_override: bool) -> Self {
        let mut options = Self::from_env();
        options.skip_embeddings |= skip_embeddings_override;
        options
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionSidecarOptions {
    pub embedding: SectionEmbeddingOptions,
    pub write_batch_size: usize,
}

impl SectionSidecarOptions {
    pub fn from_env() -> Self {
        Self {
            embedding: SectionEmbeddingOptions::from_env(),
            write_batch_size: section_write_batch_size_from_env(),
        }
    }

    pub fn from_env_with_skip_override(skip_embeddings_override: bool) -> Self {
        Self {
            embedding: SectionEmbeddingOptions::from_env_with_skip_override(
                skip_embeddings_override,
            ),
            write_batch_size: section_write_batch_size_from_env(),
        }
    }

    pub fn from_embedding_options(embedding: SectionEmbeddingOptions) -> Self {
        Self {
            embedding,
            write_batch_size: section_write_batch_size_from_env(),
        }
    }
}

impl Default for SectionSidecarOptions {
    fn default() -> Self {
        Self {
            embedding: SectionEmbeddingOptions::default(),
            write_batch_size: SECTION_WRITE_BATCH_SIZE_DEFAULT,
        }
    }
}

fn section_write_batch_size_from_env() -> usize {
    std::env::var(SECTION_WRITE_BATCH_SIZE_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(SECTION_WRITE_BATCH_SIZE_DEFAULT)
}

fn normalize_section_write_batch_size(write_batch_size: usize) -> usize {
    if write_batch_size == 0 {
        SECTION_WRITE_BATCH_SIZE_DEFAULT
    } else {
        write_batch_size
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

#[derive(Debug)]
struct SymbolRow {
    stable_symbol_id: String,
    file_path: String,
    qualified_name: String,
    entity_name: String,
    symbol_kind: String,
    embed_text: String,
    vector: Option<Vec<f32>>,
    content_hash: String,
}

pub fn write_sections_dataset(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) -> Result<()> {
    write_sections_dataset_with_sidecar_options(
        artifact,
        worktree_root,
        artifact_dir,
        SectionSidecarOptions::from_env(),
    )
}

pub fn write_sections_dataset_best_effort(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) {
    write_sections_dataset_best_effort_with_sidecar_options(
        artifact,
        worktree_root,
        artifact_dir,
        SectionSidecarOptions::from_env(),
    );
}

pub fn write_sections_dataset_best_effort_with_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionEmbeddingOptions,
) {
    if let Err(error) =
        write_sections_dataset_with_options(artifact, worktree_root, artifact_dir, options)
    {
        tracing::warn!(
            error = %error,
            artifact_dir = %artifact_dir.display(),
            "spur-graph: section sidecar write failed; graph artifact remains usable"
        );
    }
}

fn write_sections_dataset_best_effort_with_sidecar_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
) {
    write_sections_dataset_best_effort_with_sidecar_options_and_progress(
        artifact,
        worktree_root,
        artifact_dir,
        options,
        None,
    );
}

pub fn write_sections_dataset_best_effort_with_sidecar_options_and_progress(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
    progress: Option<&SectionSidecarProgressCallback<'_>>,
) {
    if let Err(error) = write_sections_dataset_with_sidecar_options_and_progress(
        artifact,
        worktree_root,
        artifact_dir,
        options,
        progress,
    ) {
        emit_progress(
            progress,
            SectionSidecarProgressEvent::Failed {
                error: error.to_string(),
            },
        );
        tracing::warn!(
            error = %error,
            artifact_dir = %artifact_dir.display(),
            "spur-graph: section sidecar write failed; graph artifact remains usable"
        );
    }
}

fn write_sections_dataset_with_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionEmbeddingOptions,
) -> Result<()> {
    write_sections_dataset_with_sidecar_options(
        artifact,
        worktree_root,
        artifact_dir,
        SectionSidecarOptions::from_embedding_options(options),
    )
}

fn write_sections_dataset_with_sidecar_options(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
) -> Result<()> {
    write_sections_dataset_with_sidecar_options_and_progress(
        artifact,
        worktree_root,
        artifact_dir,
        options,
        None,
    )
}

fn write_sections_dataset_with_sidecar_options_and_progress(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
    progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<()> {
    #[cfg(debug_assertions)]
    if matches!(
        std::env::var(SECTION_SIDECAR_TEST_FAIL_ENV),
        Ok(value) if value == "1"
    ) {
        anyhow::bail!("forced section sidecar failure for tests");
    }

    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    write_sections_dataset_without_current_runtime(
                        artifact,
                        worktree_root,
                        artifact_dir,
                        options,
                        progress,
                    )
                })
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
    }
    write_sections_dataset_without_current_runtime(
        artifact,
        worktree_root,
        artifact_dir,
        options,
        progress,
    )
}

fn write_sections_dataset_without_current_runtime(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
    progress: Option<&SectionSidecarProgressCallback<'_>>,
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
        progress,
    ))
}

async fn write_sections_dataset_async(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
    progress: Option<&SectionSidecarProgressCallback<'_>>,
) -> Result<()> {
    let mut batcher = SectionRowBatcher::new(artifact, worktree_root, options.write_batch_size);
    let total_rows = batcher.total_rows();
    emit_progress(
        progress,
        SectionSidecarProgressEvent::Started {
            total_rows,
            markdown_files: batcher.markdown_file_count(),
            embeddings_enabled: !options.embedding.skip_embeddings,
            embedding_batch_size: options.embedding.batch_size,
            write_batch_size: batcher.write_batch_size(),
        },
    );

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
    let mut table = db.open_table(SECTIONS_TABLE).execute().await.ok();
    let mut existing_versions = ExistingFileVersions::new(table.as_ref());
    let mut embedder = SectionEmbedder::new(options.embedding);
    let mut dataset_changed = false;
    let mut batch_index = 0usize;
    let mut processed_rows = 0usize;
    let mut written_rows = 0usize;
    let mut skipped_existing_rows = 0usize;

    while let Some(rows) = batcher.next_batch()? {
        batch_index += 1;
        processed_rows += rows.len();
        let candidate_rows = rows.len();
        let mut rows = existing_versions.retain_new_rows(rows).await?;
        skipped_existing_rows += candidate_rows.saturating_sub(rows.len());
        if rows.is_empty() {
            emit_progress(
                progress,
                SectionSidecarProgressEvent::BatchWritten {
                    batch_index,
                    written_rows,
                    skipped_existing_rows,
                    processed_rows,
                    total_rows,
                },
            );
            continue;
        }
        let embedding_eligible_rows = rows.iter().filter(|row| is_embedding_eligible(row)).count();
        if embedding_eligible_rows > 0 && embedder.needs_model_init() {
            emit_progress(
                progress,
                SectionSidecarProgressEvent::ModelDownloading {
                    model_name: EMBED_MODEL_NAME,
                    approximate_size_mb: EMBED_MODEL_APPROX_SIZE_MB,
                },
            );
        }
        let embeddings_available = embedding_eligible_rows > 0 && embedder.prepare_model();
        emit_progress(
            progress,
            SectionSidecarProgressEvent::BatchStarted {
                batch_index,
                batch_rows: rows.len(),
                embedding_eligible_rows,
                embeddings_available,
                processed_rows,
                total_rows,
            },
        );
        let batch_rows = rows.len();
        embedder.embed_rows_with_progress(&mut rows, |chunk| {
            emit_progress(
                progress,
                SectionSidecarProgressEvent::EmbeddingChunkStarted {
                    batch_index,
                    batch_rows,
                    chunk_index: chunk.chunk_index,
                    chunk_count: chunk.chunk_count,
                    chunk_rows: chunk.chunk_rows,
                    completed_eligible_rows: chunk.completed_eligible_rows,
                    embedding_eligible_rows: chunk.embedding_eligible_rows,
                    processed_rows,
                    total_rows,
                },
            );
        });
        let batch_rows = rows.len();
        let batch = rows_to_batch(rows, schema.clone())?;
        if let Some(table) = table.as_ref() {
            table
                .add(batch)
                .execute()
                .await
                .context("failed to append LanceDB section rows")?;
            dataset_changed = true;
        } else {
            table = Some(
                db.create_table(SECTIONS_TABLE, batch)
                    .execute()
                    .await
                    .context("failed to create LanceDB sections table")?,
            );
            dataset_changed = true;
        }
        written_rows += batch_rows;
        emit_progress(
            progress,
            SectionSidecarProgressEvent::BatchWritten {
                batch_index,
                written_rows,
                skipped_existing_rows,
                processed_rows,
                total_rows,
            },
        );
    }

    if table.is_none() {
        let empty_batch = rows_to_batch(Vec::new(), schema.clone())?;
        table = Some(
            db.create_table(SECTIONS_TABLE, empty_batch)
                .execute()
                .await
                .context("failed to create LanceDB sections table")?,
        );
        dataset_changed = true;
    }

    if dataset_changed {
        let table = table
            .as_ref()
            .expect("section table should exist after dataset change");
        emit_progress(
            progress,
            SectionSidecarProgressEvent::Indexing {
                label: "body_text FTS",
            },
        );
        ensure_body_text_fts_index(table).await?;
        emit_progress(
            progress,
            SectionSidecarProgressEvent::Indexing {
                label: "vector HNSW",
            },
        );
        ensure_vector_index(table).await?;
    }

    emit_progress(
        progress,
        SectionSidecarProgressEvent::Finished {
            total_rows,
            written_rows,
            skipped_existing_rows,
        },
    );
    write_symbol_rows_dataset_async(
        artifact,
        worktree_root,
        artifact_dir,
        options.write_batch_size,
    )
    .await?;
    Ok(())
}

async fn write_symbol_rows_dataset_async(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    write_batch_size: usize,
) -> Result<()> {
    let mut batcher = SymbolRowBatcher::new(artifact, worktree_root, write_batch_size);

    fs::create_dir_all(artifact_dir)
        .with_context(|| format!("failed to create `{}`", artifact_dir.display()))?;
    let db = lancedb::connect(artifact_dir.to_string_lossy().as_ref())
        .execute()
        .await
        .context("failed to connect to code_symbols.lance")?;
    let schema = symbol_rows_schema();
    let mut table = db.open_table(CODE_SYMBOLS_TABLE).execute().await.ok();
    let mut existing_versions = ExistingSymbolFileVersions::new(table.as_ref());

    while let Some(rows) = batcher.next_batch()? {
        let rows = existing_versions.retain_new_rows(rows).await?;
        if rows.is_empty() {
            continue;
        }
        let batch = symbol_rows_to_batch(rows, schema.clone())?;
        if let Some(table) = table.as_ref() {
            table
                .add(batch)
                .execute()
                .await
                .context("failed to append LanceDB code symbol rows")?;
        } else {
            table = Some(
                db.create_table(CODE_SYMBOLS_TABLE, batch)
                    .execute()
                    .await
                    .context("failed to create LanceDB code symbols table")?,
            );
        }
    }

    if table.is_none() {
        let empty_batch = symbol_rows_to_batch(Vec::new(), schema)?;
        db.create_table(CODE_SYMBOLS_TABLE, empty_batch)
            .execute()
            .await
            .context("failed to create LanceDB code symbols table")?;
    }

    Ok(())
}

fn emit_progress(
    progress: Option<&SectionSidecarProgressCallback<'_>>,
    event: SectionSidecarProgressEvent,
) {
    if let Some(progress) = progress {
        progress(event);
    }
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

struct ExistingFileVersions {
    table: Option<lancedb::Table>,
    missing_by_file_version: HashSet<(String, String)>,
}

impl ExistingFileVersions {
    fn new(table: Option<&lancedb::Table>) -> Self {
        Self {
            table: table.cloned(),
            missing_by_file_version: HashSet::new(),
        }
    }

    async fn retain_new_rows(&mut self, rows: Vec<SectionRow>) -> Result<Vec<SectionRow>> {
        if rows.is_empty() || self.table.is_none() {
            return Ok(rows);
        }
        let existing = self.preload_existing_file_versions(&rows).await?;
        let mut retained = Vec::with_capacity(rows.len());
        for row in rows {
            let key = (row.file_path.clone(), row.content_hash.clone());
            if !existing.contains(&key) {
                self.missing_by_file_version.insert(key);
                retained.push(row);
            }
        }
        Ok(retained)
    }

    async fn preload_existing_file_versions(
        &mut self,
        rows: &[SectionRow],
    ) -> Result<HashSet<(String, String)>> {
        let Some(table) = self.table.as_ref() else {
            return Ok(HashSet::new());
        };
        let mut file_paths: Vec<_> = rows
            .iter()
            .map(|row| row.file_path.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if file_paths.is_empty() {
            return Ok(HashSet::new());
        }
        file_paths.sort_unstable();
        let literals = file_paths
            .into_iter()
            .map(|path| format!("'{}'", sql_string_literal(path)))
            .collect::<Vec<_>>()
            .join(", ");
        let filter = format!("file_path IN ({literals})");
        let mut batches = table
            .query()
            .only_if(filter)
            .select(Select::columns(&["file_path", "content_hash"]))
            .execute()
            .await
            .context("failed to query existing LanceDB section rows")?;
        let mut existing = HashSet::new();
        while let Some(batch) = std::future::poll_fn(|cx| batches.as_mut().poll_next(cx))
            .await
            .transpose()
            .context("failed to read existing LanceDB section rows")?
        {
            let file_paths = batch
                .column_by_name("file_path")
                .context("existing section rows missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("existing section file_path column was not Utf8")?;
            let content_hashes = batch
                .column_by_name("content_hash")
                .context("existing section rows missing content_hash column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("existing section content_hash column was not Utf8")?;
            for index in 0..batch.num_rows() {
                existing.insert((
                    file_paths.value(index).to_owned(),
                    content_hashes.value(index).to_owned(),
                ));
            }
        }
        existing.retain(|key| !self.missing_by_file_version.contains(key));
        Ok(existing)
    }
}

struct ExistingSymbolFileVersions {
    table: Option<lancedb::Table>,
    missing_by_file_version: HashSet<(String, String)>,
}

impl ExistingSymbolFileVersions {
    fn new(table: Option<&lancedb::Table>) -> Self {
        Self {
            table: table.cloned(),
            missing_by_file_version: HashSet::new(),
        }
    }

    async fn retain_new_rows(&mut self, rows: Vec<SymbolRow>) -> Result<Vec<SymbolRow>> {
        if rows.is_empty() || self.table.is_none() {
            return Ok(rows);
        }
        let existing = self.preload_existing_file_versions(&rows).await?;
        let mut retained = Vec::with_capacity(rows.len());
        for row in rows {
            let key = (row.file_path.clone(), row.content_hash.clone());
            if !existing.contains(&key) {
                self.missing_by_file_version.insert(key);
                retained.push(row);
            }
        }
        Ok(retained)
    }

    async fn preload_existing_file_versions(
        &mut self,
        rows: &[SymbolRow],
    ) -> Result<HashSet<(String, String)>> {
        let Some(table) = self.table.as_ref() else {
            return Ok(HashSet::new());
        };
        let mut file_paths: Vec<_> = rows
            .iter()
            .map(|row| row.file_path.as_str())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if file_paths.is_empty() {
            return Ok(HashSet::new());
        }
        file_paths.sort_unstable();
        let literals = file_paths
            .into_iter()
            .map(|path| format!("'{}'", sql_string_literal(path)))
            .collect::<Vec<_>>()
            .join(", ");
        let filter = format!("file_path IN ({literals})");
        let mut batches = table
            .query()
            .only_if(filter)
            .select(Select::columns(&["file_path", "content_hash"]))
            .execute()
            .await
            .context("failed to query existing LanceDB code symbol rows")?;
        let mut existing = HashSet::new();
        while let Some(batch) = std::future::poll_fn(|cx| batches.as_mut().poll_next(cx))
            .await
            .transpose()
            .context("failed to read existing LanceDB code symbol rows")?
        {
            let file_paths = batch
                .column_by_name("file_path")
                .context("existing code symbol rows missing file_path column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("existing code symbol file_path column was not Utf8")?;
            let content_hashes = batch
                .column_by_name("content_hash")
                .context("existing code symbol rows missing content_hash column")?
                .as_any()
                .downcast_ref::<StringArray>()
                .context("existing code symbol content_hash column was not Utf8")?;
            for index in 0..batch.num_rows() {
                existing.insert((
                    file_paths.value(index).to_owned(),
                    content_hashes.value(index).to_owned(),
                ));
            }
        }
        existing.retain(|key| !self.missing_by_file_version.contains(key));
        Ok(existing)
    }
}

struct SectionRowBatcher<'a> {
    worktree_root: &'a Path,
    child_count_by_parent: HashMap<&'a str, u32>,
    parent_by_child: HashMap<&'a str, String>,
    manifest_by_path: BTreeMap<&'a str, &'a GraphFileManifestEntry>,
    sections_by_path: BTreeMap<&'a str, Vec<&'a GraphSymbolArtifact>>,
    ordered_paths: Vec<&'a str>,
    next_path_index: usize,
    pending_rows: VecDeque<SectionRow>,
    write_batch_size: usize,
}

impl<'a> SectionRowBatcher<'a> {
    fn new(
        artifact: &'a GraphIndexArtifact,
        worktree_root: &'a Path,
        write_batch_size: usize,
    ) -> Self {
        let write_batch_size = normalize_section_write_batch_size(write_batch_size);
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
        for sections in sections_by_path.values_mut() {
            sections.sort_by(|a, b| {
                a.byte_range[0]
                    .cmp(&b.byte_range[0])
                    .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
            });
        }

        let mut ordered_paths = BTreeSet::new();
        for path in sections_by_path.keys() {
            ordered_paths.insert(*path);
        }
        for manifest in manifest_by_path.values() {
            if is_markdown_path(&manifest.path)
                && !sections_by_path.contains_key(manifest.path.as_str())
            {
                ordered_paths.insert(manifest.path.as_str());
            }
        }

        Self {
            worktree_root,
            child_count_by_parent,
            parent_by_child,
            manifest_by_path,
            sections_by_path,
            ordered_paths: ordered_paths.into_iter().collect(),
            next_path_index: 0,
            pending_rows: VecDeque::new(),
            write_batch_size,
        }
    }

    fn next_batch(&mut self) -> Result<Option<Vec<SectionRow>>> {
        let mut batch = Vec::with_capacity(self.write_batch_size);
        while batch.len() < self.write_batch_size {
            if let Some(row) = self.pending_rows.pop_front() {
                batch.push(row);
                continue;
            }

            let Some(path) = self.ordered_paths.get(self.next_path_index).copied() else {
                break;
            };
            self.next_path_index += 1;
            self.pending_rows = VecDeque::from(self.rows_for_path(path)?);
        }

        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }

    fn total_rows(&self) -> usize {
        self.sections_by_path.values().map(Vec::len).sum::<usize>()
            + self
                .ordered_paths
                .iter()
                .filter(|path| !self.sections_by_path.contains_key(**path))
                .count()
    }

    fn markdown_file_count(&self) -> usize {
        self.ordered_paths.len()
    }

    fn write_batch_size(&self) -> usize {
        self.write_batch_size
    }

    fn rows_for_path(&self, path: &str) -> Result<Vec<SectionRow>> {
        let bytes = match read_file_bytes(self.worktree_root, path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "section_rows: skipping unreadable file");
                return Ok(Vec::new());
            }
        };
        let content_hash = blake3_hex(&bytes);
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "section_rows: skipping non-UTF-8 markdown");
                return Ok(Vec::new());
            }
        };

        if let Some(sections) = self.sections_by_path.get(path) {
            return sections
                .iter()
                .map(|section| {
                    section_row(
                        section,
                        source,
                        content_hash.as_str(),
                        &self.child_count_by_parent,
                        &self.parent_by_child,
                    )
                })
                .collect();
        }

        let Some(manifest) = self.manifest_by_path.get(path) else {
            return Ok(Vec::new());
        };
        Ok(vec![SectionRow {
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
        }])
    }
}

struct SymbolRowBatcher<'a> {
    worktree_root: &'a Path,
    symbols_by_path: BTreeMap<&'a str, Vec<&'a GraphSymbolArtifact>>,
    ordered_paths: Vec<&'a str>,
    next_path_index: usize,
    pending_rows: VecDeque<SymbolRow>,
    write_batch_size: usize,
}

impl<'a> SymbolRowBatcher<'a> {
    fn new(
        artifact: &'a GraphIndexArtifact,
        worktree_root: &'a Path,
        write_batch_size: usize,
    ) -> Self {
        let write_batch_size = normalize_section_write_batch_size(write_batch_size);
        let mut symbols_by_path: BTreeMap<&str, Vec<&GraphSymbolArtifact>> = BTreeMap::new();
        for symbol in &artifact.symbols {
            if is_code_symbol_kind(&symbol.symbol_kind) {
                symbols_by_path
                    .entry(symbol.file_path.as_str())
                    .or_default()
                    .push(symbol);
            }
        }
        for symbols in symbols_by_path.values_mut() {
            symbols.sort_by(|a, b| {
                a.byte_range[0]
                    .cmp(&b.byte_range[0])
                    .then(a.stable_symbol_id.cmp(&b.stable_symbol_id))
            });
        }
        let ordered_paths = symbols_by_path.keys().copied().collect();

        Self {
            worktree_root,
            symbols_by_path,
            ordered_paths,
            next_path_index: 0,
            pending_rows: VecDeque::new(),
            write_batch_size,
        }
    }

    fn next_batch(&mut self) -> Result<Option<Vec<SymbolRow>>> {
        let mut batch = Vec::with_capacity(self.write_batch_size);
        while batch.len() < self.write_batch_size {
            if let Some(row) = self.pending_rows.pop_front() {
                batch.push(row);
                continue;
            }

            let Some(path) = self.ordered_paths.get(self.next_path_index).copied() else {
                break;
            };
            self.next_path_index += 1;
            self.pending_rows = VecDeque::from(self.rows_for_path(path)?);
        }

        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }

    fn rows_for_path(&self, path: &str) -> Result<Vec<SymbolRow>> {
        let bytes = match read_file_bytes(self.worktree_root, path) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "symbol_rows: skipping unreadable file");
                return Ok(Vec::new());
            }
        };
        let content_hash = blake3_hex(&bytes);
        let source = match std::str::from_utf8(&bytes) {
            Ok(source) => source,
            Err(e) => {
                tracing::warn!(path = %path, error = %e, "symbol_rows: skipping non-UTF-8 source");
                return Ok(Vec::new());
            }
        };
        let Some(symbols) = self.symbols_by_path.get(path) else {
            return Ok(Vec::new());
        };

        let mut rows = Vec::new();
        for symbol in symbols {
            if let Some(row) = symbol_row(symbol, source, content_hash.as_str())? {
                rows.push(row);
            }
        }
        Ok(rows)
    }
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

fn symbol_row(
    symbol: &GraphSymbolArtifact,
    source: &str,
    content_hash: &str,
) -> Result<Option<SymbolRow>> {
    let doc_text = doc_text_for_symbol(source, symbol.byte_range[0]).with_context(|| {
        format!(
            "failed to derive doc text for symbol `{}` in `{}`",
            symbol.stable_symbol_id, symbol.file_path
        )
    })?;
    let has_long_doc = doc_text.chars().count() > 50;
    if !has_long_doc && !is_meaningful_entity_name(&symbol.entity_name) {
        return Ok(None);
    }

    let qualified_name = if symbol.qualified_name.is_empty() {
        symbol.entity_name.clone()
    } else {
        symbol.qualified_name.clone()
    };
    let embed_text = if has_long_doc {
        format!("{} {} {}", symbol.entity_name, qualified_name, doc_text)
    } else {
        format!(
            "{} {} {}",
            symbol.entity_name, qualified_name, symbol.symbol_kind
        )
    };

    Ok(Some(SymbolRow {
        stable_symbol_id: symbol.stable_symbol_id.clone(),
        file_path: symbol.file_path.clone(),
        qualified_name,
        entity_name: symbol.entity_name.clone(),
        symbol_kind: symbol.symbol_kind.clone(),
        embed_text,
        vector: None,
        content_hash: content_hash.to_owned(),
    }))
}

fn doc_text_for_symbol(source: &str, byte_start: usize) -> Result<String> {
    let prefix = source
        .get(..byte_start)
        .with_context(|| format!("symbol byte start {byte_start} is not a UTF-8 boundary"))?;
    let line_doc = preceding_line_doc_text(prefix);
    if !line_doc.is_empty() {
        return Ok(line_doc);
    }
    Ok(preceding_block_doc_text(prefix).unwrap_or_default())
}

fn preceding_line_doc_text(prefix: &str) -> String {
    let prefix = prefix.trim_end_matches([' ', '\t', '\r']);
    let mut lines = Vec::new();
    for line in prefix.lines().rev() {
        let trimmed = line.trim_start();
        let text = trimmed
            .strip_prefix("///")
            .or_else(|| trimmed.strip_prefix("//!"))
            .or_else(|| {
                trimmed
                    .strip_prefix('#')
                    .filter(|_| !trimmed.starts_with("#!") && !trimmed.starts_with("#["))
            });
        let Some(text) = text else {
            break;
        };
        lines.push(text.trim().to_owned());
    }
    lines.reverse();
    normalize_doc_lines(lines)
}

fn preceding_block_doc_text(prefix: &str) -> Option<String> {
    let prefix = prefix.trim_end_matches([' ', '\t', '\r']).trim_end();
    if !prefix.ends_with("*/") {
        return None;
    }
    let start = prefix.rfind("/*")?;
    let body = &prefix[start + 2..prefix.len().saturating_sub(2)];
    if !(body.starts_with('*') || body.starts_with('!')) {
        return None;
    }
    let body = body.trim_start_matches(['*', '!']);
    let lines = body
        .lines()
        .map(|line| {
            line.trim()
                .strip_prefix('*')
                .unwrap_or(line.trim())
                .trim()
                .to_owned()
        })
        .collect();
    Some(normalize_doc_lines(lines))
}

fn normalize_doc_lines(lines: Vec<String>) -> String {
    lines
        .into_iter()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_code_symbol_kind(symbol_kind: &str) -> bool {
    !matches!(symbol_kind, "section" | "external" | "commit")
}

fn is_meaningful_entity_name(entity_name: &str) -> bool {
    let mut has_alpha = false;
    let mut meaningful_len = 0usize;
    for ch in entity_name.chars() {
        if ch.is_alphabetic() {
            has_alpha = true;
        }
        if ch.is_alphanumeric() || ch == '_' {
            meaningful_len += 1;
        }
    }
    has_alpha && meaningful_len > 2
}

#[cfg(test)]
fn embed_eligible_rows(
    rows: &[SectionRow],
    options: SectionEmbeddingOptions,
) -> Vec<Option<Vec<f32>>> {
    let mut embedder = SectionEmbedder::new(options);
    embedder.embed_row_vectors(rows)
}

struct SectionEmbedder {
    options: SectionEmbeddingOptions,
    model: Option<Option<TextEmbedding>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionEmbeddingChunkProgress {
    chunk_index: usize,
    chunk_count: usize,
    chunk_rows: usize,
    completed_eligible_rows: usize,
    embedding_eligible_rows: usize,
}

impl SectionEmbedder {
    fn new(options: SectionEmbeddingOptions) -> Self {
        Self {
            options,
            model: None,
        }
    }

    fn needs_model_init(&self) -> bool {
        self.model.is_none() && !self.options.skip_embeddings
    }

    fn prepare_model(&mut self) -> bool {
        !self.options.skip_embeddings && self.model().is_some()
    }

    fn embed_rows_with_progress<F>(&mut self, rows: &mut [SectionRow], on_chunk_started: F)
    where
        F: FnMut(SectionEmbeddingChunkProgress),
    {
        let vectors = self.embed_row_vectors_with_progress(rows, on_chunk_started);
        for (row, vector) in rows.iter_mut().zip(vectors) {
            row.vector = vector;
        }
    }

    #[cfg(test)]
    fn embed_row_vectors(&mut self, rows: &[SectionRow]) -> Vec<Option<Vec<f32>>> {
        self.embed_row_vectors_with_progress(rows, |_| {})
    }

    fn embed_row_vectors_with_progress<F>(
        &mut self,
        rows: &[SectionRow],
        on_chunk_started: F,
    ) -> Vec<Option<Vec<f32>>>
    where
        F: FnMut(SectionEmbeddingChunkProgress),
    {
        let result = vec![None; rows.len()];
        if self.options.skip_embeddings || !rows.iter().any(is_embedding_eligible) {
            return result;
        }
        let options = self.options;
        let Some(model) = self.model().as_ref() else {
            return result;
        };

        embed_eligible_rows_with(rows, options, on_chunk_started, |texts| {
            model.embed(texts.to_vec(), None)
        })
    }

    fn model(&mut self) -> &Option<TextEmbedding> {
        self.model.get_or_insert_with(|| {
            let mut init_options = InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_show_download_progress(true);

            if let Some(cache_dir) = fastembed_cache_dir() {
                init_options = init_options.with_cache_dir(cache_dir);
            }

            match TextEmbedding::try_new(init_options) {
                Ok(model) => Some(model),
                Err(error) => {
                    tracing::warn!(error = %error, "fastembed model unavailable; skipping section embeddings");
                    None
                }
            }
        })
    }
}

fn embed_eligible_rows_with<F>(
    rows: &[SectionRow],
    options: SectionEmbeddingOptions,
    mut on_chunk_started: impl FnMut(SectionEmbeddingChunkProgress),
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

    let chunks: Vec<_> = eligible.chunks(batch_size).collect();
    let chunk_count = chunks.len();
    let mut completed_eligible_rows = 0usize;
    for (chunk_offset, chunk) in chunks.into_iter().enumerate() {
        let texts: Vec<&str> = chunk.iter().map(|(_, text)| *text).collect();
        on_chunk_started(SectionEmbeddingChunkProgress {
            chunk_index: chunk_offset + 1,
            chunk_count,
            chunk_rows: chunk.len(),
            completed_eligible_rows,
            embedding_eligible_rows: eligible.len(),
        });
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
        completed_eligible_rows += chunk.len();
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

fn symbol_rows_to_batch(rows: Vec<SymbolRow>, schema: Arc<Schema>) -> Result<RecordBatch> {
    let mut stable_symbol_ids = Vec::with_capacity(rows.len());
    let mut file_paths = Vec::with_capacity(rows.len());
    let mut qualified_names = Vec::with_capacity(rows.len());
    let mut entity_names = Vec::with_capacity(rows.len());
    let mut symbol_kinds = Vec::with_capacity(rows.len());
    let mut embed_texts = Vec::with_capacity(rows.len());
    let mut flat_vectors = Vec::with_capacity(rows.len() * SECTION_VECTOR_DIMENSIONS);
    let mut vector_validity = Vec::with_capacity(rows.len());
    let mut content_hashes = Vec::with_capacity(rows.len());

    for row in rows {
        stable_symbol_ids.push(row.stable_symbol_id);
        file_paths.push(row.file_path);
        qualified_names.push(row.qualified_name);
        entity_names.push(row.entity_name);
        symbol_kinds.push(row.symbol_kind);
        embed_texts.push(row.embed_text);
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
        content_hashes.push(row.content_hash);
    }

    let vector_array = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        SECTION_VECTOR_DIMENSIONS as i32,
        Arc::new(Float32Array::from(flat_vectors)),
        Some(NullBuffer::from(vector_validity)),
    )
    .context("failed to build LanceDB code symbol vector array")?;

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(stable_symbol_ids)),
            Arc::new(StringArray::from(file_paths)),
            Arc::new(StringArray::from(qualified_names)),
            Arc::new(StringArray::from(entity_names)),
            Arc::new(StringArray::from(symbol_kinds)),
            Arc::new(LargeStringArray::from(embed_texts)),
            Arc::new(vector_array),
            Arc::new(StringArray::from(content_hashes)),
        ],
    )
    .context("failed to build LanceDB code symbols batch")
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

fn symbol_rows_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("stable_symbol_id", DataType::Utf8, false),
        Field::new("file_path", DataType::Utf8, false),
        Field::new("qualified_name", DataType::Utf8, false),
        Field::new("entity_name", DataType::Utf8, false),
        Field::new("symbol_kind", DataType::Utf8, false),
        Field::new("embed_text", DataType::LargeUtf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                SECTION_VECTOR_DIMENSIONS as i32,
            ),
            true,
        ),
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

fn fastembed_cache_dir() -> Option<PathBuf> {
    // Check XDG_CACHE_HOME first (Linux/Unix)
    if let Ok(xdg_cache) = std::env::var("XDG_CACHE_HOME") {
        return Some(PathBuf::from(xdg_cache).join("spur").join("fastembed"));
    }

    // Fall back to HOME directory
    if let Ok(home) = std::env::var("HOME") {
        let home_path = PathBuf::from(home);

        // Use platform-specific cache directories
        #[cfg(target_os = "macos")]
        {
            return Some(home_path.join("Library/Caches/spur/fastembed"));
        }

        #[cfg(not(target_os = "macos"))]
        {
            return Some(home_path.join(".cache/spur/fastembed"));
        }
    }

    // If we can't determine a cache directory, return None to use fastembed's default
    None
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

    fn versioned_section_row(
        stable_symbol_id: &str,
        file_path: &str,
        content_hash: &str,
    ) -> SectionRow {
        SectionRow {
            stable_symbol_id: stable_symbol_id.to_owned(),
            file_path: file_path.to_owned(),
            qualified_name: stable_symbol_id.to_owned(),
            heading_level: 2,
            body_text: format!("## {stable_symbol_id}\n\nBody."),
            body_byte_start: 0,
            body_byte_end: 0,
            child_count: 0,
            parent_stable_id: None,
            content_hash: content_hash.to_owned(),
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
            DataType::FixedSizeList(item, 384) => {
                assert_eq!(item.name(), "item");
                assert_eq!(item.data_type(), &DataType::Float32);
                assert!(item.is_nullable());
            }
            data_type => panic!("expected FixedSizeList<Float32, 384>, got {data_type:?}"),
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
    fn section_embedding_options_from_env_with_skip_override_matches_env_skip() {
        let _lock = env_lock();
        let _skip = EnvGuard::remove(SECTION_EMBED_SKIP_ENV);
        let _batch = EnvGuard::set(SECTION_EMBED_BATCH_SIZE_ENV, "7");

        assert_eq!(
            SectionEmbeddingOptions::from_env_with_skip_override(true),
            SectionEmbeddingOptions {
                skip_embeddings: true,
                batch_size: 7,
            }
        );
    }

    #[test]
    fn section_sidecar_options_from_env_uses_default_write_batch_for_missing_invalid_and_zero() {
        let _lock = env_lock();
        let _skip = EnvGuard::remove(SECTION_EMBED_SKIP_ENV);
        let _embed_batch = EnvGuard::remove(SECTION_EMBED_BATCH_SIZE_ENV);
        let write_batch = EnvGuard::remove(SECTION_WRITE_BATCH_SIZE_ENV);

        assert_eq!(
            SectionSidecarOptions::from_env().write_batch_size,
            SECTION_WRITE_BATCH_SIZE_DEFAULT
        );

        std::env::set_var(SECTION_WRITE_BATCH_SIZE_ENV, "not-a-number");
        assert_eq!(
            SectionSidecarOptions::from_env().write_batch_size,
            SECTION_WRITE_BATCH_SIZE_DEFAULT
        );

        std::env::set_var(SECTION_WRITE_BATCH_SIZE_ENV, "0");
        assert_eq!(
            SectionSidecarOptions::from_env().write_batch_size,
            SECTION_WRITE_BATCH_SIZE_DEFAULT
        );

        drop(write_batch);
    }

    #[test]
    fn section_sidecar_options_from_embedding_options_reads_write_batch_env() {
        let _lock = env_lock();
        let _write_batch = EnvGuard::set(SECTION_WRITE_BATCH_SIZE_ENV, "2");
        let embedding = SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 13,
        };

        let options = SectionSidecarOptions::from_embedding_options(embedding);

        assert_eq!(options.embedding, embedding);
        assert_eq!(options.write_batch_size, 2);
    }

    #[test]
    fn section_row_batcher_yields_configured_batch_lengths() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path();
        let path = "docs/sections.md";
        let source = "## One\n\nBody one.\n## Two\n\nBody two.\n## Three\n\nBody three.\n## Four\n\nBody four.\n## Five\n\nBody five.\n";
        std::fs::create_dir_all(root.join("docs")).expect("mkdir docs");
        std::fs::write(root.join(path), source).expect("write markdown");

        let ranges = section_ranges(
            source,
            &["## One", "## Two", "## Three", "## Four", "## Five"],
        );
        let symbols = ranges
            .iter()
            .enumerate()
            .rev()
            .map(|(index, [start, end])| GraphSymbolArtifact {
                stable_symbol_id: format!("section-{index}"),
                file_path: path.to_owned(),
                byte_range: [*start, *end],
                line_range: [1, 1],
                entity_name: format!("Section {index}"),
                qualified_name: format!("docs/sections.md::Section{index}"),
                symbol_kind: "section".to_owned(),
                anchor_hash: format!("anchor-{index}"),
                enclosing_scope: None,
            })
            .collect();
        let artifact = GraphIndexArtifact {
            header: crate::GraphIndexHeader {
                graph_index_version: "test".to_owned(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_owned(),
            graph_content_hash: "test".to_owned(),
            file_manifests: vec![GraphFileManifestEntry {
                stable_file_id: "file-sections".to_owned(),
                path: path.to_owned(),
                content_oid: "sections".to_owned(),
                node_ids: Vec::new(),
            }],
            files: vec![crate::GraphFileArtifact {
                stable_file_id: "file-sections".to_owned(),
                file_path: path.to_owned(),
            }],
            file_node_ids: Vec::new(),
            symbols,
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        };
        let mut batcher = SectionRowBatcher::new(&artifact, root, 2);
        let mut lengths = Vec::new();

        while let Some(batch) = batcher.next_batch().expect("section batch") {
            assert!(batch.len() <= 2);
            lengths.push(batch.len());
        }

        assert_eq!(lengths, vec![2, 2, 1]);
    }

    #[tokio::test]
    async fn lance_sections_existing_file_versions_cache_absent_versions_across_appends() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dataset_dir = tempdir.path().join(SECTIONS_DATASET_DIR);
        let db = lancedb::connect(dataset_dir.to_str().expect("dataset path"))
            .execute()
            .await
            .expect("connect lancedb");
        let schema = sections_schema();
        let table = db
            .create_table(
                SECTIONS_TABLE,
                rows_to_batch(
                    vec![versioned_section_row(
                        "existing-1",
                        "docs/existing.md",
                        "hash-existing",
                    )],
                    schema.clone(),
                )
                .expect("existing batch"),
            )
            .execute()
            .await
            .expect("create table");
        let mut existing_versions = ExistingFileVersions::new(Some(&table));

        let retained = existing_versions
            .retain_new_rows(vec![
                versioned_section_row("existing-2", "docs/existing.md", "hash-existing"),
                versioned_section_row("new-1", "docs/new.md", "hash-new"),
            ])
            .await
            .expect("filter first chunk");
        let retained_ids: Vec<_> = retained
            .iter()
            .map(|row| row.stable_symbol_id.as_str())
            .collect();
        assert_eq!(retained_ids, vec!["new-1"]);
        table
            .add(rows_to_batch(retained, schema.clone()).expect("retained batch"))
            .execute()
            .await
            .expect("append retained rows");

        let retained = existing_versions
            .retain_new_rows(vec![
                versioned_section_row("existing-3", "docs/existing.md", "hash-existing"),
                versioned_section_row("new-2", "docs/new.md", "hash-new-2"),
            ])
            .await
            .expect("filter second chunk");
        let retained_ids: Vec<_> = retained
            .iter()
            .map(|row| row.stable_symbol_id.as_str())
            .collect();
        assert_eq!(retained_ids, vec!["new-2"]);
    }

    #[tokio::test]
    async fn lance_sections_existing_file_versions_preloads_batch_file_paths() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dataset_dir = tempdir.path().join(SECTIONS_DATASET_DIR);
        let db = lancedb::connect(dataset_dir.to_str().expect("dataset path"))
            .execute()
            .await
            .expect("connect lancedb");
        let schema = sections_schema();
        let table = db
            .create_table(
                SECTIONS_TABLE,
                rows_to_batch(
                    vec![
                        versioned_section_row("existing-1", "docs/existing.md", "hash-existing"),
                        versioned_section_row("quote-1", "docs/it's.md", "hash-quote"),
                        versioned_section_row("other-1", "docs/other.md", "hash-other"),
                    ],
                    schema,
                )
                .expect("existing batch"),
            )
            .execute()
            .await
            .expect("create table");
        let mut existing_versions = ExistingFileVersions::new(Some(&table));

        let existing = existing_versions
            .preload_existing_file_versions(&[
                versioned_section_row("existing-2", "docs/existing.md", "hash-existing"),
                versioned_section_row("new-1", "docs/existing.md", "hash-new"),
                versioned_section_row("quote-2", "docs/it's.md", "hash-quote"),
                versioned_section_row("new-2", "docs/new.md", "hash-new"),
            ])
            .await
            .expect("preload existing versions");

        assert_eq!(
            existing,
            std::collections::HashSet::from([
                ("docs/existing.md".to_owned(), "hash-existing".to_owned()),
                ("docs/it's.md".to_owned(), "hash-quote".to_owned()),
            ])
        );
    }

    fn section_ranges(source: &str, headings: &[&str]) -> Vec<[usize; 2]> {
        headings
            .iter()
            .enumerate()
            .map(|(index, heading)| {
                let start = source.find(heading).expect("heading");
                let end = headings
                    .get(index + 1)
                    .and_then(|next| source.find(next))
                    .unwrap_or(source.len());
                [start, end]
            })
            .collect()
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
            embed_eligible_rows_with(
                &rows,
                options,
                |_| panic!("progress should not be called"),
                |_| panic!("embedder should not be called"),
            ),
            vec![None]
        );
    }

    #[test]
    fn section_embedder_does_not_initialize_model_for_skipped_or_ineligible_rows() {
        let rows = vec![section_row_fixture(1, "# Title\n\nSkipped.".to_owned())];
        let mut embedder = SectionEmbedder::new(SectionEmbeddingOptions {
            skip_embeddings: false,
            batch_size: 1,
        });

        assert_eq!(embedder.embed_row_vectors(&rows), vec![None]);
        assert!(embedder.model.is_none());

        let rows = vec![section_row_fixture(
            2,
            "## Install\n\nInstall body.".to_owned(),
        )];
        let mut embedder = SectionEmbedder::new(SectionEmbeddingOptions {
            skip_embeddings: true,
            batch_size: 1,
        });

        assert_eq!(embedder.embed_row_vectors(&rows), vec![None]);
        assert!(embedder.model.is_none());
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

        let vectors = embed_eligible_rows_with(
            &rows,
            options,
            |_| {},
            |texts| {
                batch_sizes.push(texts.len());
                Ok(vec![vec![0.25; SECTION_VECTOR_DIMENSIONS]; texts.len()])
            },
        );

        assert_eq!(batch_sizes, vec![2, 1]);
        assert!(vectors[0].is_some());
        assert!(vectors[1].is_none());
        assert!(vectors[2].is_some());
        assert!(vectors[3].is_some());
    }

    #[test]
    fn embed_eligible_rows_reports_chunk_progress() {
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
        let mut progress = Vec::new();

        let vectors = embed_eligible_rows_with(
            &rows,
            options,
            |chunk| progress.push(chunk),
            |texts| Ok(vec![vec![0.25; SECTION_VECTOR_DIMENSIONS]; texts.len()]),
        );

        assert_eq!(
            progress,
            vec![
                SectionEmbeddingChunkProgress {
                    chunk_index: 1,
                    chunk_count: 2,
                    chunk_rows: 2,
                    completed_eligible_rows: 0,
                    embedding_eligible_rows: 3,
                },
                SectionEmbeddingChunkProgress {
                    chunk_index: 2,
                    chunk_count: 2,
                    chunk_rows: 1,
                    completed_eligible_rows: 2,
                    embedding_eligible_rows: 3,
                },
            ]
        );
        assert!(vectors[0].is_some());
        assert!(vectors[1].is_none());
        assert!(vectors[2].is_some());
        assert!(vectors[3].is_some());
    }
}
