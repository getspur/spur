# Brain Continuation Phase 4 — Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` to implement this plan task-by-task.

**Goal:** Close out the three concrete Phase 3 carryovers documented in `docs/superpowers/plans/2026-04-25-brain-continuation-phase-3-verification.md` — INV-D9 proptest, CompletionAuditFields struct refactor, and `total_bytes` field in `outcome_namespace_deleted`.

**Architecture:** Three independent tasks. Each touches a narrow surface (1-3 files). No new abstractions; just delivers what Phase 3 review flagged as deferred.

**Tech Stack:** Rust workspace; `proptest` (dev-dep); existing OutcomeStore trait + audit-sentinel plumbing.

---

## Task 1: INV-D9 proptest for materializer envelope clipping

**Files:**
- Modify: `crates/spur-mcp/Cargo.toml` (add `proptest` dev-dep)
- Create: `crates/spur-mcp/tests/inv_d9_proptest.rs`

**What:** Property test asserting that for any `DelegationStatus` variant, `OutcomeMaterializer::materialize` produces a `BrainContinuation` whose envelope (via `estimate_envelope_cost`) stays under `MERGE_BUDGET_DEFAULT_BYTES`. Generalizes the existing per-variant unit tests to exhaustive variant coverage.

- [ ] **Step 1: Add `proptest` to spur-mcp dev-deps**

Edit `crates/spur-mcp/Cargo.toml`'s `[dev-dependencies]`:

```toml
proptest = { workspace = true }
```

(Already in `[workspace.dependencies]` as `proptest = "1"` — confirmed by spur-core usage.)

- [ ] **Step 2: Write the proptest**

Create `crates/spur-mcp/tests/inv_d9_proptest.rs`:

```rust
//! INV-D9 proptest: every `DelegationStatus` variant, when run through
//! `OutcomeMaterializer::materialize`, produces a `BrainContinuation`
//! whose envelope (conservative estimate) fits under
//! `MERGE_BUDGET_DEFAULT_BYTES`.
//!
//! Generalizes the existing unit tests
//! (`materialize_clips_oversized_status_error`, etc.) to exhaustive
//! variant coverage. Round 9 P3-S2: arb generators broadened so every
//! status produces a continuation under budget.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use spur_acp::{BrainSessionId, SessionId};
use spur_acp::domain::{
    ContinuationSource, DelegationId, DelegationResult,
    delegation::{DelegationStatus, TimeoutFallback},
    events::DiffSummary,
    merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
};
use spur_blob_store::{MemoryOutcomeStore, OutcomeStore};
use spur_mcp::outcome_materializer::{estimate_envelope_cost, OutcomeMaterializer};

prop_compose! {
    /// Bounded printable string so the generator doesn't randomly exceed
    /// the budget at the input layer (we want to test the materializer's
    /// CLIPPING, not the budget).
    fn arb_text(max_len: usize)(s in proptest::string::string_regex("[a-z ]{0,512}").unwrap())
        -> String { s.chars().take(max_len).collect() }
}

fn arb_diff_summary() -> impl Strategy<Value = DiffSummary> {
    (
        0u32..200,
        0u64..200_000,
        0u64..200_000,
        proptest::collection::vec(
            proptest::string::string_regex("[a-z/_.0-9]{1,64}").unwrap(),
            0..40,
        ),
    )
        .prop_map(|(files_changed, insertions, deletions, files)| DiffSummary {
            files_changed,
            insertions,
            deletions,
            files: files.into_iter().map(PathBuf::from).collect(),
        })
}

fn arb_delegation_status() -> impl Strategy<Value = DelegationStatus> {
    prop_oneof![
        Just(DelegationStatus::Success),
        Just(DelegationStatus::Timeout),
        arb_text(2_000).prop_map(|error| DelegationStatus::Failed { error }),
        proptest::collection::vec(
            proptest::string::string_regex("[a-z/_.0-9]{1,128}").unwrap(),
            0..40,
        )
        .prop_map(|files| DelegationStatus::Conflict {
            files: files.into_iter().map(PathBuf::from).collect(),
        }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::Rejected { reason }),
        arb_text(1_000).prop_map(|reviewer_note| DelegationStatus::Modified { reviewer_note }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::Cancelled { reason }),
        arb_text(1_000).prop_map(|reason| DelegationStatus::TimedOut {
            waited_for: Duration::from_secs(60),
            fallback: TimeoutFallback::Reject { reason },
        }),
    ]
}

prop_compose! {
    fn arb_delegation_result()(
        status in arb_delegation_status(),
        summary in proptest::option::of(arb_text(2_000)),
        diff in proptest::option::of(arb_text(20_000)),
        diff_summary in proptest::option::of(arb_diff_summary()),
        worker_branch in proptest::option::of(arb_text(512)),
        cost_micros in 0u64..1_000_000_000,
    ) -> DelegationResult {
        DelegationResult {
            status,
            diff,
            diff_summary,
            summary,
            estimated_cost_usd: cost_micros as f64 / 1_000_000.0,
            worker_branch,
            artifact: None,
        }
    }
}

fn brain_session() -> BrainSessionId {
    BrainSessionId::new(SessionId(
        "550e8400-e29b-41d4-a716-446655440000".into(),
    ))
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn inv_d9_arb_delegation_status_clips_under_budget(result in arb_delegation_result()) {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let cont = futures::executor::block_on(mat.materialize(
            result,
            DelegationId::from("deadbeef-1111-2222-3333-444455556666"),
            1,
            brain_session(),
            ContinuationSource::BlockTimeout,
            None,
        ));
        let envelope = estimate_envelope_cost(&cont.payload);
        prop_assert!(
            envelope <= MERGE_BUDGET_DEFAULT_BYTES,
            "INV-D9 violation: envelope={envelope} > budget={}",
            MERGE_BUDGET_DEFAULT_BYTES,
        );
    }
}
```

- [ ] **Step 3: Run the proptest (red, then verify under default config)**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --test inv_d9_proptest`
Expected: 256 cases pass. If any case fails, the materializer's clip helpers have a gap; fix the helpers before continuing.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/tests/inv_d9_proptest.rs
git commit -m "test(spur-mcp): INV-D9 proptest for materializer envelope clipping

For any DelegationStatus variant + arbitrary summary/diff/branch
inputs, OutcomeMaterializer::materialize must produce a continuation
whose envelope fits under MERGE_BUDGET_DEFAULT_BYTES. 256 cases per
run. Generalizes the existing per-variant unit tests to exhaustive
variant coverage (Round 9 P3-S2; deferred from Phase 3).

Phase 4 of plan-5; closes the materializer-clipping invariant gap."
```

---

## Task 2: Extract `CompletionAuditFields` struct

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs` (add struct)
- Modify: `crates/spur-mcp/src/plan/mod.rs` (`emit_completion_audit`, `persist_completion_result`, `persist_completion_result_and_notify`)
- Modify: `crates/spur-mcp/src/plan/reconciler.rs` (one caller)
- Modify: any callers of `persist_completion_result` (audit-test crates)

**What:** Address gemini's T9 SHOULD-FIX. The 3 plumbing functions accumulated `worker_branch`, `result_summary`, `artifact_uri` parameters — extract them into a `CompletionAuditFields` struct so adding the next field (e.g., `outcome_byte_size`) doesn't ripple to every caller. Removes the `#[allow(clippy::too_many_arguments)]` silencer added in T12.

- [ ] **Step 1: Define the struct in audit_sentinel.rs**

Add near the top of `crates/spur-mcp/src/plan/audit_sentinel.rs`:

```rust
/// Optional fields propagated from a completed delegation into the beads
/// audit comment. Bundled to keep the plumbing functions
/// (`emit_completion_audit`, `persist_completion_result`,
/// `persist_completion_result_and_notify`) at a manageable arg count and to
/// localize future field additions to one struct.
#[derive(Debug, Default, Clone)]
pub struct CompletionAuditFields {
    pub worker_branch: Option<String>,
    pub result_summary: Option<String>,
    pub artifact_uri: Option<String>,
}
```

- [ ] **Step 2: Update `emit_completion_audit` signature**

In `crates/spur-mcp/src/plan/mod.rs:997`, change `emit_completion_audit` to accept `fields: &CompletionAuditFields` instead of three separate params. Inside the body, replace `worker_branch`, `result_summary`, `artifact_uri` references with `fields.worker_branch.clone()`, etc.

- [ ] **Step 3: Update `persist_completion_result` signature**

Replace 3 separate params with `fields: CompletionAuditFields` (taken by value since the function moves them downstream). Remove `#[allow(clippy::too_many_arguments)]` — the new arg count is 6.

- [ ] **Step 4: Update `persist_completion_result_and_notify` body**

After the materializer runs, build a `CompletionAuditFields`:

```rust
let fields = CompletionAuditFields {
    worker_branch: result.worker_branch.clone(),
    result_summary: cont.payload.summary.clone(),
    artifact_uri,
};
persist_completion_result(pm, issue_id, plan_id, delegation_id, completion_state, fields).await?;
```

For the Superseded early-return path:

```rust
let fields = CompletionAuditFields {
    worker_branch: result.worker_branch.clone(),
    result_summary: result.summary.clone(),
    artifact_uri: None,
};
persist_completion_result(pm, issue_id, plan_id, delegation_id, completion_state, fields).await?;
```

- [ ] **Step 5: Update the reconciler caller**

In `crates/spur-mcp/src/plan/reconciler.rs:421`, the call to `persist_completion_result_and_notify` already passes the full result via the materializer path — no change needed there.

- [ ] **Step 6: Update direct test callers**

Run: `grep -rn "persist_completion_result\|emit_completion_audit" crates/spur-mcp/src/ crates/spur-mcp/tests/ | grep -v "fn persist_completion_result\|fn emit_completion_audit"`

For each call site that passes the old 3 separate strings, construct a `CompletionAuditFields { worker_branch, result_summary, artifact_uri }` literal.

- [ ] **Step 7: Run tests + clippy**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib audit_sentinel`
Expected: green.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib persist_completion_result_and_notify_materializes_artifact_uri_in_audit`
Expected: green.

Run: `RUSTC_WRAPPER= cargo clippy -p spur-mcp --no-deps -- -D warnings`
Expected: clean (the silencer should now be unnecessary).

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp`
Expected: no regressions.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/tests/
git commit -m "refactor(spur-mcp): extract CompletionAuditFields struct

Bundle worker_branch / result_summary / artifact_uri into one
CompletionAuditFields struct so the plumbing functions
(emit_completion_audit, persist_completion_result,
persist_completion_result_and_notify) stay below clippy's
too-many-arguments threshold.

Removes the #[allow(clippy::too_many_arguments)] silencer added
during Phase 3 verification (commit 340f2ea). Future audit-comment
fields can extend the struct without rippling through every caller.

Phase 4 of plan-5; addresses gemini T9 SHOULD-FIX
(parameter creep / future-proofing)."
```

---

## Task 3: Surface `total_bytes` in `outcome_namespace_deleted` event

**Files:**
- Modify: `crates/spur-blob-store/src/trait_def.rs` (or wherever `OutcomeStore` is defined)
- Modify: `crates/spur-blob-store/src/fs_store.rs` (FsOutcomeStore impl)
- Modify: `crates/spur-blob-store/src/measured.rs` (MeasuredOutcomeStore impl)
- Modify: `crates/spur-blob-store/src/test_helpers.rs` (MockFailingOutcomeStore impl)
- Modify: `crates/spur-worktree/src/git_blob_store.rs` (GitBlobOutcomeStore impl)
- Modify: `crates/spur-cli/src/main.rs` (emit total_bytes)

**What:** Address Phase 3's documented gap. `OutcomeStore::delete_namespace` returns `Result<usize, StoreError>` (count only); the `outcome_namespace_deleted` event in §10.1 expects a `total_bytes` field too. Extend the trait return type to a small `DeleteNamespaceReport { count: usize, total_bytes: u64 }` struct so the CLI can emit accurate metrics.

- [ ] **Step 1: Add the report struct + change the trait**

In `crates/spur-blob-store/src/types.rs` (or the file that defines `OutcomeStore` companion types):

```rust
/// Result of `OutcomeStore::delete_namespace`. Phase 4 added `total_bytes`
/// per spec §10.1 so the `outcome_namespace_deleted` metric can report
/// reclaimed disk usage instead of a placeholder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteNamespaceReport {
    /// Number of artifacts (blobs/refs) removed.
    pub count: usize,
    /// Total bytes reclaimed.
    pub total_bytes: u64,
}
```

In the `OutcomeStore` trait definition, change:

```rust
async fn delete_namespace(&self, brain_session_id: &BrainSessionId)
    -> Result<usize, StoreError>;
```

to:

```rust
async fn delete_namespace(&self, brain_session_id: &BrainSessionId)
    -> Result<DeleteNamespaceReport, StoreError>;
```

- [ ] **Step 2: Update FsOutcomeStore impl**

In `crates/spur-blob-store/src/fs_store.rs`, before deleting each blob, sum its file size into a `total_bytes` accumulator. Return `DeleteNamespaceReport { count, total_bytes }`.

Existing pattern uses `tokio::fs::remove_dir_all`; switch to enumerate-then-remove so we can sum sizes:

```rust
async fn delete_namespace(&self, brain_session_id: &BrainSessionId)
    -> Result<DeleteNamespaceReport, StoreError> {
    let ns_dir = self.namespace_dir(brain_session_id);
    if !ns_dir.exists() {
        return Ok(DeleteNamespaceReport::default());
    }
    let mut count = 0usize;
    let mut total_bytes = 0u64;
    let mut entries = tokio::fs::read_dir(&ns_dir).await
        .map_err(StoreError::Io)?;
    while let Some(entry) = entries.next_entry().await.map_err(StoreError::Io)? {
        let metadata = entry.metadata().await.map_err(StoreError::Io)?;
        if metadata.is_file() {
            total_bytes += metadata.len();
            count += 1;
        }
    }
    tokio::fs::remove_dir_all(&ns_dir).await.map_err(StoreError::Io)?;
    Ok(DeleteNamespaceReport { count, total_bytes })
}
```

- [ ] **Step 3: Update GitBlobOutcomeStore impl**

In `crates/spur-worktree/src/git_blob_store.rs`, the existing impl deletes refs via `git update-ref -d`. Sum sizes via `git cat-file -s <sha>` per blob before deletion.

```rust
// Pseudo-flow:
// 1. List refs under refs/spur/outcomes/<session>/* via `git for-each-ref`.
// 2. For each ref, `git rev-parse` to get the blob SHA, `git cat-file -s` for size.
// 3. Sum sizes; delete the ref.
// 4. Also clean up legacy refs/spur/artifacts/<session> per Round 11 design.
// 5. Return DeleteNamespaceReport { count, total_bytes }.
```

(Sketch only — implementer fills in. Use the existing `run_git` helper.)

- [ ] **Step 4: Update MeasuredOutcomeStore wrapper**

The wrapper just forwards; change the return type to match.

- [ ] **Step 5: Update MockFailingOutcomeStore**

In `crates/spur-blob-store/src/test_helpers.rs`, the mock returns errors regardless. Just update the return type from `Result<usize, StoreError>` to `Result<DeleteNamespaceReport, StoreError>`. The error path is unchanged.

- [ ] **Step 6: Update CLI to emit total_bytes**

In `crates/spur-cli/src/main.rs::run_gc_outcomes`, the namespace-delete path already calls `delete_namespace`. Update to use the new return:

```rust
let report = store.delete_namespace(&session_id).await?;
tracing::info!(
    target: "spur.metrics.outcome_namespace_deleted",
    brain_session_id = %session_id,
    artifact_count = report.count,
    total_bytes = report.total_bytes,
    source = "cli.gc_outcomes",
);
println!(
    "Deleted {} blobs ({} bytes) in namespace {session_id}",
    report.count, report.total_bytes,
);
```

Remove the doc comment about the omitted-field gap.

- [ ] **Step 7: Update any other callers**

Run: `grep -rn "delete_namespace" crates/`

Each caller needs to switch from `usize` to `DeleteNamespaceReport`. If there are callers that just want the count, use `report.count`.

- [ ] **Step 8: Add a regression test for FsOutcomeStore**

In `crates/spur-blob-store/src/fs_store.rs`'s test module:

```rust
#[tokio::test]
async fn fs_store_delete_namespace_reports_total_bytes() {
    let td = TempDir::new().unwrap();
    let store = FsOutcomeStore::new(td.path().to_path_buf());
    let session = brain_session_id();
    let key = OutcomeKey { brain_session_id: session.clone(), delegation_id: "d".into(), attempt: 1 };
    let bytes = b"x".repeat(1_024);
    let metadata = test_metadata(&bytes);
    store.put(&key, &bytes, &metadata).await.unwrap();
    let report = store.delete_namespace(&session).await.unwrap();
    assert_eq!(report.count, 1);
    assert!(report.total_bytes >= 1_024, "expected ≥1024, got {}", report.total_bytes);
}
```

- [ ] **Step 9: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-blob-store --lib`
Run: `RUSTC_WRAPPER= cargo test -p spur-worktree --lib`
Run: `RUSTC_WRAPPER= cargo test -p spur-cli --bin spur`
Run: `RUSTC_WRAPPER= cargo check --workspace`
Expected: green.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-blob-store/ crates/spur-worktree/src/git_blob_store.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-blob-store): DeleteNamespaceReport with total_bytes

Extend OutcomeStore::delete_namespace return type from usize to
DeleteNamespaceReport { count, total_bytes } so CLI/operator metrics
can report reclaimed bytes per spec §10.1. The FsOutcomeStore impl
enumerates entries before remove_dir_all so it can sum file sizes;
GitBlobOutcomeStore queries git cat-file -s per ref before deletion.

CLI's outcome_namespace_deleted tracing event now emits the real
total_bytes value instead of the placeholder 0u64 from Phase 3 T11.

Phase 4 of plan-5; addresses Phase 3 documented gap (verification
report carryover #3)."
```

---

## Task 4: Phase 4 verification

**Files:** none (verification-only).

**What:** Confirm Tasks 1–3 land cleanly with no regressions across Phase 3 + 4 crates.

- [ ] **Step 1: Run all Phase 3 + 4 crate lib tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp -p spur-blob-store -p spur-worktree -p spur-mcp -p spur-core -p spur-cli --lib`
Expected: counts ≥ Phase 3 baseline (608) + new tests from Tasks 1, 3.

- [ ] **Step 2: Clippy gate**

Run: `RUSTC_WRAPPER= cargo clippy -p spur-acp -p spur-blob-store -p spur-worktree -p spur-mcp -p spur-core -p spur-cli --no-deps -- -D warnings`
Expected: clean. (The Phase 3 silencer on `persist_completion_result` should be removed by Task 2.)

- [ ] **Step 3: Targeted INV proptest**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --test inv_d9_proptest`
Expected: 256 cases pass.

- [ ] **Step 4: Workspace check**

Run: `RUSTC_WRAPPER= cargo check -p spur-acp -p spur-blob-store -p spur-worktree -p spur-mcp -p spur-core -p spur-cli --all-targets`
Expected: exit 0.

- [ ] **Step 5: Write the verification report**

Save inline in the delegation summary or as `docs/superpowers/plans/2026-04-26-brain-continuation-phase-4-verification.md` if convenient.
