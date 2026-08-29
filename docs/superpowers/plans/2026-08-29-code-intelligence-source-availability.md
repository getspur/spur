# Code-Intelligence Source Availability Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-29-code-intelligence-source-availability-design.ipynb`

**Formal @spec cells:** `CODE-EVAL-SOURCE-STATE`, `CODE-EVAL-LOCK-SHAPE`, `CODE-EVAL-RUN-SCOPE`, `CODE-EVAL-REPORT-GATE`

**Design epic:** `bd-1fud` (closed after user approval)

**Execution base:** reviewed live-harness tip `c3f667797898b79685a174743a21d571f79dfeda`, with the approved design and this plan overlaid

**Goal:** Produce a genuine SPUR code-intelligence available-corpus benchmark while retaining every unavailable upstream case in exact accounting and rejecting any complete-source publication claim.

**Architecture:** Extend the immutable live source lock with a versioned algebraic source state: verified original, authoritative archive recovery, or evidenced source-unavailable. CrossCodeEval normalization consumes the verified census and separates runnable records from unavailable denominator members; the evaluator persists unavailable terminal checkpoints without materialization or backend calls. Reporting keeps manifest and evaluated denominators distinct, and the runner may generate an `available_corpus` report while the publication gate remains fail-closed.

**Tech Stack:** Rust 2021, Serde, SHA-256, existing `spur-code-eval` adapters/evaluator/runner, `scripts/spur-cargo`, Z3-backed design invariants, pinned real RepoQA/CrossCodeEval/JCG archives.

---

## File ownership map

| Task | Owned implementation files | Owned test/evidence files |
|---|---|---|
| `lock-v2` | `crates/spur-code-eval/src/live/mod.rs` | `crates/spur-code-eval/tests/live_foundation.rs` |
| `availability-normalization` | `crates/spur-code-eval/src/live/availability.rs`, `crates/spur-code-eval/src/live/crosscodeeval.rs`, module export in `live/mod.rs` | `crates/spur-code-eval/tests/live_crosscodeeval.rs` |
| `unavailable-checkpoints` | `crates/spur-code-eval/src/live/evaluate.rs` | `crates/spur-code-eval/tests/live_evaluate.rs` |
| `report-policy` | `crates/spur-code-eval/src/report.rs` | `crates/spur-code-eval/tests/live_report.rs` |
| `runner-integration` | `crates/spur-code-eval/src/runner.rs` | `crates/spur-code-eval/tests/live_runner.rs` |
| `real-run` | no production source edits | `docs/superpowers/reviews/2026-08-29-code-intelligence-available-corpus-benchmark.md`; uncommitted evidence under `.spur/bench-evidence/bd-11qh-available-v1/` |

`availability-normalization` is the only task permitted to make the small `live/mod.rs` module-export edit after `lock-v2`; its substantive lock code remains out of scope. All other production files have a single owner.

## Dependency DAG

```text
lock-v2
├── availability-normalization ─┐
├── unavailable-checkpoints ────┼── runner-integration ── real-run
└── report-policy ───────────────┘
```

The dependency inequalities were solver-checked as acyclic and minimum-depth: `sol_4d84c0ca9ee94634` gives levels `0 → 1 → 2 → 3`, with the three level-1 tasks parallel.

---

### Task 1: Source-lock v2 and typed availability rows

**Task ID:** `lock-v2`

**Files:**
- Modify: `crates/spur-code-eval/src/live/mod.rs`
- Test: `crates/spur-code-eval/tests/live_foundation.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `LIVE_SOURCE_LOCK_VERSION` is 2 and newly serialized locks use only the v2 wire schema.
- [ ] Resolved-original, resolved-archive, and source-unavailable entries have closed, typed constructors matching `CODE-EVAL-LOCK-SHAPE`.
- [ ] Existing resolved-only callers keep a compile-safe `repositories()` view; new code can iterate the complete canonical entry union.
- [ ] V1 resolved-only JSON is accepted, fully revalidated, and promoted deterministically to resolved-original v2 entries.
- [ ] Canonical identity ordering is `(original_uri, requested_revision, subdirectory)` and duplicate identities are rejected across all states.
- [ ] Evidence digests, archive URIs, full commits, materialization hashes, licenses, and diagnostics are validated without network or filesystem access.
- [ ] Targeted tests and `scripts/spur-cargo check -p spur-code-eval` pass.

**Suggested Worker:** codex — focused additive contract and Serde work in one source/test pair.

**Scope Boundary:**
- IN scope: live lock types, validation errors, canonical serialization/deserialization, v1 promotion, accessors.
- OUT of scope: census parsing, Git/network resolution, CrossCodeEval record changes, evaluator checkpoints, report fields, runner orchestration.
- If the v2 shape cannot remain additive for current resolved-only callers, emit `scope_drift` before changing another production file.

**Implementation:**

- [ ] **Step 1: Add failing v2 shape and migration tests**

```rust
#[test]
fn lock_v2_preserves_resolved_archive_and_unavailable_rows() {
    let archive = LiveRepositoryLock::new_archive(
        "https://github.com/original/project.git",
        "abc1234",
        "abc1234000000000000000000000000000000000",
        None,
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        vec!["MIT".to_owned()],
        "https://archive.example/project.git",
        "software_heritage_original_snapshot",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    )
    .unwrap();
    let unavailable = LiveUnavailableRepositoryLock::new(
        "https://github.com/missing/project.git",
        "def5678",
        None,
        vec!["Apache-2.0".to_owned()],
        "repository_not_found",
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
    )
    .unwrap();

    let lock = LiveSourceLock::from_availability(
        &config(None),
        vec![archive],
        vec![unavailable],
        "d".repeat(64),
    )
    .unwrap();
    let json = serde_json::to_value(&lock).unwrap();
    assert_eq!(json["lock_version"], 2);
    assert_eq!(lock.repository_entries().count(), 2);
}
```

- [ ] **Step 2: Prove RED**

Run: `scripts/spur-cargo test -p spur-code-eval --test live_foundation lock_v2 -- --nocapture`

Expected: compile failure for absent v2 constructors/types or assertion failure on lock version 1.

- [ ] **Step 3: Implement the additive v2 algebra**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LiveRepositoryEntry {
    ResolvedOriginal(LiveRepositoryLock),
    ResolvedArchive(LiveRepositoryLock),
    SourceUnavailable(LiveUnavailableRepositoryLock),
}

impl LiveSourceLock {
    pub fn repository_entries(&self) -> impl ExactSizeIterator<Item = LiveRepositoryEntryRef<'_>>;
    pub fn unavailable_repositories(&self) -> &[LiveUnavailableRepositoryLock];
}
```

Preserve `LiveRepositoryLock::new` as the resolved-original constructor. Add a separate archive constructor requiring `archive_uri`, provenance kind, and evidence digest. Compute the v1-promotion evidence digest from a domain-separated canonical v1 row; never invent an archival URI.

- [ ] **Step 4: Add negative fixtures**

Cover unavailable rows carrying commit/hash/archive fields, archive rows without archival provenance, noncanonical digests, duplicate identity across states, requested/full-object mismatch, mutable URI, and v2 unknown fields.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-code-eval --test live_foundation
scripts/spur-cargo check -p spur-code-eval
```

Expected: all targeted tests and check pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-code-eval/src/live/mod.rs crates/spur-code-eval/tests/live_foundation.rs
git commit -m "feat(spur-code-eval): lock-v2 model source availability"
```

---

### Task 2: Verified census ingestion and CrossCodeEval availability normalization

**Task ID:** `availability-normalization`

**Files:**
- Create: `crates/spur-code-eval/src/live/availability.rs`
- Modify: `crates/spur-code-eval/src/live/mod.rs` (module export only)
- Modify: `crates/spur-code-eval/src/live/crosscodeeval.rs`
- Test: `crates/spur-code-eval/tests/live_crosscodeeval.rs`

**Depends on:** `lock-v2`

**Acceptance Criteria:**
- [ ] Canonical census JSONL, evidence manifest, and checksum-root files are parsed with deny-unknown/typed validation and exact SHA-256 verification.
- [ ] The evidence inventory contains exactly one row for each CrossCodeEval repository request; missing, duplicate, extra, or identity-mismatched rows fail closed.
- [ ] Original available objects are resolved through the production Git resolver; authoritative recovery rows fetch only the evidenced archive URI/full object; source-unavailable rows perform no Git call.
- [ ] Generic Git/network/auth/transient failures remain fatal and are never reclassified as source-unavailable.
- [ ] `NormalizedCrossCodeEval` retains the exact 9,928 total / 6,021 eligible denominator, resolved runnable records, unavailable records, and all 1,002 repository identities.
- [ ] Existing oracle/result members remain ignored and no completion leakage enters an unavailable record.
- [ ] Targeted synthetic tests pass, and the pinned real archive test reproduces counts when `SPUR_CROSSCODEEVAL_ARCHIVE` and census paths are supplied.

**Suggested Worker:** claude-code-acp — coupled parser/resolver/adapter change with four tightly related files.

**Scope Boundary:**
- IN scope: census/evidence parsing, checksum verification, typed availability resolution, CrossCodeEval normalized resolved/unavailable partitions.
- OUT of scope: source-lock core validation beyond using Task 1 APIs, evaluator/checkpoint persistence, report accounting, runner CLI/phase logic.
- If the census evidence cannot prove an unavailable or archival state without a new external authority, emit `risk`; do not weaken provenance.

**Scope Drift Checkpoint:**
- If another suite adapter must change to compile, emit `scope_drift` before editing it.
- If any transport error would be converted into `source_unavailable`, emit `risk` and stop.

**Implementation:**

- [ ] **Step 1: Add a failing mixed-availability archive test**

```rust
#[test]
fn normalization_keeps_unavailable_records_denominator_visible_without_resolving() {
    let evidence = synthetic_evidence([
        available_row("owner/ready", "abc1234"),
        unavailable_row("owner/gone", "def5678", "repository_not_found"),
    ]);
    let mut resolver = RecordingResolver::default();

    let normalized = normalize_archive_with_availability(
        synthetic_archive_with_two_projects(),
        &evidence,
        &mut resolver,
        None,
    )
    .unwrap();

    assert_eq!(normalized.total_count(), 2);
    assert_eq!(normalized.runnable_records().len(), 1);
    assert_eq!(normalized.unavailable_records().len(), 1);
    assert_eq!(resolver.requests.len(), 1);
}
```

Define `synthetic_evidence`, `available_row`, `unavailable_row`, and `synthetic_archive_with_two_projects` as local deterministic fixture builders in `live_crosscodeeval.rs`; each builder must serialize through the production evidence/archive schema rather than constructing private production fields.

- [ ] **Step 2: Prove RED**

Run: `scripts/spur-cargo test -p spur-code-eval --test live_crosscodeeval availability -- --nocapture`

Expected: compile failure for absent availability types/functions.

- [ ] **Step 3: Implement the evidence parser**

```rust
pub struct LiveAvailabilityEvidence {
    census_sha256: String,
    evidence_manifest_sha256: String,
    rows: BTreeMap<RepositoryRequestIdentity, AvailabilityEvidenceRow>,
}

impl LiveAvailabilityEvidence {
    pub fn from_readers(
        census_jsonl: impl Read,
        evidence_manifest: impl Read,
        checksum_root: impl Read,
    ) -> Result<Self, AvailabilityError>;
}
```

Bind every row to the pinned CrossCodeEval archive SHA, original URI, requested revision, license set, diagnostic class, attempt references, and optional authoritative recovery. Stable lock evidence uses content digests; observation timestamps remain evidence metadata.

- [ ] **Step 4: Extend normalization without weakening the existing resolver**

Add `normalize_archive_with_availability`. Keep `normalize_archive` as the strict all-resolved wrapper for existing callers/tests. Store runnable and unavailable records separately so existing resolved-record access remains compile-safe.

- [ ] **Step 5: Add tamper and error-class tests**

Cover checksum mismatch, duplicate census identity, absent identity, extra identity, prefix/full-object mismatch, recovery without authoritative provenance, recovery materialization mismatch, changed license set, and a Git 429/auth error that remains fatal.

- [ ] **Step 6: Verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-code-eval --test live_crosscodeeval
SPUR_REMOTE=0 SPUR_CROSSCODEEVAL_ARCHIVE=/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-11qh-real-source-20260828/sources/crosscodeeval_data.tar.xz scripts/spur-cargo test -p spur-code-eval --test live_crosscodeeval genuine_archive
```

Expected: synthetic suite and pinned real-count test pass; no network is used by tests.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-code-eval/src/live/availability.rs crates/spur-code-eval/src/live/mod.rs crates/spur-code-eval/src/live/crosscodeeval.rs crates/spur-code-eval/tests/live_crosscodeeval.rs
git commit -m "feat(spur-code-eval): availability-normalization ingest verified census"
```

---

### Task 3: Source-unavailable terminal checkpoints

**Task ID:** `unavailable-checkpoints`

**Files:**
- Modify: `crates/spur-code-eval/src/live/evaluate.rs`
- Test: `crates/spur-code-eval/tests/live_evaluate.rs`

**Depends on:** `lock-v2`

**Acceptance Criteria:**
- [ ] A typed `LiveSourceUnavailableCase` can be created without a `RepositoryPin` while retaining suite, case ID, original URI/revision, diagnostic class, evidence digest, and complete input fingerprint.
- [ ] Persisting an unavailable case never calls `MaterializationBoundary`, `IndexBoundary`, or `QueryBackend`.
- [ ] Unavailable terminal checkpoints are content-addressed, atomic, resumable, and invalidated by schema, evidence, policy, identity, or input changes.
- [ ] The terminal payload has an explicit `source_unavailable` status and cannot be confused with runtime `Failed`, capability `Unsupported`, or adapter `Invalid`.
- [ ] Failed checkpoints remain non-resumable; unavailable checkpoints are resumable only after full verification.
- [ ] Targeted evaluator tests and crate check pass.

**Suggested Worker:** codex — isolated evaluator/checkpoint contract.

**Scope Boundary:**
- IN scope: unavailable case/result types, checkpoint schema v2, atomic persistence/resume verification, spy-boundary tests.
- OUT of scope: census parsing, CrossCodeEval normalization, metric aggregation, report schema, runner orchestration.
- Do not make `RepositoryPin` fields optional in the existing runnable `LiveCase`; use a distinct unavailable-case type.

**Implementation:**

- [ ] **Step 1: Add the failing no-boundary-call test**

```rust
#[test]
fn source_unavailable_checkpoint_never_materializes_indexes_or_queries() {
    let root = TestDirectory::new("source-unavailable");
    let case = LiveSourceUnavailableCase::new(
        Suite::CrossCodeEval,
        "python-17",
        "https://github.com/missing/project.git",
        "abc1234",
        "repository_not_found",
        "a".repeat(64),
        serde_json::json!({"prompt_sha256": "b".repeat(64)}),
    )
    .unwrap();

    let first = checkpoint_source_unavailable(&config(&root), &case).unwrap();
    let resumed = checkpoint_source_unavailable(&config(&root), &case).unwrap();
    assert!(!first.resumed());
    assert!(resumed.resumed());
}
```

The unavailable-checkpoint API deliberately accepts no materializer, indexer, or backend parameter; the type boundary plus the test proves those calls are impossible on this path.

- [ ] **Step 2: Prove RED**

Run: `scripts/spur-cargo test -p spur-code-eval --test live_evaluate source_unavailable -- --nocapture`

Expected: compile failure for absent case/checkpoint APIs.

- [ ] **Step 3: Add the explicit terminal contract**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LiveUnavailableTerminalOutcome {
    SourceUnavailable {
        suite: Suite,
        case_id: String,
        source: LiveUnavailableSourceIdentity,
        diagnostic_class: String,
        evidence_digest: String,
    },
}
```

Use a domain-separated checkpoint/input hash including the v2 source-lock contract, evidence digest, query policy, case fingerprint, adapter version, and evaluator version. Keep runnable `LiveCaseResult` unchanged.

- [ ] **Step 4: Add tamper/resume regressions**

Modify each identity/evidence/payload field in serialized fixtures and assert resume returns no trusted result. Assert truncated/invalid JSON is replaced atomically and no temporary file survives.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-code-eval --test live_evaluate
scripts/spur-cargo check -p spur-code-eval
```

Expected: evaluator suite and check pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-code-eval/src/live/evaluate.rs crates/spur-code-eval/tests/live_evaluate.rs
git commit -m "feat(spur-code-eval): unavailable-checkpoints persist terminal sources"
```

---

### Task 4: Available-corpus accounting, metrics, and publication policy

**Task ID:** `report-policy`

**Files:**
- Modify: `crates/spur-code-eval/src/report.rs`
- Create: `crates/spur-code-eval/tests/live_report.rs`

**Depends on:** `lock-v2`

**Acceptance Criteria:**
- [ ] The live report schema records `source_unavailable` explicitly and exposes `RunScope::{InvalidAccounting, ExecutionIncomplete, FilteredPartial, AvailableCorpus, CompleteSource}`.
- [ ] Per-suite/global conservation includes every validated source member exactly once and every eligible member in exactly one terminal eligible bucket.
- [ ] Metrics may be present for `answered < eligible` only when the difference is exactly explained by typed unavailable/filtered/failed accounting.
- [ ] Native metric denominators equal verified evaluated (`answered`) checkpoints while manifest `eligible` remains unchanged.
- [ ] An available-corpus report renders deterministic JSON and always has `ReleaseStatus::Reject`.
- [ ] Only complete-source accounting with every integrity gate true may produce `PublishDeterministic`.
- [ ] Checksum-identical inputs render checksum-identical reports.

**Suggested Worker:** codex — isolated policy/data-model change with a dedicated test file.

**Scope Boundary:**
- IN scope: report schema v2, `LiveAccounting`, run-scope derivation, live metric validation, release gate, report JSON tests.
- OUT of scope: building accounting from evaluator results, CLI, source resolution, real execution.
- Preserve the fixture `BenchmarkReport` contract unless a versioned compatibility field is required; fixture reports must not become live reports.

**Implementation:**

- [ ] **Step 1: Add failing available-corpus and publish-counterexample tests**

```rust
#[test]
fn available_corpus_report_has_real_metrics_but_cannot_publish() {
    let accounting = LiveAccounting {
        total: 2,
        eligible: 2,
        answered: 1,
        source_unavailable: 1,
        ..LiveAccounting::default()
    };
    let report = live_report(accounting, complete_metrics_for_one_answer()).unwrap();
    assert_eq!(report.run_scope(), RunScope::AvailableCorpus);
    assert_eq!(report.release_status(), ReleaseStatus::Reject);
}
```

Also add a complete-source positive control and a malformed conservation negative fixture.

- [ ] **Step 2: Prove RED**

Run: `scripts/spur-cargo test -p spur-code-eval --test live_report -- --nocapture`

Expected: compile failure for absent `source_unavailable`/`RunScope` APIs.

- [ ] **Step 3: Implement schema-v2 accounting**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunScope {
    InvalidAccounting,
    ExecutionIncomplete,
    FilteredPartial,
    AvailableCorpus,
    CompleteSource,
}
```

Use `InvalidAccounting` in the pure classifier and convert it to `ReportError::InvalidLiveAccounting` before constructing a report. For conserved input, derive scope in this order: execution failure/pending/skipped, explicit filtered, source unavailable, complete. Do not encode unavailable as a zero-quality retrieval answer.

- [ ] **Step 4: Relax metric completeness only to the evaluated denominator**

Require native aggregate `answered` to equal live `answered`, carry manifest `eligible` separately, and reject missing suite aggregates when `answered > 0`. Retain source-unavailable counts even when zero metrics exist for that suite.

- [ ] **Step 5: Verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-code-eval --test live_report
scripts/spur-cargo test -p spur-code-eval --test report
scripts/spur-cargo check -p spur-code-eval
```

Expected: live and fixture report tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-code-eval/src/report.rs crates/spur-code-eval/tests/live_report.rs
git commit -m "feat(spur-code-eval): report-policy account available corpus"
```

---

### Task 5: Runner and CLI availability-aware orchestration

**Task ID:** `runner-integration`

**Files:**
- Modify: `crates/spur-code-eval/src/runner.rs`
- Test: `crates/spur-code-eval/tests/live_runner.rs`

**Depends on:** `availability-normalization`, `unavailable-checkpoints`, `report-policy`

**Acceptance Criteria:**
- [ ] CLI accepts `--availability-evidence-root <DIR>` with the canonical existing evidence layout and includes its stable digests in input identity/reproducibility metadata.
- [ ] `validate` creates a v2 lock when the requested lock path is absent and verifies it byte-for-byte/semantically when present.
- [ ] Normalization observes all validated denominators, all 1,002 CrossCodeEval identities, and all resolved/unavailable case mappings before advancing phase.
- [ ] Index/retrieval operate only on resolved repository pins; unavailable cases receive Task 3 terminal checkpoints without materialization/index/backend calls.
- [ ] Score recomputes suite metrics only from verified completed checkpoints and constructs exact answered/unavailable/filtered/failed accounting.
- [ ] Report produces `available_corpus` plus `reject` for the current census; explicit filters remain `filtered_partial`.
- [ ] Resume verifies both runnable and unavailable checkpoint sets and preserves deterministic report bytes.
- [ ] Synthetic end-to-end runner tests, the full crate suite, and CLI help pass.

**Suggested Worker:** claude-code-acp — large existing runner with several tightly coupled phase seams.

**Scope Boundary:**
- IN scope: `Cli`, live input loading, phase snapshots/hashes, preparation, accounting, scoring, report construction, resume, runner tests.
- OUT of scope: changing source-lock validation rules, census parsing internals, suite scorer algorithms, public SPUR backend schemas, unrelated crates.
- If a public backend response lacks required evidence, retain the existing typed failure; do not add a heuristic.

**Scope Drift Checkpoint:**
- If implementation requires changing `query.rs`, `jcg.rs`, or suite adapter source files, emit `scope_drift` with the exact missing contract.
- If the runner cannot prove checkpoint conservation before scoring, emit `risk` and keep the phase fail-closed.

**Implementation:**

- [ ] **Step 1: Add a failing mixed run integration test**

```rust
#[test]
fn mixed_availability_run_scores_resolved_cases_and_rejects_publication() {
    let fixture = MixedAvailabilityFixture::one_resolved_one_unavailable();
    let report = fixture.run_through_report().unwrap();

    assert_eq!(fixture.materialize_calls(), 1);
    assert_eq!(fixture.query_calls(), 1);
    assert_eq!(report.global().answered, 1);
    assert_eq!(report.global().source_unavailable, 1);
    assert_eq!(report.run_scope(), RunScope::AvailableCorpus);
    assert_eq!(report.release_status(), ReleaseStatus::Reject);
}
```

- [ ] **Step 2: Prove RED**

Run: `scripts/spur-cargo test -p spur-code-eval --test live_runner mixed_availability -- --nocapture`

Expected: CLI/runner lacks availability evidence and unavailable accounting.

- [ ] **Step 3: Add canonical evidence-root loading and lock creation**

The directory contract is exactly:

```text
<root>/CENSUS-EVIDENCE-MANIFEST-v1.json
<root>/CENSUS-SHA256SUMS-v1
<root>/census/census.jsonl
```

Resolve original/recovery rows only through Task 2. Write the v2 lock atomically after all identities and hashes validate; never leave a partially valid canonical lock at the requested path.

- [ ] **Step 4: Route unavailable cases around execution boundaries**

Build runnable `LiveCase` values and `LiveSourceUnavailableCase` values as separate lists. Persist both terminal result sets, hash them into the checkpoint-set identity, and join only at accounting/report boundaries.

- [ ] **Step 5: Recompute native metrics and report scope**

Keep all existing RepoQA/CrossCodeEval/JCG scorer paths. Feed only verified completed results to native metric inputs, while `LiveAccounting` retains the manifest denominator and unavailable terminal count.

- [ ] **Step 6: Add resume/tamper and filter tests**

Cover unavailable evidence digest change, v1/v2 lock identity change, census checksum change, recovered materialization drift, deleted checkpoint, explicit case filter, zero runnable answers, and byte-identical resume.

- [ ] **Step 7: Verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-code-eval --test live_runner
scripts/spur-cargo test -p spur-code-eval
scripts/spur-cargo run -p spur-code-eval -- --help
```

Expected: runner and full crate tests pass; help documents the evidence-root contract.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-code-eval/src/runner.rs crates/spur-code-eval/tests/live_runner.rs
git commit -m "feat(spur-code-eval): runner-integration execute available corpus"
```

---

### Task 6: Genuine available-corpus benchmark and evidence handoff

**Task ID:** `real-run`

**Files:**
- Create: `docs/superpowers/reviews/2026-08-29-code-intelligence-available-corpus-benchmark.md`
- Generate, do not commit: `.spur/bench-evidence/bd-11qh-available-v1/**`

**Depends on:** `runner-integration`

**Acceptance Criteria:**
- [ ] All source archives and census evidence are independently checksum-verified before execution.
- [ ] The v2 lock accounts for exactly 1,002 CrossCodeEval repository identities: 945 original, 5 authoritative recovery, 52 unavailable.
- [ ] The unfiltered run executes every verifiably materializable eligible case for RepoQA, CrossCodeEval, and JCG; each suite has a nonzero real evaluated denominator.
- [ ] The report conserves every validated source member and eligible case, exposes evaluated and unavailable denominators, and has `run_scope = available_corpus` plus `release_status = reject`.
- [ ] No fixture result is mixed into the real report.
- [ ] Resume produces byte-identical report JSON/checksum and reuses verified checkpoints.
- [ ] Crate tests, crate-only Clippy, rustdoc, CLI help, and the semantic benchmark pass.
- [ ] The review document records exact commands, exit codes, timings, source/lock/report/checkpoint hashes, suite metrics, denominators, unavailable count, and the complete-source limitation.

**Suggested Worker:** codex — command execution, evidence verification, and precise result reporting after implementation converges.

**Scope Boundary:**
- IN scope: running the reviewed binary on pinned local inputs, building the immutable v2 lock/cache, resuming, verifying evidence, writing the review document.
- OUT of scope: production code changes, source substitution, new mirrors, metric/scorer changes, suppressing failures, or claiming `publish_deterministic`.
- If implementation defects appear, emit `retry_exhausted` or `risk` with logs; do not patch production files in this task.

**Implementation:**

- [ ] **Step 1: Verify immutable inputs**

Use these exact inputs:

```text
/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-11qh-real-source-20260828/sources/repoqa-2024-06-23.json.gz
/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-11qh-real-source-20260828/sources/crosscodeeval_data.tar.xz
/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-11qh-real-source-20260828/sources/jcg-4737aac8c2652acded1c4505961b6af52d06ceeb.tar.gz
/Volumes/Projects/Projects/spur/.spur/bench-evidence/bd-11qh-live
```

Expected archive SHA-256 values are respectively `c050a2ad90a7df89d9dc1f1c3b3b20683edd20a56293b35fcaae43dec115d681`, `d65c0316f63df3434deac3b67ae95478cbe00c706c14ff1a91e1173619962b88`, and `db109e8628f4279d2847d803c0c922a07d64b5c6c242cbe9f92c474555f4b4f8`. The census checksum root must remain `26774f8fe6d78474cb7d728a3479db6c104f0de264a3d5fa342edc3cbe4ca7a1`.

- [ ] **Step 2: Run validation and create the v2 lock**

```bash
SPUR_REMOTE=0 scripts/spur-cargo run -p spur-code-eval -- \
  --run-dir .spur/bench-evidence/bd-11qh-available-v1/run \
  --repoqa-source .spur/bench-evidence/bd-11qh-real-source-20260828/sources/repoqa-2024-06-23.json.gz \
  --crosscodeeval-source .spur/bench-evidence/bd-11qh-real-source-20260828/sources/crosscodeeval_data.tar.xz \
  --jcg-source .spur/bench-evidence/bd-11qh-real-source-20260828/sources/jcg-4737aac8c2652acded1c4505961b6af52d06ceeb.tar.gz \
  --source-lock .spur/bench-evidence/bd-11qh-available-v1/source-lock-v2.json \
  --availability-evidence-root .spur/bench-evidence/bd-11qh-live \
  --repository-cache-root .spur/bench-evidence/bd-11qh-available-v1/repository-cache \
  --top-k 10 \
  --exact-followup-limit 5 \
  validate
```

Expected: exit 0, complete v2 lock, no partial canonical lock.

- [ ] **Step 3: Execute every unfiltered phase**

Invoke the same common arguments sequentially with `index`, `retrieve`, `score`, and `report`. Capture stdout/stderr and exit code for every command under `.spur/bench-evidence/bd-11qh-available-v1/logs/`. Do not pass `--suite` or `--case` filters.

- [ ] **Step 4: Verify report semantics and checksum**

Assert with `jq` that each suite has nonzero `answered`, global conservation holds, `source_unavailable > 0`, `run_scope == "available_corpus"`, and `release_status == "reject"`. Recompute the SHA-256 sidecar from report bytes.

- [ ] **Step 5: Verify deterministic resume**

Save the first report/checksum, invoke `resume` with identical inputs, and compare report bytes and checkpoint-set digest. The second run must report resumed verified checkpoints and must not rewrite immutable artifacts with differing bytes.

- [ ] **Step 6: Run regression gates**

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-code-eval
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-code-eval --all-targets --no-deps -- -D warnings
scripts/spur-cargo doc -p spur-code-eval --no-deps
scripts/spur-cargo run -p spur-code-eval -- --help
scripts/spur-cargo test -p spur-graph --test semantic_benchmark
```

Expected: every command exits 0. `--no-deps` intentionally scopes Clippy to `spur-code-eval`; it does not suppress any lint in the benchmark crate.

- [ ] **Step 7: Write and commit the evidence review**

The review must state that this is a genuine available-corpus result, not a full complete-source industry benchmark. Include exact suite metrics and both manifest/evaluated denominators; do not reuse the fixture `1.0` as a live metric.

```bash
git add docs/superpowers/reviews/2026-08-29-code-intelligence-available-corpus-benchmark.md
git commit -m "docs(spur-code-eval): real-run record available-corpus benchmark"
```

---

## Plan-wide review gates

Before approving each worker result, the brain must verify:

1. The task changed only its declared files, except an explicitly approved signal-driven scope amendment.
2. RED evidence predates GREEN implementation for every production task.
3. No network failure is encoded as source-unavailable without verified census evidence.
4. No unavailable case is silently removed, marked unsupported, or assigned zero retrieval quality.
5. Metrics are computed only from verified completed checkpoints.
6. `source_unavailable > 0` implies `ReleaseStatus::Reject` for all report paths.
7. The final report is not described as complete-source, publishable, or directly comparable to a different availability set.

## Expected final outcome

The plan produces the first genuine SPUR code-intelligence scores over every verifiably materializable case in the pinned RepoQA, CrossCodeEval, and JCG sources. It also gives an exact, auditable explanation for the remaining unavailable cases. A future complete-source result requires a new immutable lock with zero unavailable eligible cases; it does not require changing the scoring policy again.
