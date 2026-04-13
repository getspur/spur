# Orchestrator Review Loopback Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the observe → review → next-execute loop so TUI `ReviewDecision`s translate into typed `DelegationStatus` outcomes the brain sees.

**Architecture:** Insert an optional review gate in `execute_delegation` after the worker produces a candidate `DelegationResult`. A `pending_reviews: Arc<Mutex<HashMap<ExecutorId, (u32, oneshot::Sender<ReviewDecision>)>>>` routes TUI decisions back to the awaiting delegation task via a separate user-input dispatcher task. `Retry` loops internally (bounded); `Approve/Reject/Modify/TimedOut` surface as distinct `DelegationStatus` variants.

**Tech Stack:** Rust 2021 (MSRV 1.88), tokio (mpsc, oneshot, Mutex, broadcast, select!, time::pause), serde, tracing, existing spur-acp/spur-core/spur-tui/spur-cli workspace.

**Spec:** `docs/superpowers/specs/2026-04-13-orchestrator-review-loopback-design.md`

---

## File Structure

**Create:**
- `crates/spur-core/src/review_sink.rs` — thin wrapper around `pending_reviews` HashMap; exposes `register(executor_id, attempt_n) -> oneshot::Receiver`, `submit(executor_id, attempt_n, decision)`, `remove(executor_id)`. One clear responsibility: correlation-id routing with attempt-n supersession guard.

**Modify:**
- `crates/spur-acp/src/domain/delegation.rs` — add `Rejected`, `Modified`, `TimedOut` variants; add `TimeoutFallback` enum; mark `DelegationStatus` as `#[non_exhaustive]`.
- `crates/spur-acp/src/domain/events.rs` — add `attempt_n: u32` to `ExecutorReviewRequested`; add `ExecutorReviewCancelled { id: String, reason: String }` body variant.
- `crates/spur-acp/src/config.rs` — add `AgentReviewPolicy` struct; add `review` field to `AgentConfig`.
- `crates/spur-acp/src/lib.rs` — re-export `AgentReviewPolicy`, `TimeoutFallback`.
- `crates/spur-core/src/orchestrator.rs` — add `review_sink: Arc<ReviewSink>` field; add `InteractiveInput::SubmitReview`; spawn dispatcher task in `run_interactive` (or add a second input channel wired in at construction); thread `review_sink` through `handle_delegations` → `execute_delegation`; insert review gate; retry loop; worktree preservation on `Rejected`/`TimedOut`; brain-cancellation audit emission.
- `crates/spur-core/src/lineage/projection.rs` — handle new `attempt_n` field on `ExecutorReviewRequested` (set on `ReviewRequest`); handle `ExecutorReviewCancelled` (clear `pending_review`, log). Update existing `match` arms to accommodate new event.
- `crates/spur-core/src/lineage/types.rs` — add `attempt_n: u32` to `ReviewRequest`.
- `crates/spur-core/src/lib.rs` — re-export `ReviewSink`, `TimeoutFallback`.
- `crates/spur-tui/src/app.rs` — extend `UserInput::SubmitReview` with `attempt_n: u32`.
- `crates/spur-tui/src/action.rs` — extend `Action::SubmitReview` with `attempt_n: u32`.
- `crates/spur-tui/src/views/dashboard.rs` — read `attempt_n` from focused node's pending_review; pass through to `Action::SubmitReview`.
- `crates/spur-cli/src/main.rs:393` — replace the TODO stub with a send into the orchestrator's review dispatcher channel.
- Match sites for new `DelegationStatus` variants: `crates/spur-core/src/lineage/adapter.rs`, `crates/spur-tui/src/views/dashboard.rs`, `crates/spur-tui/src/views/session_detail.rs`, any activity-log renderer.

**Test files touched/created:**
- `crates/spur-acp/tests/executor_events_roundtrip.rs` — extend with new variant round-trips.
- `crates/spur-core/tests/review_sink.rs` — new (unit tests for ReviewSink).
- `crates/spur-core/tests/review_gate_integration.rs` — new (integration: gate + decision → status).
- `crates/spur-tui/tests/review_submission.rs` — extend to cover `attempt_n`.

---

## Task 1: Expand `DelegationStatus` enum + add `TimeoutFallback`

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs`
- Modify: `crates/spur-acp/src/lib.rs` (re-export `TimeoutFallback`)
- Test: `crates/spur-acp/tests/delegation_status_roundtrip.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/delegation_status_roundtrip.rs`:

```rust
use spur_acp::{DelegationStatus, TimeoutFallback};
use std::time::Duration;

fn roundtrip(status: &DelegationStatus) {
    let json = serde_json::to_string(status).expect("serialize");
    let back: DelegationStatus = serde_json::from_str(&json).expect("deserialize");
    let json2 = serde_json::to_string(&back).expect("re-serialize");
    assert_eq!(json, json2, "round-trip mismatch");
}

#[test]
fn every_variant_round_trips() {
    roundtrip(&DelegationStatus::Success);
    roundtrip(&DelegationStatus::Failed {
        error: "boom".into(),
    });
    roundtrip(&DelegationStatus::Conflict { files: vec![] });
    roundtrip(&DelegationStatus::Timeout);
    roundtrip(&DelegationStatus::Rejected {
        reason: "too large".into(),
    });
    roundtrip(&DelegationStatus::Modified {
        reviewer_note: "fix naming".into(),
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Approve,
    });
    roundtrip(&DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    });
}

#[test]
fn rejected_is_distinguishable_from_timed_out_reject() {
    let human = DelegationStatus::Rejected {
        reason: "refactor this".into(),
    };
    let system = DelegationStatus::TimedOut {
        waited_for: Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    };
    let j_human = serde_json::to_value(&human).unwrap();
    let j_system = serde_json::to_value(&system).unwrap();
    assert_ne!(j_human, j_system);
    assert!(j_human.to_string().contains("Rejected"));
    assert!(j_system.to_string().contains("TimedOut"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test delegation_status_roundtrip`
Expected: FAIL — `Rejected`, `Modified`, `TimedOut`, `TimeoutFallback` don't exist yet.

- [ ] **Step 3: Extend `DelegationStatus` and add `TimeoutFallback`**

Replace the contents of `crates/spur-acp/src/domain/delegation.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Result status of a delegation to a worker.
///
/// `Rejected` is reserved for human-issued rejections arriving via the
/// review gate. System-applied timeouts use `TimedOut` so the brain can
/// distinguish actionable feedback from "nobody reviewed in time."
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DelegationStatus {
    // Pre-existing worker-level variants.
    Success,
    Failed { error: String },
    Conflict { files: Vec<PathBuf> },
    /// Worker hung past the hard worker-hang deadline (distinct from
    /// review-gate timeout, which is `TimedOut`).
    Timeout,

    // Review-gate variants.
    /// Human reviewer rejected the work. `reason` is actionable feedback
    /// the brain can address on a retry.
    Rejected { reason: String },
    /// Human reviewer approved-with-modifications; `reviewer_note` is a
    /// caveat the brain should consider alongside the accepted diff.
    Modified { reviewer_note: String },
    /// Review timeout fired. `fallback` records the configured
    /// `TimeoutFallback` that was applied.
    TimedOut {
        #[serde(with = "duration_serde")]
        waited_for: Duration,
        fallback: TimeoutFallback,
    },
}

/// Policy for what to apply when a review gate's timeout fires.
///
/// Shared by `AgentReviewPolicy::review_timeout_default` (config input)
/// and `DelegationStatus::TimedOut.fallback` (status discriminant).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TimeoutFallback {
    /// Auto-approve — worker's diff/summary retained as if reviewed.
    Approve,
    /// Auto-reject — carries the configured reason.
    Reject { reason: String },
    /// Explicit "nobody reviewed" signal (headless/batch modes).
    Abandon,
}

/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
}

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}
```

- [ ] **Step 4: Re-export `TimeoutFallback` from `spur-acp`**

Edit `crates/spur-acp/src/lib.rs` — wherever `DelegationStatus`/`DelegationResult` are re-exported, add `TimeoutFallback`:

```rust
pub use domain::delegation::{DelegationResult, DelegationStatus, TimeoutFallback};
```

(If the re-export is `pub use domain::delegation::*;` then `TimeoutFallback` is already exported. Confirm.)

- [ ] **Step 5: Patch existing `match status { ... }` sites to compile under `#[non_exhaustive]`**

Run: `cargo build --workspace`
Observe which files fail to compile with non-exhaustive match errors. Known sites (add a `_ => unreachable!("new variant — handle in downstream spec task")` or a sensible fall-through, one-line each):

- `crates/spur-core/src/lineage/adapter.rs` — wherever `DelegationCompleted { status, .. }` is matched.
- `crates/spur-tui/src/views/dashboard.rs` — `DelegationCompleted` match site.
- `crates/spur-tui/src/views/session_detail.rs` — same.
- Any activity-log renderer.

For each match, add only the necessary wildcard arm to compile — the semantic handling for Rejected/Modified/TimedOut comes in later tasks:

```rust
// Temporary fall-through — replaced in Task 11 with variant-specific rendering.
_ => { /* handled in Task 11 */ }
```

- [ ] **Step 6: Run the tests and make sure they pass**

Run: `cargo test -p spur-acp --test delegation_status_roundtrip`
Expected: PASS (both tests).

Run: `cargo build --workspace`
Expected: clean build.

Run: `cargo test --workspace`
Expected: all pre-existing tests still pass (no semantic regressions from the new variants — they are additive).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-acp/src/domain/delegation.rs crates/spur-acp/src/lib.rs \
        crates/spur-acp/tests/delegation_status_roundtrip.rs \
        crates/spur-core/src/lineage/adapter.rs \
        crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-acp): expand DelegationStatus with review-gate variants

Adds Rejected, Modified, TimedOut { waited_for, fallback } variants and
a shared TimeoutFallback enum used by both AgentReviewPolicy config and
the TimedOut status discriminant. DelegationStatus is now #[non_exhaustive].
Match sites updated with stub arms; semantic handling follows in later
tasks."
```

---

## Task 2: Add `attempt_n` to `ExecutorReviewRequested` event

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-core/src/lineage/types.rs`
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Test: `crates/spur-acp/tests/executor_events_roundtrip.rs` (extend)

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-acp/tests/executor_events_roundtrip.rs`:

```rust
#[test]
fn executor_review_requested_carries_attempt_n() {
    use spur_acp::{ReviewKind, ReviewPayload, SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 2,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_value(&event).unwrap();
    assert_eq!(j["body"]["ExecutorReviewRequested"]["attempt_n"], 2);
    let _back: SpurEvent = serde_json::from_value(j).expect("round-trip");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test executor_events_roundtrip executor_review_requested_carries_attempt_n`
Expected: FAIL — no `attempt_n` field.

- [ ] **Step 3: Add `attempt_n` to the event body**

Edit `crates/spur-acp/src/domain/events.rs`. Find:

```rust
    ExecutorReviewRequested {
        id: String,
        kind: ReviewKind,
        payload: ReviewPayload,
        // Note: requested_at removed — envelope `occurred_at` carries it now.
    },
```

Replace with:

```rust
    ExecutorReviewRequested {
        id: String,
        /// Which attempt this review gates. Propagated back via
        /// `UserInput::SubmitReview` for supersession guard.
        attempt_n: u32,
        kind: ReviewKind,
        payload: ReviewPayload,
    },
```

- [ ] **Step 4: Add `attempt_n` to the projection's `ReviewRequest`**

Edit `crates/spur-core/src/lineage/types.rs`. Find the `ReviewRequest` struct and add:

```rust
pub struct ReviewRequest {
    pub kind: ReviewKind,
    pub payload: ReviewPayload,
    pub requested_at: SystemTime,
    /// Carried from the event; used by the dispatcher to reject stale
    /// decisions targeting a superseded attempt.
    pub attempt_n: u32,
}
```

- [ ] **Step 5: Wire `attempt_n` through the projection**

Edit `crates/spur-core/src/lineage/projection.rs`. Find the `ExecutorReviewRequested` arm in `apply_inner` and update its `ReviewRequest` construction to include `attempt_n: *attempt_n`. If the arm currently destructures `{ id, kind, payload }`, change to `{ id, attempt_n, kind, payload }`.

- [ ] **Step 6: Patch emission sites to supply `attempt_n`**

Run: `cargo build --workspace`
Expected errors: call sites that construct `ExecutorReviewRequested { id, kind, payload }` without `attempt_n`.

For each site (there are at most a handful; they exist in tests/fixtures today because the orchestrator emission is added in Task 7), add `attempt_n: 1` as the default value for the first-attempt case.

- [ ] **Step 7: Run tests and confirm pass**

Run: `cargo test -p spur-acp --test executor_events_roundtrip`
Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "feat(spur-acp): add attempt_n to ExecutorReviewRequested

Propagates attempt_n through the event so the dispatcher can reject
stale review decisions targeting a superseded attempt. Projection's
ReviewRequest gains an attempt_n field carried from the event."
```

---

## Task 3: Add `ExecutorReviewCancelled` event

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Test: `crates/spur-acp/tests/executor_events_roundtrip.rs` (extend)
- Test: `crates/spur-core/tests/lineage_projection.rs` (extend)

- [ ] **Step 1: Write the failing test (event round-trip)**

Add to `crates/spur-acp/tests/executor_events_roundtrip.rs`:

```rust
#[test]
fn executor_review_cancelled_round_trips() {
    use spur_acp::{SpurEvent, SpurEventBody};
    let body = SpurEventBody::ExecutorReviewCancelled {
        id: "exec-1".into(),
        reason: "brain call cancelled".into(),
    };
    let event = SpurEvent::now(body);
    let j = serde_json::to_string(&event).expect("serialize");
    let _back: SpurEvent = serde_json::from_str(&j).expect("round-trip");
    assert!(j.contains("ExecutorReviewCancelled"));
    assert!(j.contains("brain call cancelled"));
}
```

- [ ] **Step 2: Write the failing test (projection clears pending_review)**

Add to `crates/spur-core/tests/lineage_projection.rs`:

```rust
#[test]
fn review_cancelled_clears_pending_review() {
    use spur_acp::{ReviewKind, ReviewPayload, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ExecutorLineage};
    let mut lineage = ExecutorLineage::default();
    // Spawn + request review first (uses existing helpers if present; else construct events inline).
    let spawn = SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: None,
        session_id: spur_acp::SessionId::new(),
        agent: "worker".into(),
        role: spur_acp::Role::Executor,
        task_spec: "t".into(),
    });
    lineage.apply(&spawn);
    let req = SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-1".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
    });
    lineage.apply(&req);
    assert!(lineage
        .node(&ExecutorId("exec-1".into()))
        .unwrap()
        .pending_review
        .is_some());

    let cancel = SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: "exec-1".into(),
        reason: "brain cancel".into(),
    });
    lineage.apply(&cancel);
    assert!(lineage
        .node(&ExecutorId("exec-1".into()))
        .unwrap()
        .pending_review
        .is_none());
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p spur-acp --test executor_events_roundtrip executor_review_cancelled_round_trips`
Run: `cargo test -p spur-core --test lineage_projection review_cancelled_clears_pending_review`
Expected: both FAIL — `ExecutorReviewCancelled` doesn't exist.

- [ ] **Step 4: Add the event body variant**

Edit `crates/spur-acp/src/domain/events.rs`. Add to the `SpurEventBody` enum next to `ExecutorReviewResolved`:

```rust
    /// The orchestrator abandoned a pending review (e.g., because the
    /// brain's tool call was cancelled). Emitted so the lineage
    /// projection records the abandonment rather than showing a silent
    /// disappearance.
    ExecutorReviewCancelled {
        id: String,
        reason: String,
    },
```

- [ ] **Step 5: Handle the new variant in the projection**

Edit `crates/spur-core/src/lineage/projection.rs`. Add an arm in `apply_inner`'s `match body`:

```rust
        SpurEventBody::ExecutorReviewCancelled { id, reason } => {
            let exec_id = ExecutorId(id.clone());
            if let Some(node) = self.nodes.get_mut(&exec_id) {
                node.pending_review = None;
                tracing::info!(
                    executor_id = %id,
                    reason = %reason,
                    "review cancelled — pending_review cleared"
                );
            }
        }
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-acp --test executor_events_roundtrip`
Run: `cargo test -p spur-core --test lineage_projection`
Expected: both PASS.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "feat(spur-acp): add ExecutorReviewCancelled event

Adds a body variant the orchestrator emits when it abandons a pending
review (e.g., brain tool-call cancellation). Projection clears
pending_review on receipt so the lineage UI does not show orphaned
review cards."
```

---

## Task 4: Add `AgentReviewPolicy` config

**Files:**
- Modify: `crates/spur-acp/src/config.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Test: `crates/spur-acp/tests/agent_review_policy.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/agent_review_policy.rs`:

```rust
use spur_acp::config::{AgentConfig, AgentReviewPolicy};
use spur_acp::TimeoutFallback;
use std::time::Duration;

#[test]
fn review_defaults_when_section_absent() {
    let toml_src = r#"
name = "codex"
command = "codex"
transport = "stdio"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.review.review_required, false);
    assert_eq!(cfg.review.review_timeout, Duration::from_secs(30 * 60));
    assert_eq!(cfg.review.max_review_retries, 3);
    assert!(matches!(
        cfg.review.review_timeout_default,
        TimeoutFallback::Reject { .. }
    ));
}

#[test]
fn review_reads_explicit_values() {
    let toml_src = r#"
name = "codex"
command = "codex"
transport = "stdio"

[review]
review_required = true
review_timeout_secs = 60
max_review_retries = 5

[review.review_timeout_default]
Approve = {}
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.review.review_required);
    assert_eq!(cfg.review.review_timeout, Duration::from_secs(60));
    assert_eq!(cfg.review.max_review_retries, 5);
    assert_eq!(cfg.review.review_timeout_default, TimeoutFallback::Approve);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-acp --test agent_review_policy`
Expected: FAIL — `AgentReviewPolicy` / `cfg.review` don't exist.

- [ ] **Step 3: Add `AgentReviewPolicy` and wire into `AgentConfig`**

Edit `crates/spur-acp/src/config.rs`. Add at top-level:

```rust
use crate::domain::delegation::TimeoutFallback;

/// Per-agent human-review policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReviewPolicy {
    /// When true, the orchestrator gates every delegation to this agent
    /// on a human review. Default: false.
    #[serde(default)]
    pub review_required: bool,
    /// How long to wait for a human decision before applying the default.
    #[serde(
        default = "default_review_timeout",
        rename = "review_timeout_secs",
        with = "duration_secs_serde"
    )]
    pub review_timeout: Duration,
    /// What to apply on timeout. Default:
    /// `TimeoutFallback::Reject { reason: "review timeout" }`.
    #[serde(default = "default_review_timeout_default")]
    pub review_timeout_default: TimeoutFallback,
    /// Cap on `Retry` loops. Default: 3.
    #[serde(default = "default_max_review_retries")]
    pub max_review_retries: u32,
}

impl Default for AgentReviewPolicy {
    fn default() -> Self {
        Self {
            review_required: false,
            review_timeout: default_review_timeout(),
            review_timeout_default: default_review_timeout_default(),
            max_review_retries: default_max_review_retries(),
        }
    }
}

fn default_review_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}
fn default_review_timeout_default() -> TimeoutFallback {
    TimeoutFallback::Reject {
        reason: "review timeout".into(),
    }
}
fn default_max_review_retries() -> u32 {
    3
}

mod duration_secs_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;
    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        d.as_secs().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        u64::deserialize(d).map(Duration::from_secs)
    }
}
```

Add to `AgentConfig`:

```rust
    #[serde(default)]
    pub review: AgentReviewPolicy,
```

- [ ] **Step 4: Re-export from `spur-acp`**

Edit `crates/spur-acp/src/lib.rs`:

```rust
pub use config::{AgentConfig, AgentReviewPolicy, ...};  // merge with existing re-exports
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-acp --test agent_review_policy`
Expected: both PASS.

Run: `cargo test --workspace`
Expected: no regressions (the new field has a default, so pre-existing `AgentConfig` constructions still compile).

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat(spur-acp): add AgentReviewPolicy config

Per-agent review policy: review_required (bool), review_timeout,
review_timeout_default (shared TimeoutFallback type), max_review_retries.
Defaults preserve existing behavior (review_required=false)."
```

---

## Task 5: Create `ReviewSink` (pending_reviews wrapper)

**Files:**
- Create: `crates/spur-core/src/review_sink.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Test: `crates/spur-core/tests/review_sink.rs` (create)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-core/tests/review_sink.rs`:

```rust
use spur_acp::ReviewDecision;
use spur_core::{ExecutorId, ReviewSink};

#[tokio::test]
async fn register_then_submit_delivers_decision() {
    let sink = ReviewSink::new();
    let rx = sink
        .register(ExecutorId("e1".into()), 1)
        .await
        .expect("registered");
    let submitted = sink
        .submit(ExecutorId("e1".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(submitted, "submit should succeed");
    let decision = rx.await.expect("decision");
    assert!(matches!(decision, ReviewDecision::Approve));
}

#[tokio::test]
async fn attempt_n_mismatch_drops_decision() {
    let sink = ReviewSink::new();
    let rx = sink
        .register(ExecutorId("e1".into()), 2)
        .await
        .expect("registered");
    let submitted = sink
        .submit(
            ExecutorId("e1".into()),
            1, // stale attempt
            ReviewDecision::Reject {
                reason: "r".into(),
            },
        )
        .await;
    assert!(!submitted, "stale attempt_n must be dropped");
    // Sender still in place — legitimate attempt-2 reviewer can still submit.
    let submitted2 = sink
        .submit(ExecutorId("e1".into()), 2, ReviewDecision::Approve)
        .await;
    assert!(submitted2);
    let decision = rx.await.expect("decision");
    assert!(matches!(decision, ReviewDecision::Approve));
}

#[tokio::test]
async fn unknown_executor_id_is_dropped() {
    let sink = ReviewSink::new();
    let submitted = sink
        .submit(ExecutorId("unknown".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(!submitted);
}

#[tokio::test]
async fn remove_cleans_up_entry() {
    let sink = ReviewSink::new();
    let _rx = sink
        .register(ExecutorId("e1".into()), 1)
        .await
        .expect("registered");
    sink.remove(&ExecutorId("e1".into())).await;
    let submitted = sink
        .submit(ExecutorId("e1".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(!submitted);
}

#[tokio::test]
async fn double_register_fails() {
    let sink = ReviewSink::new();
    let _rx1 = sink
        .register(ExecutorId("e1".into()), 1)
        .await
        .expect("first");
    let second = sink.register(ExecutorId("e1".into()), 2).await;
    assert!(second.is_err(), "must not overwrite active entry");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test review_sink`
Expected: FAIL — `ReviewSink` doesn't exist.

- [ ] **Step 3: Implement `ReviewSink`**

Create `crates/spur-core/src/review_sink.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use spur_acp::ReviewDecision;
use tokio::sync::{oneshot, Mutex};

use crate::ExecutorId;

/// Routes TUI `ReviewDecision`s back to the orchestrator task that is
/// awaiting one for a specific `(executor_id, attempt_n)`.
///
/// Internally a map `ExecutorId → (attempt_n, oneshot::Sender)`. The
/// attempt_n guard prevents a stale decision (e.g., for a superseded
/// attempt) from delivering to the sender registered for the next
/// attempt.
pub struct ReviewSink {
    inner: Arc<Mutex<HashMap<ExecutorId, (u32, oneshot::Sender<ReviewDecision>)>>>,
}

impl ReviewSink {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending review. Returns the receiver the caller awaits.
    /// Errors if an entry already exists for this executor_id.
    pub async fn register(
        &self,
        executor_id: ExecutorId,
        attempt_n: u32,
    ) -> Result<oneshot::Receiver<ReviewDecision>, ReviewSinkError> {
        let (tx, rx) = oneshot::channel();
        let mut map = self.inner.lock().await;
        if map.contains_key(&executor_id) {
            return Err(ReviewSinkError::AlreadyRegistered);
        }
        map.insert(executor_id, (attempt_n, tx));
        Ok(rx)
    }

    /// Submit a decision. Returns true if routed, false if dropped
    /// (unknown executor_id or attempt_n mismatch).
    pub async fn submit(
        &self,
        executor_id: ExecutorId,
        attempt_n: u32,
        decision: ReviewDecision,
    ) -> bool {
        let mut map = self.inner.lock().await;
        match map.get(&executor_id) {
            Some((stored, _)) if *stored != attempt_n => {
                tracing::warn!(
                    executor_id = %executor_id.0,
                    got = attempt_n,
                    expected = *stored,
                    "review decision dropped — attempt_n mismatch"
                );
                false
            }
            Some(_) => {
                // attempt_n matches — pop and send.
                let (_, tx) = map.remove(&executor_id).expect("checked above");
                tx.send(decision).is_ok()
            }
            None => {
                tracing::warn!(
                    executor_id = %executor_id.0,
                    "review decision dropped — no pending review registered"
                );
                false
            }
        }
    }

    /// Explicitly remove a pending review (used by timeout and
    /// brain-cancellation paths to avoid stale entries).
    pub async fn remove(&self, executor_id: &ExecutorId) {
        self.inner.lock().await.remove(executor_id);
    }

    pub fn share(&self) -> Arc<Self> {
        // `ReviewSink` itself holds an `Arc<Mutex<_>>`; callers clone via Arc.
        Arc::new(Self {
            inner: Arc::clone(&self.inner),
        })
    }
}

impl Default for ReviewSink {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReviewSinkError {
    #[error("a review is already registered for this executor_id")]
    AlreadyRegistered,
}
```

Add to `crates/spur-core/src/lib.rs`:

```rust
mod review_sink;
pub use review_sink::{ReviewSink, ReviewSinkError};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --test review_sink`
Expected: all 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/review_sink.rs crates/spur-core/src/lib.rs \
        crates/spur-core/tests/review_sink.rs
git commit -m "feat(spur-core): add ReviewSink correlation-id router

ReviewSink wraps pending_reviews as HashMap<ExecutorId,
(attempt_n, oneshot::Sender<ReviewDecision>)>. Dispatcher interface:
register / submit (with attempt_n guard) / remove. Stale or unknown
decisions logged and dropped. No sender overwrite on double-register."
```

---

## Task 6: Extend `UserInput::SubmitReview` + TUI wiring with `attempt_n`

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/components/review_card.rs` (if needed)
- Test: `crates/spur-tui/tests/review_submission.rs` (extend)

- [ ] **Step 1: Write the failing test**

Extend `crates/spur-tui/tests/review_submission.rs` with:

```rust
#[test]
fn submit_review_carries_attempt_n() {
    use spur_tui::UserInput;
    use spur_core::ReviewDecision;
    let input = UserInput::SubmitReview {
        executor_id: "exec-1".into(),
        attempt_n: 2,
        decision: ReviewDecision::Approve,
    };
    match input {
        UserInput::SubmitReview { attempt_n, .. } => assert_eq!(attempt_n, 2),
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-tui --test review_submission submit_review_carries_attempt_n`
Expected: FAIL — no `attempt_n` field.

- [ ] **Step 3: Extend `UserInput::SubmitReview`**

Edit `crates/spur-tui/src/app.rs` around line 38:

```rust
    SubmitReview {
        executor_id: String,
        /// The attempt_n from the pending review card the user acted on.
        /// The orchestrator's dispatcher uses this as a supersession guard.
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
```

Edit `crates/spur-tui/src/action.rs` around line 42:

```rust
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
```

- [ ] **Step 4: Read `attempt_n` from the focused node and thread through the Action**

Edit `crates/spur-tui/src/views/dashboard.rs` around line 328. The current code:

```rust
if let Some(id) = self.focused_node.clone() {
    return Some(Action::SubmitReview {
        executor_id: id.0,
        decision,
    });
}
```

Replace with:

```rust
if let Some(id) = self.focused_node.clone() {
    // Look up the pending review's attempt_n from the lineage.
    let attempt_n = self
        .lineage
        .node(&id)
        .and_then(|n| n.pending_review.as_ref().map(|r| r.attempt_n))
        .unwrap_or(1);
    return Some(Action::SubmitReview {
        executor_id: id.0,
        attempt_n,
        decision,
    });
}
```

(If `self.lineage` is not in scope in this view, pass it in as a method parameter or use an accessor. Grep for how the existing `pending_review` reference is resolved.)

- [ ] **Step 5: Thread through `Action::SubmitReview → UserInput::SubmitReview`**

Edit `crates/spur-tui/src/app.rs` around line 500 (the `Action::SubmitReview` handler):

```rust
Action::SubmitReview {
    executor_id,
    attempt_n,
    decision,
} => {
    let has_review = self
        .lineage
        .node(&spur_core::ExecutorId(executor_id.clone()))
        .map(|n| n.pending_review.is_some())
        .unwrap_or(false);
    if !has_review {
        tracing::warn!(
            executor_id = %executor_id,
            "SubmitReview ignored: no pending review on this node"
        );
        return;
    }
    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::SubmitReview {
            executor_id: executor_id.clone(),
            attempt_n,
            decision: decision.clone(),
        });
    }
    // ... rest of optimistic UI update stays as-is
}
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-tui --test review_submission`
Run: `cargo build --workspace`
Expected: PASS + clean build.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "feat(spur-tui): propagate attempt_n through SubmitReview

UserInput::SubmitReview and Action::SubmitReview now carry attempt_n
read from the focused node's pending_review. Supersession guard at the
orchestrator side will reject decisions whose attempt_n does not match
the currently-registered review."
```

---

## Task 7: Add `review_sink` to `Orchestrator`, dispatcher task, and `InteractiveInput::SubmitReview`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-cli/src/main.rs:393`
- Test: `crates/spur-core/tests/review_gate_integration.rs` (create)

- [ ] **Step 1: Write the failing test (dispatcher routes a decision)**

Create `crates/spur-core/tests/review_gate_integration.rs`:

```rust
use spur_acp::ReviewDecision;
use spur_core::{ExecutorId, InteractiveInput, ReviewSink};

#[tokio::test]
async fn dispatcher_routes_submit_review_to_sink() {
    let sink = ReviewSink::new();
    let rx = sink
        .register(ExecutorId("e1".into()), 1)
        .await
        .expect("registered");
    let (tx, input_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(4);

    let sink_for_task = std::sync::Arc::new(sink);
    let sink_for_assert = std::sync::Arc::clone(&sink_for_task);
    let handle = tokio::spawn(spur_core::review_dispatcher_loop(
        input_rx,
        sink_for_task,
    ));

    tx.send(InteractiveInput::SubmitReview {
        executor_id: "e1".into(),
        attempt_n: 1,
        decision: ReviewDecision::Approve,
    })
    .await
    .unwrap();

    let decision = rx.await.expect("decision delivered");
    assert!(matches!(decision, ReviewDecision::Approve));

    drop(tx);
    handle.await.unwrap();
    let _ = sink_for_assert;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: FAIL — `InteractiveInput::SubmitReview` and `review_dispatcher_loop` don't exist.

- [ ] **Step 3: Extend `InteractiveInput`**

Edit `crates/spur-core/src/orchestrator.rs` around line 62:

```rust
pub enum InteractiveInput {
    Message { text: String, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
    SetSessionMode { mode_id: String },
    /// Submit a human review decision. Routed to the ReviewSink by the
    /// dispatcher task, not handled inline in `run_interactive`.
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_acp::ReviewDecision,
    },
}
```

- [ ] **Step 4: Add `review_sink` field + dispatcher helper**

In `crates/spur-core/src/orchestrator.rs`, add to `Orchestrator`:

```rust
pub struct Orchestrator {
    pub registry: AgentRegistry,
    pub config: SpurConfig,
    pub worktrees: WorktreeManager,
    pub cost_tracker: Option<CostTracker>,
    pub event_tx: broadcast::Sender<SpurEvent>,
    pub review_sink: Arc<ReviewSink>,  // NEW
    repo_root: PathBuf,
}
```

In `Orchestrator::new`, add:

```rust
        let review_sink = Arc::new(ReviewSink::new());
```

and include `review_sink` in the struct init.

Add a free function below the `impl Orchestrator` (so it's usable as a dispatcher task):

```rust
/// Dispatcher loop: forwards `SubmitReview` messages to the `ReviewSink`.
/// All other `InteractiveInput` variants are left untouched (they are
/// consumed by `run_interactive`'s own loop, not this one).
///
/// This is spawned as a separate task so review-decision latency is
/// decoupled from brain-turn I/O latency.
pub async fn review_dispatcher_loop(
    mut rx: tokio::sync::mpsc::Receiver<InteractiveInput>,
    sink: Arc<ReviewSink>,
) {
    while let Some(input) = rx.recv().await {
        if let InteractiveInput::SubmitReview {
            executor_id,
            attempt_n,
            decision,
        } = input
        {
            let _ = sink
                .submit(ExecutorId(executor_id), attempt_n, decision)
                .await;
        }
        // All other variants: noop in the dispatcher. Run_interactive
        // owns its own receiver for those.
    }
}
```

Re-export from `crates/spur-core/src/lib.rs`:

```rust
pub use orchestrator::review_dispatcher_loop;
```

- [ ] **Step 5: Run the dispatcher integration test**

Run: `cargo test -p spur-core --test review_gate_integration dispatcher_routes_submit_review_to_sink`
Expected: PASS.

- [ ] **Step 6: Wire spur-cli — replace TODO stub**

Edit `crates/spur-cli/src/main.rs` around line 393. Current stub:

```rust
spur_tui::UserInput::SubmitReview { executor_id, .. } => {
    // TODO(follow-up spec): orchestrator converts decision to
    // the tool-call result that unblocks brain's delegate tool.
    tracing::info!(?executor_id, "review decision captured (orchestrator plumbing pending)");
    continue;
}
```

Replace with:

```rust
spur_tui::UserInput::SubmitReview {
    executor_id,
    attempt_n,
    decision,
} => {
    spur_core::InteractiveInput::SubmitReview {
        executor_id,
        attempt_n,
        decision,
    }
}
```

(The `continue` branch is replaced by the plain translation — this `SubmitReview` flows through the same `InteractiveInput` channel; the dispatcher task routes it, and `run_interactive` ignores it for brain-turn purposes.)

In the main spawn of orchestrator tasks (search for where `run_interactive` is spawned), clone the sender before passing it into `run_interactive` and also spawn the dispatcher:

```rust
let (input_tx, input_rx_for_dispatch) = ...;  // existing
// Clone the receiver end into the dispatcher via a fan-out: simplest
// is to split — one receiver consumed by run_interactive, review
// submissions piped to a dedicated dispatcher receiver. If a single
// shared receiver is already flowing into run_interactive, use a
// fan-out wrapper:
let (dispatch_tx, dispatch_rx) = tokio::sync::mpsc::channel(32);
let orchestrator_sink = orchestrator.review_sink.clone();
tokio::spawn(spur_core::review_dispatcher_loop(dispatch_rx, orchestrator_sink));
```

Then, when forwarding input from the TUI channel to the orchestrator, clone `SubmitReview` messages into `dispatch_tx` *in addition* to forwarding other variants to the `run_interactive` input channel (or, equivalently, let `run_interactive` read all variants and have it forward `SubmitReview` to `dispatch_tx` internally — pick whichever fits the existing main.rs topology more cleanly; the test above only requires the dispatcher task be reachable).

**Concretely** for the current spur-cli `while let Some(input) = ui_input_rx.recv().await` loop:

```rust
while let Some(input) = ui_input_rx.recv().await {
    let translated = match input {
        spur_tui::UserInput::SubmitReview { executor_id, attempt_n, decision } => {
            spur_core::InteractiveInput::SubmitReview { executor_id, attempt_n, decision }
        }
        // ... existing translations ...
    };

    // SubmitReview goes to the dispatcher; everything else goes to run_interactive.
    if matches!(translated, spur_core::InteractiveInput::SubmitReview { .. }) {
        let _ = dispatch_tx.send(translated).await;
    } else {
        let _ = interactive_tx.send(translated).await;
    }
}
```

- [ ] **Step 7: Run full build + tests**

Run: `cargo build --workspace`
Run: `cargo test --workspace`
Expected: PASS across the board.

- [ ] **Step 8: Commit**

```bash
git add -u
git commit -m "feat(spur-core): add ReviewSink to Orchestrator + dispatcher task

InteractiveInput::SubmitReview carries review decisions from the TUI.
review_dispatcher_loop is a separate task that routes SubmitReview to
the ReviewSink; run_interactive is unaffected so review latency is
decoupled from brain-turn I/O. spur-cli replaces the TODO stub with
the real wiring."
```

---

## Task 8: Thread `review_sink` into `execute_delegation`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (signatures only; no semantic change yet)

- [ ] **Step 1: Write the failing test**

No new test — this is a pure refactor. The existing `cargo build --workspace` is the failing signal (it'll pass before edits, pass after).

- [ ] **Step 2: Thread the parameter**

Edit `crates/spur-core/src/orchestrator.rs`. Find `handle_delegations`:

```rust
async fn handle_delegations(
    mut channel: DelegationChannel,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    max_concurrent: usize,
    event_tx: broadcast::Sender<SpurEvent>,
) {
```

Add `review_sink: Arc<ReviewSink>` as a new parameter:

```rust
async fn handle_delegations(
    mut channel: DelegationChannel,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    max_concurrent: usize,
    event_tx: broadcast::Sender<SpurEvent>,
    review_sink: Arc<ReviewSink>,  // NEW
) {
```

In the `tokio::spawn(async move { ... })` block inside `handle_delegations`, clone `review_sink` alongside the existing clones, and pass it into `execute_delegation`:

```rust
let review_sink = Arc::clone(&review_sink);
// ...
Self::execute_delegation(
    agent,
    task,
    context_files,
    repo_root,
    agent_configs,
    event_tx,
    review_sink,
)
```

Extend `execute_delegation`'s signature:

```rust
async fn execute_delegation(
    agent: String,
    task: String,
    _context_files: Vec<String>,
    repo_root: PathBuf,
    agent_configs: Vec<spur_acp::config::AgentConfig>,
    event_tx: broadcast::Sender<SpurEvent>,
    _review_sink: Arc<ReviewSink>,  // used in Task 9
) -> DelegationResult {
```

Update every call site of `handle_delegations` (there is one — in `Orchestrator::run` or equivalent entry point; grep `handle_delegations(` to find). Pass `Arc::clone(&self.review_sink)` as the new argument.

- [ ] **Step 3: Run full build + tests**

Run: `cargo build --workspace`
Run: `cargo test --workspace`
Expected: PASS. Pure refactor, no semantic change.

- [ ] **Step 4: Commit**

```bash
git add -u
git commit -m "refactor(spur-core): thread ReviewSink into execute_delegation

Mechanical parameter plumbing from handle_delegations into
execute_delegation. No semantic change — review gate logic follows in
the next task."
```

---

## Task 9: Insert review gate in `execute_delegation`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` around line 1594
- Test: `crates/spur-core/tests/review_gate_integration.rs` (extend)

- [ ] **Step 1: Write the failing test (gate → Approve → Success)**

Add to `crates/spur-core/tests/review_gate_integration.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn approve_decision_produces_success_status() {
    use spur_acp::{DelegationStatus, ReviewDecision};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());
    let sink_for_test = std::sync::Arc::clone(&sink);

    // Drive the gate directly — we don't need a real worker here. This
    // test exercises the `run_gate_for_candidate` helper we factor out
    // in Task 9 so the gate is unit-testable without spawning ACP.
    let gate = tokio::spawn(async move {
        spur_core::orchestrator::run_gate_for_candidate(
            ExecutorId("e1".into()),
            /* attempt_n */ 1,
            /* candidate */ DelegationStatus::Success,
            /* review_timeout */ std::time::Duration::from_secs(300),
            /* timeout_default */
            spur_acp::TimeoutFallback::Reject { reason: "t".into() },
            sink,
        )
        .await
    });

    // Give the gate a tick to register.
    tokio::task::yield_now().await;

    let routed = sink_for_test
        .submit(ExecutorId("e1".into()), 1, ReviewDecision::Approve)
        .await;
    assert!(routed);

    let status = gate.await.unwrap();
    assert!(matches!(status, DelegationStatus::Success));
}

#[tokio::test(start_paused = true)]
async fn timeout_produces_timed_out_status_and_removes_entry() {
    use spur_acp::{DelegationStatus, TimeoutFallback};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());
    let sink_for_test = std::sync::Arc::clone(&sink);

    let gate = tokio::spawn(async move {
        spur_core::orchestrator::run_gate_for_candidate(
            ExecutorId("e1".into()),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(60),
            TimeoutFallback::Reject {
                reason: "review timeout".into(),
            },
            sink,
        )
        .await
    });

    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(120)).await;

    let status = gate.await.unwrap();
    match status {
        DelegationStatus::TimedOut {
            waited_for,
            fallback: TimeoutFallback::Reject { reason },
        } => {
            assert_eq!(waited_for, std::time::Duration::from_secs(60));
            assert_eq!(reason, "review timeout");
        }
        other => panic!("expected TimedOut, got {:?}", other),
    }
    // Post-timeout: entry must be gone (explicit-remove contract).
    let stale = sink_for_test
        .submit(ExecutorId("e1".into()), 1, spur_acp::ReviewDecision::Approve)
        .await;
    assert!(!stale, "timeout path must remove the entry");
}

#[tokio::test(start_paused = true)]
async fn reject_decision_produces_rejected_status() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());
    let sink_for_test = std::sync::Arc::clone(&sink);

    let gate = tokio::spawn(async move {
        spur_core::orchestrator::run_gate_for_candidate(
            ExecutorId("e1".into()),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(300),
            TimeoutFallback::Reject { reason: "t".into() },
            sink,
        )
        .await
    });
    tokio::task::yield_now().await;
    sink_for_test
        .submit(
            ExecutorId("e1".into()),
            1,
            ReviewDecision::Reject {
                reason: "too large".into(),
            },
        )
        .await;
    let status = gate.await.unwrap();
    match status {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "too large"),
        other => panic!("expected Rejected, got {:?}", other),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: FAIL — `run_gate_for_candidate` doesn't exist.

- [ ] **Step 3: Factor out `run_gate_for_candidate` and insert it**

Add a module-level function in `crates/spur-core/src/orchestrator.rs` (public within the crate + re-exported for testing):

```rust
/// Run the review gate on a candidate `DelegationStatus`. Returns the
/// final status — Approve keeps the candidate; Reject/Modify re-shape;
/// timeout produces `TimedOut { fallback }`.
///
/// **NB:** this function does NOT handle `Retry` — Retry is handled
/// by the caller (who owns the worker spawn/despawn lifecycle). If the
/// caller receives `ReviewDecision::Retry`, it loops back to respawn.
/// Returns `Ok(final_status)` on terminal decisions, `Err(Retry { new_constraints })`
/// on Retry so the caller can loop.
pub async fn run_gate_for_candidate(
    executor_id: ExecutorId,
    attempt_n: u32,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_default: TimeoutFallback,
    review_sink: Arc<ReviewSink>,
) -> DelegationStatus {
    // Register.
    let rx = match review_sink.register(executor_id.clone(), attempt_n).await {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!(executor_id = %executor_id.0, error = %e,
                "review_sink registration failed");
            return DelegationStatus::Failed {
                error: format!("review registration failed: {e}"),
            };
        }
    };

    // Select on decision vs. timeout.
    tokio::select! {
        r = rx => {
            match r {
                Ok(decision) => apply_decision(decision, candidate_status),
                Err(_) => {
                    // Sender dropped — treat as timeout.
                    review_sink.remove(&executor_id).await;
                    DelegationStatus::TimedOut {
                        waited_for: review_timeout,
                        fallback: timeout_default,
                    }
                }
            }
        }
        _ = tokio::time::sleep(review_timeout) => {
            // Explicit-remove contract.
            review_sink.remove(&executor_id).await;
            DelegationStatus::TimedOut {
                waited_for: review_timeout,
                fallback: timeout_default,
            }
        }
    }
}

fn apply_decision(
    decision: spur_acp::ReviewDecision,
    candidate: DelegationStatus,
) -> DelegationStatus {
    use spur_acp::ReviewDecision;
    match decision {
        ReviewDecision::Approve => candidate,
        ReviewDecision::Reject { reason } => DelegationStatus::Rejected { reason },
        ReviewDecision::Modify { note } => {
            DelegationStatus::Modified { reviewer_note: note }
        }
        // Retry: caller loops. We encode by returning a sentinel — but
        // the integration test doesn't exercise Retry through this
        // function; Retry is handled in the outer loop (Task 10).
        // For cleanliness, the public `run_gate_for_candidate` is
        // Retry-free (Task 10 renames/extends this).
        ReviewDecision::Retry { .. } => {
            // Should be unreachable in this helper — Task 10 wraps the
            // helper with Retry handling.
            DelegationStatus::Failed {
                error: "internal: Retry reached run_gate_for_candidate \
                        (caller must wrap with retry loop)"
                    .into(),
            }
        }
    }
}
```

Then, at `execute_delegation`'s review-gate insertion point (post-worker, pre-respond), add:

```rust
// After constructing the candidate DelegationResult (around line 1594):
let status = if agent_config.review.review_required {
    // Emit review-requested event.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: executor_id.0.clone(),
        attempt_n: 1, // Task 10 extends this for retries.
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: summary.clone().unwrap_or_default(),
            diff_summary: None,  // TODO: build DiffSummary from diff in a later polish pass
            pr_url: None,
            error: None,
        },
    }));
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: executor_id.0.clone(),
        phase: LifecycleState::AwaitingReview,
    }));

    let final_status = run_gate_for_candidate(
        executor_id.clone(),
        1,
        status.clone(),
        agent_config.review.review_timeout,
        agent_config.review.review_timeout_default.clone(),
        Arc::clone(&_review_sink),
    )
    .await;

    // Emit resolved event.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewResolved {
        id: executor_id.0.clone(),
        decision: decision_from_status(&final_status),  // helper; or skip and re-emit nothing
    }));

    final_status
} else {
    status
};
```

(Adapt to the actual `executor_id` source at the insertion point. If the current `execute_delegation` does not already construct an `ExecutorId`, construct one from the `worker_session` — grep for where `SpurEvent::now(SpurEventBody::WorkerSpawned { session, .. })` is emitted nearby.)

Rename the `_review_sink` param to `review_sink` (drop the underscore now that it's used).

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: all 3 integration tests PASS.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "feat(spur-core): insert review gate in execute_delegation

run_gate_for_candidate registers on ReviewSink, selects on
(decision_rx, sleep(review_timeout)), and shapes the final
DelegationStatus per the decision. Timeout path explicitly removes
the sink entry. Gate is skipped when agent.review.review_required is
false (default)."
```

---

## Task 10: Retry loop with `attempt_n` bumping

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — wrap `execute_delegation`'s worker-spawn + gate in a retry loop
- Test: `crates/spur-core/tests/review_gate_integration.rs` (extend)

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-core/tests/review_gate_integration.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn retry_then_approve_produces_success() {
    // Two Retrys then Approve should end in Success.
    use spur_acp::{DelegationStatus, ReviewDecision};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());

    let (decisions_tx, mut decisions_rx) =
        tokio::sync::mpsc::channel::<ReviewDecision>(8);
    decisions_tx
        .send(ReviewDecision::Retry {
            new_constraints: "try harder".into(),
        })
        .await
        .unwrap();
    decisions_tx
        .send(ReviewDecision::Retry {
            new_constraints: "try harder 2".into(),
        })
        .await
        .unwrap();
    decisions_tx.send(ReviewDecision::Approve).await.unwrap();

    let sink_task = std::sync::Arc::clone(&sink);
    tokio::spawn(async move {
        let mut attempt = 1;
        while let Some(d) = decisions_rx.recv().await {
            // Wait for the gate to register before submitting.
            loop {
                tokio::task::yield_now().await;
                if sink_task
                    .submit(ExecutorId("e1".into()), attempt, d.clone())
                    .await
                {
                    break;
                }
            }
            attempt += 1;
        }
    });

    // Drive a dummy retry loop using run_gate_for_candidate_with_retries
    let final_status = spur_core::orchestrator::run_gate_with_retries(
        ExecutorId("e1".into()),
        DelegationStatus::Success,
        std::time::Duration::from_secs(60),
        spur_acp::TimeoutFallback::Reject { reason: "t".into() },
        3, // max_review_retries
        sink,
    )
    .await;
    assert!(matches!(final_status, DelegationStatus::Success));
}

#[tokio::test(start_paused = true)]
async fn retry_limit_exceeded_produces_failed() {
    use spur_acp::{DelegationStatus, ReviewDecision};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ReviewDecision>(8);
    for i in 0..4 {
        tx.send(ReviewDecision::Retry {
            new_constraints: format!("try {}", i + 1),
        })
        .await
        .unwrap();
    }
    let sink_task = std::sync::Arc::clone(&sink);
    tokio::spawn(async move {
        let mut attempt = 1;
        while let Some(d) = rx.recv().await {
            loop {
                tokio::task::yield_now().await;
                if sink_task
                    .submit(ExecutorId("e1".into()), attempt, d.clone())
                    .await
                {
                    break;
                }
            }
            attempt += 1;
        }
    });
    let final_status = spur_core::orchestrator::run_gate_with_retries(
        ExecutorId("e1".into()),
        DelegationStatus::Success,
        std::time::Duration::from_secs(60),
        spur_acp::TimeoutFallback::Reject { reason: "t".into() },
        3,
        sink,
    )
    .await;
    match final_status {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded"));
            assert!(error.contains("3"));
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: both tests FAIL — `run_gate_with_retries` doesn't exist.

- [ ] **Step 3: Implement `run_gate_with_retries`**

Add to `crates/spur-core/src/orchestrator.rs`:

```rust
/// Wraps `run_gate_for_candidate` with orchestrator-internal Retry
/// handling. On `Retry`, emits `ExecutorRetryStarted`, increments
/// `attempt_n`, and re-enters the gate. Bounded by
/// `max_review_retries`.
///
/// NB: this helper only handles the *gate* side of retry (decision
/// loop + attempt_n bookkeeping). Worker respawn is the caller's
/// responsibility; this helper just signals the caller which decision
/// was made. In the full `execute_delegation` path, the caller wraps
/// worker-spawn + candidate-production + this helper in a loop.
///
/// For test purposes, we use a fixed candidate — production callers
/// re-spawn the worker and produce a fresh candidate each iteration.
pub async fn run_gate_with_retries(
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_default: TimeoutFallback,
    max_review_retries: u32,
    review_sink: Arc<ReviewSink>,
) -> DelegationStatus {
    let mut attempt_n: u32 = 1;
    loop {
        // One-shot gate check for this attempt.
        let rx = match review_sink
            .register(executor_id.clone(), attempt_n)
            .await
        {
            Ok(rx) => rx,
            Err(e) => {
                return DelegationStatus::Failed {
                    error: format!("review registration failed: {e}"),
                };
            }
        };
        let decision = tokio::select! {
            r = rx => r.ok(),
            _ = tokio::time::sleep(review_timeout) => {
                review_sink.remove(&executor_id).await;
                return DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_default,
                };
            }
        };
        use spur_acp::ReviewDecision;
        match decision {
            Some(ReviewDecision::Approve) => return candidate_status,
            Some(ReviewDecision::Reject { reason }) => {
                return DelegationStatus::Rejected { reason }
            }
            Some(ReviewDecision::Modify { note }) => {
                return DelegationStatus::Modified { reviewer_note: note }
            }
            Some(ReviewDecision::Retry { .. }) => {
                if attempt_n >= max_review_retries {
                    return DelegationStatus::Failed {
                        error: format!(
                            "retry limit exceeded after {} attempts",
                            max_review_retries
                        ),
                    };
                }
                attempt_n += 1;
                // Loop continues; caller is expected to have respawned
                // the worker and produced a new candidate. In this
                // helper (test shape), we reuse the same candidate.
                continue;
            }
            None => {
                review_sink.remove(&executor_id).await;
                return DelegationStatus::TimedOut {
                    waited_for: review_timeout,
                    fallback: timeout_default,
                };
            }
        }
    }
}
```

- [ ] **Step 4: Update `execute_delegation` to use the retry loop**

In `execute_delegation`, wrap the worker-spawn + candidate-build + gate in a retry loop. Pseudocode of the shape (adapt to existing code):

```rust
let mut attempt_n: u32 = 1;
let final_status = loop {
    // [existing worker-spawn + candidate-build code — factor into a closure
    //  or keep inline; re-runs on Retry]
    let candidate = build_candidate_by_running_worker(...).await;

    if !agent_config.review.review_required {
        break candidate;
    }

    // Emit events for this attempt.
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: executor_id.0.clone(),
        attempt_n,
        kind: ReviewKind::Completion,
        payload: /* ... */,
    }));

    let rx = match review_sink.register(executor_id.clone(), attempt_n).await {
        Ok(rx) => rx,
        Err(e) => break DelegationStatus::Failed {
            error: format!("review registration failed: {e}"),
        },
    };
    let decision = tokio::select! {
        r = rx => r.ok(),
        _ = tokio::time::sleep(agent_config.review.review_timeout) => None,
    };
    match decision {
        Some(ReviewDecision::Approve) => break candidate,
        Some(ReviewDecision::Reject { reason }) => break DelegationStatus::Rejected { reason },
        Some(ReviewDecision::Modify { note }) => break DelegationStatus::Modified { reviewer_note: note },
        Some(ReviewDecision::Retry { new_constraints }) => {
            if attempt_n >= agent_config.review.max_review_retries {
                break DelegationStatus::Failed {
                    error: format!("retry limit exceeded after {} attempts",
                                   agent_config.review.max_review_retries),
                };
            }
            // Emit retry-started.
            let new_session = SessionId::new();
            let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
                id: executor_id.0.clone(),
                attempt_n: attempt_n + 1,
                reason: new_constraints.clone(),
                new_session_id: new_session,
            }));
            // Append constraints to task for next iteration.
            task = format!("{}\n\n## Additional constraints\n{}",
                           original_task, new_constraints);
            attempt_n += 1;
            continue;
        }
        None => {
            review_sink.remove(&executor_id).await;
            break DelegationStatus::TimedOut {
                waited_for: agent_config.review.review_timeout,
                fallback: agent_config.review.review_timeout_default.clone(),
            };
        }
    }
};
```

Key points to preserve:
- Capture `original_task` outside the loop so Retry appends consistently.
- The worktree is reused across attempts (per spec) — the existing worktree despawn+respawn happens on Retry before the next iteration. Add the despawn + respawn before `continue`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: 5+ tests PASS (all the gate + retry tests).

Run: `cargo test --workspace`
Expected: no regressions.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "feat(spur-core): retry loop with attempt_n bumping

run_gate_with_retries handles ReviewDecision::Retry internally: bumps
attempt_n, emits ExecutorRetryStarted, re-enters the gate with a fresh
ReviewSink registration. Bounded by agent.review.max_review_retries
(default 3). Terminal decisions (Approve/Reject/Modify/TimedOut) return
the shaped DelegationStatus unchanged."
```

---

## Task 11: Worktree preservation on `Rejected` and `TimedOut`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` (cleanup path)
- Test: extend `crates/spur-core/tests/review_gate_integration.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-core/tests/review_gate_integration.rs`:

```rust
#[test]
fn should_preserve_worktree_matches_expected_variants() {
    use spur_acp::{DelegationStatus, TimeoutFallback};
    use spur_core::orchestrator::should_preserve_worktree;
    use std::path::PathBuf;

    assert!(!should_preserve_worktree(&DelegationStatus::Success));
    assert!(!should_preserve_worktree(&DelegationStatus::Failed {
        error: "e".into()
    }));
    assert!(!should_preserve_worktree(&DelegationStatus::Conflict {
        files: vec![PathBuf::from("a")]
    }));
    assert!(!should_preserve_worktree(&DelegationStatus::Timeout));
    assert!(!should_preserve_worktree(&DelegationStatus::Modified {
        reviewer_note: "n".into()
    }));

    assert!(should_preserve_worktree(&DelegationStatus::Rejected {
        reason: "r".into()
    }));
    assert!(should_preserve_worktree(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Reject { reason: "r".into() },
    }));
    assert!(should_preserve_worktree(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    }));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p spur-core --test review_gate_integration should_preserve_worktree_matches_expected_variants`
Expected: FAIL — helper doesn't exist.

- [ ] **Step 3: Add the helper and wire into cleanup**

Add to `crates/spur-core/src/orchestrator.rs`:

```rust
/// Returns true if the worktree should be preserved (not removed) for
/// this final `DelegationStatus`. Preserve on `Rejected` and
/// `TimedOut` — the worker did real work but no one validated it, and
/// a human may want to inspect the diff before discarding.
pub fn should_preserve_worktree(status: &DelegationStatus) -> bool {
    matches!(
        status,
        DelegationStatus::Rejected { .. } | DelegationStatus::TimedOut { .. }
    )
}
```

In `execute_delegation`, find the existing cleanup line `let _ = worktrees.remove_worktree(&worker_session).await;` and replace with:

```rust
if should_preserve_worktree(&status) {
    if let Some(path) = worktrees.path_for(&worker_session).await {
        tracing::info!(
            worktree = %path.display(),
            ?status,
            "preserving worktree for review inspection"
        );
    }
} else {
    let _ = worktrees.remove_worktree(&worker_session).await;
}
```

If `WorktreeManager` has no `path_for` method, log whatever path information is available (e.g., the `worktree_info.path` captured earlier in `execute_delegation`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --test review_gate_integration`
Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "feat(spur-core): preserve worktree on Rejected/TimedOut

should_preserve_worktree returns true for Rejected and TimedOut (any
fallback). execute_delegation logs the preserved path so the human can
find it for manual inspection. All other terminal states fall through
to the normal remove_worktree cleanup."
```

---

## Task 12: Brain-cancellation audit event

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs` — in the `execute_delegation` respond_to send-or-cleanup path
- Test: extend `crates/spur-core/tests/review_gate_integration.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-core/tests/review_gate_integration.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn brain_cancellation_during_review_emits_review_cancelled() {
    // Drop the respond_to sender while a review is pending; assert the
    // orchestrator emits ExecutorReviewCancelled before cleanup.
    //
    // This test exercises the `cleanup_cancelled_review` helper added
    // in this task. End-to-end validation (real MCP server + real
    // worker) is deferred to the smoke test in Task 14.
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ReviewSink};
    let sink = std::sync::Arc::new(ReviewSink::new());
    let _rx = sink.register(ExecutorId("e1".into()), 1).await.unwrap();
    let (tx, mut event_rx) = tokio::sync::broadcast::channel::<SpurEvent>(8);

    spur_core::orchestrator::cleanup_cancelled_review(
        &ExecutorId("e1".into()),
        "brain call cancelled",
        &tx,
        &sink,
    )
    .await;

    let ev = event_rx.recv().await.expect("event");
    match ev.body {
        SpurEventBody::ExecutorReviewCancelled { id, reason } => {
            assert_eq!(id, "e1");
            assert_eq!(reason, "brain call cancelled");
        }
        other => panic!("expected ExecutorReviewCancelled, got {:?}", other),
    }
    // Sink entry must be gone.
    let stale = sink
        .submit(
            ExecutorId("e1".into()),
            1,
            spur_acp::ReviewDecision::Approve,
        )
        .await;
    assert!(!stale);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p spur-core --test review_gate_integration brain_cancellation_during_review_emits_review_cancelled`
Expected: FAIL — helper missing.

- [ ] **Step 3: Add helper + wire into cleanup path**

Add to `crates/spur-core/src/orchestrator.rs`:

```rust
/// Emit `ExecutorReviewCancelled` and remove the sink entry.
/// Called from the brain-cancellation path (when `respond_to.send`
/// returns `Err`) if a review is still registered.
pub async fn cleanup_cancelled_review(
    executor_id: &ExecutorId,
    reason: &str,
    event_tx: &broadcast::Sender<SpurEvent>,
    review_sink: &ReviewSink,
) {
    let _ = event_tx.send(SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: executor_id.0.clone(),
        reason: reason.into(),
    }));
    review_sink.remove(executor_id).await;
}
```

In `execute_delegation` (and/or `handle_delegations`'s `let _ = respond_to.send(result);` site), when the send returns `Err` AND a review is/was pending, call `cleanup_cancelled_review`. The simplest site is in `handle_delegations`:

```rust
if let Err(_returned) = respond_to.send(result) {
    // Brain call was cancelled. If the delegation task is still
    // inside the review gate, emit the cancellation event so the
    // lineage projection records it.
    spur_core::orchestrator::cleanup_cancelled_review(
        &executor_id_here,
        "brain call cancelled",
        &event_tx,
        &review_sink,
    )
    .await;
}
```

(If the `ExecutorId` is not in scope at the respond_to site, plumb it through. Alternatively, guard the call on `review_sink.has(&executor_id)` — adding a small `has` method to `ReviewSink` in this task.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --test review_gate_integration`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -u
git commit -m "feat(spur-core): emit ExecutorReviewCancelled on brain cancel

cleanup_cancelled_review emits the audit event and removes the sink
entry. Invoked from handle_delegations when respond_to.send returns
Err (brain tool-call cancelled during an active review)."
```

---

## Task 13: DelegationResult text formatter + brain-facing distinctness

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (or wherever `DelegationResult` is serialized for the brain)
- Test: extend `crates/spur-acp/tests/delegation_status_roundtrip.rs` OR create new `crates/spur-mcp/tests/delegation_text.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/tests/delegation_status_display.rs`:

```rust
use spur_acp::{DelegationResult, DelegationStatus, TimeoutFallback};

fn render(r: &DelegationResult) -> String {
    // Uses the same serializer the MCP server uses (pretty JSON).
    serde_json::to_string_pretty(r).expect("serialize")
}

#[test]
fn each_review_variant_renders_distinctly() {
    let base = |status| DelegationResult {
        status,
        diff: None,
        summary: Some("s".into()),
        estimated_cost_usd: 0.0,
    };
    let success = render(&base(DelegationStatus::Success));
    let rejected = render(&base(DelegationStatus::Rejected {
        reason: "too large".into(),
    }));
    let modified = render(&base(DelegationStatus::Modified {
        reviewer_note: "fix naming".into(),
    }));
    let timed_out = render(&base(DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(1800),
        fallback: TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
    }));

    // Each must contain its discriminator so the brain can pattern-match.
    assert!(success.contains("\"Success\""));
    assert!(rejected.contains("\"Rejected\""));
    assert!(rejected.contains("too large"));
    assert!(modified.contains("\"Modified\""));
    assert!(modified.contains("fix naming"));
    assert!(timed_out.contains("\"TimedOut\""));
    assert!(timed_out.contains("review timeout"));

    // Must be mutually distinguishable.
    for (a, b) in &[
        (&success, &rejected),
        (&success, &modified),
        (&success, &timed_out),
        (&rejected, &modified),
        (&rejected, &timed_out),
        (&modified, &timed_out),
    ] {
        assert_ne!(a, b, "variants must render distinctly");
    }
}
```

- [ ] **Step 2: Run test to verify its state**

Run: `cargo test -p spur-acp --test delegation_status_display`
Expected: PASS straight away if serde is already tagging variants (default `externally tagged` serialization produces `"Rejected": { ... }` etc.). If the test fails for any variant, that variant's serde attribute needs fixing in `crates/spur-acp/src/domain/delegation.rs`.

- [ ] **Step 3: If the test fails, fix serde tagging**

Most likely the test passes as-is (Rust serde defaults to externally-tagged enums). If it fails for some variant, add `#[serde(tag = ...)]` or adjust. Leave a comment in the struct near each variant explaining why distinctness matters (brain prompt parses).

- [ ] **Step 4: If test passes, no code changes — just commit the test**

```bash
git add crates/spur-acp/tests/delegation_status_display.rs
git commit -m "test(spur-acp): brain-facing distinctness of review variants

Regression guard: DelegationResult JSON output for Success / Rejected /
Modified / TimedOut must be mutually distinguishable and must contain
the variant discriminator string so the brain's prompt can pattern-
match on it."
```

---

## Task 14: End-to-end smoke test (integration, real worker loop)

**Files:**
- Create: `crates/spur-core/tests/review_loopback_e2e.rs`
- Manual: check against a live `spur watch` session with a test agent

- [ ] **Step 1: Write the integration test**

Create `crates/spur-core/tests/review_loopback_e2e.rs`:

```rust
//! End-to-end validation of the review loopback using a mocked worker
//! path (no real ACP agent). Asserts the happy paths: Approve → Success,
//! Reject → Rejected, Modify → Modified, Retry×N → Success,
//! RetryLimit → Failed, Timeout → TimedOut.

use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
use spur_core::{ExecutorId, ReviewSink};
use std::sync::Arc;
use std::time::Duration;

async fn drive(
    decisions: Vec<ReviewDecision>,
    max_retries: u32,
) -> DelegationStatus {
    let sink = Arc::new(ReviewSink::new());
    let sink_for_driver = Arc::clone(&sink);
    let decisions_task = tokio::spawn(async move {
        let mut attempt = 1u32;
        for d in decisions {
            loop {
                tokio::task::yield_now().await;
                if sink_for_driver
                    .submit(ExecutorId("e1".into()), attempt, d.clone())
                    .await
                {
                    break;
                }
            }
            if matches!(d, ReviewDecision::Retry { .. }) {
                attempt += 1;
            }
        }
    });
    let status = spur_core::orchestrator::run_gate_with_retries(
        ExecutorId("e1".into()),
        DelegationStatus::Success,
        Duration::from_secs(60),
        TimeoutFallback::Reject {
            reason: "review timeout".into(),
        },
        max_retries,
        sink,
    )
    .await;
    decisions_task.await.unwrap();
    status
}

#[tokio::test]
async fn e2e_approve() {
    let s = drive(vec![ReviewDecision::Approve], 3).await;
    assert!(matches!(s, DelegationStatus::Success));
}

#[tokio::test]
async fn e2e_reject() {
    let s = drive(
        vec![ReviewDecision::Reject {
            reason: "nope".into(),
        }],
        3,
    )
    .await;
    match s {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "nope"),
        other => panic!("{:?}", other),
    }
}

#[tokio::test]
async fn e2e_modify() {
    let s = drive(
        vec![ReviewDecision::Modify {
            note: "fix naming".into(),
        }],
        3,
    )
    .await;
    match s {
        DelegationStatus::Modified { reviewer_note } => {
            assert_eq!(reviewer_note, "fix naming");
        }
        other => panic!("{:?}", other),
    }
}

#[tokio::test]
async fn e2e_retry_then_approve() {
    let s = drive(
        vec![
            ReviewDecision::Retry {
                new_constraints: "c1".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c2".into(),
            },
            ReviewDecision::Approve,
        ],
        3,
    )
    .await;
    assert!(matches!(s, DelegationStatus::Success));
}

#[tokio::test]
async fn e2e_retry_limit_exceeded() {
    let s = drive(
        vec![
            ReviewDecision::Retry {
                new_constraints: "c1".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c2".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c3".into(),
            },
            ReviewDecision::Retry {
                new_constraints: "c4".into(),
            },
        ],
        3,
    )
    .await;
    match s {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded"));
        }
        other => panic!("{:?}", other),
    }
}

#[tokio::test(start_paused = true)]
async fn e2e_timeout_produces_timed_out() {
    let sink = Arc::new(ReviewSink::new());
    let gate = tokio::spawn({
        let sink = Arc::clone(&sink);
        async move {
            spur_core::orchestrator::run_gate_with_retries(
                ExecutorId("e1".into()),
                DelegationStatus::Success,
                Duration::from_secs(60),
                TimeoutFallback::Reject {
                    reason: "review timeout".into(),
                },
                3,
                sink,
            )
            .await
        }
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(120)).await;
    let s = gate.await.unwrap();
    match s {
        DelegationStatus::TimedOut { fallback, .. } => {
            assert!(matches!(fallback, TimeoutFallback::Reject { .. }));
        }
        other => panic!("{:?}", other),
    }
}
```

- [ ] **Step 2: Run the e2e tests**

Run: `cargo test -p spur-core --test review_loopback_e2e`
Expected: all 6 PASS.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --workspace`
Expected: PASS across all crates.

- [ ] **Step 4: Manual smoke**

Configure a test agent entry in `.spur/config.toml`:

```toml
[[agents.entries]]
name = "test-reviewable"
command = "echo"  # or whichever test agent the repo uses
args = []
transport = "stdio"
role = "worker"

[agents.entries.review]
review_required = true
review_timeout_secs = 300
max_review_retries = 3
```

Start `spur watch`, delegate to the test agent from the brain, verify:
- TUI shows the review card with attempt_n = 1.
- Press `a` (Approve) — brain sees `"Success"`.
- Rerun, press `d` with a reason — brain sees `"Rejected": { "reason": "..." }`.
- Rerun, press `R` with constraints — worker respawns (observe `ExecutorRetryStarted` in the activity log with `attempt_n = 2`); approve on next card — brain sees `"Success"`.
- Rerun, do nothing past `review_timeout` — brain sees `"TimedOut"`, not `"Rejected"`.

Document observations in a scratchpad or local notes; no code commit for the manual smoke.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/tests/review_loopback_e2e.rs
git commit -m "test(spur-core): e2e review-loopback scenarios

Covers Approve/Reject/Modify/Retry-then-approve/Retry-limit-exceeded/
Timeout. Uses ReviewSink-driven test harness (no real ACP agent).
Manual smoke validation documented in the implementation plan."
```

---

## Self-Review

**1. Spec coverage — each spec section mapped to a task:**

| Spec section | Task(s) |
|---|---|
| Expanded `DelegationStatus` enum (Unit 1) | 1 |
| `TimeoutFallback` / removal of `Abandoned` | 1 |
| `AgentReviewPolicy` config (type unification) | 4 |
| `pending_reviews` + dispatcher (Unit 2, Unit 3) | 5, 7 |
| Separate-channel rationale (dispatcher topology) | 7 (InteractiveInput::SubmitReview + `review_dispatcher_loop`) |
| Attempt supersession (incl. `attempt_n` on event + UserInput) | 2, 6, 7 (ReviewSink guard), 10 (retry bump) |
| Worker side-effect idempotency contract | Documentation-only in spec; no code task — contract enforced by worker configs outside this crate. |
| Review gate insertion (Stages 5+6 in spec) | 8 (plumb), 9 (insert) |
| Retry loop (Stage 7 in spec) | 10 |
| Worktree preservation (Stage 8) | 11 |
| `ExecutorReviewCancelled` audit (Stage 9) | 3 (event), 12 (emission) |
| DelegationResult text formatter (Stage 10) | 13 |
| E2E smoke (Stage 11) | 14 |
| Timeout cleanup contract (explicit `remove`) | 9 (helper), 10 (retry loop) |
| Semaphore permit across retries (known limitation) | No task — documented in spec only. |

**2. Placeholder scan:** No "TBD", "implement later", "similar to Task N", or unspecified steps. Each task's code is complete enough to paste. Three caveats that are intentional, not placeholders:
- Task 9's `build_candidate_by_running_worker(...)` in the pseudocode is a narrative placeholder; the task text explicitly says "adapt to existing code" and points to the insertion-point line number.
- Task 7 Step 6 offers two wiring shapes in spur-cli and explicitly says "pick whichever fits existing main.rs topology more cleanly"; this is a known architectural flexibility, not an underspecified step — the test only requires the dispatcher be reachable.
- Task 10 Step 4's `execute_delegation` rewrite is pseudocode with explicit adaptation notes; the production edit must line up with whatever `execute_delegation`'s existing variable names are (they were not all captured in this plan).

**3. Type consistency:**
- `ExecutorId` is consistently `ExecutorId(String)` throughout.
- `ReviewSink::register/submit/remove` signatures are consistent across Task 5 (definition) and Tasks 7, 9, 10, 11, 12 (uses).
- `TimeoutFallback` is consistently the shared type across `AgentReviewPolicy` (Task 4) and `DelegationStatus::TimedOut.fallback` (Task 1).
- `attempt_n: u32` is consistent across event (Task 2), UserInput (Task 6), ReviewSink (Task 5).

No drift found.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-13-orchestrator-review-loopback.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
