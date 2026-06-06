# Code Graph Section Embedding Memory Fix Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** User-requested hotfix plan from the 2026-06-06/2026-06-07 RCA on `spur graph build --workspace` being killed during section embedding sidecar generation.
**Design epic:** none; this is a focused reliability hotfix.

**Goal:** Keep `spur graph build` publishable under memory pressure by bounding optional section embeddings and making the Lance section sidecar non-fatal.

**Architecture:** The structural Parquet artifact is the critical output and must publish before optional embedding/index sidecar work can fail the command. Section embeddings become bounded and opt-out via environment/CLI configuration, while DuckDB analyst search naturally degrades when `sections.lancedb` is absent.

**Tech Stack:** Rust 2021, `fastembed`, LanceDB, Parquet graph store, `scripts/spur-cargo`.

---

## File Structure Mapping

| File | Responsibility |
|---|---|
| `crates/spur-graph/src/store/lance_sections.rs` | Section sidecar writer, embedding eligibility, batching, skip configuration, sidecar best-effort helper |
| `crates/spur-graph/src/store/cache.rs` | Default/canonical graph artifact publication path; must publish Parquet even if sidecar work fails |
| `crates/spur-cli/src/commands/graph.rs` | CLI graph build publication path, especially temporal builds; must publish Parquet/CURRENT before sidecar work can fail |
| `crates/spur-cli/src/main.rs` | Parse user-facing graph-build opt-out flag |
| `crates/spur-graph/tests/lance_sections.rs` | Lance sidecar tests for skipped embeddings and vector-null behavior |
| `crates/spur-cli/tests/graph_build_cli.rs` | CLI regression tests that graph publication succeeds with embeddings disabled |

## Dependency DAG

```text
task-embed-config
  -> task-publish-sidecar-best-effort
      -> task-cli-regression
```

## Task 1: Bounded and Skippable Section Embeddings

**Task ID:** `task-embed-config`

**Files:**
- Modify: `crates/spur-graph/src/store/lance_sections.rs`
- Test: `crates/spur-graph/tests/lance_sections.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS=1` prevents fastembed model initialization and writes `section_bodies.vector` as NULL for every row.
- [ ] Embedding is processed in bounded batches, defaulting to 64 eligible sections per `model.embed(...)` call.
- [ ] `SPUR_GRAPH_SECTION_EMBED_BATCH_SIZE=<N>` overrides the batch size, with invalid/zero values falling back to the default.
- [ ] Existing `write_sections_dataset(...)` callers keep compiling.
- [ ] Focused tests pass through `scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `lance_sections.rs` embedding configuration, batching, and tests that inspect the Lance table.
- OUT of scope: CLI parsing, graph publication order, DuckDB analyst SQL.
- If fastembed's public API prevents batch-level control without larger refactoring, emit `scope_drift` with the exact API mismatch.

**Implementation:**
- [ ] **Step 1: Add a failing skip test**

Add a test in `crates/spur-graph/tests/lance_sections.rs` that sets `SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS=1`, writes a markdown fixture with an H2 section shorter than 4 KB, then asserts the `vector` column exists and has zero non-null vectors:

```rust
#[tokio::test]
async fn skip_section_embeddings_writes_null_vectors() {
    let _env = EnvGuard::set("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS", "1");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("repo");
    std::fs::create_dir_all(root.join("docs")).expect("mkdir docs");
    std::fs::write(root.join("docs/guide.md"), "# Guide\n\n## Install\n\nInstall body.\n")
        .expect("write guide");

    let facts = build_facts(&root, None).expect("build facts").0;
    let artifact = artifact_from_facts(&facts, &root).expect("artifact");
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
        table
            .count_rows(Some("vector IS NOT NULL".to_owned()))
            .await
            .expect("count vector rows"),
        0
    );
}
```

Add the local test helper:

```rust
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
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}
```

- [ ] **Step 2: Add embedding options and env parsing**

In `lance_sections.rs`, add constants and a small options type:

```rust
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
        let skip_embeddings = matches!(std::env::var(SECTION_EMBED_SKIP_ENV), Ok(value) if value == "1");
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
```

Keep `write_sections_dataset(...)` as the default public API and have it call a new options-aware helper:

```rust
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
```

- [ ] **Step 3: Batch embedding work**

Change `embed_eligible_rows` to accept `SectionEmbeddingOptions`. If `skip_embeddings` is true, return `None` for every row before constructing `TextEmbedding`. Otherwise, call `model.embed(...)` per chunk:

```rust
for chunk in eligible.chunks(options.batch_size) {
    let texts: Vec<&str> = chunk.iter().map(|(_, text)| *text).collect();
    let embeddings = match model.embed(texts, None) {
        Ok(embeddings) => embeddings,
        Err(error) => {
            tracing::warn!(error = %error, "fastembed encode failed for section embedding batch; skipping remaining section embeddings");
            return result;
        }
    };
    for ((index, _), embedding) in chunk.iter().copied().zip(embeddings) {
        if embedding.len() == SECTION_VECTOR_DIMENSIONS {
            result[index] = Some(embedding);
        }
    }
}
```

- [ ] **Step 4: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture
```

Expected: all `lance_sections` tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-graph/src/store/lance_sections.rs crates/spur-graph/tests/lance_sections.rs
git commit -m "fix(spur-graph): bound section embedding batches"
```

## Task 2: Publish Graph Artifacts Before Optional Sidecar Work

**Task ID:** `task-publish-sidecar-best-effort`

**Files:**
- Modify: `crates/spur-graph/src/store/lance_sections.rs`
- Modify: `crates/spur-graph/src/store/cache.rs`
- Modify: `crates/spur-cli/src/commands/graph.rs`
- Test: `crates/spur-cli/tests/graph_build_cli.rs`

**Depends on:** `task-embed-config`

**Acceptance Criteria:**
- [ ] A Lance sidecar failure cannot prevent publication of a valid Parquet artifact directory.
- [ ] A Lance sidecar failure cannot prevent `.spur/graph/CURRENT` from being written on default output paths.
- [ ] Both cache-backed git builds and direct/temporal output builds use the same best-effort sidecar policy.
- [ ] Analyst build still sees `sections.lancedb` when the sidecar succeeds and naturally skips `init_search.sql` when it is absent.
- [ ] Focused tests pass through `scripts/spur-cargo test -p spur-cli graph_build -- --nocapture`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: publication order and best-effort sidecar handling in graph build/cache paths.
- OUT of scope: embedding model internals, new DuckDB SQL, code graph MCP query behavior.
- If a test needs a sidecar failure hook, keep it crate-local/test-only and do not introduce a production panic path.

**Implementation:**
- [ ] **Step 1: Add a best-effort helper**

In `lance_sections.rs`, add:

```rust
pub fn write_sections_dataset_best_effort(
    artifact: &GraphIndexArtifact,
    worktree_root: &Path,
    artifact_dir: &Path,
) {
    if let Err(error) = write_sections_dataset(artifact, worktree_root, artifact_dir) {
        tracing::warn!(
            error = %error,
            artifact_dir = %artifact_dir.display(),
            "spur-graph: section sidecar write failed; graph artifact remains usable"
        );
    }
}
```

- [ ] **Step 2: Change cache publication order**

In `crates/spur-graph/src/store/cache.rs`, update `write_canonical_atomically` and `write_artifact_to_worktree` so they commit the Parquet staging directory first, then call the best-effort sidecar writer on the final directory:

```rust
let final_path = staging.commit()?;
write_sections_dataset_best_effort(artifact, worktree_root, &final_path);
Ok(final_path)
```

Update imports from `write_sections_dataset` to `write_sections_dataset_best_effort`.

- [ ] **Step 3: Change temporal graph build publication order**

In `crates/spur-cli/src/commands/graph.rs`, in the temporal staging branch, move sidecar work after `staging.commit()` and after `write_current_pointer(...)` succeeds:

```rust
let written_dir = staging.commit()?;
if !uses_output_override {
    spur_graph::write_current_pointer(&root, &written_dir)?;
}
write_sections_dataset_best_effort(&artifact, &root, &written_dir);
Ok::<_, anyhow::Error>(written_dir)
```

The non-temporal default path is covered by `store::cache::write_with_dedup`.

- [ ] **Step 4: Add a regression test**

Add a CLI test that disables embeddings and verifies publication still succeeds and loads:

```rust
#[test]
fn graph_build_publishes_current_when_section_embeddings_are_skipped() {
    let dir = fixture_tree();

    let output = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .env("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS", "1")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    assert!(artifact_path.is_dir(), "expected graph index artifact dir");
    let artifact = read_artifact_parquet(&artifact_path).expect("load artifact");
    assert_eq!(artifact.files.len(), 1);
}
```

- [ ] **Step 5: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-cli graph_build -- --nocapture
```

Expected: graph build CLI tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-graph/src/store/lance_sections.rs crates/spur-graph/src/store/cache.rs crates/spur-cli/src/commands/graph.rs crates/spur-cli/tests/graph_build_cli.rs
git commit -m "fix(spur-graph): publish graph before section sidecar"
```

## Task 3: CLI Opt-Out and End-to-End Verification

**Task ID:** `task-cli-regression`

**Files:**
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-cli/src/commands/graph.rs`
- Modify: `crates/spur-graph/src/store/lance_sections.rs`
- Test: `crates/spur-cli/tests/graph_build_cli.rs`

**Depends on:** `task-publish-sidecar-best-effort`

**Acceptance Criteria:**
- [ ] `spur graph build --no-section-embeddings` parses and disables section embeddings without requiring an environment variable.
- [ ] The CLI flag and `SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS=1` have equivalent behavior.
- [ ] The final graph artifact remains loadable and `.spur/graph/CURRENT` is written when using the flag.
- [ ] Verification uses `scripts/spur-cargo`, not bare `cargo`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: CLI parsing, graph build option threading, tests for user-facing opt-out.
- OUT of scope: changing the analyst SQL, changing embedding model choice, deleting stale local `.spur/graph` files.
- If threading the flag through cache-backed publication requires touching more than the listed files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add CLI parse test**

In `crates/spur-cli/src/main.rs`, extend the existing CLI parse tests:

```rust
#[test]
fn cli_accepts_graph_build_no_section_embeddings_flag() {
    Cli::command()
        .try_get_matches_from([
            "spur",
            "graph",
            "build",
            "--workspace",
            "--no-section-embeddings",
        ])
        .expect("graph build --no-section-embeddings should parse");
}
```

- [ ] **Step 2: Add option plumbing**

Add the flag to `GraphCommands::Build`:

```rust
/// Skip fastembed section vectors while still writing searchable section bodies.
/// Also honored via SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS=1.
#[arg(long)]
no_section_embeddings: bool,
```

Thread it into `GraphBuildOptions`.

- [ ] **Step 3: Let graph build pass an explicit skip override**

Add an override-aware constructor in `lance_sections.rs`:

```rust
impl SectionEmbeddingOptions {
    pub fn from_env_with_skip_override(skip_embeddings_override: bool) -> Self {
        let mut options = Self::from_env();
        options.skip_embeddings |= skip_embeddings_override;
        options
    }
}
```

Use it from graph build paths that already have `GraphBuildOptions`. Cache-backed paths may keep the env-only default if the cache API would otherwise require broad refactoring; in that case, set `SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS=1` internally only for the duration of graph build with an `EnvGuard`, and document why the process-global guard is safe for the single-threaded CLI command.

- [ ] **Step 4: Add CLI regression**

Add a test in `graph_build_cli.rs`:

```rust
#[test]
fn graph_build_no_section_embeddings_flag_publishes_loadable_artifact() {
    let dir = fixture_tree();

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
        .env_remove("SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS")
        .output()
        .expect("spawn spur graph build");

    assert!(
        output.status.success(),
        "expected success; stderr = {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let artifact_path = read_current_pointer(dir.path()).expect("read CURRENT");
    assert!(artifact_path.is_dir(), "expected graph index artifact dir");
    let artifact = read_artifact_parquet(&artifact_path).expect("load artifact");
    assert_eq!(artifact.files.len(), 1);
}
```

- [ ] **Step 5: Verify**

Run:

```bash
scripts/spur-cargo test -p spur-cli cli_accepts_graph_build_no_section_embeddings_flag -- --nocapture
scripts/spur-cargo test -p spur-cli graph_build_no_section_embeddings -- --nocapture
scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture
```

Expected: focused tests pass. If remote compile falls back locally and hits sandbox/disk failures, rerun with:

```bash
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-cli cli_accepts_graph_build_no_section_embeddings_flag -- --nocapture
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-cli graph_build_no_section_embeddings -- --nocapture
SPUR_REMOTE=1 scripts/spur-cargo test -p spur-graph lance_sections -- --nocapture
```

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/main.rs crates/spur-cli/src/commands/graph.rs crates/spur-graph/src/store/lance_sections.rs crates/spur-cli/tests/graph_build_cli.rs
git commit -m "fix(spur-cli): add graph build section embedding opt-out"
```

## Self-Review

**Spec coverage:** The plan covers bounded embedding memory, explicit opt-out, graph publication resilience, and regression coverage. It intentionally does not change `init_search.sql` because the failed build died before analyst SQL could run.

**Placeholder scan:** No task uses placeholder implementation language; every task lists files, steps, commands, and expected outcomes.

**Type consistency:** `SectionEmbeddingOptions` is introduced in Task 1 and extended in Task 3. Existing `write_sections_dataset(...)` remains the compatibility entry point.

**DAG validation:** `task-embed-config -> task-publish-sidecar-best-effort -> task-cli-regression` is acyclic. The chain is intentional because publication order depends on the sidecar helper and CLI plumbing depends on the option type.

**beads compatibility:** Every task has a unique task ID, explicit dependencies, acceptance criteria, suggested worker, and scope boundary.
