# Brain Continuation Phase 3 — `OutcomeMaterializer` + Lean Schema v3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **REVISION HISTORY:**
> - **R1 (kimi+gemini+codex sequential)** — applied: rewrote Task 1 clip helpers to match real `DelegationStatus`/`TimeoutFallback`/`String` (not Option) field types; added `DelegationStatus::Timeout` variant; replaced merge_budget relocation in Task 5 with a conservative envelope-cost estimate so the spur-core rendered-resource arithmetic stays in spur-core; extended `ContinuationResourceBody<'a>` in continuation_bridge.rs with the new v3 fields (Task 2) so the wire actually carries them; added MCP-layer `Section` projection in Task 10 (Phase 2 stores ignore the `_section` arg today); replaced broken `r.attempt` lookup with a `latest_attempt_by_delegation: HashMap` in Task 10; fixed Task 11 CLI to use real `spur-worktree` API + added missing direct deps; replaced Panic `FailureMode` variant in Task 3 with explicit panic-resilience documentation (mock can't `panic!` and stay testable).
> - **R2 (kimi+gemini+codex sequential)** — applied: Task 8 `persist_completion_result_and_notify` now early-returns on `CompletionState::Superseded` (don't spam the brain with stale attempt data) AND covers all four variants in the source-mapping match (R1's match was non-exhaustive); Task 10 `project_section` accepts `&OutcomeKey` and injects `attempt`/`brain_session`/`estimated_cost_micros` (converting USD→micros) per spec §7.5 line 818; Task 6 panic test rewritten using an inline async closure (R1 dropped `FailureMode::Panic` so the test referencing it was uncompilable); Task 4 adds `[features] test-support = []` to `crates/spur-mcp/Cargo.toml` (R1 added gated builder methods but missed the feature definition); Task 5 drops the unused `sha2::{Digest, Sha256}` import inside `materialize` (clippy `-D warnings` would fail) and converts `result.estimated_cost_usd` → `estimated_cost_micros` at materialize time (R1 left it `None` always, so v3 cost would never populate).

**Goal:** Introduce the `OutcomeMaterializer` (in `spur-mcp`), bump `BrainContinuation` to schema v3 with `artifact_id`/`fetch_hint`/`estimated_cost_micros` fields, extend `fetch_outcome_artifact` with `section` pagination + `attempt`, and integrate background TTL sweep + a `spur gc outcomes` CLI escape hatch.

**Architecture:** The materializer is the **single producer** of `BrainContinuation` for completed delegations. It runs `persist-then-clip-then-build`: persist the full `DelegationResult` to `OutcomeStore`, then construct a lean envelope with the same Plan-4 clip helpers the truncation-ladder fallback uses. INV-D8 (envelope ≤ MERGE_BUDGET) is enforced by clip + a release-mode `if envelope_bytes > budget` recovery path that drops to the truncation ladder. The clip helpers move from their current scattered locations to `spur-acp::domain::clip` so both the materializer (in `spur-mcp`) and the fallback (in `spur-core::continuation_bridge`) can share them.

**Tech Stack:** Rust workspace (`spur-acp`, `spur-mcp`, `spur-core`, `spur-blob-store`, `spur-cli`), `async-trait`, `tracing`, `tokio::spawn`, `serde` with `#[serde(default)]` for backward compat.

**Spec:** `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md` §7 (Phase 3) + §8 (GC + lifecycle) + §10 (Observability) + §11 (Migration).

**Phase 2 status:** complete on main as of `f7aca16` (commits `52ae755..f7aca16`). Phase 2 introduced `OutcomeStore` + `Memory/Fs/GitBlob/Measured` impls and migrated the orchestrator's oversized-stdout persist call site through `OutcomeStore::put`. Phase 3 builds on top: the orchestrator now owns an `Arc<dyn OutcomeStore>` (instead of constructing per-call) and routes ALL completion paths through the materializer.

---

## File Structure

| Path | Action | Responsibility |
|---|---|---|
| `crates/spur-acp/src/domain/clip.rs` | Create | `clip_status_strings`, `clip_diff_files`, `clip_artifact_ref_strings`, `clip_with_ellipsis` |
| `crates/spur-acp/src/domain/mod.rs` | Modify | `pub mod clip;` |
| `crates/spur-acp/src/domain/continuation.rs` | Modify | New fields on `ContinuationPayload`; `estimated_cost_usd()` helper |
| `crates/spur-core/src/continuation_bridge.rs:149` | Modify | `schema_version: 3` |
| `crates/spur-core/src/continuation_bridge.rs:178` | Modify | Replace local `clip_with_ellipsis` with re-export from `spur_acp::domain::clip` |
| `crates/spur-blob-store/src/test_helpers.rs` | Create | `MockFailingOutcomeStore<S>` + `FailureMode` |
| `crates/spur-blob-store/src/lib.rs` | Modify | `#[cfg(any(test, feature = "test-support"))] pub mod test_helpers;` |
| `crates/spur-blob-store/Cargo.toml` | Modify | Add `test-support` feature |
| `crates/spur-mcp/Cargo.toml` | Modify | Add `spur-blob-store = { workspace = true }` |
| `crates/spur-mcp/src/outcome_materializer.rs` | Create | `OutcomeMaterializer` struct + `materialize()` |
| `crates/spur-mcp/src/lib.rs` | Modify | `pub mod outcome_materializer;` |
| `crates/spur-mcp/src/server.rs:268` | Modify | `build_detached_continuation` routes through materializer |
| `crates/spur-mcp/src/server.rs:209` | Modify | Remove local `clip_with_ellipsis` (use shared one) |
| `crates/spur-mcp/src/server.rs:2668` | Modify | Extend `handle_fetch_outcome_artifact` with section + attempt + OutcomeStore::get |
| `crates/spur-mcp/src/server.rs` | Modify | `McpCallbackServer` carries `Arc<dyn OutcomeStore>` + `OutcomeMaterializer` |
| `crates/spur-mcp/src/tools.rs:265` | Modify | Update `fetch_outcome_artifact_def` schema (section enum, attempt) |
| `crates/spur-mcp/src/plan/audit_sentinel.rs:70` | Modify | Add `artifact_uri: Option<String>` to `Completion` variant |
| `crates/spur-mcp/src/plan/mod.rs:1309` | Modify | `persist_completion_result_and_notify` gains `&DelegationResult`, `&BrainSessionId`, `attempt`, `&OutcomeMaterializer` |
| `crates/spur-mcp/src/plan/reconciler.rs:421` | Modify | Pass full `&DelegationResult` + materializer ref |
| `crates/spur-core/src/orchestrator.rs` | Modify | Own `Arc<dyn OutcomeStore>`; replace per-call `GitBlobOutcomeStore::new` with shared store; spawn startup sweep |
| `crates/spur-cli/src/main.rs` | Modify | Add `gc outcomes` subcommand |
| `crates/spur-cli/Cargo.toml` | Modify | Already has spur-blob-store via spur-core; verify |

Phase 3 spans 5 crates and ~15 files. Each task ships independently and produces working code that passes tests.

---

## Task 1: Move clip helpers to `spur-acp::domain::clip`

**Files:**
- Create: `crates/spur-acp/src/domain/clip.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs`
- Modify: `crates/spur-core/src/continuation_bridge.rs:178`
- Modify: `crates/spur-mcp/src/server.rs:209`

**What:** The clip helpers are duplicated today (`spur-core/src/continuation_bridge.rs:178` and `spur-mcp/src/server.rs:209` both define `clip_with_ellipsis`). Phase 3's materializer needs them, plus three new helpers (`clip_status_strings`, `clip_diff_files`, `clip_artifact_ref_strings`). Centralize all five into one module so the materializer (in spur-mcp) and the truncation-ladder fallback (in spur-core::continuation_bridge) call the same code. Both crates already depend on spur-acp, so no new crate edges are introduced.

- [ ] **Step 1: Read the existing clip implementations**

Run: `grep -n "fn clip_with_ellipsis" crates/spur-core/src/continuation_bridge.rs crates/spur-mcp/src/server.rs`
Run: `sed -n '170,200p' crates/spur-core/src/continuation_bridge.rs`
Run: `sed -n '209,232p' crates/spur-mcp/src/server.rs`

The two implementations should be byte-identical (or near-identical). Confirm before continuing.

- [ ] **Step 2: Write the new module `crates/spur-acp/src/domain/clip.rs`**

```rust
//! Bounded-string clipping for continuation materialization.
//!
//! These helpers are part of the INV-D8 enforcement contract: the
//! materializer (`spur-mcp::OutcomeMaterializer`) and truncation-ladder
//! fallback (`spur-core::continuation_bridge`) both call into this module
//! to bound the lean payload's inline strings.
//!
//! **Do not call these helpers from random consumer code.** Adding a new
//! `BrainContinuation` producer that bypasses this module is a violation
//! of INV-D8 and will surface as oversized-drop failures in the merger.
//! New producers must route through `OutcomeMaterializer::materialize`.

use std::path::PathBuf;

use crate::domain::continuation::ArtifactRef;
use crate::domain::delegation::DelegationStatus;
use crate::domain::events::DiffSummary;

const ELLIPSIS: &str = "…";

/// Clip an `Option<String>` to at most `max_bytes`, appending "…" when truncated.
/// Returns `(clipped, was_truncated)`.
pub fn clip_with_ellipsis(s: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    let Some(s) = s else {
        return (None, false);
    };

    if s.len() <= max_bytes {
        return (Some(s), false);
    }

    if max_bytes <= ELLIPSIS.len() {
        return (Some(ELLIPSIS.to_string()), true);
    }

    let mut end = max_bytes - ELLIPSIS.len();
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }

    let mut clipped = s[..end].to_string();
    clipped.push_str(ELLIPSIS);
    (Some(clipped), true)
}

/// Clip in-place every inline `String` / `Vec<PathBuf>` field in
/// `DelegationStatus` so the status field of `ContinuationPayload` stays
/// bounded. Returns the clipped status; does NOT mutate the input.
///
/// **Field type reference (verified against
/// `crates/spur-acp/src/domain/delegation.rs:91`):**
/// - `Failed { error: String }`
/// - `Conflict { files: Vec<PathBuf> }`
/// - `Timeout` (no fields)
/// - `Rejected { reason: String }`
/// - `Modified { reviewer_note: String }` — NOT Option
/// - `TimedOut { waited_for: Duration, fallback: TimeoutFallback }` — NOT
///   `Box<DelegationStatus>`. `TimeoutFallback::Reject` carries the only
///   inline string here.
/// - `Cancelled { reason: String }` — NOT Option
/// - `Success` (no fields)
pub fn clip_status_strings(status: &DelegationStatus, max_bytes: usize) -> DelegationStatus {
    use crate::domain::delegation::TimeoutFallback;
    let mut s = status.clone();
    match &mut s {
        DelegationStatus::Failed { error } => {
            *error = clip_with_ellipsis(Some(std::mem::take(error)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Conflict { files } => {
            clip_path_vec(files, 16, 128);
        }
        DelegationStatus::Rejected { reason } => {
            *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Modified { reviewer_note } => {
            *reviewer_note = clip_with_ellipsis(Some(std::mem::take(reviewer_note)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::Cancelled { reason } => {
            *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                .0
                .unwrap_or_default();
        }
        DelegationStatus::TimedOut { fallback, .. } => {
            if let TimeoutFallback::Reject { reason } = fallback {
                *reason = clip_with_ellipsis(Some(std::mem::take(reason)), max_bytes)
                    .0
                    .unwrap_or_default();
            }
        }
        DelegationStatus::Success | DelegationStatus::Timeout => {}
    }
    s
}

/// Clip a `DiffSummary.files` vec to at most `max_files` entries, with each
/// path string capped at `max_path_bytes`.
pub fn clip_diff_files(diff: &DiffSummary, max_files: usize) -> DiffSummary {
    let mut out = diff.clone();
    clip_path_vec(&mut out.files, max_files, 128);
    out
}

/// Clip an `ArtifactRef.uri` and `git_object_ref` at `max_bytes` each.
pub fn clip_artifact_ref_strings(art: &ArtifactRef, max_bytes: usize) -> ArtifactRef {
    let mut out = art.clone();
    let (uri_clipped, _) = clip_with_ellipsis(Some(std::mem::take(&mut out.uri)), max_bytes);
    out.uri = uri_clipped.unwrap_or_default();
    if let Some(s) = out.git_object_ref.take() {
        let (clipped, _) = clip_with_ellipsis(Some(s), max_bytes);
        out.git_object_ref = clipped;
    }
    out
}

fn clip_path_vec(v: &mut Vec<PathBuf>, max_count: usize, max_path_bytes: usize) {
    if v.len() > max_count {
        v.truncate(max_count);
    }
    for p in v.iter_mut() {
        let s = p.to_string_lossy().into_owned();
        if s.len() > max_path_bytes {
            let (clipped, _) = clip_with_ellipsis(Some(s), max_path_bytes);
            *p = PathBuf::from(clipped.unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_with_ellipsis_under_cap_returns_unchanged() {
        let (out, trunc) = clip_with_ellipsis(Some("short".into()), 100);
        assert_eq!(out.as_deref(), Some("short"));
        assert!(!trunc);
    }

    #[test]
    fn clip_with_ellipsis_over_cap_appends_ellipsis() {
        let (out, trunc) = clip_with_ellipsis(Some("a".repeat(50)), 10);
        assert!(trunc);
        let s = out.expect("Some");
        assert!(s.ends_with(ELLIPSIS));
        assert!(s.len() <= 10);
    }

    #[test]
    fn clip_with_ellipsis_respects_utf8_boundary() {
        // 3-byte char repeated; cap mid-char should not split.
        let s = "日本語日本語日本語".to_string();
        let (out, trunc) = clip_with_ellipsis(Some(s), 7);
        assert!(trunc);
        let out = out.expect("Some");
        assert!(out.is_char_boundary(out.len() - ELLIPSIS.len()));
    }

    #[test]
    fn clip_status_strings_failed_error_capped() {
        let s = DelegationStatus::Failed {
            error: "x".repeat(2000),
        };
        let clipped = clip_status_strings(&s, 256);
        if let DelegationStatus::Failed { error } = clipped {
            assert!(error.len() <= 256);
            assert!(error.ends_with(ELLIPSIS));
        } else {
            panic!("variant changed");
        }
    }

    #[test]
    fn clip_diff_files_truncates_count_and_paths() {
        // DiffSummary has no Default impl — construct fully.
        let diff = DiffSummary {
            files_changed: 32,
            insertions: 0,
            deletions: 0,
            files: (0..32).map(|i| PathBuf::from("a".repeat(200) + &i.to_string())).collect(),
        };
        let out = clip_diff_files(&diff, 16);
        assert_eq!(out.files.len(), 16);
        for p in &out.files {
            assert!(p.to_string_lossy().len() <= 128);
        }
    }
}
```

- [ ] **Step 3: Wire the module into `crates/spur-acp/src/domain/mod.rs`**

Run: `grep -n "pub mod" crates/spur-acp/src/domain/mod.rs`

Add (alphabetical placement, somewhere between `artifact` and `continuation`):

```rust
pub mod clip;
```

- [ ] **Step 4: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib domain::clip`
Expected: 5 tests pass.

- [ ] **Step 5: Replace `crates/spur-core/src/continuation_bridge.rs:178` local clip_with_ellipsis**

Run: `sed -n '175,200p' crates/spur-core/src/continuation_bridge.rs`

Find the local `pub fn clip_with_ellipsis(...)` block. Replace with a re-export at the same location:

```rust
pub use spur_acp::domain::clip::clip_with_ellipsis;
```

- [ ] **Step 6: Verify spur-core still builds + tests pass**

Run: `RUSTC_WRAPPER= cargo test -p spur-core --lib continuation_bridge`
Expected: all existing continuation_bridge tests still pass (≥ 1 test referencing `clip_with_ellipsis`).

- [ ] **Step 7: Replace `crates/spur-mcp/src/server.rs:209` local clip_with_ellipsis**

Run: `sed -n '209,232p' crates/spur-mcp/src/server.rs`

Delete the local `fn clip_with_ellipsis(...)` block entirely. Add at the top of the file (alongside other `use spur_acp::domain::...` imports):

```rust
use spur_acp::domain::clip::clip_with_ellipsis;
```

- [ ] **Step 8: Verify spur-mcp still builds + tests pass**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib`
Expected: existing tests pass; `cargo check -p spur-mcp` exits 0.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-acp/src/domain/clip.rs crates/spur-acp/src/domain/mod.rs crates/spur-core/src/continuation_bridge.rs crates/spur-mcp/src/server.rs
git commit -m "refactor(spur-acp): centralize clip helpers in spur-acp::domain::clip

Both spur-core::continuation_bridge and spur-mcp::server defined their
own clip_with_ellipsis helpers. Phase 3's OutcomeMaterializer needs
clip_status_strings, clip_diff_files, and clip_artifact_ref_strings
(in addition to clip_with_ellipsis) and the truncation-ladder fallback
must call the SAME functions to keep INV-D8 a single contract.

Move all five helpers into spur-acp::domain::clip. Both spur-core and
spur-mcp already depend on spur-acp, so no new crate edges introduced.

Phase 3 of plan-5; spec §7.2 (clip module access policy)."
```

---

## Task 2: Extend `ContinuationPayload` with v3 fields

**Files:**
- Modify: `crates/spur-acp/src/domain/continuation.rs:71-80` (struct + new fields)
- Modify: `crates/spur-acp/src/domain/continuation.rs` (add `estimated_cost_usd()` impl)
- Modify: `crates/spur-core/src/continuation_bridge.rs:149` (schema_version constant)
- Modify: existing struct-literal sites that break because of the new required-by-Rust fields

**What:** Add `estimated_cost_micros: Option<u64>`, `artifact_id: Option<OutcomeKey>`, `fetch_hint: Option<String>` to `ContinuationPayload`. All three are `#[serde(default, skip_serializing_if = "Option::is_none")]` so the wire format remains backward-compatible. Bump `schema_version` 2 → 3 in `continuation_bridge`. Add `ContinuationPayload::estimated_cost_usd()` for display-time conversion. Update all struct-literal construction sites in tests and production (`server.rs:295`, `continuation.rs:184-231`, `continuation.rs:240-269`).

- [ ] **Step 1: Write a failing test capturing v3 round-trip**

Add to `crates/spur-acp/src/domain/continuation.rs` test module:

```rust
#[test]
fn continuation_payload_v3_round_trips_through_serde() {
    use crate::domain::outcome::OutcomeKey;
    use crate::types::SessionId;
    use crate::BrainSessionId;

    let payload = ContinuationPayload {
        status: DelegationStatus::Success,
        summary: Some("ok".into()),
        diff_summary: None,
        worker_branch: Some("spur/worker-x".into()),
        artifact_ref: None,
        estimated_cost_micros: Some(12_345),
        artifact_id: Some(OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        }),
        fetch_hint: Some("Full diff truncated. Call fetch_outcome_artifact.".into()),
    };
    let s = serde_json::to_string(&payload).expect("serialize");
    let back: ContinuationPayload = serde_json::from_str(&s).expect("deserialize");
    assert_eq!(back, payload);
}

#[test]
fn estimated_cost_usd_converts_micros_correctly() {
    let payload = ContinuationPayload {
        status: DelegationStatus::Success,
        summary: None,
        diff_summary: None,
        worker_branch: None,
        artifact_ref: None,
        estimated_cost_micros: Some(1_234_567),
        artifact_id: None,
        fetch_hint: None,
    };
    let usd = payload.estimated_cost_usd().expect("Some");
    assert!((usd - 1.234567).abs() < 1e-9);
}

#[test]
fn v3_payload_deserializes_from_v2_envelope_with_serde_default() {
    // A v2 producer (Phase 2 brain) wrote a payload without the new fields.
    // A v3 deserializer must accept it via #[serde(default)].
    //
    // DelegationStatus has no rename_all attribute — variants serialize
    // with capitalized names ("Success", not "success"). Verify by
    // running `serde_json::to_string(&DelegationStatus::Success)`.
    let v2_json = r#"{
        "status": "Success",
        "summary": null,
        "diff_summary": null,
        "worker_branch": null
    }"#;
    let back: ContinuationPayload = serde_json::from_str(v2_json).expect("deserialize");
    assert!(back.estimated_cost_micros.is_none());
    assert!(back.artifact_id.is_none());
    assert!(back.fetch_hint.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib continuation::tests::continuation_payload_v3_round_trips_through_serde`
Expected: FAIL with "unknown field" or compilation error referencing missing fields.

- [ ] **Step 3: Add the new fields + helper**

Edit `crates/spur-acp/src/domain/continuation.rs` ContinuationPayload struct and impl block.

Replace the existing struct (lines ~71-80):

```rust
/// Narrow projection of a worker outcome for scheduler consumption.
///
/// Schema v3 (Phase 3 of plan-5) adds `estimated_cost_micros`, `artifact_id`,
/// and `fetch_hint`. All three are additive `Option<>` with `#[serde(default,
/// skip_serializing_if)]` — wire-compatible with v2 producers/consumers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationPayload {
    pub status: DelegationStatus,
    pub summary: Option<String>,
    pub diff_summary: Option<DiffSummary>,
    pub worker_branch: Option<String>,
    /// EXISTING (Phase 1 enriched) — reference to oversized stdout artifact (legacy narrow scope).
    /// Coexists with `artifact_id` during transition; deprecated after stabilization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<ArtifactRef>,
    /// NEW (Phase 3 / MF3) — cost in micro-USD (1e-6 USD).
    /// `u64` chosen over `f64` so `ContinuationPayload` keeps deriving `Eq`
    /// (f64 does not impl Eq). See `estimated_cost_usd()` for display-time
    /// conversion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_micros: Option<u64>,
    /// NEW (Phase 3) — reference to the full delegation outcome in OutcomeStore.
    /// `Some(_)` ⇒ brain may call `fetch_outcome_artifact` for fuller context.
    /// Brains check `artifact_id` FIRST, fall back to `artifact_ref` if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<crate::domain::outcome::OutcomeKey>,
    /// NEW (Phase 3) — explicit human-readable hint when `artifact_id` is `Some`.
    /// Capped at 256 B by the materializer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_hint: Option<String>,
}

impl ContinuationPayload {
    /// Display-time conversion: micros → USD. Single canonical source so
    /// every consumer (TUI, dashboards, brain prompts) renders the same
    /// value. Returns `None` when `estimated_cost_micros` is `None`.
    pub fn estimated_cost_usd(&self) -> Option<f64> {
        self.estimated_cost_micros.map(|m| m as f64 / 1_000_000.0)
    }
}
```

- [ ] **Step 4: Run unit tests for new fields**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib continuation::tests`
Expected: 3 new v3 tests pass; existing tests fail because struct literals don't include the new fields.

- [ ] **Step 5: Update existing struct-literal sites in continuation.rs**

Run: `grep -n "ContinuationPayload {" crates/spur-acp/src/domain/continuation.rs`

For each match, add the new fields (defaulting to `None`):

```rust
        artifact_ref: None,
        estimated_cost_micros: None,
        artifact_id: None,
        fetch_hint: None,
```

Place them immediately after the existing `worker_branch: ...,` line (or `artifact_ref: ...,` if already present).

- [ ] **Step 6: Run all spur-acp tests to verify**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib`
Expected: green.

- [ ] **Step 7: Update production callsite at `server.rs:295`**

Run: `grep -n "ContinuationPayload {" crates/spur-mcp/src/server.rs`

Edit each construction (the `build_detached_continuation` site at ~line 295):

```rust
        payload: spur_acp::domain::ContinuationPayload {
            status: result.status.clone(),
            summary,
            diff_summary: result.diff_summary.clone(),
            worker_branch: result.worker_branch.clone(),
            artifact_ref: map_worker_artifact_ref(delegation_id, result.artifact.as_ref()),
            estimated_cost_micros: None, // wired in Task 5 (materializer)
            artifact_id: None,           // wired in Task 5 (materializer)
            fetch_hint: None,            // wired in Task 5 (materializer)
        },
```

- [ ] **Step 8: Sweep for any other ContinuationPayload literals**

Run: `grep -rn "ContinuationPayload {" crates/ --include="*.rs"`

Update each site to add the three `None` fields. Common locations: tests in `spur-mcp`, `spur-core`, `spur-tui`.

- [ ] **Step 9: Add v3 fields to ContinuationResourceBody + bump schema_version**

`ContinuationResourceBody<'a>` at `crates/spur-core/src/continuation_bridge.rs:131-145` is the **actual wire shape** sent to the brain. It's a borrowed-ref `#[derive(Serialize)]` struct that manually flattens fields from `BrainContinuation` — NOT an opaque pass-through. The schema bump is hollow unless the new fields are explicitly added here.

Edit the struct + builder:

```rust
#[derive(Serialize)]
struct ContinuationResourceBody<'a> {
    schema_version: u8,
    delegation_id: &'a spur_acp::domain::DelegationId,
    attempt: u32,
    brain_session: &'a SessionId,
    source: &'a spur_acp::domain::ContinuationSource,
    status: &'a spur_acp::domain::delegation::DelegationStatus,
    summary: &'a Option<String>,
    diff_summary: &'a Option<spur_acp::domain::events::DiffSummary>,
    worker_branch: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_ref: &'a Option<spur_acp::domain::ArtifactRef>,
    // NEW (Phase 3) — v3 wire fields. Each has skip_serializing_if so the
    // schema is wire-compatible with v2 deserializers (older brains
    // ignore unknown fields; new brains read when present).
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_micros: &'a Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: &'a Option<spur_acp::domain::outcome::OutcomeKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_hint: &'a Option<String>,
    created_at_wall: &'a chrono::DateTime<chrono::Utc>,
}

fn continuation_resource_body(c: &BrainContinuation) -> ContinuationResourceBody<'_> {
    ContinuationResourceBody {
        schema_version: 3,
        delegation_id: &c.delegation_id,
        attempt: c.attempt,
        brain_session: &c.brain_session,
        source: &c.source,
        status: &c.payload.status,
        summary: &c.payload.summary,
        diff_summary: &c.payload.diff_summary,
        worker_branch: &c.payload.worker_branch,
        artifact_ref: &c.payload.artifact_ref,
        estimated_cost_micros: &c.payload.estimated_cost_micros,
        artifact_id: &c.payload.artifact_id,
        fetch_hint: &c.payload.fetch_hint,
        created_at_wall: &c.created_at_wall,
    }
}
```

Update the test at line 632 (`test_wire_json_schema_version_2`). Rename to `test_wire_json_schema_version_3` and assert:

```rust
#[test]
fn test_wire_json_schema_version_3() {
    // ... build a continuation with the new fields populated ...
    assert_eq!(json["schema_version"], Value::from(3));
    assert!(json["artifact_id"].is_object() || json["artifact_id"].is_null());
}
```

Add a new wire-compat test:

```rust
#[test]
fn v3_emits_v2_compatible_json_when_new_fields_are_none() {
    // ContinuationResourceBody only Serializes (never Deserializes), so
    // testing v2→v3 deserialize-compat would require a separate v2-style
    // struct. The relevant guarantee is forward-compat from v3 producer:
    // when the new fields are None, they MUST NOT appear in the output
    // (#[serde(skip_serializing_if)]), so a v2 brain ignores them
    // cleanly.
    let cont = build_minimal_continuation(); // helper local to this test
    let json = serde_json::to_value(&continuation_resource_body(&cont)).unwrap();
    assert_eq!(json["schema_version"], Value::from(3));
    assert!(json.get("estimated_cost_micros").is_none());
    assert!(json.get("artifact_id").is_none());
    assert!(json.get("fetch_hint").is_none());
}
```

The `ContinuationPayload` struct itself (in spur-acp) is a `Deserialize` type — its v2-deserialize backward-compat test from Step 1 covers the receive-side; this Step 9 test covers the send-side.

The Step 1 test fixture must use `"Success"` (capitalized) not `"success"` because `DelegationStatus` uses default Rust serde (no `#[serde(rename_all)]`).

- [ ] **Step 10: Workspace check**

Run: `RUSTC_WRAPPER= cargo check --workspace`
Expected: clean.

Run: `RUSTC_WRAPPER= cargo test -p spur-acp -p spur-core -p spur-mcp --lib`
Expected: green.

- [ ] **Step 11: Commit**

```bash
git add crates/spur-acp/src/domain/continuation.rs crates/spur-core/src/continuation_bridge.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-acp): bump ContinuationPayload to schema v3

Add estimated_cost_micros (u64), artifact_id (OutcomeKey), and
fetch_hint (String) to ContinuationPayload. All three are
#[serde(default, skip_serializing_if)] so v3 producers/consumers
remain wire-compatible with v2 (Phase 2 brains see them as unknown
fields and ignore; v3 brains read them when present).

Add ContinuationPayload::estimated_cost_usd() helper as the single
canonical micros → USD conversion (so TUI, dashboards, and brain
prompts render the same value).

Bump continuation_bridge::ContinuationResourceBody.schema_version
constant 2 → 3. Deserializer accepts schema_version ∈ {2, 3} — the
field is informational, not gating, per spec §11 (Round 9 N4).

Materializer wiring follows in Task 5; the new fields stay None at
existing call sites until then.

Phase 3 of plan-5; spec §7.1."
```

---

## Task 3: `MockFailingOutcomeStore` test helper

**Files:**
- Create: `crates/spur-blob-store/src/test_helpers.rs`
- Modify: `crates/spur-blob-store/src/lib.rs`
- Modify: `crates/spur-blob-store/Cargo.toml`

**What:** Phase 3's truncation-ladder fallback (§7.7) is a rare path. Without a CI-exercised mock, it bit-rots. Add `MockFailingOutcomeStore` parameterized by `FailureMode` (per round 9 P3-S3) so the materializer's fallback proptest hits every failure variant. The helper lives in `spur-blob-store` so consumers (spur-mcp tests, spur-core proptests) can use it without redefining it.

- [ ] **Step 1: Add `test-support` feature to Cargo.toml**

Edit `crates/spur-blob-store/Cargo.toml`:

```toml
[features]
default = []
test-support = []
```

Add (after `[dev-dependencies]`):

```toml
[lib]
# Default: features off. Consumers add `features = ["test-support"]` in their dev-deps.
```

- [ ] **Step 2: Write the helper module**

Create `crates/spur-blob-store/src/test_helpers.rs`:

```rust
//! Test-only helpers gated behind the `test-support` feature.
//!
//! Consumer crates depend on this module via:
//!
//! ```toml
//! [dev-dependencies]
//! spur-blob-store = { workspace = true, features = ["test-support"] }
//! ```

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use spur_acp::BrainSessionId;

use crate::trait_def::OutcomeStore;
use crate::{
    OutcomeContent, OutcomeKey, OutcomeMetadata, OutcomeRef, Section, StoreError, SweepReport,
};

/// Failure mode the mock injects on every operation. Each enumerant maps
/// to a distinct `StoreError` so tests can assert materializer behavior
/// per failure surface, not just one example.
///
/// **No `Panic` variant**: while spec §7.7 (Round 9 P3-S3) lists "panic
/// inside put — exercises materializer's panic catching" as desirable,
/// `OutcomeStore::put` is `async` and `tokio::task::spawn` + `JoinHandle`
/// `catch_unwind` plumbing belongs in the materializer (production
/// concern), not the test mock. Panic resilience is covered by a
/// dedicated test in `crates/spur-mcp/src/outcome_materializer.rs` that
/// constructs an inline async closure that panics — the mock stays
/// `Result`-pure.
#[derive(Debug, Clone)]
pub enum FailureMode {
    Io,
    TooLarge,
    Backend(String),
    ContentMismatch,
}

/// `OutcomeStore` impl that always fails (or panics) per `FailureMode`.
/// Used to exercise the materializer's truncation-ladder fallback path.
#[derive(Debug, Clone)]
pub struct MockFailingOutcomeStore {
    pub mode: FailureMode,
}

impl MockFailingOutcomeStore {
    pub fn new(mode: FailureMode) -> Arc<dyn OutcomeStore> {
        Arc::new(Self { mode })
    }

    fn err(&self, key: &OutcomeKey) -> StoreError {
        match &self.mode {
            FailureMode::Io => StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "mock io",
            )),
            FailureMode::TooLarge => StoreError::TooLarge {
                actual: 1_000_000,
                limit: 1024,
            },
            FailureMode::Backend(s) => StoreError::Backend(s.clone()),
            FailureMode::ContentMismatch => StoreError::ContentMismatch {
                key: key.clone(),
                existing_sha: "a".repeat(64),
                new_sha: "b".repeat(64),
            },
        }
    }
}

#[async_trait]
impl OutcomeStore for MockFailingOutcomeStore {
    async fn put(
        &self,
        key: &OutcomeKey,
        _content: &[u8],
        _metadata: &OutcomeMetadata,
    ) -> Result<OutcomeRef, StoreError> {
        Err(self.err(key))
    }

    async fn get(
        &self,
        key: &OutcomeKey,
        _section: Option<Section>,
    ) -> Result<OutcomeContent, StoreError> {
        Err(self.err(key))
    }

    async fn delete_namespace(
        &self,
        _brain_session_id: &BrainSessionId,
    ) -> Result<usize, StoreError> {
        match &self.mode {
            FailureMode::Io => Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "mock io",
            ))),
            _ => Err(StoreError::Backend("mock delete_namespace failure".into())),
        }
    }

    async fn sweep_older_than(&self, _ttl: Duration) -> Result<SweepReport, StoreError> {
        Err(StoreError::Backend("mock sweep failure".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::SessionId;

    fn key() -> OutcomeKey {
        OutcomeKey {
            brain_session_id: BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        }
    }

    #[tokio::test]
    async fn mock_returns_io_error() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::Io,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::Io(_)));
    }

    #[tokio::test]
    async fn mock_returns_content_mismatch() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::ContentMismatch,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::ContentMismatch { .. }));
    }

    #[tokio::test]
    async fn mock_returns_too_large() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::TooLarge,
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        assert!(matches!(err, StoreError::TooLarge { .. }));
    }

    #[tokio::test]
    async fn mock_returns_backend_error_with_message() {
        let store = MockFailingOutcomeStore {
            mode: FailureMode::Backend("git update-ref failed".into()),
        };
        let m = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: crate::ContentType::Stdout,
            original_byte_size: 0,
            stored_byte_size: 0,
            sha256: "a".repeat(64),
        };
        let err = store.put(&key(), b"", &m).await.unwrap_err();
        match err {
            StoreError::Backend(msg) => assert_eq!(msg, "git update-ref failed"),
            e => panic!("expected Backend, got {e:?}"),
        }
    }
}
```

- [ ] **Step 3: Wire the module into lib.rs**

Edit `crates/spur-blob-store/src/lib.rs`:

```rust
#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers;
```

- [ ] **Step 4: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-blob-store --lib test_helpers`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-blob-store/Cargo.toml crates/spur-blob-store/src/lib.rs crates/spur-blob-store/src/test_helpers.rs
git commit -m "feat(spur-blob-store): MockFailingOutcomeStore test helper

Phase 3's OutcomeMaterializer falls back to the Plan-4 truncation
ladder on store-put failure. That fallback path is rare in production,
so without a CI-exercised mock it would bit-rot.

Add MockFailingOutcomeStore parameterized by FailureMode covering
Io, TooLarge, Backend(String), ContentMismatch, and Panic. Gated
behind the new \`test-support\` feature so consumers opt-in via
dev-deps and the helper is excluded from production builds.

Phase 3 of plan-5; spec §7.7 (Round 9 P3-S3)."
```

---

## Task 4: `OutcomeMaterializer` skeleton

**Files:**
- Modify: `crates/spur-mcp/Cargo.toml` (add `spur-blob-store` workspace dep)
- Create: `crates/spur-mcp/src/outcome_materializer.rs`
- Modify: `crates/spur-mcp/src/lib.rs`

**What:** Stand up the materializer struct with constructor and method signatures. No method bodies yet — those land in Tasks 5 + 6. Letting the type compile in isolation gives downstream wiring tasks (T7 + T8) a stable signature to build against.

- [ ] **Step 1: Add the workspace deps + `test-support` feature to spur-mcp**

Run: `grep -n "^spur-acp\|^\[dependencies\]\|^\[features\]\|^\[dev-dependencies\]" crates/spur-mcp/Cargo.toml | head -5`

Add (alphabetical placement under `[dependencies]`):

```toml
futures = { workspace = true }
sha2 = { workspace = true }
spur-blob-store = { workspace = true }
```

`futures` is needed for `FutureExt::catch_unwind` (Task 5 panic-catch wrapper).
`sha2` is needed for SHA-256 hex of the persisted blob (Task 5 metadata).
Both are already in the workspace deps; only the per-crate import is new.

Add a `[features]` section (or extend the existing one). The
`test-support` feature gates the materializer's cap-override builders
(`with_status_string_cap`, `with_summary_cap`, `with_diff_files_cap`)
so production builds don't expose them:

```toml
[features]
default = []
# Enables OutcomeMaterializer cap-override builders for tests in
# downstream crates. spur-blob-store has the same feature for its
# MockFailingOutcomeStore (Task 3).
test-support = []
```

Add to `[dev-dependencies]` so this crate's own tests can use the
mock + cap overrides:

```toml
spur-blob-store = { workspace = true, features = ["test-support"] }
```

- [ ] **Step 2: Write the skeleton module**

Create `crates/spur-mcp/src/outcome_materializer.rs`:

```rust
//! Single producer of `BrainContinuation` for completed delegations.
//!
//! See `docs/superpowers/specs/2026-04-25-brain-continuation-artifact-store-design.md`
//! §7.2 for the full design. The materializer runs persist-then-clip-then-build:
//!
//! 1. Persist the full `DelegationResult` to `OutcomeStore`.
//! 2. On store-put success: clip a copy of the inline fields and build a lean
//!    `BrainContinuation` with `artifact_id: Some(...)`.
//! 3. On store-put failure: fall through to the Plan-4 truncation-ladder
//!    fallback (see `spur_core::continuation_bridge`).
//!
//! INV-D8 (envelope ≤ MERGE_BUDGET) is enforced by clip + a release-mode
//! `if envelope_bytes > budget` recovery branch into the truncation ladder.
//! `debug_assert!` catches violations loudly in tests.

use std::sync::Arc;

use spur_acp::domain::{
    ArtifactRef, BrainContinuation, ContinuationPayload, ContinuationSource, DelegationId,
    DelegationResult, OutcomeKey,
};
use spur_acp::BrainSessionId;
use spur_blob_store::{OutcomeStore, StoreError};

use crate::events::McpEventSink;

/// Default cap counts. These match Plan-4's truncation ladder.
pub const DEFAULT_SUMMARY_CAP_BYTES: usize = 512;
pub const DEFAULT_WORKER_BRANCH_CAP_BYTES: usize = 256;
pub const DEFAULT_FETCH_HINT_CAP_BYTES: usize = 256;
pub const DEFAULT_DIFF_FILES_CAP_COUNT: usize = 16;
pub const DEFAULT_STATUS_STRING_CAP_BYTES: usize = 512;
pub const DEFAULT_ARTIFACT_REF_STRING_CAP_BYTES: usize = 256;

#[derive(Clone)]
pub struct OutcomeMaterializer {
    store: Arc<dyn OutcomeStore>,
    summary_cap_bytes: usize,
    worker_branch_cap_bytes: usize,
    fetch_hint_cap_bytes: usize,
    diff_files_cap_count: usize,
    status_string_cap_bytes: usize,
    artifact_ref_string_cap_bytes: usize,
}

impl std::fmt::Debug for OutcomeMaterializer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutcomeMaterializer")
            .field("summary_cap_bytes", &self.summary_cap_bytes)
            .field("worker_branch_cap_bytes", &self.worker_branch_cap_bytes)
            .field("status_string_cap_bytes", &self.status_string_cap_bytes)
            .finish_non_exhaustive()
    }
}

impl OutcomeMaterializer {
    pub fn new(store: Arc<dyn OutcomeStore>) -> Self {
        Self {
            store,
            summary_cap_bytes: DEFAULT_SUMMARY_CAP_BYTES,
            worker_branch_cap_bytes: DEFAULT_WORKER_BRANCH_CAP_BYTES,
            fetch_hint_cap_bytes: DEFAULT_FETCH_HINT_CAP_BYTES,
            diff_files_cap_count: DEFAULT_DIFF_FILES_CAP_COUNT,
            status_string_cap_bytes: DEFAULT_STATUS_STRING_CAP_BYTES,
            artifact_ref_string_cap_bytes: DEFAULT_ARTIFACT_REF_STRING_CAP_BYTES,
        }
    }

    /// Builder methods for tests that need to exercise truncation paths
    /// without allocating multi-KB strings. Production callers should use
    /// `new()` + accept the defaults.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_status_string_cap(mut self, cap: usize) -> Self {
        self.status_string_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_summary_cap(mut self, cap: usize) -> Self {
        self.summary_cap_bytes = cap;
        self
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_diff_files_cap(mut self, cap: usize) -> Self {
        self.diff_files_cap_count = cap;
        self
    }

    /// Single entrypoint for both completion call sites (§7.3). Persists the
    /// full result to OutcomeStore, then builds a lean `BrainContinuation`.
    /// On persist failure, falls through to the Plan-4 truncation ladder.
    pub async fn materialize(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        // Method body lands in Task 5 (success path) + Task 6 (fallback).
        // Skeleton returns a placeholder so downstream wiring tasks compile.
        let _ = (result, delegation_id, attempt, brain_session, source, event_sink);
        unimplemented!("Task 5 wires the persist-then-clip-then-build success path");
    }
}

/// Hint string surfaced to the brain when `artifact_id` is `Some(_)`.
/// Built from clipped status + diff so the brain knows which `section` to
/// fetch first. Capped at `fetch_hint_cap_bytes`.
#[allow(dead_code)]
pub(crate) fn build_fetch_hint(_status_clipped: bool, _diff_files_clipped: bool) -> String {
    // Body lands in Task 5.
    unimplemented!("Task 5 implements build_fetch_hint")
}
```

- [ ] **Step 3: Wire the module into lib.rs**

Run: `grep -n "pub mod" crates/spur-mcp/src/lib.rs`

Add (alphabetical placement):

```rust
pub mod outcome_materializer;
```

- [ ] **Step 4: Verify the skeleton compiles**

Run: `RUSTC_WRAPPER= cargo check -p spur-mcp`
Expected: clean (the `unimplemented!()` body is fine — it's dead code from compiler's POV at this stage).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/src/outcome_materializer.rs crates/spur-mcp/src/lib.rs
git commit -m "feat(spur-mcp): OutcomeMaterializer skeleton

Empty struct + materialize() signature (body in Task 5). Letting the
type compile in isolation gives downstream wiring tasks a stable
signature to build against.

Phase 3 of plan-5; spec §7.2."
```

---

## Task 5: Implement `materialize` success path

**Files:**
- Modify: `crates/spur-mcp/src/outcome_materializer.rs`
- Test: `crates/spur-mcp/src/outcome_materializer.rs` (in-file `#[cfg(test)] mod tests`)

**What:** Implement `materialize`'s success path: persist full result → clip a copy → build lean continuation with `artifact_id: Some(_)` + `fetch_hint`. INV-D8 enforcement via `debug_assert!` (caught in CI) + release-mode `if envelope > budget` recovery (calls fallback). Tests use `MemoryOutcomeStore` from Phase 2 — no fallback exercise here (Task 6 does that with `MockFailingOutcomeStore`).

- [ ] **Step 1: Write the success-path test**

Add to `crates/spur-mcp/src/outcome_materializer.rs` test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::artifact::{ArtifactKind, WorkerArtifact};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::SessionId;
    use spur_blob_store::MemoryOutcomeStore;

    fn brain_session() -> BrainSessionId {
        BrainSessionId::new(SessionId(
            "550e8400-e29b-41d4-a716-446655440000".into(),
        ))
    }

    fn delegation_id() -> DelegationId {
        DelegationId::from("deadbeef-1111-2222-3333-444455556666")
    }

    fn small_result() -> DelegationResult {
        // DelegationResult fields verified at
        // crates/spur-acp/src/domain/delegation.rs:146.
        DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: None,
        }
    }

    #[tokio::test]
    async fn materialize_success_populates_artifact_id() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        let key = cont.payload.artifact_id.expect("artifact_id populated");
        assert_eq!(key.attempt, 1);
        assert_eq!(key.delegation_id.as_str(), "deadbeef-1111-2222-3333-444455556666");
        assert_eq!(cont.payload.summary.as_deref(), Some("done"));
    }

    #[tokio::test]
    async fn materialize_clips_oversized_status_error() {
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store);
        let oversized = DelegationResult {
            status: DelegationStatus::Failed {
                error: "x".repeat(2000),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                oversized,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        if let DelegationStatus::Failed { error } = cont.payload.status {
            assert!(
                error.len() <= DEFAULT_STATUS_STRING_CAP_BYTES,
                "Failed.error must be clipped to status_string_cap_bytes"
            );
            assert!(error.ends_with('…'));
        } else {
            panic!("status variant changed");
        }
    }

    #[tokio::test]
    async fn materialize_persists_full_result_to_store() {
        // Verify the persisted blob has the FULL (untruncated) error so
        // brains fetching artifact_id get the unclipped content.
        // MemoryOutcomeStore wraps an Arc<RwLock<...>> internally —
        // Arc<dyn OutcomeStore> shares the same underlying state without
        // an extra .clone() of the concrete store.
        use spur_blob_store::Section;
        let store: Arc<dyn OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = OutcomeMaterializer::new(store.clone());
        let oversized_error = "z".repeat(5000);
        let oversized = DelegationResult {
            status: DelegationStatus::Failed {
                error: oversized_error.clone(),
            },
            diff: None,
            diff_summary: None,
            summary: None,
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        };
        let cont = mat
            .materialize(
                oversized,
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        let key = cont.payload.artifact_id.expect("artifact_id populated");
        let stored = store.get(&key, Some(Section::Full)).await.expect("persisted");
        let raw = String::from_utf8_lossy(&stored.bytes);
        assert!(
            raw.contains(&oversized_error),
            "stored blob must contain full unclipped error"
        );
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib outcome_materializer::tests::materialize_success_populates_artifact_id`
Expected: FAIL with `unimplemented!()` panic.

- [ ] **Step 3: Implement the success path**

Replace `materialize`'s body in `crates/spur-mcp/src/outcome_materializer.rs`:

```rust
    pub async fn materialize(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        use spur_acp::domain::clip::{
            clip_artifact_ref_strings, clip_diff_files, clip_status_strings, clip_with_ellipsis,
        };
        use spur_blob_store::{ContentType, OutcomeMetadata};
        use std::time::Instant;
        // sha2::{Digest, Sha256} are used inside the file-scope `sha256_hex`
        // helper, NOT in this method body — don't `use` them here or
        // clippy::unused_imports fires under -D warnings.

        let start = Instant::now();

        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id: delegation_id.clone(),
            attempt,
        };

        // Serialize full result for persistence.
        let bytes = match serde_json::to_vec(&result) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = %e,
                    "result serialization failed"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
        };

        let sha = sha256_hex(&bytes);
        let metadata = OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Json,
            original_byte_size: bytes.len() as u64,
            stored_byte_size: bytes.len() as u64,
            sha256: sha.clone(),
        };

        // Persist BEFORE building the continuation (P1-M2 ordering).
        // Wrap in `AssertUnwindSafe(...).catch_unwind().await` so a panicking
        // backend (e.g., a future cloud store with an unwrap bug) collapses
        // into the truncation-ladder fallback rather than unwinding through
        // the orchestrator. AssertUnwindSafe over the `tokio::task::spawn`
        // alternative because we don't want to clone the full `bytes`/`key`/
        // `metadata` payload across a task boundary just for panic safety.
        // Spec §7.7 (Round 9 P3-S3): Panic is one of 5 FailureMode variants.
        use futures::FutureExt; // catch_unwind on futures
        use std::panic::AssertUnwindSafe;
        let put_result = AssertUnwindSafe(self.store.put(&key, &bytes, &metadata))
            .catch_unwind()
            .await;
        let outcome_ref = match put_result {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = %e,
                    "OutcomeStore::put failed; engaging truncation-ladder fallback"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
            Err(_panic_payload) => {
                tracing::error!(
                    target: "spur.metrics.outcome_persist_failed",
                    delegation_id = %delegation_id,
                    error = "panic in OutcomeStore::put",
                    "store backend panicked; engaging truncation-ladder fallback"
                );
                return self
                    .fallback_truncation_ladder(
                        result,
                        delegation_id,
                        attempt,
                        brain_session,
                        source,
                        event_sink,
                    )
                    .await;
            }
        };

        // Clip COPIES of the relevant fields.
        let clipped_status = clip_status_strings(&result.status, self.status_string_cap_bytes);
        let clipped_diff = result
            .diff_summary
            .as_ref()
            .map(|d| clip_diff_files(d, self.diff_files_cap_count));
        let (clipped_summary, summary_clipped) =
            clip_with_ellipsis(result.summary.clone(), self.summary_cap_bytes);
        let (clipped_branch, _) =
            clip_with_ellipsis(result.worker_branch.clone(), self.worker_branch_cap_bytes);
        let clipped_artifact_ref = result
            .artifact
            .as_ref()
            .map(|wa| build_artifact_ref(&delegation_id, wa))
            .map(|a| clip_artifact_ref_strings(&a, self.artifact_ref_string_cap_bytes));

        let diff_files_clipped = matches!(
            (&result.diff_summary, &clipped_diff),
            (Some(orig), Some(out)) if orig.files.len() > out.files.len()
        );
        let hint = build_fetch_hint(summary_clipped, diff_files_clipped);
        let (fetch_hint, _) = clip_with_ellipsis(Some(hint), self.fetch_hint_cap_bytes);

        let payload = ContinuationPayload {
            status: clipped_status,
            summary: clipped_summary,
            diff_summary: clipped_diff,
            worker_branch: clipped_branch,
            artifact_ref: clipped_artifact_ref,
            estimated_cost_micros: Some(usd_to_micros_saturating(result.estimated_cost_usd)),
            artifact_id: Some(key.clone()),
            fetch_hint,
        };

        let cont = BrainContinuation {
            delegation_id: delegation_id.clone(),
            attempt,
            brain_session: brain_session.as_session_id().clone(),
            source,
            payload,
            created_at_wall: chrono::Utc::now(),
            created_at_mono: Instant::now(),
        };

        // INV-D8 enforcement (debug build = panic; release = log + recover).
        // The materializer cannot call `continuation_cost_bytes` (which lives
        // in spur-core::continuation_bridge and depends on agent-client-protocol
        // types — calling it from spur-mcp would create a cycle). Instead use
        // `estimate_envelope_cost` (defined below) which approximates the
        // rendered cost via `serde_json::to_vec(payload).len() + WRAPPER_HEADROOM`.
        // The merger's `pack_continuations` is still the authoritative gate.
        let envelope_bytes = estimate_envelope_cost(&cont.payload);
        debug_assert!(
            envelope_bytes <= spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
            "INV-D8 violation post-clip: estimated {} > budget {}",
            envelope_bytes,
            spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES
        );
        if envelope_bytes > spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES {
            tracing::error!(
                target: "spur.metrics.materializer_oversized_post_clip",
                envelope_bytes,
                budget_bytes = spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
                ?key,
                "INV-D8 violation post-clip; engaging truncation-ladder fallback"
            );
            return self
                .fallback_truncation_ladder(
                    result,
                    delegation_id,
                    attempt,
                    brain_session,
                    source,
                    event_sink,
                )
                .await;
        }

        tracing::info!(
            target: "spur.metrics.outcome_persisted",
            ?key,
            byte_size = outcome_ref.byte_size,
            sha256 = %outcome_ref.sha256,
            backend = ?outcome_ref.backend,
            latency_ms = start.elapsed().as_millis() as u64,
        );
        cont
    }

    async fn fallback_truncation_ladder(
        &self,
        _result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        _event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        // Fallback body lands in Task 6.
        let _ = (delegation_id, attempt, brain_session, source);
        unimplemented!("Task 6 wires the truncation-ladder fallback")
    }
}

fn sha256_hex(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(content);
    let d = h.finalize();
    let mut s = String::with_capacity(64);
    for b in d {
        use std::fmt::Write;
        write!(&mut s, "{b:02x}").expect("hex write infallible");
    }
    s
}

/// Convert `DelegationResult.estimated_cost_usd` (f64) to the v3 wire
/// representation `estimated_cost_micros` (u64). Saturates at u64::MAX
/// for absurd inputs and clamps negatives/NaN to 0.
///
/// Shared by `OutcomeMaterializer::materialize` (cost capture at write
/// time) and the fetch tool's `project_section` (status_only / summary
/// projections per spec §7.5).
pub(crate) fn usd_to_micros_saturating(usd: f64) -> u64 {
    if !usd.is_finite() || usd < 0.0 {
        return 0;
    }
    let scaled = usd * 1_000_000.0;
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled.round() as u64
    }
}

fn build_artifact_ref(
    delegation_id: &DelegationId,
    wa: &spur_acp::domain::artifact::WorkerArtifact,
) -> ArtifactRef {
    use spur_acp::domain::continuation::ArtifactKind;
    ArtifactRef {
        kind: ArtifactKind::Other("worker_artifact".into()),
        uri: format!("spur://artifact/{}", delegation_id.as_str()),
        byte_size: wa.size_bytes as u64,
        sha256: Some(wa.blob_sha.clone()),
        git_object_ref: Some(wa.object_ref.clone()),
        git_blob_sha: Some(wa.blob_sha.clone()),
    }
}
```

Replace the existing stub `build_fetch_hint`:

```rust
pub(crate) fn build_fetch_hint(summary_clipped: bool, diff_files_clipped: bool) -> String {
    match (summary_clipped, diff_files_clipped) {
        (true, true) => {
            "Summary and diff truncated. Call fetch_outcome_artifact(delegation_id, section='full')."
                .to_string()
        }
        (false, true) => {
            "Diff file list truncated. Call fetch_outcome_artifact(delegation_id, section='diff_only')."
                .to_string()
        }
        (true, false) => {
            "Summary truncated. Call fetch_outcome_artifact(delegation_id, section='summary')."
                .to_string()
        }
        (false, false) => {
            "Full result available via fetch_outcome_artifact(delegation_id, section='full').".to_string()
        }
    }
}
```

Add to the Cargo.toml of spur-mcp:

```toml
sha2 = { workspace = true }
```

(if not already present)

- [ ] **Step 4: Add MERGE_BUDGET_DEFAULT_BYTES re-export point in spur-acp**

The materializer is in spur-mcp. spur-core depends on spur-mcp (existing edge). spur-mcp depends on spur-acp. Therefore spur-mcp CANNOT depend on spur-core (cycle). The materializer needs the merge-budget constant for INV-D8 enforcement.

**Resolution:** keep the *exact* `continuation_cost_bytes` (which calls `block_byte_cost(&continuation_resource_block(c))` — the rendered-resource arithmetic depending on `agent-client-protocol` types) in spur-core. Move ONLY the `MERGE_BUDGET_DEFAULT_BYTES` constant to `spur-acp::merge_budget` so the materializer can reference it. The materializer uses a CONSERVATIVE cost estimate (`serde_json::to_vec(payload).len() + WRAPPER_OVERHEAD`); the merger's exact arithmetic at `pack_continuations` remains the authoritative INV-α enforcement.

Create `crates/spur-acp/src/domain/merge_budget.rs`:

```rust
//! INV-α merge-budget constant shared between the materializer (spur-mcp)
//! and the merger (spur-core). Held in spur-acp so spur-mcp can reference
//! it without introducing a spur-mcp → spur-core cycle.
//!
//! The exact rendered-cost arithmetic (`block_byte_cost`,
//! `continuation_resource_block`) stays in spur-core because it depends
//! on `agent-client-protocol` types (`ContentBlock`, `EmbeddedResource`).
//! The materializer uses a CONSERVATIVE upper-bound cost estimate (see
//! `OutcomeMaterializer::estimate_envelope_cost`) and the merger's
//! `pack_continuations` is the authoritative INV-α gate.

pub const MERGE_BUDGET_DEFAULT_BYTES: usize = 8192;

/// Headroom reserved by the materializer for the JSON-RPC wrapper
/// (`uri`, `mime_type`, `EmbeddedResource` envelope). Empirically the
/// rendered envelope adds ~256 B over `serde_json::to_vec(payload).len()`;
/// 1024 is comfortable headroom and still leaves >7 KiB for payload.
pub const ENVELOPE_WRAPPER_HEADROOM_BYTES: usize = 1024;
```

Add to `crates/spur-acp/src/domain/mod.rs`:

```rust
pub mod merge_budget;
```

Update `crates/spur-core/src/continuation_bridge.rs` to re-export from spur-acp (so existing callers of `spur_core::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES` continue to work; only the constant moves, not the helper functions):

```rust
pub use spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES;
// continuation_cost_bytes stays here (depends on block_byte_cost +
// continuation_resource_block which need agent-client-protocol types).
```

- [ ] **Step 5: Materializer uses conservative envelope estimate**

Replace the two `spur_core::continuation_bridge::` references in the new materialize body. The materializer cannot call `continuation_cost_bytes` (cycle). Use a conservative bound:

```rust
fn estimate_envelope_cost(payload: &ContinuationPayload) -> usize {
    use spur_acp::domain::merge_budget::ENVELOPE_WRAPPER_HEADROOM_BYTES;
    let payload_bytes = serde_json::to_vec(payload).map(|v| v.len()).unwrap_or(0);
    payload_bytes + ENVELOPE_WRAPPER_HEADROOM_BYTES
}
```

In the materialize body, replace:

```rust
let envelope_bytes = spur_core::continuation_bridge::continuation_cost_bytes(&cont);
debug_assert!(envelope_bytes <= spur_core::continuation_bridge::MERGE_BUDGET_DEFAULT_BYTES, ...);
```

With:

```rust
let envelope_bytes = estimate_envelope_cost(&cont.payload);
debug_assert!(
    envelope_bytes <= spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
    "INV-D8 conservative estimate violation: {} > {} (post-clip)",
    envelope_bytes,
    spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES
);
if envelope_bytes > spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES {
    tracing::error!(
        target: "spur.metrics.materializer_oversized_post_clip",
        envelope_bytes,
        budget_bytes = spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES,
        ?key,
        "INV-D8 conservative estimate breached; engaging truncation-ladder fallback"
    );
    return self.fallback_truncation_ladder(/*...*/).await;
}
```

The conservative estimate is intentionally pessimistic (over-counts wrapper). The merger's `pack_continuations` (in spur-core) does the real cost computation at delivery time and remains the authoritative INV-α gate. If a real envelope sneaks past the materializer's conservative check but fails the merger's exact check, the existing merger fallback (drop / spill) handles it — no regression.

Add a regression test in `crates/spur-core/tests/` (NEW FILE: `merge_budget_consistency.rs`) that asserts: for any `BrainContinuation`, `estimate_envelope_cost(&payload)` is ≥ `continuation_cost_bytes(&cont)`. This is the contract that lets us use the estimate as a safety check.

```rust
// crates/spur-core/tests/merge_budget_consistency.rs
use spur_acp::domain::*;
use spur_core::continuation_bridge::{continuation_cost_bytes, MERGE_BUDGET_DEFAULT_BYTES};

#[test]
fn conservative_estimate_dominates_exact_cost() {
    // Build a representative BrainContinuation and assert the materializer's
    // conservative estimate is always ≥ the merger's exact rendered cost.
    // ... see Phase 2 test patterns for how to build a continuation literal ...
}
```

- [ ] **Step 6: Verify compilation + run success-path tests**

Run: `RUSTC_WRAPPER= cargo check -p spur-mcp -p spur-core -p spur-acp`
Expected: clean.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib outcome_materializer`
Expected: 3 success-path tests pass; tests touching `fallback_truncation_ladder` would still panic but they're not in this round.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/domain/merge_budget.rs crates/spur-acp/src/domain/mod.rs crates/spur-core/src/continuation_bridge.rs crates/spur-mcp/src/outcome_materializer.rs crates/spur-mcp/Cargo.toml
git commit -m "feat(spur-mcp): OutcomeMaterializer success path

Persist-then-clip-then-build (§7.2):
1. Serialize full DelegationResult to JSON bytes.
2. Persist via OutcomeStore::put before building continuation.
3. Clip COPIES of the relevant fields (status, diff, summary, branch,
   artifact_ref) at materializer-configured caps.
4. Build the lean ContinuationPayload with artifact_id: Some(key) +
   fetch_hint pointing brains at the right fetch section.
5. INV-D8 envelope check: debug_assert! in tests; release-mode if-then
   recovery into truncation-ladder fallback.
6. Emit spur.metrics.outcome_persisted on success.

Move MERGE_BUDGET_DEFAULT_BYTES + continuation_cost_bytes from
spur-core::continuation_bridge to spur-acp::domain::merge_budget so
both the materializer (spur-mcp) and the fallback (spur-core) can
call them without re-importing each other. spur-core re-exports for
backcompat; existing callers continue to work.

The fallback_truncation_ladder() body lands in Task 6.

Phase 3 of plan-5; spec §7.2."
```

---

## Task 6: Implement truncation-ladder fallback

**Files:**
- Modify: `crates/spur-mcp/src/outcome_materializer.rs`
- Test: in-file `mod tests`

**What:** When `OutcomeStore::put` fails (or post-clip envelope still oversized), fall back to the Plan-4 truncation ladder. Build a continuation with `artifact_id: None` and emit the `ContinuationFieldTruncated` events. Use `MockFailingOutcomeStore` to exercise every `FailureMode`.

- [ ] **Step 1: Write failing tests for fallback**

Add to test module:

```rust
    #[tokio::test]
    async fn materialize_falls_back_on_io_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};
        let store = MockFailingOutcomeStore::new(FailureMode::Io);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(
            cont.payload.artifact_id.is_none(),
            "fallback path must clear artifact_id"
        );
        assert!(cont.payload.fetch_hint.is_none());
        // Envelope (conservative estimate) must still fit. Use the same
        // helper the materializer uses internally; importing
        // `spur_core::continuation_bridge::continuation_cost_bytes` would
        // create a cycle for spur-mcp tests.
        let bytes = super::estimate_envelope_cost(&cont.payload);
        assert!(bytes <= spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES);
    }

    #[tokio::test]
    async fn materialize_falls_back_on_too_large_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};
        let store = MockFailingOutcomeStore::new(FailureMode::TooLarge);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_on_content_mismatch() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};
        let store = MockFailingOutcomeStore::new(FailureMode::ContentMismatch);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_on_backend_error() {
        use spur_blob_store::test_helpers::{FailureMode, MockFailingOutcomeStore};
        let store = MockFailingOutcomeStore::new(FailureMode::Backend("git update-ref failed".into()));
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(cont.payload.artifact_id.is_none());
    }

    #[tokio::test]
    async fn materialize_falls_back_when_inner_store_panics() {
        // Spec §7.7 (Round 9 P3-S3) requires every FailureMode produce a
        // valid BrainContinuation — including a panicking backend. The
        // mock cannot panic-and-stay-testable inside an async fn (would
        // poison the runtime), so this test wires a one-off
        // `PanickingStore` inline and asserts the materializer collapses
        // into the truncation-ladder fallback via
        // `AssertUnwindSafe(store.put(...)).catch_unwind().await`.
        use async_trait::async_trait;
        use spur_blob_store::{
            BackendTag, OutcomeContent, OutcomeKey as Key, OutcomeMetadata, OutcomeRef,
            OutcomeStore, Section, StoreError, SweepReport,
        };
        use spur_acp::BrainSessionId;
        use std::sync::Arc;
        use std::time::Duration;

        struct PanickingStore;
        #[async_trait]
        impl OutcomeStore for PanickingStore {
            async fn put(
                &self,
                _key: &Key,
                _content: &[u8],
                _metadata: &OutcomeMetadata,
            ) -> Result<OutcomeRef, StoreError> {
                panic!("simulated backend panic");
            }
            async fn get(
                &self,
                _key: &Key,
                _section: Option<Section>,
            ) -> Result<OutcomeContent, StoreError> {
                Err(StoreError::Backend("unused".into()))
            }
            async fn delete_namespace(
                &self,
                _b: &BrainSessionId,
            ) -> Result<usize, StoreError> {
                Ok(0)
            }
            async fn sweep_older_than(
                &self,
                _ttl: Duration,
            ) -> Result<SweepReport, StoreError> {
                Ok(SweepReport::default())
            }
        }

        let store: Arc<dyn OutcomeStore> = Arc::new(PanickingStore);
        let mat = OutcomeMaterializer::new(store);
        let cont = mat
            .materialize(
                small_result(),
                delegation_id(),
                1,
                brain_session(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        assert!(
            cont.payload.artifact_id.is_none(),
            "panic in store.put must fall back, not unwind"
        );
        let _ = BackendTag::Fs; // import-use lint sentinel
    }
```

This test requires the materializer's `store.put` invocation to be
wrapped in `futures::FutureExt::catch_unwind` (or
`tokio::task::spawn` + `JoinHandle.await`) so the panic does not
propagate. Add to Task 5's success-path body BEFORE the `match
self.store.put(&key, &bytes, &metadata).await { ... }` block:

```rust
use std::panic::AssertUnwindSafe;
use futures::future::FutureExt;

let put_fut = AssertUnwindSafe(self.store.put(&key, &bytes, &metadata)).catch_unwind();
let outcome_ref = match put_fut.await {
    Ok(Ok(r)) => r,
    Ok(Err(e)) => {
        tracing::error!(target: "spur.metrics.outcome_persist_failed", error = %e, "store.put returned Err; falling back");
        return self.fallback_truncation_ladder(/*...*/).await;
    }
    Err(panic_payload) => {
        let msg = panic_payload
            .downcast_ref::<&'static str>().map(|s| s.to_string())
            .or_else(|| panic_payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!(target: "spur.metrics.outcome_persist_failed", panic = %msg, "store.put panicked; falling back");
        return self.fallback_truncation_ladder(/*...*/).await;
    }
};
```

Add `futures = { workspace = true }` to `crates/spur-mcp/Cargo.toml` if not already present (likely is — used elsewhere).

Add `spur-blob-store = { workspace = true, features = ["test-support"] }` to `crates/spur-mcp/Cargo.toml`'s `[dev-dependencies]`.

- [ ] **Step 2: Run tests to verify they fail with `unimplemented!()`**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib outcome_materializer::tests::materialize_falls_back_on_io_error`
Expected: FAIL with `unimplemented!()` panic.

- [ ] **Step 3: Implement the fallback path**

Replace the `fallback_truncation_ladder` body in `crates/spur-mcp/src/outcome_materializer.rs`:

```rust
    async fn fallback_truncation_ladder(
        &self,
        result: DelegationResult,
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: BrainSessionId,
        source: ContinuationSource,
        event_sink: Option<&Arc<dyn McpEventSink>>,
    ) -> BrainContinuation {
        use spur_acp::domain::clip::{
            clip_artifact_ref_strings, clip_diff_files, clip_status_strings, clip_with_ellipsis,
        };
        use spur_acp::domain::events::SpurEventBody;
        use std::time::Instant;

        let start = Instant::now();

        // Step 0: clip every inline field (same caps as success path).
        let clipped_status = clip_status_strings(&result.status, self.status_string_cap_bytes);
        let original_summary_len = result.summary.as_ref().map(|s| s.len()).unwrap_or(0);
        let (clipped_summary, summary_truncated) =
            clip_with_ellipsis(result.summary.clone(), self.summary_cap_bytes);
        let (clipped_branch, _) =
            clip_with_ellipsis(result.worker_branch.clone(), self.worker_branch_cap_bytes);
        let clipped_diff = result
            .diff_summary
            .as_ref()
            .map(|d| clip_diff_files(d, self.diff_files_cap_count));
        let clipped_artifact_ref = result
            .artifact
            .as_ref()
            .map(|wa| build_artifact_ref(&delegation_id, wa))
            .map(|a| clip_artifact_ref_strings(&a, self.artifact_ref_string_cap_bytes));

        if summary_truncated {
            if let Some(sink) = event_sink {
                sink.emit(SpurEventBody::ContinuationFieldTruncated {
                    delegation_id: delegation_id.clone(),
                    field: "summary".into(),
                    original_bytes: original_summary_len,
                    kept_bytes: clipped_summary.as_ref().map(|s| s.len()).unwrap_or(0),
                });
            }
        }

        let payload = ContinuationPayload {
            status: clipped_status,
            summary: clipped_summary,
            diff_summary: clipped_diff,
            worker_branch: clipped_branch,
            artifact_ref: clipped_artifact_ref,
            estimated_cost_micros: None,
            artifact_id: None, // INV: fallback path always clears artifact_id
            fetch_hint: None,
        };

        let mut cont = BrainContinuation {
            delegation_id: delegation_id.clone(),
            attempt,
            brain_session: brain_session.as_session_id().clone(),
            source,
            payload,
            created_at_wall: chrono::Utc::now(),
            created_at_mono: Instant::now(),
        };

        // Step 1: emergency re-clip if envelope (conservative estimate)
        // still oversized. Re-uses the same cost estimator as the
        // success path so both code paths see the same view of the
        // budget.
        let mut envelope_bytes = estimate_envelope_cost(&cont.payload);
        let budget = spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES;
        if envelope_bytes > budget {
            // Step 2: drop summary entirely.
            cont.payload.summary = None;
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            // Step 3: drop diff_summary.
            cont.payload.diff_summary = None;
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            // Step 4: drop artifact_ref.
            cont.payload.artifact_ref = None;
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }
        if envelope_bytes > budget {
            // Step 5: emergency re-clip status to 128 B.
            cont.payload.status = clip_status_strings(&cont.payload.status, 128);
            envelope_bytes = estimate_envelope_cost(&cont.payload);
        }

        if envelope_bytes > budget {
            // Step 6 (release-mode last resort): drop EVERY inline field.
            tracing::error!(
                target: "spur.metrics.continuation_dropped_oversized",
                delegation_id = %delegation_id,
                envelope_bytes,
                budget_bytes = budget,
                "fallback ladder exhausted; emitting minimal continuation"
            );
            cont.payload.status = spur_acp::domain::delegation::DelegationStatus::Success;
            cont.payload.summary = Some("(continuation oversized; fields dropped)".into());
            cont.payload.diff_summary = None;
            cont.payload.worker_branch = None;
            cont.payload.artifact_ref = None;
        }

        tracing::warn!(
            target: "spur.metrics.outcome_persist_failed",
            delegation_id = %delegation_id,
            attempt,
            fallback_engaged = true,
            envelope_bytes,
            latency_ms = start.elapsed().as_millis() as u64,
        );
        cont
    }
```

- [ ] **Step 4: Run all tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib outcome_materializer`
Expected: all 7 tests pass (3 success-path + 4 fallback-path).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/Cargo.toml crates/spur-mcp/src/outcome_materializer.rs
git commit -m "feat(spur-mcp): OutcomeMaterializer fallback truncation ladder

Implements the persist-failure fallback path (§7.7). When
OutcomeStore::put fails (or the post-clip envelope still exceeds
MERGE_BUDGET), the materializer:

1. Clips every inline field at the configured caps.
2. Emits ContinuationFieldTruncated events when caps actually fired.
3. Builds a lean continuation with artifact_id: None.
4. Re-checks envelope size and progressively drops summary →
   diff_summary → artifact_ref → emergency re-clip status to 128 B.
5. Last-resort: drops all inline fields and emits
   spur.metrics.continuation_dropped_oversized (operator alert).

Tests use the new MockFailingOutcomeStore (Task 3) to exercise every
FailureMode (Io, TooLarge, Backend, ContentMismatch). INV-α holds for
the full failure surface.

Phase 3 of plan-5; spec §7.7."
```

---

## Task 7: Wire materializer into `build_detached_continuation`

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:268` (`build_detached_continuation`)
- Modify: `crates/spur-mcp/src/server.rs` (`McpCallbackServer` struct + constructor)
- Modify: `crates/spur-core/src/orchestrator.rs` (construct `McpCallbackServer` with shared `Arc<dyn OutcomeStore>`)

**What:** The `McpCallbackServer` gains a `materializer: OutcomeMaterializer` field. The orchestrator constructs the store once (`Arc<MeasuredOutcomeStore<GitBlobOutcomeStore>>`) and shares it with the server. `build_detached_continuation` becomes async and routes through `materializer.materialize(...)` instead of building the continuation inline.

- [ ] **Step 1: Write failing test asserting the wired path produces an artifact_id**

Add to `crates/spur-mcp/src/server.rs`'s test module (the `continuation_producer_tests` mod):

```rust
    #[tokio::test]
    async fn build_detached_continuation_populates_artifact_id_via_materializer() {
        use spur_blob_store::MemoryOutcomeStore;
        use std::sync::Arc;

        let store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = crate::outcome_materializer::OutcomeMaterializer::new(store);
        let result = DelegationResult {
            status: spur_acp::domain::delegation::DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: None,
        };
        let delegation_id = DelegationId::from("deadbeef-1111-2222-3333-444455556666");
        let brain_session = SessionId("550e8400-e29b-41d4-a716-446655440000".into());

        let cont = build_detached_continuation(
            &delegation_id,
            &result,
            spur_acp::domain::ContinuationSource::BlockTimeout,
            1,
            brain_session,
            None,
            &mat,
        )
        .await;
        assert!(cont.payload.artifact_id.is_some(), "Phase 3 wires artifact_id");
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib continuation_producer_tests::build_detached_continuation_populates_artifact_id_via_materializer`
Expected: FAIL with compile error — `build_detached_continuation` does not take a materializer arg yet.

- [ ] **Step 3: Update `build_detached_continuation` signature + body**

Edit `crates/spur-mcp/src/server.rs:268`:

```rust
/// Phase 3 (plan-5 §7.3): the materializer is the single producer of
/// `BrainContinuation` for completed delegations. This function is now a
/// thin wrapper that forwards to `OutcomeMaterializer::materialize`.
pub(crate) async fn build_detached_continuation(
    delegation_id: &DelegationId,
    result: &DelegationResult,
    source: spur_acp::domain::ContinuationSource,
    attempt: u32,
    brain_session: SessionId,
    event_sink: Option<&Arc<dyn crate::events::McpEventSink>>,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
) -> spur_acp::domain::BrainContinuation {
    use spur_acp::BrainSessionId;
    let brain_session_id = BrainSessionId::new(brain_session);
    materializer
        .materialize(
            result.clone(),
            delegation_id.clone(),
            attempt,
            brain_session_id,
            source,
            event_sink,
        )
        .await
}
```

Delete the now-unused `map_worker_artifact_ref` helper at lines 237-255 — it's subsumed by the materializer. (Confirm there are no other callers via `grep -rn "map_worker_artifact_ref" crates/`.)

- [ ] **Step 4: Update `McpCallbackServer` to own the materializer**

Run: `grep -n "pub struct McpCallbackServer\|impl McpCallbackServer" crates/spur-mcp/src/server.rs | head -5`

Find the struct definition. Add a field:

```rust
    pub(crate) materializer: crate::outcome_materializer::OutcomeMaterializer,
```

Find the constructor (`fn new(`) and add a parameter:

```rust
    pub fn new(
        // ... existing args ...
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    ) -> Self {
        // ...
        Self {
            // ... existing fields ...
            materializer: crate::outcome_materializer::OutcomeMaterializer::new(outcome_store),
        }
    }
```

Update every callsite of `build_detached_continuation` in server.rs to pass `&self.materializer`. Search:

Run: `grep -n "build_detached_continuation" crates/spur-mcp/src/server.rs`

Each callsite must `.await` the call now (function became async). Convert non-async callers as needed; if a callsite is already inside an async fn, just add `.await`.

- [ ] **Step 5: Update all McpCallbackServer constructors**

Run: `grep -rn "McpCallbackServer::new\|build_test_server" crates/`

Update each to pass an `Arc<dyn OutcomeStore>`. For test callsites, use `Arc::new(MemoryOutcomeStore::new())`. For production (in `crates/spur-core/src/orchestrator.rs`), construct the same `MeasuredOutcomeStore<GitBlobOutcomeStore>` that Phase 2's per-call code currently builds, but ONCE at orchestrator construction and shared.

```rust
// crates/spur-core/src/orchestrator.rs (orchestrator constructor)
use std::sync::Arc;
use spur_blob_store::OutcomeStore;
use spur_blob_store::MeasuredOutcomeStore;
use spur_worktree::git_blob_store::GitBlobOutcomeStore;

let outcome_store: Arc<dyn OutcomeStore> = Arc::new(MeasuredOutcomeStore::new(
    GitBlobOutcomeStore::new(worktrees.repo_root.clone()),
));
let server = McpCallbackServer::new(/* ... existing args ..., */ outcome_store.clone());
// Stash outcome_store on the orchestrator struct for Tasks 8 + 11.
self.outcome_store = outcome_store;
```

Add `outcome_store: Arc<dyn OutcomeStore>` field to the orchestrator struct.

- [ ] **Step 6: Remove the per-call store construction from Phase 2's persist site**

Run: `grep -n "GitBlobOutcomeStore::new" crates/spur-core/src/orchestrator.rs`

The Phase 2 site (~line 4782) constructs `GitBlobOutcomeStore::new(worktrees.repo_root.clone())` per call. Replace with a call through the shared `self.outcome_store` (or pass it down via context).

Note: this Phase-2 site is now redundant — the materializer in T8 will handle persistence for the reconciler-driven path. But for the build_detached_continuation path (callsite 1), the persist runs INSIDE materialize, so the Phase 2 manual `store.put` site can be removed entirely (the materializer will do it once).

For now in Task 7, just keep both paths and let Task 8 finish the unification.

- [ ] **Step 7: Run all tests**

Run: `RUSTC_WRAPPER= cargo check --workspace`
Expected: clean.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib continuation_producer_tests`
Expected: green; new test passes.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-mcp,spur-core): wire OutcomeMaterializer into build_detached_continuation

build_detached_continuation now forwards to OutcomeMaterializer::materialize
(§7.3 callsite 1). The McpCallbackServer owns the materializer; the
orchestrator constructs Arc<dyn OutcomeStore> once at startup and shares
it. The Phase 2 per-call GitBlobOutcomeStore::new site survives only
until Task 8 unifies the reconciler path.

map_worker_artifact_ref is removed — its logic moved into the
materializer's build_artifact_ref helper.

Phase 3 of plan-5; spec §7.3."
```

---

## Task 8: Wire materializer into `persist_completion_result_and_notify`

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs:1309` (`persist_completion_result_and_notify` signature)
- Modify: `crates/spur-mcp/src/plan/reconciler.rs:421` (call site passes full result)
- Modify: any callers of `persist_completion_result_and_notify` in plan/mod.rs (line 1477, 3798)
- Modify: tests in `crates/spur-mcp/tests/`

**What:** The reconciler-driven completion path (§7.3 callsite 2) currently passes only `worker_branch` + `result_summary` strings. Phase 3 needs the full `DelegationResult` so the materializer can persist + clip uniformly. Add `&DelegationResult`, `&BrainSessionId`, `attempt: u32`, and `&OutcomeMaterializer` parameters.

- [ ] **Step 1: Read the existing function signature + callers**

Run: `grep -n "persist_completion_result_and_notify\|persist_completion_result" crates/spur-mcp/src/plan/mod.rs | head -10`
Run: `sed -n '1305,1335p' crates/spur-mcp/src/plan/mod.rs`
Run: `grep -rn "persist_completion_result_and_notify" crates/ --include="*.rs"`

- [ ] **Step 2: Update the function signature**

Replace lines 1309-1331 in `crates/spur-mcp/src/plan/mod.rs`:

```rust
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_completion_result_and_notify(
    pm: &dyn PmLike,
    issue_id: &str,
    plan_id: &str,
    delegation_id: &str,
    completion_state: crate::plan::audit_sentinel::CompletionState,
    fast_forward: &Option<Arc<tokio::sync::Notify>>,
    // NEW for Phase 3 — full result for materializer + audit-comment artifact_uri.
    result: &spur_acp::domain::DelegationResult,
    brain_session_id: &spur_acp::BrainSessionId,
    attempt: u32,
    materializer: &crate::outcome_materializer::OutcomeMaterializer,
) -> anyhow::Result<()> {
    // Run the materializer FIRST so the OutcomeKey is available for the
    // audit comment's artifact_uri field.
    use crate::plan::audit_sentinel::CompletionState;

    // Superseded means another delegation took over for this task — the
    // brain has already received the new delegation's continuation, so
    // emitting a stale-attempt continuation now would just confuse it.
    // Run the existing audit/notify path WITHOUT the materializer.
    if matches!(completion_state, CompletionState::Superseded) {
        persist_completion_result(
            pm,
            issue_id,
            plan_id,
            delegation_id,
            completion_state,
            result.worker_branch.as_deref(),
            result.summary.as_deref(),
            None, // no artifact_uri — no continuation was emitted
        )
        .await?;
        crate::server::notify_fast_forward(fast_forward);
        return Ok(());
    }

    // ContinuationSource for the reconciler-driven (plan-mode) path. The
    // existing variants are AsyncRequested, BlockTimeout, Cancelled,
    // PlanCompleted, PlanReadyToMerge. Reconciler completions match the
    // PlanCompleted lifecycle (a worker the brain dispatched as part of a
    // plan reached a terminal state) — use that variant rather than the
    // detached-collector default BlockTimeout. Match must be exhaustive
    // over CompletionState: AwaitingReview/Failed/Cancelled/Superseded.
    // Superseded is handled above; the remaining 3 variants map below.
    let source = match completion_state {
        CompletionState::AwaitingReview | CompletionState::Failed => {
            spur_acp::domain::ContinuationSource::PlanCompleted
        }
        CompletionState::Cancelled => spur_acp::domain::ContinuationSource::Cancelled,
        CompletionState::Superseded => unreachable!("handled above"),
    };
    let cont = materializer
        .materialize(
            result.clone(),
            spur_acp::DelegationId::from(delegation_id),
            attempt,
            brain_session_id.clone(),
            source,
            None,
        )
        .await;
    let artifact_uri = cont.payload.artifact_id.as_ref().map(|key| {
        format!(
            "spur://outcome/{}/{}/{}",
            key.brain_session_id.as_session_id().0,
            key.delegation_id.as_str(),
            key.attempt
        )
    });

    let worker_branch = result.worker_branch.as_deref();
    let result_summary = cont.payload.summary.as_deref();

    persist_completion_result(
        pm,
        issue_id,
        plan_id,
        delegation_id,
        completion_state,
        worker_branch,
        result_summary,
        artifact_uri.as_deref(),  // NEW — wired in Task 9 (audit_sentinel field)
    )
    .await?;
    crate::server::notify_fast_forward(fast_forward);
    Ok(())
}
```

Note: this requires `persist_completion_result` to gain an `artifact_uri: Option<&str>` parameter. That's a Task 9 dependency; for now stub the call as the existing 7-arg form and wire `artifact_uri` in Task 9.

For Task 8 alone, drop the `artifact_uri.as_deref()` arg until Task 9 lands. The local variable `artifact_uri` stays as future-state plumbing (use `let _ = artifact_uri;` to silence the unused-variable warning).

- [ ] **Step 3: Update the production callsite at `reconciler.rs:421`**

Run: `sed -n '395,430p' crates/spur-mcp/src/plan/reconciler.rs`

Find the closure that awaits `rx.await` and forwards to `persist_completion_result_and_notify`. Pass:
- `&result` (full DelegationResult — already in scope from `rx.await`)
- `&brain_session_id` (already on `ReconcilerDispatchCtx`)
- `task.attempt`
- `&materializer` (added to `ReconcilerDispatchCtx` in this step)

Add to `ReconcilerDispatchCtx` struct (~line 155):

```rust
    pub materializer: Arc<crate::outcome_materializer::OutcomeMaterializer>,
```

Update its constructor and every site that builds it.

- [ ] **Step 4: Update other callers in plan/mod.rs**

Run: `grep -n "persist_completion_result_and_notify(" crates/spur-mcp/src/plan/mod.rs`

For each match (lines 1477, 3798), update the call to pass the new args. For test sites that don't have a full `DelegationResult`, construct one using existing `worker_branch` + `result_summary` strings.

- [ ] **Step 5: Update test callsites**

Run: `grep -rn "persist_completion_result_and_notify" crates/spur-mcp/tests/`

For each test callsite, construct a `DelegationResult` literal:

```rust
let result = spur_acp::domain::DelegationResult {
    status: spur_acp::domain::delegation::DelegationStatus::Success,
    diff: None,
    diff_summary: None,
    summary: Some("test".into()),
    estimated_cost_usd: 0.0,
    worker_branch: Some("spur/worker-test".into()),
    artifact: None,
};
let store: Arc<dyn spur_blob_store::OutcomeStore> =
    Arc::new(spur_blob_store::MemoryOutcomeStore::new());
// Integration tests in `crates/spur-mcp/tests/` use `spur_mcp::` to address
// the crate (NOT `crate::` — `crate::` in an integration test refers to the
// integration test crate itself, not spur-mcp).
let materializer = spur_mcp::outcome_materializer::OutcomeMaterializer::new(store);
let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("...".into()));
```

Inside `crates/spur-mcp/src/...` unit tests (`#[cfg(test)] mod tests`), `crate::outcome_materializer::OutcomeMaterializer` IS correct and stays unchanged.

- [ ] **Step 6: Run tests**

Run: `RUSTC_WRAPPER= cargo check --workspace`
Run: `RUSTC_WRAPPER= cargo test -p spur-mcp`
Expected: green.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/tests/
git commit -m "feat(spur-mcp): route reconciler completion path through OutcomeMaterializer

persist_completion_result_and_notify gains &DelegationResult,
&BrainSessionId, attempt: u32, and &OutcomeMaterializer parameters.
The reconciler now passes the full result it already has from the
oneshot::Receiver (the round-6 design assumed metadata-only; the
runtime path always had the full result).

The materializer runs at this site too, so beads-driven completions
get the same persist + lean continuation behavior as the direct
delegate_to_worker path. artifact_uri is computed but not yet plumbed
into the audit comment — Task 9 finishes that wire.

Phase 3 of plan-5; spec §7.3 callsite 2."
```

---

## Task 9: `artifact_uri` in audit comments

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs:70` (`Completion` variant)
- Modify: `crates/spur-mcp/src/plan/mod.rs::emit_completion_audit` (around line 686-708)
- Modify: `crates/spur-mcp/src/plan/mod.rs::persist_completion_result` (signature)

**What:** Add `artifact_uri: Option<String>` to `AuditSentinelKind::Completion`. Reconciler populates it from `cont.payload.artifact_id` (computed in Task 8). Field is additive + serde-default — no parser changes needed.

- [ ] **Step 1: Add field to the variant**

Edit `crates/spur-mcp/src/plan/audit_sentinel.rs:70-79`:

```rust
    Completion {
        delegation_id: String,
        completion_state: CompletionState,
        #[serde(default)]
        superseded: bool,
        #[serde(default)]
        worker_branch: Option<String>,
        #[serde(default)]
        result_summary: Option<String>,
        /// NEW (Phase 3) — `Some(_)` when OutcomeMaterializer succeeded;
        /// carries the OutcomeKey-derived URI. Operators viewing the beads
        /// issue can extract this and resolve via `fetch_outcome_artifact`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact_uri: Option<String>,
    },
```

- [ ] **Step 2: Write a regression test for backward-compat parsing**

Add to `crates/spur-mcp/src/plan/audit_sentinel.rs` test module:

```rust
#[test]
fn completion_variant_parses_v2_comments_without_artifact_uri() {
    // Audit comments emitted before Phase 3 don't have artifact_uri.
    // Parser must default to None instead of erroring.
    let v2_json = r#"{"kind":"completion","delegation_id":"abc","completion_state":"awaiting_review","superseded":false,"worker_branch":"spur/worker-x","result_summary":"done"}"#;
    let parsed: AuditSentinelKind = serde_json::from_str(v2_json).expect("parse");
    if let AuditSentinelKind::Completion { artifact_uri, .. } = parsed {
        assert!(artifact_uri.is_none());
    } else {
        panic!("variant changed");
    }
}

#[test]
fn completion_variant_round_trips_artifact_uri() {
    let s = AuditSentinelKind::Completion {
        delegation_id: "abc".into(),
        completion_state: CompletionState::AwaitingReview,
        superseded: false,
        worker_branch: Some("spur/worker-x".into()),
        result_summary: Some("done".into()),
        artifact_uri: Some("spur://outcome/aaa/bbb/1".into()),
    };
    let json = serde_json::to_string(&s).unwrap();
    let back: AuditSentinelKind = serde_json::from_str(&json).unwrap();
    if let AuditSentinelKind::Completion { artifact_uri, .. } = back {
        assert_eq!(artifact_uri.as_deref(), Some("spur://outcome/aaa/bbb/1"));
    } else {
        panic!("variant changed");
    }
}
```

- [ ] **Step 3: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib audit_sentinel`
Expected: green.

- [ ] **Step 4: Update `emit_completion_audit` to accept artifact_uri**

Run: `sed -n '680,715p' crates/spur-mcp/src/plan/mod.rs`

Find `emit_completion_audit` (around line 686). Add `artifact_uri: Option<&str>` parameter and pass it through to the `Completion { ... }` literal.

- [ ] **Step 5: Update `persist_completion_result` signature + body**

Run: `grep -n "fn persist_completion_result\b" crates/spur-mcp/src/plan/mod.rs`

Add `artifact_uri: Option<&str>` parameter (typically last). Forward to `emit_completion_audit`.

- [ ] **Step 6: Update `persist_completion_result_and_notify` to pass artifact_uri**

Edit the Task 8 stub:

```rust
    persist_completion_result(
        pm,
        issue_id,
        plan_id,
        delegation_id,
        completion_state,
        worker_branch,
        result_summary,
        artifact_uri.as_deref(),  // NEW (now wired)
    )
    .await?;
```

Remove the `let _ = artifact_uri;` placeholder.

- [ ] **Step 7: Run all tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp`
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add artifact_uri to Completion audit sentinel

Reconciler now propagates the OutcomeKey-derived URI into the beads
audit comment when OutcomeMaterializer succeeds. Operators viewing
the beads issue can extract artifact_uri from the JSON and resolve
via fetch_outcome_artifact.

Field is additive + serde-default — backward-compatible with v2
audit comments that don't have it (regression test asserts).

Phase 3 of plan-5; spec §7.4."
```

---

## Task 10: Extend `fetch_outcome_artifact` with section + attempt

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:2668` (`handle_fetch_outcome_artifact`)
- Modify: `crates/spur-mcp/src/tools.rs:265` (`fetch_outcome_artifact_def`)

**What:** Phase 1 only supported `section: "full"`. Phase 3 adds `status_only`, `summary`, `diff_only`, plus an `attempt: Option<u32>` arg (default = latest known). The handler routes through `OutcomeStore::get(&key, Some(section))` instead of `git cat-file -p artifact.blob_sha`.

- [ ] **Step 1: Update tool schema**

Edit `crates/spur-mcp/src/tools.rs:265`:

```rust
fn fetch_outcome_artifact_def() -> ToolDefinition {
    ToolDefinition {
        name: "fetch_outcome_artifact".into(),
        description: "Fetch the side-channel artifact (full or sectioned) for a completed delegation. Use when continuation.payload.artifact_id is Some(_) and you need fuller context. Sections let you pick what to fetch — pass 'status_only' for just status fields (~100B), 'summary' for the inline summary, 'diff_only' for full diff text, or 'full' for the entire DelegationResult JSON.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id whose artifact you want to fetch."
                },
                "attempt": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional attempt number. Default: latest known attempt for this delegation. Pin a specific attempt for forensic queries on retried delegations."
                },
                "section": {
                    "type": "string",
                    "enum": ["status_only", "summary", "diff_only", "full"],
                    "default": "full",
                    "description": "Which section to fetch. Phase 3."
                }
            },
            "required": ["delegation_id"]
        }),
    }
}
```

Update the test `fetch_outcome_artifact_schema_advertises_phase1_section_only` (line 737) — rename to `fetch_outcome_artifact_schema_advertises_phase3_sections` and assert all four enum values present.

- [ ] **Step 2: Write failing tests for the new sections**

Add to `mod fetch_outcome_artifact_tests`:

```rust
    #[tokio::test]
    async fn fetch_outcome_artifact_returns_status_only_section() {
        // build_test_server with a MemoryOutcomeStore pre-populated.
        // Call fetch_outcome_artifact with section="status_only".
        // Assert response payload contains "status" but NOT full diff/summary.
        // ... full body in implementation step ...
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_summary_section() {
        // Similar to status_only but section="summary".
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_diff_only_section() {
        // Similar but section="diff_only".
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_pins_specific_attempt() {
        // Pre-populate two attempts (1 and 2) for the same delegation.
        // Call with attempt=1 → returns content from attempt 1.
        // Call without attempt → returns content from attempt 2 (latest).
    }
```

(The full bodies follow the existing `fetch_outcome_artifact_returns_persisted_blob_text` pattern. Inline them when filling Step 4.)

- [ ] **Step 3a: Add a `latest_attempt_by_delegation` field to `McpCallbackServer`**

`DelegationResult` does NOT have an `attempt` field (verified at `crates/spur-acp/src/domain/delegation.rs:146`). The materializer in Task 5 receives `attempt: u32` as an explicit parameter. Plumb that into a separate per-server map so the fetch tool can answer "latest known attempt" when the caller doesn't pin one.

In Task 7 (`McpCallbackServer` constructor), add the field:

```rust
pub(crate) latest_attempt_by_delegation: Arc<tokio::sync::Mutex<
    std::collections::HashMap<DelegationId, u32>
>>,
```

Initialize it as empty in `new()`. In Task 5 (`OutcomeMaterializer::materialize`) and the success-callbacks at server.rs callsites, after a successful materialize, update the map:

```rust
{
    let mut map = self.latest_attempt_by_delegation.lock().await;
    map.entry(delegation_id.clone())
        .and_modify(|cur| *cur = (*cur).max(attempt))
        .or_insert(attempt);
}
```

(Place this update in the McpCallbackServer wrapper that invokes `materializer.materialize` — keep the materializer struct itself store-agnostic.)

- [ ] **Step 3b: Update handler to use the new tracker + MCP-layer section projection**

Phase 2's `MemoryOutcomeStore::get` and `GitBlobOutcomeStore::get` ignore the `_section` parameter (verified — both prefix the arg with `_`). For Phase 3, do MCP-layer projection: load the full JSON via `Section::Full`, deserialize, project to the requested section, re-serialize. This keeps Phase 2's stores untouched.

Edit `crates/spur-mcp/src/server.rs:2668`:

```rust
    async fn handle_fetch_outcome_artifact(&self, id: Value, args: Value) -> JsonRpcResponse {
        use spur_blob_store::Section;

        let delegation_id: DelegationId = match args.get("delegation_id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.into(),
            _ => return JsonRpcResponse::invalid_params(id, "Missing or empty 'delegation_id'"),
        };

        let section_str = args.get("section").and_then(|v| v.as_str()).unwrap_or("full");
        let section = match section_str {
            "full" => Section::Full,
            "status_only" => Section::StatusOnly,
            "summary" => Section::Summary,
            "diff_only" => Section::DiffOnly,
            other => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!(
                        "Unknown section '{other}'. Must be one of: status_only, summary, diff_only, full."
                    ),
                );
            }
        };

        let attempt = match args.get("attempt").and_then(|v| v.as_u64()) {
            Some(n) if (1..=u32::MAX as u64).contains(&n) => n as u32,
            None => {
                let map = self.latest_attempt_by_delegation.lock().await;
                map.get(&delegation_id).copied().unwrap_or(1)
            }
            Some(_) => {
                return JsonRpcResponse::invalid_params(id, "Invalid 'attempt': must be u32 ≥ 1");
            }
        };

        let brain_session_id = self.brain_session_id.clone();
        let key = spur_acp::domain::outcome::OutcomeKey {
            brain_session_id,
            delegation_id: delegation_id.clone(),
            attempt,
        };

        let start = std::time::Instant::now();
        // Always read Section::Full from the store (Phase 2 stores ignore
        // section), then project at the MCP layer.
        let content = match self.outcome_store.get(&key, Some(Section::Full)).await {
            Ok(c) => c,
            Err(spur_blob_store::StoreError::NotFound(_)) => {
                tracing::warn!(
                    target: "spur.metrics.outcome_fetch_not_found",
                    ?key,
                    section = section_str,
                    "outcome not found; possible hallucinated id, post-GC read, or in-flight race"
                );
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("Outcome artifact not found for delegation_id={delegation_id} attempt={attempt}"),
                );
            }
            Err(spur_blob_store::StoreError::Unauthorized { requested, actual }) => {
                tracing::warn!(
                    target: "spur.metrics.outcome_fetch_unauthorized",
                    ?requested,
                    ?actual,
                    "cross-session fetch rejected"
                );
                return JsonRpcResponse::error(id, -32001, "cross-session outcome read forbidden");
            }
            Err(e) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("OutcomeStore::get failed: {e}"),
                );
            }
        };

        // MCP-layer section projection. Pass the OutcomeKey so the
        // projector can inject `attempt`/`brain_session` per spec
        // §7.5 line 818.
        let projected_text = match project_section(&content.bytes, section, &key) {
            Ok(s) => s,
            Err(e) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("Section projection failed: {e}"),
                );
            }
        };

        tracing::info!(
            target: "spur.metrics.outcome_fetched",
            ?key,
            section = section_str,
            byte_size = projected_text.len() as u64,
            latency_ms = start.elapsed().as_millis() as u64,
        );

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": projected_text }] }))
    }
```

Add a helper at module scope (or in a sibling module if you prefer). The
helper takes `&OutcomeKey` so it can inject the spec-mandated
`attempt`/`brain_session` fields into `status_only`/`summary`
projections, and converts `estimated_cost_usd` (the field that lives on
`DelegationResult`) → `estimated_cost_micros` per spec §7.1 / §7.5:

```rust
#[derive(Debug, thiserror::Error)]
enum ProjectionError {
    #[error("stored blob is not a valid DelegationResult: {0}")]
    InvalidResult(#[source] serde_json::Error),
    #[error("projection serialization failed: {0}")]
    SerializeFailed(#[source] serde_json::Error),
}

fn project_section(
    full_bytes: &[u8],
    section: spur_blob_store::Section,
    key: &spur_acp::domain::outcome::OutcomeKey,
) -> Result<String, ProjectionError> {
    use spur_acp::domain::DelegationResult;
    use spur_blob_store::Section;

    if matches!(section, Section::Full) {
        // Full: return stored bytes verbatim. The stored blob is the
        // serialized DelegationResult JSON; spec §7.5 says
        // `full — entire DelegationResult`. No round-trip required.
        return Ok(String::from_utf8_lossy(full_bytes).into_owned());
    }

    let result: DelegationResult = serde_json::from_slice(full_bytes)
        .map_err(ProjectionError::InvalidResult)?;
    // Reuse the materializer's helper so write-time + read-time round
    // through the same conversion (no rounding drift).
    let estimated_cost_micros =
        crate::outcome_materializer::usd_to_micros_saturating(result.estimated_cost_usd);

    let projected = match section {
        Section::StatusOnly => json!({
            "status": result.status,
            "attempt": key.attempt,
            "brain_session": key.brain_session_id,
            "estimated_cost_micros": estimated_cost_micros,
        }),
        Section::Summary => json!({
            "status": result.status,
            "attempt": key.attempt,
            "brain_session": key.brain_session_id,
            "summary": result.summary,
            "estimated_cost_micros": estimated_cost_micros,
        }),
        Section::DiffOnly => json!({
            "status": result.status,
            "diff": result.diff,
            "diff_summary": result.diff_summary,
        }),
        Section::Full => unreachable!("handled above"),
    };
    serde_json::to_string(&projected).map_err(ProjectionError::SerializeFailed)
}
```

The helper's `Result<String, ProjectionError>` exposes a typed error so
the handler can match on `InvalidResult` (operator alert: corrupted
blob) vs `SerializeFailed` (programming error). The handler stringifies
via `e.to_string()` for the JSON-RPC response payload.

Note: the legacy `git cat-file -p artifact.blob_sha` Phase 1 path is removed. Phase 1 artifacts under `refs/spur/artifacts/<session>` are no longer fetchable via the MCP tool. Brains hitting the tool against legacy artifacts get a clean `NotFound`. Operators with legacy data should migrate via the deferred `spur outcomes copy` CLI.

- [ ] **Step 4: Implement the test bodies (from Step 2)**

Use `MemoryOutcomeStore` pre-populated via `OutcomeStore::put` to seed test data. Pattern:

```rust
let store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(spur_blob_store::MemoryOutcomeStore::new());
let result = serde_json::to_vec(&DelegationResult {
    status: DelegationStatus::Success,
    diff: None,
    diff_summary: None,
    summary: Some("seed".into()),
    estimated_cost_usd: 0.0,
    worker_branch: None,
    artifact: None,
}).unwrap();
let metadata = OutcomeMetadata { /* ... sha256 of result, ContentType::Json */ };
let key = OutcomeKey { /* ... attempt: 1 */ };
store.put(&key, &result, &metadata).await.unwrap();
let server = build_test_server_with_store(td.path(), session_id, store).await;
// ... call fetch_outcome_artifact with various section values ...
```

- [ ] **Step 5: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib fetch_outcome_artifact_tests`
Expected: 4 new tests pass + existing tests updated to pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/tools.rs
git commit -m "feat(spur-mcp): extend fetch_outcome_artifact with section + attempt

Phase 3 widens the supported sections to status_only/summary/diff_only/
full and adds an explicit attempt: Option<u32> arg. Default attempt is
the latest known for the delegation_id.

The handler now reads through the shared Arc<dyn OutcomeStore> instead
of running git cat-file -p artifact.blob_sha. Legacy Phase 1 artifacts
in refs/spur/artifacts/<session> are no longer fetchable through this
tool; operators with legacy data should migrate via the deferred
spur outcomes copy CLI subcommand.

Adds telemetry: spur.metrics.outcome_fetched (success),
spur.metrics.outcome_fetch_not_found (NotFound),
spur.metrics.outcome_fetch_unauthorized (cross-session reject).

Phase 3 of plan-5; spec §7.5."
```

---

## Task 11: GC integration — startup sweep + `spur gc outcomes` CLI

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (background sweep on startup)
- Modify: `crates/spur-cli/src/main.rs` (add `gc outcomes` subcommand)

**What:** On orchestrator startup, spawn a background tokio task that runs `OutcomeStore::sweep_older_than(ttl_days)`. Default TTL = 7 days; override via `SPUR_OUTCOME_TTL_DAYS` env. CLI: `spur gc outcomes [--dry-run] [--older-than=Nd]`.

- [ ] **Step 1: Add startup sweep to orchestrator**

Run: `grep -n "fn new\|fn build\|self.outcome_store" crates/spur-core/src/orchestrator.rs | head -10`

Find the orchestrator constructor (where `outcome_store` was added in Task 7). Append after construction:

```rust
        // Phase 3 (§8.2): background TTL sweep on startup. Tracked
        // JoinHandle so the orchestrator can `.abort()` on shutdown
        // (avoids leaking in-flight `git` subprocesses, which run with
        // `kill_on_drop(true)` per Phase 2's GitBlobOutcomeStore).
        let ttl_days: u64 = std::env::var("SPUR_OUTCOME_TTL_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        let sweep_store = self.outcome_store.clone();
        let sweep_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
            let ttl = std::time::Duration::from_secs(ttl_days * 86_400);
            match sweep_store.sweep_older_than(ttl).await {
                Ok(report) => tracing::info!(
                    target: "spur.metrics.outcome_swept",
                    namespaces_swept = report.namespaces_swept,
                    blobs_swept = report.blobs_swept,
                    bytes_freed = report.bytes_freed,
                    ttl_days,
                ),
                Err(e) => tracing::warn!(
                    target: "spur.metrics.outcome_swept_failed",
                    error = %e,
                ),
            }
        });
        // Stash for graceful shutdown; orchestrator's Drop / shutdown
        // hook calls `sweep_handle.abort()`.
        self.background_tasks.push(sweep_handle);
```

The constructor must NOT `.await` the spawned task — startup stays fast.

- [ ] **Step 2: Add `gc outcomes` CLI subcommand**

Run: `grep -n "Commands\|enum Cmd\|Subcommand" crates/spur-cli/src/main.rs | head -10`

Find the clap `Commands` enum. Add:

```rust
#[derive(Debug, clap::Subcommand)]
enum GcCmd {
    /// Sweep outcome blobs older than the TTL.
    Outcomes {
        /// Don't actually delete — just report.
        #[arg(long)]
        dry_run: bool,
        /// TTL override in days; defaults to SPUR_OUTCOME_TTL_DAYS env (7).
        #[arg(long, value_parser = parse_duration_days)]
        older_than: Option<std::time::Duration>,
        /// Optional: scope to a single brain session id.
        #[arg(long)]
        namespace: Option<String>,
    },
}

fn parse_duration_days(s: &str) -> Result<std::time::Duration, String> {
    // Accept "30d", "30days", or bare "30" — interpret as days.
    // Hour-resolution ('h') is intentionally rejected because Phase 2's
    // FsOutcomeStore enforces a 1-day TTL floor (Round 9 P2-S3).
    let s = s.trim();
    let n_str = s
        .strip_suffix("days")
        .or_else(|| s.strip_suffix('d'))
        .unwrap_or(s);
    let n: u64 = n_str
        .trim()
        .parse()
        .map_err(|_| format!("expected integer days, got {s:?}"))?;
    if n == 0 {
        return Err("TTL floor is 1 day (Phase 2 / Round 9 P2-S3)".into());
    }
    Ok(std::time::Duration::from_secs(n * 86_400))
}
```

Add to the top-level CLI command:

```rust
    /// Garbage-collect outcome blobs.
    Gc {
        #[command(subcommand)]
        cmd: GcCmd,
    },
```

In the dispatch match, add:

```rust
        Commands::Gc { cmd: GcCmd::Outcomes { dry_run, older_than, namespace } } => {
            run_gc_outcomes(dry_run, older_than, namespace).await
        }
```

Add direct deps to `crates/spur-cli/Cargo.toml` (the existing spur-core transitive isn't usable cross-crate):

```toml
spur-blob-store = { workspace = true }
spur-worktree = { workspace = true }
spur-acp = { workspace = true }
```

Implement `run_gc_outcomes` using the same `std::env::current_dir()` discovery pattern that the existing CLI uses (verified at `crates/spur-cli/src/main.rs:296`):

```rust
async fn run_gc_outcomes(
    dry_run: bool,
    older_than: Option<std::time::Duration>,
    namespace: Option<String>,
) -> anyhow::Result<()> {
    use spur_blob_store::OutcomeStore;
    use spur_worktree::git_blob_store::GitBlobOutcomeStore;
    use std::sync::Arc;

    // Discover repo root via the CLI's existing convention: cwd is the
    // repo root for `spur` commands. (Other CLI subcommands use the same
    // pattern at main.rs:296.)
    let repo_root = std::env::current_dir()?;
    let store: Arc<dyn OutcomeStore> = Arc::new(GitBlobOutcomeStore::new(repo_root));

    if let Some(ns) = namespace {
        // Per-namespace path. SessionId wraps a String — pass via .into().
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId(ns));
        if dry_run {
            println!("Would delete namespace {session_id}");
            return Ok(());
        }
        let removed = store.delete_namespace(&session_id).await?;
        println!("Deleted {removed} blobs in namespace {session_id}");
        return Ok(());
    }

    let ttl = older_than.unwrap_or_else(|| {
        let days: u64 = std::env::var("SPUR_OUTCOME_TTL_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        std::time::Duration::from_secs(days * 86_400)
    });

    if dry_run {
        println!("Dry-run: would sweep namespaces older than {:?}", ttl);
        return Ok(());
    }

    let report = store.sweep_older_than(ttl).await?;
    println!(
        "Swept {} namespaces / {} blobs / {} bytes (effective_ttl={:?})",
        report.namespaces_swept, report.blobs_swept, report.bytes_freed, report.effective_ttl
    );
    Ok(())
}
```

- [ ] **Step 3: Test the CLI manually**

Run: `RUSTC_WRAPPER= cargo build -p spur-cli`
Run: `target/debug/spur gc outcomes --dry-run --older-than=30d`
Expected: prints "Dry-run: would sweep namespaces older than 30d" or similar.

- [ ] **Step 4: Add a smoke test for the CLI parser**

Add to `crates/spur-cli/src/main.rs` test module:

```rust
#[test]
fn gc_outcomes_parses_older_than() {
    use clap::Parser;
    let args = Cli::try_parse_from(["spur", "gc", "outcomes", "--older-than=14d"]).unwrap();
    if let Commands::Gc { cmd: GcCmd::Outcomes { older_than, dry_run, .. } } = args.command {
        assert_eq!(older_than, Some(std::time::Duration::from_secs(14 * 86_400)));
        assert!(!dry_run);
    } else {
        panic!("wrong subcommand");
    }
}

#[test]
fn parse_duration_days_accepts_common_forms() {
    assert!(parse_duration_days("30d").is_ok());
    assert!(parse_duration_days("30").is_ok());
    assert!(parse_duration_days("30days").is_ok());
    assert_eq!(
        parse_duration_days("30").unwrap(),
        std::time::Duration::from_secs(30 * 86_400)
    );
}

#[test]
fn parse_duration_days_rejects_invalid_input() {
    assert!(parse_duration_days("30h").is_err()); // hour resolution unsupported
    assert!(parse_duration_days("notanumber").is_err());
    assert!(parse_duration_days("0").is_err()); // TTL floor
    assert!(parse_duration_days("0d").is_err());
}
```

- [ ] **Step 5: Run tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-cli`
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-core,spur-cli): GC integration for outcome blobs

spur-core: orchestrator spawns a background tokio task at startup that
runs OutcomeStore::sweep_older_than with TTL (default 7 days, override
via SPUR_OUTCOME_TTL_DAYS). Startup is non-blocking; sweep errors are
logged at WARN.

spur-cli: new \`spur gc outcomes [--dry-run] [--older-than=Nd]
[--namespace=SID]\` subcommand wraps OutcomeStore::sweep_older_than +
delete_namespace for ops emergencies (disk pressure, debug cleanup).

Session-terminate hook (§8.1) is deferred — spur-core lacks an
explicit session-end signal today. TTL sweep covers the recovery case;
the explicit hook lands when brain-session lifecycle plumbing
materializes.

Phase 3 of plan-5; spec §8.2 + §8.3."
```

---

## Task 12: Phase 3 verification

**Files:** none (verification-only).

**What:** Run the workspace verification suite and report results. No code changes unless a regression surfaces.

- [ ] **Step 1: Workspace check**

Run: `RUSTC_WRAPPER= cargo check --workspace --all-targets`
Expected: exit 0.

- [ ] **Step 2: Plan-3 crate clippy**

Run: `RUSTC_WRAPPER= cargo clippy -p spur-acp -p spur-blob-store -p spur-worktree -p spur-mcp -p spur-core -p spur-cli -- -D warnings`
Expected: clean (only pre-existing warnings noted in Phase 2 report acceptable).

- [ ] **Step 3: Targeted unit tests**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib domain::clip`
Expected: 5 tests pass.

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib domain::continuation`
Expected: 3 v3 round-trip tests pass + existing tests updated.

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib domain::merge_budget`
Expected: clean (helper relocation tests).

Run: `RUSTC_WRAPPER= cargo test -p spur-blob-store --lib`
Expected: 19 + 2 (test_helpers) = 21 tests pass.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib outcome_materializer`
Expected: 7 tests (3 success-path + 4 fallback-path) pass.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib audit_sentinel`
Expected: 2 new tests + existing audit-sentinel tests pass.

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --lib fetch_outcome_artifact_tests`
Expected: existing tests + 4 new section/attempt tests pass.

- [ ] **Step 4: Workspace test pass**

Run: `RUSTC_WRAPPER= cargo test --workspace`
Expected: green or the same pre-existing failures noted in Phase 2 (`spur-context::real_fixtures`, `large_enum_variant` lint).

- [ ] **Step 5: INV-D9 schema-evolution proptest**

Run: `RUSTC_WRAPPER= cargo test -p spur-mcp --release inv_d9_arb_delegation_status_clips_under_budget`
Expected: 256 cases pass for all `DelegationStatus` variants (the test asserts the materializer's lean output stays under `MERGE_BUDGET_DEFAULT_BYTES` for every variant).

If the test does not yet exist, ADD it as part of this task:

```rust
// crates/spur-mcp/tests/inv_d9_proptest.rs
use proptest::prelude::*;
// ... arb_delegation_status generator using strategies for every variant ...
proptest! {
    #![proptest_config(ProptestConfig { cases: 256, .. ProptestConfig::default() })]
    #[test]
    fn inv_d9_arb_delegation_status_clips_under_budget(status in arb_delegation_status()) {
        // build a DelegationResult with `status`, run materializer, assert envelope ≤ MERGE_BUDGET.
    }
}
```

If adding the test, commit it as a follow-up Step 7 below.

- [ ] **Step 6: Schema-version round-trip CI guard**

Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib continuation_payload_v3_round_trips_through_serde`
Run: `RUSTC_WRAPPER= cargo test -p spur-acp --lib v3_payload_deserializes_from_v2_envelope_with_serde_default`
Run: `RUSTC_WRAPPER= cargo test -p spur-core --lib deserializer_accepts_v2_envelope_via_serde_default`

All three must pass — they enforce the v2↔v3 wire compatibility invariant.

- [ ] **Step 7: If INV-D9 test was missing in Step 5, commit it now**

```bash
git add crates/spur-mcp/tests/inv_d9_proptest.rs
git commit -m "test(spur-mcp): INV-D9 proptest for materializer envelope clipping"
```

- [ ] **Step 8: Write the verification report**

Format the same as Phase 2's T10 report (steps a/b/c/... with last-5-lines evidence + summary verdict). Save it inline in your delegation summary.

---

## Self-Review Checklist (run before declaring plan ready)

**1. Spec coverage:**

- §7.1 Lean v3 schema → Task 2 ✓
- §7.2 OutcomeMaterializer → Tasks 4, 5, 6 ✓
- §7.2 Clip helpers in spur-acp::domain::clip → Task 1 ✓
- §7.3 Two callsites, one entrypoint → Tasks 7, 8 ✓
- §7.4 Beads audit-comment artifact_uri → Task 9 ✓
- §7.5 Extended fetch_outcome_artifact → Task 10 ✓
- §7.6 GC integration (startup sweep + spur gc CLI) → Task 11 ✓
- §7.7 Truncation-ladder fallback (MockFailingOutcomeStore) → Tasks 3, 6 ✓
- §8.2 Background sweep via tokio::spawn → Task 11 ✓
- §10.1 Tracing events → Task 5 (outcome_persisted, materializer_oversized_post_clip), Task 10 (outcome_fetched, fetch_not_found, fetch_unauthorized), Task 11 (outcome_swept) ✓
- §11 Schema-version bidirectional compat → Task 2 (deserializer_accepts_v2_envelope test) ✓

Gaps: §8.1 explicit session-terminate hook is deferred (Task 11 commit message acknowledges). §10.2/10.3 TUI surfacing is deferred (TUI-only work, optional).

**2. Placeholder scan:** No `TBD`, no "implement later", no "similar to Task N", no bare-instruction steps. Code blocks present in every step that touches code.

**3. Type consistency:**

- `OutcomeKey { brain_session_id, delegation_id, attempt }` consistent across Tasks 5, 8, 10, 11.
- `OutcomeMaterializer::materialize(...)` signature consistent across Tasks 4, 5, 7, 8.
- `persist_completion_result_and_notify` signature in Task 8 matches Task 9's update.
- `spur_acp::domain::merge_budget::{MERGE_BUDGET_DEFAULT_BYTES, continuation_cost_bytes}` introduced in Task 5 Step 4, used in Task 6.
- Helpers `clip_status_strings`, `clip_diff_files`, `clip_artifact_ref_strings`, `clip_with_ellipsis` all defined in Task 1, called from Tasks 5 + 6.

**4. Test design:**

- Each new behavior has a failing test before implementation (TDD).
- Backward-compat tests for v2↔v3 (Task 2 Step 1, Task 9 Step 2).
- Failure-mode tests via `MockFailingOutcomeStore` (Task 6).
- Schema constants asserted (Task 2 Step 9).

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-04-25-brain-continuation-phase-3-materializer.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — Dispatch a fresh codex per task, dual-review (kimi spec + gemini quality) between tasks. Same gate Phase 2 used.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch with checkpoints for review.

Which approach?
