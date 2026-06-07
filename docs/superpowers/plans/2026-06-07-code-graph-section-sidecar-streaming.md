# Code Graph Section Sidecar Streaming Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Follow-up to `docs/superpowers/plans/2026-06-07-code-graph-section-embedding-memory-fix.md`, PR 25, and the 2026-06-06/2026-06-07 RCA where `spur graph build --workspace` was killed while writing section embeddings.
**Design epic:** `bd-2vmz` follow-up; the prior mitigation bounded embedding calls and made the sidecar best-effort, but it still materializes all section rows and vectors before writing LanceDB.

**Goal:** Make Lance section sidecar generation truly chunked so section rows, embedding vectors, and Arrow batches are never materialized for the entire graph in memory at once.

**Architecture:** Add a sidecar write batch option and refactor `lance_sections.rs` around bounded row batches. The writer should generate section rows one markdown file at a time, filter existing file versions with a stateful cache, embed each retained chunk with one lazily initialized fastembed model, append/create LanceDB batches incrementally, and build indexes once after writes complete.

**Tech Stack:** Rust 2021, `fastembed`, Arrow `RecordBatch`, LanceDB, Parquet graph artifacts, `scripts/spur-cargo`.

---

## File Structure Mapping

| File | Responsibility |
|---|---|
| `crates/spur-graph/src/store/lance_sections.rs` | Section sidecar options, row generation, existing-version filtering, embedding, chunked LanceDB writes, unit tests |
| `crates/spur-graph/tests/lance_sections.rs` | LanceDB integration coverage for chunked writes and skipped embeddings |
| `crates/spur-cli/tests/graph_build_cli.rs` | End-to-end CLI regression proving chunked sidecar writes work through `spur graph build` |

## Memory and Correctness Invariants

- The sidecar write path must not call `section_rows(artifact, worktree_root)?` followed by embedding all rows. That path holds all section bodies and all vector slots at once.
- The sidecar write path must not call `embed_eligible_rows(&all_rows, ...)`. Embedding should happen per retained write chunk.
- The sidecar write path must not call `rows_to_batch(...)` with more than `SectionSidecarOptions::write_batch_size` rows. This bounds the Arrow `flat_vectors` allocation to `write_batch_size * 768 * 4` bytes plus Arrow/string overhead.
- The fastembed model should be initialized at most once per sidecar write, and only after an eligible retained row exists.
- Existing file-version checks must stay correct across chunks: if any row for `(file_path, content_hash)` already exists in LanceDB, every row for that file version should be skipped.
- The existing public entry points remain source-compatible: `write_sections_dataset(...)`, `write_sections_dataset_best_effort(...)`, and `write_sections_dataset_best_effort_with_options(...)` should keep existing callers compiling.

## Dependency DAG

```text
task-sidecar-options-and-batcher
  -> task-stream-lance-writes
      -> task-cli-streaming-regression
```

## Task 1: Sidecar Options and Row Batcher

**Task ID:** `task-sidecar-options-and-batcher`

**Files:**
- Modify: `crates/spur-graph/src/store/lance_sections.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] Add `SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE`, defaulting to `512`, with invalid and zero values falling back to the default.
- [ ] Add a sidecar options type used by the writer that wraps the existing embedding options and carries `write_batch_size`.
- [ ] Preserve the current `SectionEmbeddingOptions { skip_embeddings, batch_size }` public API so current CLI/cache callers compile without changes outside this task.
- [ ] Add a row-batching helper that yields `Vec<SectionRow>` batches no larger than `write_batch_size`.
- [ ] The row-batching helper reads/generates rows one markdown file at a time and does not construct a full `Vec<SectionRow>` for the artifact.
- [ ] Unit tests prove env parsing and batch sizes, including a fixture that produces five section rows with `write_batch_size = 2` and observes batch lengths `[2, 2, 1]`.
- [ ] Focused tests pass through `scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: options parsing, row generation refactor, unit tests inside `lance_sections.rs`.
- OUT of scope: LanceDB append/create behavior, CLI tests, DuckDB analyst SQL, graph publication order.
- If the cleanest row-batcher requires changing artifact model types outside `lance_sections.rs`, emit `scope_drift` before editing other files.

**Implementation:**
- [ ] **Step 1: Add sidecar constants and options**

In `crates/spur-graph/src/store/lance_sections.rs`, add:

```rust
const SECTION_WRITE_BATCH_SIZE_DEFAULT: usize = 512;
const SECTION_WRITE_BATCH_SIZE_ENV: &str = "SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionSidecarOptions {
    embedding: SectionEmbeddingOptions,
    pub write_batch_size: usize,
}
```

Implement env parsing with explicit fallback for invalid/zero values:

```rust
fn positive_env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

impl SectionSidecarOptions {
    pub fn from_env() -> Self {
        Self {
            embedding: SectionEmbeddingOptions::from_env(),
            write_batch_size: positive_env_usize(
                SECTION_WRITE_BATCH_SIZE_ENV,
                SECTION_WRITE_BATCH_SIZE_DEFAULT,
            ),
        }
    }

    pub fn from_env_with_skip_override(skip_embeddings_override: bool) -> Self {
        Self {
            embedding: SectionEmbeddingOptions::from_env_with_skip_override(
                skip_embeddings_override,
            ),
            write_batch_size: positive_env_usize(
                SECTION_WRITE_BATCH_SIZE_ENV,
                SECTION_WRITE_BATCH_SIZE_DEFAULT,
            ),
        }
    }

    fn from_embedding_options(embedding: SectionEmbeddingOptions) -> Self {
        Self {
            embedding,
            write_batch_size: positive_env_usize(
                SECTION_WRITE_BATCH_SIZE_ENV,
                SECTION_WRITE_BATCH_SIZE_DEFAULT,
            ),
        }
    }

    fn skip_embeddings(self) -> bool {
        self.embedding.skip_embeddings
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
```

Keep existing source compatibility with the prior mitigation by leaving this existing type shape intact:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionEmbeddingOptions {
    pub skip_embeddings: bool,
    pub batch_size: usize,
}
```

Then update writer internals to pass `SectionSidecarOptions` while keeping public helpers that accept `SectionEmbeddingOptions` wrapping it through `SectionSidecarOptions::from_embedding_options(options)`.

- [ ] **Step 2: Add the row batcher**

Replace `section_rows(...) -> Result<Vec<SectionRow>>` in the write path with an iterator-style batcher. Keep any old helper only if tests still use it; the production writer should not call it.

Add a focused helper shaped like this:

```rust
use std::collections::VecDeque;

enum SectionRowSource<'a> {
    SectionFile {
        path: &'a str,
        sections: Vec<&'a GraphSymbolArtifact>,
    },
    WholeMarkdownFile {
        manifest: &'a GraphFileManifestEntry,
    },
}

struct SectionRowBatcher<'a> {
    worktree_root: &'a Path,
    write_batch_size: usize,
    child_count_by_parent: HashMap<&'a str, u32>,
    parent_by_child: HashMap<&'a str, String>,
    sources: Vec<SectionRowSource<'a>>,
    next_source_index: usize,
    pending_rows: VecDeque<SectionRow>,
}
```

Build the source list once in sorted path order, then expose `next_batch()`:

```rust
impl<'a> SectionRowBatcher<'a> {
    fn new(
        artifact: &'a GraphIndexArtifact,
        worktree_root: &'a Path,
        write_batch_size: usize,
    ) -> Self {
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

        let mut source_paths: BTreeSet<&str> = sections_by_path.keys().copied().collect();
        for manifest in manifest_by_path.values() {
            if is_markdown_path(&manifest.path) {
                source_paths.insert(manifest.path.as_str());
            }
        }

        let mut sources = Vec::new();
        for path in source_paths {
            if let Some(sections) = sections_by_path.remove(path) {
                sources.push(SectionRowSource::SectionFile { path, sections });
            } else if let Some(manifest) = manifest_by_path.get(path).copied() {
                sources.push(SectionRowSource::WholeMarkdownFile { manifest });
            }
        }

        Self {
            worktree_root,
            write_batch_size: write_batch_size.max(1),
            child_count_by_parent,
            parent_by_child,
            sources,
            next_source_index: 0,
            pending_rows: VecDeque::new(),
        }
    }

    fn next_batch(&mut self) -> Result<Option<Vec<SectionRow>>> {
        let mut batch = Vec::with_capacity(self.write_batch_size);
        loop {
            while batch.len() < self.write_batch_size {
                let Some(row) = self.pending_rows.pop_front() else {
                    break;
                };
                batch.push(row);
            }
            if batch.len() >= self.write_batch_size {
                return Ok(Some(batch));
            }

            if self.next_source_index >= self.sources.len() {
                break;
            }
            let rows = self.load_source_rows(self.next_source_index)?;
            self.next_source_index += 1;
            self.pending_rows = rows.into();
        }

        if batch.is_empty() {
            Ok(None)
        } else {
            Ok(Some(batch))
        }
    }
}
```

Implement `load_source_rows(index)` by reading exactly one source file, calculating its `content_hash`, and returning rows for that one source. For `SectionFile`, sort the file's section symbols by `byte_range[0]` and `stable_symbol_id` before building rows so the row order matches the previous final sort. For `WholeMarkdownFile`, return a single row for markdown files that have no section symbols.

Preserve the current non-UTF-8/unreadable-file warning behavior:

```rust
let bytes = match read_file_bytes(self.worktree_root, path) {
    Ok(bytes) => bytes,
    Err(error) => {
        tracing::warn!(path = %path, error = %error, "section_rows: skipping unreadable file");
        continue;
    }
};
let source = match std::str::from_utf8(&bytes) {
    Ok(source) => source,
    Err(error) => {
        tracing::warn!(path = %path, error = %error, "section_rows: skipping non-UTF-8 markdown");
        continue;
    }
};
```

- [ ] **Step 3: Add tests for options and row batching**

Extend the unit test module with a write-batch env test:

```rust
#[test]
fn section_sidecar_options_from_env_uses_default_write_batch_for_missing_invalid_and_zero() {
    let _lock = env_lock();
    let _skip = EnvGuard::remove(SECTION_EMBED_SKIP_ENV);
    let _embed = EnvGuard::remove(SECTION_EMBED_BATCH_SIZE_ENV);
    let _write = EnvGuard::remove(SECTION_WRITE_BATCH_SIZE_ENV);

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
}
```

Add a test fixture that writes one markdown file with five headings, builds an artifact, and observes chunk sizes:

```rust
#[test]
fn section_row_batcher_flushes_configured_batch_sizes() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    std::fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    std::fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\n## One\n\nBody.\n\n## Two\n\nBody.\n\n## Three\n\nBody.\n\n## Four\n\nBody.\n",
    )
    .expect("write guide");

    let facts = crate::build_facts(&root, None).expect("build facts").0;
    let artifact = crate::artifact_from_facts(&facts, &root).expect("artifact");
    let mut sizes = Vec::new();
    let mut batcher = SectionRowBatcher::new(&artifact, &root, 2);
    while let Some(rows) = batcher.next_batch().expect("batch rows") {
        sizes.push(rows.len());
    }

    assert_eq!(sizes, vec![2, 2, 1]);
}
```

If the exact heading count differs because the markdown extractor includes the H1 and each H2, adjust only the fixture text so the artifact produces five section rows; do not weaken the expected batch-size assertion.

- [ ] **Step 4: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture
```

Expected: all `lance_sections` unit and integration tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/store/lance_sections.rs
git commit -m "refactor(spur-graph): add section sidecar row batching"
```

## Task 2: Stream LanceDB Writes and Embeddings

**Task ID:** `task-stream-lance-writes`

**Files:**
- Modify: `crates/spur-graph/src/store/lance_sections.rs`
- Test: `crates/spur-graph/tests/lance_sections.rs`

**Depends on:** `task-sidecar-options-and-batcher`

**Acceptance Criteria:**
- [ ] `write_sections_dataset_async(...)` processes batches from `SectionRowBatcher` and never collects all `SectionRow` values before writing.
- [ ] Existing-version filtering works incrementally with a cache of checked `(file_path, content_hash)` keys across chunks.
- [ ] Embeddings are applied per retained chunk using one lazily initialized `TextEmbedding` model per sidecar write.
- [ ] `rows_to_batch(...)` is only called for chunk-sized inputs, and empty-table creation still works when no rows are emitted.
- [ ] LanceDB indexes are created once after all chunks finish, and only when the dataset changed.
- [ ] An integration test writes more rows than the configured write batch size and verifies all rows are present in LanceDB.
- [ ] Focused tests pass through `scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `lance_sections.rs` write pipeline, incremental filtering, embedding helper, LanceDB integration test.
- OUT of scope: CLI flag parsing, graph cache/publication code, DuckDB analyst SQL.
- If LanceDB requires a different API to append multiple batches safely, emit `scope_drift` with the exact API limitation and a minimal alternative.

**Implementation:**
- [ ] **Step 1: Add stateful existing-version filter**

Replace the all-rows filtering helper with a chunk-retaining helper:

```rust
#[derive(Default)]
struct ExistingFileVersions {
    table: Option<lancedb::Table>,
    checked: HashMap<(String, String), bool>,
}

impl ExistingFileVersions {
    fn new(table: Option<lancedb::Table>) -> Self {
        Self {
            table,
            checked: HashMap::new(),
        }
    }

    async fn retain_new_rows(&mut self, rows: &mut Vec<SectionRow>) -> Result<()> {
        let Some(table) = self.table.as_ref() else {
            return Ok(());
        };

        for row in rows.iter() {
            let key = (row.file_path.clone(), row.content_hash.clone());
            if self.checked.contains_key(&key) {
                continue;
            }
            let filter = format!(
                "file_path = '{}' AND content_hash = '{}'",
                sql_string_literal(row.file_path.as_str()),
                sql_string_literal(row.content_hash.as_str())
            );
            let unchanged = table
                .count_rows(Some(filter))
                .await
                .context("failed to check existing LanceDB section rows")?
                > 0;
            self.checked.insert(key, unchanged);
        }

        rows.retain(|row| {
            !self
                .checked
                .get(&(row.file_path.clone(), row.content_hash.clone()))
                .copied()
                .unwrap_or(false)
        });
        Ok(())
    }
}
```

This preserves the old semantics while avoiding a global `HashSet` pass over all rows.

- [ ] **Step 2: Add a lazy section embedder**

Add a helper that owns options and initializes fastembed only when needed:

```rust
struct SectionEmbedder {
    options: SectionSidecarOptions,
    model: Option<TextEmbedding>,
    model_unavailable: bool,
}

impl SectionEmbedder {
    fn new(options: SectionSidecarOptions) -> Self {
        Self {
            options,
            model: None,
            model_unavailable: false,
        }
    }

    fn embed_rows(&mut self, rows: &mut [SectionRow]) {
        if self.options.skip_embeddings() || !rows.iter().any(is_embedding_eligible) {
            return;
        }
        let Some(model) = self.model() else {
            return;
        };

        let vectors = embed_eligible_rows_with(
            rows,
            self.options,
            |texts| model.embed(texts.to_vec(), None).map_err(Into::into),
        );
        for (row, vector) in rows.iter_mut().zip(vectors) {
            row.vector = vector;
        }
    }

    fn model(&mut self) -> Option<&TextEmbedding> {
        if self.model_unavailable {
            return None;
        }
        if self.model.is_none() {
            match TextEmbedding::try_new(
                InitOptions::new(EmbeddingModel::NomicEmbedTextV15)
                    .with_show_download_progress(false),
            ) {
                Ok(model) => self.model = Some(model),
                Err(error) => {
                    tracing::warn!(error = %error, "fastembed model unavailable; skipping section embeddings");
                    self.model_unavailable = true;
                    return None;
                }
            }
        }
        self.model.as_ref()
    }
}
```

Update `embed_eligible_rows_with` to continue accepting `SectionEmbeddingOptions`, and pass `self.options.embedding` from `SectionEmbedder`. Keep the existing unit tests, adding one that verifies skipped chunks do not call the injected embedder.

- [ ] **Step 3: Stream write batches into LanceDB**

Refactor `write_sections_dataset_async(...)` to create/open the table once and append chunk by chunk:

```rust
async fn write_sections_dataset_async(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
    options: SectionSidecarOptions,
) -> Result<()> {
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
    let mut existing_versions = ExistingFileVersions::new(table.clone());
    let mut embedder = SectionEmbedder::new(options);
    let mut dataset_changed = false;
    let mut emitted_any_batch = false;
    let mut batcher = SectionRowBatcher::new(artifact, worktree_root, options.write_batch_size);
    while let Some(mut rows) = batcher.next_batch()? {
        existing_versions.retain_new_rows(&mut rows).await?;
        if rows.is_empty() {
            continue;
        }

        embedder.embed_rows(&mut rows);
        let batch = rows_to_batch(rows, schema.clone())?;
        emitted_any_batch = true;
        if let Some(open_table) = table.as_ref() {
            open_table
                .add(batch)
                .execute()
                .await
                .context("failed to append LanceDB section rows")?;
        } else {
            table = Some(
                db.create_table(SECTIONS_TABLE, batch)
                    .execute()
                    .await
                    .context("failed to create LanceDB sections table")?,
            );
        }
        dataset_changed = true;
    }

    if table.is_none() && !emitted_any_batch {
        let empty = rows_to_batch(Vec::new(), schema)?;
        table = Some(
            db.create_table(SECTIONS_TABLE, empty)
                .execute()
                .await
                .context("failed to create LanceDB sections table")?,
        );
        dataset_changed = true;
    }

    if dataset_changed {
        if let Some(table) = table.as_ref() {
            ensure_body_text_fts_index(table).await?;
            ensure_vector_index(table).await?;
        }
    }

    Ok(())
}
```

The hard requirement is that each yielded batch is processed through existing-version filtering, embedding, Arrow conversion, and LanceDB append before the next source file's rows are retained.

- [ ] **Step 4: Add integration coverage for chunked writes**

In `crates/spur-graph/tests/lance_sections.rs`, add an integration test that sets both env vars and verifies all rows are written:

```rust
#[tokio::test]
async fn lance_sections_streams_rows_across_configured_write_batches() {
    let _skip = EnvGuard::set("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS", "1");
    let _batch = EnvGuard::set("SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE", "2");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    fs::write(
        root.join("docs/guide.md"),
        "# Guide\n\n## One\n\nBody.\n\n## Two\n\nBody.\n\n## Three\n\nBody.\n\n## Four\n\nBody.\n",
    )
    .expect("write guide");

    let facts = build_facts(&root, None).expect("build facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
    let expected_rows = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.symbol_kind == "section")
        .count();
    assert!(expected_rows > 2, "fixture must cross the write batch boundary");
    let out_dir = tempdir.path().join("artifact");

    write_sections_dataset(&artifact, &root, &out_dir).expect("write sections sidecar");

    let db = lancedb::connect(
        out_dir
            .join(SECTIONS_DATASET_DIR)
            .to_str()
            .expect("dataset path"),
    )
    .execute()
    .await
    .expect("connect lancedb");
    let table = db.open_table(SECTIONS_TABLE).execute().await.expect("open table");

    assert_eq!(
        table.count_rows(None).await.expect("count rows"),
        expected_rows
    );
    assert_eq!(
        table
            .count_rows(Some("vector IS NOT NULL".to_owned()))
            .await
            .expect("count vector rows"),
        0
    );
}
```

Extend the local `EnvGuard` in that integration test file with a second guard variable if needed; both guards must restore previous environment values in `Drop`.

- [ ] **Step 5: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture
```

Expected: all `lance_sections` unit and integration tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-graph/src/store/lance_sections.rs crates/spur-graph/tests/lance_sections.rs
git commit -m "fix(spur-graph): stream section sidecar writes"
```

## Task 3: CLI Regression for Chunked Sidecar Writes

**Task ID:** `task-cli-streaming-regression`

**Files:**
- Modify: `crates/spur-cli/Cargo.toml`
- Modify: `crates/spur-cli/tests/graph_build_cli.rs`

**Depends on:** `task-stream-lance-writes`

**Acceptance Criteria:**
- [ ] Add a CLI integration test that runs `spur graph build --workspace --no-analyst --quiet --no-section-embeddings` with `SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE=2`.
- [ ] The fixture contains enough markdown sections to force multiple LanceDB write batches.
- [ ] The test verifies the graph artifact is loadable and `sections.lancedb/section_bodies` contains the expected section row count.
- [ ] The test does not require downloading or initializing the fastembed model.
- [ ] Focused CLI tests pass through `scripts/spur-cargo test -p spur-cli graph_build_section_sidecar_streaming -- --nocapture` and `scripts/spur-cargo test -p spur-cli graph_build -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `graph_build_cli.rs` test fixture/assertions and `crates/spur-cli/Cargo.toml` dev-dependency additions needed for LanceDB row counting.
- OUT of scope: CLI argument parsing, graph build implementation, Lance writer internals.
- If the CLI binary cannot expose LanceDB row counts without adding new public API, use LanceDB directly inside the test; do not add a production CLI command.

**Implementation:**
- [ ] **Step 1: Add the CLI test dependency**

In `crates/spur-cli/Cargo.toml`, add the workspace LanceDB dependency under `[dev-dependencies]`:

```toml
lancedb = { workspace = true }
```

`tokio` is already present as a dev-dependency in this crate, and the test only needs `count_rows`, so no `futures` dev-dependency is required.

- [ ] **Step 2: Add needed imports**

At the top of `crates/spur-cli/tests/graph_build_cli.rs`, extend imports:

```rust
use spur_graph::store::lance_sections::{SECTIONS_DATASET_DIR, SECTIONS_TABLE};
```

- [ ] **Step 3: Add a markdown-heavy fixture helper**

Add a helper near `fixture_tree()`:

```rust
fn fixture_tree_with_markdown_sections() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(".git")).expect("mkdir .git");
    std::fs::create_dir_all(dir.path().join("docs")).expect("mkdir docs");
    std::fs::write(
        dir.path().join("docs/guide.md"),
        "# Guide\n\n## One\n\nBody.\n\n## Two\n\nBody.\n\n## Three\n\nBody.\n\n## Four\n\nBody.\n",
    )
    .expect("write guide");
    dir
}
```

- [ ] **Step 4: Add the CLI regression test**

Add:

```rust
#[test]
fn graph_build_section_sidecar_streaming_writes_all_rows() {
    let dir = fixture_tree_with_markdown_sections();

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args([
            "graph",
            "build",
            "--workspace",
            "--no-analyst",
            "--quiet",
            "--no-section-embeddings",
        ])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .env("SPUR_GRAPH_SECTION_WRITE_BATCH_SIZE", "2")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    let artifact = read_artifact_parquet(&artifact_path).expect("load artifact");
    let expected_rows = artifact
        .symbols
        .iter()
        .filter(|symbol| symbol.symbol_kind == "section")
        .count();
    assert!(expected_rows > 2, "fixture should force multiple sidecar batches");

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let actual_rows = runtime.block_on(async {
        let db = lancedb::connect(
            artifact_path
                .join(SECTIONS_DATASET_DIR)
                .to_str()
                .expect("dataset path"),
        )
        .execute()
        .await
        .expect("connect lancedb");
        let table = db.open_table(SECTIONS_TABLE).execute().await.expect("open table");
        table.count_rows(None).await.expect("count rows")
    });

    assert_eq!(actual_rows, expected_rows);
}
```

Keep this test synchronous like the rest of `graph_build_cli.rs` and create a local Tokio runtime only for the LanceDB row count.

- [ ] **Step 5: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-cli graph_build_section_sidecar_streaming -- --nocapture
scripts/spur-cargo test -p spur-cli graph_build -- --nocapture
```

Expected: the new focused test passes, and the existing graph build CLI regression group remains green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/Cargo.toml crates/spur-cli/tests/graph_build_cli.rs
git commit -m "test(spur-cli): cover chunked section sidecar graph build"
```

## Plan Self-Review

- Spec coverage: Task 1 introduces the write-batch configuration and row chunking substrate; Task 2 wires chunking through dedupe, embedding, Arrow batches, and LanceDB writes; Task 3 verifies the behavior through the CLI path users run.
- Placeholder scan: The plan has concrete file scopes, function/type shapes, test names, commands, and commit messages.
- Type consistency: `SectionSidecarOptions` wraps the existing source-compatible `SectionEmbeddingOptions` and adds `write_batch_size`. Later tasks refer to `write_batch_size` and `SectionEmbeddingOptions::batch_size` consistently.
- DAG validation: `task-sidecar-options-and-batcher -> task-stream-lance-writes -> task-cli-streaming-regression` is acyclic. The CLI regression depends on the graph writer implementation because it asserts LanceDB rows written by that implementation.
