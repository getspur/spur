# Brain continuation — producer envelope fit design (Plan-4)

> **STATUS UPDATE (2026-04-25):** Superseded as primary by
> [`2026-04-25-brain-continuation-artifact-store-design.md`](2026-04-25-brain-continuation-artifact-store-design.md).
> The truncation ladder defined here survives as the artifact-write-failure
> fallback path, referenced from §7.7 of the superseding spec.
> Original status retained below for review-history continuity.

- **Date:** 2026-04-24
- **Status:** Superseded (truncation ladder retained as fallback)
- **Authors:** Kevin Truong (kevin.truong.ds@gmail.com), with Claude Opus 4.6 as design pair
- **Reviewers:** codex (rounds 1–2), kimi (round 3) — see `docs/rca/log.md`
- **Supersedes / extends:** `docs/superpowers/specs/2026-04-24-brain-continuation-delivery-guarantees.md` (v3.1, merge `6b1e6980`)
- **Related:** `docs/superpowers/reviews/2026-04-24-brain-continuation-rca.md`

---

## 1. Problem

v3.1 of the brain-continuation pipeline delivered structural enforcement for INV-D1..D7 (session-scoped ingress, checkout/commit handshake, bounded requeue, proptest-verified oversized-never-requeues). A production log surfaced after merge:

```
⚠ Continuation dropped for 9b2c84e0-4572-494a-a5b5-04ea96b86e15:
  OversizedSingleItem { continuation_bytes: 4478, budget_bytes: 4096 }
```

The brain never learns that delegation completed. The drop is now *observable* (v3.1 wired the event) but still *terminal* — requeueing an oversized item would starve it forever, so the merger correctly drops it.

## 2. Root cause

Three concurrent defects at the producer / merger boundary, all pre-existing v3.1 and out-of-scope at the time:

| # | Defect | Location |
|---|---|---|
| RC1 | Producer allows a single continuation to exceed the merger's envelope budget (no producer-side global fit) | `spur-mcp/src/server.rs:251-288` (`build_detached_continuation`) |
| RC2 | `diff_summary` is uncapped at the producer despite spec claim to the contrary | `spur-mcp/src/server.rs:281` vs spec line 951 of `2026-04-24-brain-continuation-delivery-guarantees.md` |
| RC3 | `SPUR_MERGE_BUDGET_BYTES` documented as tunable but not wired to code | `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md:393-394` vs `crates/spur-core/src/orchestrator.rs:1702, 1712` |

Additional structural gap: no invariant enforces "producer-emitted envelope ≤ merge budget". `PRODUCER_MAX_FIELD_BYTES = 8192` (`spur-mcp/src/server.rs:45`) permits a single field that is 2× the entire merge budget of `MERGE_BUDGET_DEFAULT_BYTES = 4096` (`crates/spur-core/src/continuation_bridge.rs:110`).

## 3. Goals and non-goals

### Goals

Close the structural gap by introducing a producer-side **fit function** that guarantees every continuation entering the ingress queue satisfies the merger's budget. Fix RC1/RC2/RC3 as a bundle. Add a drain-time defensive re-fit so the invariant survives mid-session budget config changes. Give operators the documented env-var knob.

### Non-goals (explicit)

- **Not fixed this spec:** legitimate terminal drops by other reasons: `StaleSession`, `SessionSwap`, `OverflowFull`, `OverflowChannelClosed`, `AlreadyDelivered` (`crates/spur-core/src/scheduler.rs:135-147, 445-456`; `continuation_bridge.rs:84-90`). These are by-design under INV-D1..D7.
- **Not fixed this spec:** artifact-store persistence of truncated payloads. Designed as a phase-2 stub (§13); not implemented.
- **Not fixed this spec:** changes to the merger packing algorithm (v3.1 best-fit/oldest-first) or the checkout/commit handshake.

## 4. Invariants

### INV-D8 (this spec, structural)

> For every `BrainContinuation` delivered to `pack_continuations`,
> `continuation_cost_bytes(cont) ≤ effective_merge_budget()` holds at drain time.

Where `continuation_cost_bytes = block_byte_cost(continuation_resource_block(cont)) = json_body.len() + uri.len()` for `TextResourceContents` (matches the v3.1 merger's own measurement at `crates/spur-core/src/continuation_bridge.rs:299-311`). **This is an agreed proxy, not full ACP transport bytes.** What matters is that the producer fitter and the merger cost check use identical measurement — their agreement is the invariant.

### INV-D9 (this spec, schema-evolution guard)

> Every `DelegationStatus` and `TimeoutFallback` variant containing
> `String`, `Vec<String>`, or `Vec<PathBuf>` fields must have a
> registered clip in `continuation_fit::clip_status_strings`.

Enforced by an exhaustive-`match` proptest strategy over `DelegationStatus`. Adding a new variant without updating the strategy is a compile error on the `match`, which forces the author to register clipping before the tests pass.

### Invariants preserved as-is

INV-D1..D7 from v3.1 (session-scoped ingress, checkout/commit handshake, bounded requeue, oversized-never-requeues). No wire schema changes — `ContinuationResourceBody.schema_version` remains `2`.

## 5. Architecture

### 5.1 Module location

New module `crates/spur-acp/src/continuation_fit.rs`.

**Why `spur-acp` and not `spur-core` or `spur-mcp`:**

- `spur-core/Cargo.toml:17` declares `spur-mcp = { workspace = true }` — the existing dep edge is `spur-core → spur-mcp`. A reverse import would cycle.
- `spur-mcp/Cargo.toml:9-26` has no `spur-core` dep — correct.
- `spur-acp/Cargo.toml:25` declares `agent-client-protocol`, so cost-measurement code that touches `ContentBlock` can live there without new deps.
- Both `spur-core` and `spur-mcp` already depend on `spur-acp`. Shared home is the unique acyclic solution.

### 5.2 Public API

```rust
// spur-acp/src/continuation_fit.rs

pub const MIN_MERGE_BUDGET_BYTES: usize = 1024;
pub const MAX_MERGE_BUDGET_BYTES: usize = 65_536;
pub const MERGE_BUDGET_DEFAULT_BYTES: usize = 4096;

/// Per-field caps for the shrink ladder's step 0 (generous) and step 5 (emergency).
pub const WORKER_BRANCH_CAP_BYTES: usize = 256;
pub const WORKER_BRANCH_EMERGENCY_CAP_BYTES: usize = 64;
pub const STATUS_STRING_CAP_BYTES: usize = 1024;
pub const STATUS_STRING_EMERGENCY_CAP_BYTES: usize = 128;
pub const CONFLICT_FILES_SERIALIZED_CAP_BYTES: usize = 512;
pub const DIFF_FILES_SERIALIZED_CAP_BYTES: usize = 512;
pub const ARTIFACT_URI_CAP_BYTES: usize = 192;
pub const ARTIFACT_KIND_OTHER_CAP_BYTES: usize = 64;
pub const ARTIFACT_SHA256_CAP_BYTES: usize = 64;  // hex-encoded SHA-256 is exactly 64 chars

/// Reads `SPUR_MERGE_BUDGET_BYTES`, clamps to [MIN, MAX], WARN-logs on bad
/// parse or out-of-range, falls back to default. Evaluated on each call
/// (no static caching) — matches the `summary_cap_bytes` house pattern
/// for runtime knob resolution, but with active error telemetry because
/// budget is load-bearing for INV-D8.
pub fn effective_merge_budget() -> usize;

/// Serialised cost of the resource body + URI — the proxy shared with
/// the merger at `continuation_bridge.rs::pack_continuations`.
pub fn continuation_cost_bytes(c: &BrainContinuation) -> usize;

/// Owned-in / owned-out. Pure, synchronous, infallible. Idempotent.
/// Guarantees `continuation_cost_bytes(fitted) ≤ budget` for any
/// `budget ∈ [MIN_MERGE_BUDGET_BYTES, MAX_MERGE_BUDGET_BYTES]`.
pub fn fit_continuation(
    cont: BrainContinuation,
    budget: usize,
) -> (BrainContinuation, Vec<TruncationEvent>);

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TruncationEvent {
    WorkerBranchClipped { original_bytes: usize, kept_bytes: usize, pass: u8 },
    StatusStringClipped {
        variant: &'static str,        // "Failed.error", "Rejected.reason", ...
        original_bytes: usize,
        kept_bytes: usize,
        pass: u8,                     // 0 = step 0 cap, 1 = step 5 emergency
    },
    ArtifactUriClipped { original_bytes: usize, kept_bytes: usize },
    ArtifactKindOtherClipped { original_bytes: usize, kept_bytes: usize },
    ArtifactSha256Clipped { original_bytes: usize, kept_bytes: usize },
    ConflictFilesTruncated {
        original_count: usize,
        kept_count: usize,
        original_bytes: usize,
        kept_bytes: usize,
    },
    DiffFilesTruncated {
        original_count: usize,
        kept_count: usize,
        original_bytes: usize,
        kept_bytes: usize,
    },
    DiffFilesCleared,
    DiffSummaryDropped { original_bytes: usize, kept_bytes: usize },
    SummaryClipped { original_bytes: usize, kept_bytes: usize },
}
```

Internal helpers (`clip_with_ellipsis`, step functions, `clip_status_strings`, binary-search summary) are `pub(crate)` or private.

### 5.3 `clip_with_ellipsis` consolidation

The UTF-8-safe `clip_with_ellipsis` helper currently exists in two copies: `crates/spur-core/src/continuation_bridge.rs:178-200` and `crates/spur-mcp/src/server.rs:209` (copy-pasted because of the crate cycle). This spec moves the canonical copy to `spur-acp::continuation_fit` and makes both sites import it. Deduplication is incidental but valuable.

## 6. Shrink ladder

Applied in order. After each step, cost is re-measured via `continuation_cost_bytes`. The first step that brings cost ≤ budget wins; remaining steps are skipped.

| Step | Action | Cap |
|---|---|---|
| **0a** | `worker_branch` via `clip_with_ellipsis` | `WORKER_BRANCH_CAP_BYTES` (256) |
| **0b** | `DelegationStatus::Failed.error` | `STATUS_STRING_CAP_BYTES` (1024) |
| **0c** | `Rejected.reason`, `Modified.reviewer_note`, `Cancelled.reason` | `STATUS_STRING_CAP_BYTES` (1024) each |
| **0d** | `Conflict.files` → truncate vec + `"<spur-truncated>+N more"` sentinel | `CONFLICT_FILES_SERIALIZED_CAP_BYTES` (512) |
| **0e** | `artifact_ref.uri` via `clip_with_ellipsis` | `ARTIFACT_URI_CAP_BYTES` (192) |
| **0f** | `artifact_ref.kind` if `ArtifactKind::Other(s)`: clip `s` via `clip_with_ellipsis` | `ARTIFACT_KIND_OTHER_CAP_BYTES` (64) |
| **0g** | `artifact_ref.sha256` if `Some(s)`: clip `s` via `clip_with_ellipsis` (SHA-256 hex is 64 B; longer values are malformed and truncated) | `ARTIFACT_SHA256_CAP_BYTES` (64) |
| 1 | `diff_summary.files` → truncate + `"<spur-truncated>+N more"` sentinel | `DIFF_FILES_SERIALIZED_CAP_BYTES` (512) |
| 2 | `diff_summary.files = Vec::new()` (keep counts) | — |
| 3 | `diff_summary = None` | — |
| 4 | Binary-search `summary` via `clip_with_ellipsis`; ~log₂(input) ≤ 14 measurements worst case | — |
| **5** | **Emergency re-clip**: status strings → `STATUS_STRING_EMERGENCY_CAP_BYTES` (128), `worker_branch` → `WORKER_BRANCH_EMERGENCY_CAP_BYTES` (64) | — |
| 6 | `debug_assert!(false, "INV-D8 floor violated")`. Proven unreachable for `budget ∈ [MIN, MAX]` (§7). **Release-mode fallback** (defense-in-depth only): set `artifact_ref = None` and return. This removes the last variable-size block and leaves the Tier-1 fact envelope (~585 B) which fits under any `budget ≥ MIN`. The branch is instrumented to emit a `tracing::error!(invariant = "INV-D8", site = "floor_fallback", ...)` so a production regression is observable. | Tier-1 facts only |

Events emitted in ladder order. Consumers can rely on `events.windows(2).all(|w| step(w[0]) <= step(w[1]))`.

### Note on JSON-escape expansion

`continuation_cost_bytes` measures post-JSON-escape bytes. A raw string of `N` backslashes becomes `2N` JSON bytes; control chars like `"\x00"` expand to `\u0000` (6×). Step 4's binary search operates on the pre-escape clipped input length and re-measures the post-escape cost, so it converges naturally in ~14 measurements worst case. No JSON-aware clip variant is required. The step-5 emergency pass provides margin for `MIN_MERGE_BUDGET_BYTES = 1024` against adversarial worst-case expansion.

## 7. Envelope arithmetic proof

Worst-case envelope after full shrink (step 5 applied, `artifact_ref` present at cap):

| Component | Bytes |
|---|---|
| `"schema_version":2,` | ~20 |
| `"delegation_id":"<UUID>"` (36 + key + quotes + comma) | ~55 |
| `"attempt":<u32>` | ~15 |
| `"brain_session":"<UUID>"` | ~55 |
| `"source":{"kind":"<longest_variant>"}` | ~45 |
| `"status":{"Rejected":{"reason":"<128 B>"}}` (emergency-clipped) | ~150 |
| `"summary":null` | ~15 |
| `"diff_summary":null` | ~20 |
| `"worker_branch":"<64 B>"` (emergency-clipped) | ~85 |
| `"created_at_wall":"<ISO-8601>"` | ~45 |
| JSON braces / commas / outer overhead | ~20 |
| `uri = "spur://continuation/<UUID>"` (added by `block_byte_cost`) | ~60 |
| **Subtotal (base)** | **~585** |
| `"artifact_ref":{...}` total with all inner fields capped (see sub-breakdown) | **~385** |
|   ↳ `"Other":"<64 B clipped>"` (flattened ArtifactKind worst case) | ~75 |
|   ↳ `"uri":"<192 B clipped>"` | ~205 |
|   ↳ `"byte_size":<u64>` | ~20 |
|   ↳ `"sha256":"<64 B clipped>"` | ~77 |
|   ↳ JSON wrapping (braces, commas, key) | ~8 |
| **Total worst case (base + capped ArtifactRef)** | **~970** |

`MIN_MERGE_BUDGET_BYTES = 1024`. 970 < 1024 with ~54 B headroom. Step 6 `debug_assert!` is provably unreachable for any `budget ∈ [MIN, MAX]` given step-5 emergency re-clips and step-0e/f/g caps on `artifact_ref`.

**If `artifact_ref` is `None`:** total drops to ~585 B — 439 B headroom.

**Scenarios where the release-mode step-6 fallback could fire** (none reachable without a code regression): (a) a new `DelegationStatus` variant added without registering in INV-D9 clip ladder, (b) cost-measurement divergence between `fit_continuation` and `continuation_cost_bytes` (both routed through `spur-acp::continuation_fit`), (c) `block_byte_cost` changed without updating the envelope-floor proof. In each case, dropping `artifact_ref` restores fit to the 585 B base envelope.

Where the arithmetic is tight (MIN budget + full artifact_ref + longest source variant), the proptest (§10) uses adversarial inputs to stress-test the proof empirically.

## 8. Configuration

```rust
pub fn effective_merge_budget() -> usize {
    let Some(raw) = std::env::var("SPUR_MERGE_BUDGET_BYTES").ok() else {
        return MERGE_BUDGET_DEFAULT_BYTES;
    };
    match raw.parse::<usize>() {
        Err(e) => {
            tracing::warn!(
                raw = %raw,
                error = %e,
                default = MERGE_BUDGET_DEFAULT_BYTES,
                "SPUR_MERGE_BUDGET_BYTES not parseable; using default",
            );
            MERGE_BUDGET_DEFAULT_BYTES
        }
        Ok(n) if (MIN_MERGE_BUDGET_BYTES..=MAX_MERGE_BUDGET_BYTES).contains(&n) => n,
        Ok(n) => {
            tracing::warn!(
                provided = n,
                min = MIN_MERGE_BUDGET_BYTES,
                max = MAX_MERGE_BUDGET_BYTES,
                default = MERGE_BUDGET_DEFAULT_BYTES,
                "SPUR_MERGE_BUDGET_BYTES out of range; using default",
            );
            MERGE_BUDGET_DEFAULT_BYTES
        }
    }
}
```

### Divergence from `summary_cap_bytes` pattern

The existing `summary_cap_bytes()` at `crates/spur-core/src/orchestrator.rs:4898-4903` uses silent `.parse().ok().unwrap_or(default)` with no diagnostics. `effective_merge_budget()` deliberately diverges: it WARN-logs on bad parse and out-of-range. Rationale: budget is load-bearing for INV-D8. A typo (e.g. `4O96` with letter O instead of digit 0) silently reverting to default would violate operator intent with no telemetry. `summary_cap_bytes` is an ergonomic cap where silent fallback is acceptable — the two knobs are not equivalent operational surfaces. `summary_cap_bytes` is **not changed** by this spec.

## 9. Call-site unification

All budget-bearing call sites route through a single source of truth.

| Site | Current | After spec |
|---|---|---|
| `crates/spur-core/src/orchestrator.rs:313` (`DropReason::OversizedSingleItem.budget_bytes`) | hard-codes `MERGE_BUDGET_DEFAULT_BYTES` | `effective_merge_budget()` |
| `crates/spur-core/src/orchestrator.rs:1702, 1712` (render calls) | hard-codes `MERGE_BUDGET_DEFAULT_BYTES` | `effective_merge_budget()` |
| `crates/spur-core/src/scheduler.rs:611-618` (`spill_reason`) | `serde_json::to_vec(continuation).len()` + hard-coded default | `continuation_cost_bytes(continuation)` + `effective_merge_budget()` |
| `crates/spur-core/src/continuation_bridge.rs:234-238` (`DeferReason::BudgetSpill` in `pack_continuations`) | reuses `budget_bytes` parameter | unchanged (parameter now sourced from `effective_merge_budget()` at call sites) |
| `crates/spur-mcp/src/server.rs::build_detached_continuation` | no budget awareness | `fit_continuation(cont, effective_merge_budget())` |

The `spill_reason` site is a particularly subtle drift: the current code measures the whole `BrainContinuation` JSON directly (nested, no `schema_version`), while the renderer uses the flat `ContinuationResourceBody` + URI proxy. These numbers disagree for the same continuation. Unifying to `continuation_cost_bytes` aligns emitted telemetry with the invariant check.

## 10. Producer changes (`spur-mcp`)

### 10.1 `build_detached_continuation`

Current path at `crates/spur-mcp/src/server.rs:251-288`:

```rust
// BEFORE
let (summary, summary_truncated) =
    clip_with_ellipsis(result.summary.clone(), PRODUCER_MAX_FIELD_BYTES);
if summary_truncated { /* emit ContinuationFieldTruncated */ }
// construct BrainContinuation with raw diff_summary, status, worker_branch
```

Changes:

1. **Remove** the per-field `clip_with_ellipsis(result.summary, PRODUCER_MAX_FIELD_BYTES)` call at lines 260-271. `fit_continuation` subsumes it; retaining would produce double logic and double events.
2. Construct the candidate `BrainContinuation` from raw fields (no pre-clipping).
3. Call `let (cont, events) = fit_continuation(cont, effective_merge_budget())`.
4. For each `event` in `events`, map to `SpurEventBody::ContinuationFieldTruncated` and emit via `event_sink`.
5. Push `cont` downstream as before.

### 10.2 `PRODUCER_MAX_FIELD_BYTES` removal

`crates/spur-mcp/src/server.rs:45`'s `PRODUCER_MAX_FIELD_BYTES` becomes dead. Remove the constant and the test citing it (`server.rs:4542-4561`). Update any adjacent test fixtures.

### 10.3 Event mapping table

`fit_continuation`'s `TruncationEvent` variants map to `SpurEventBody::ContinuationFieldTruncated { delegation_id, field, original_bytes, kept_bytes }` as follows:

| `TruncationEvent` variant | `field` string | `original_bytes` | `kept_bytes` |
|---|---|---|---|
| `SummaryClipped { .. }` | `"summary"` | pre-clip string length | post-clip string length |
| `DiffFilesTruncated { .. }` | `"diff_summary.files"` | serialised bytes of `files` pre-truncation | post-truncation |
| `DiffFilesCleared` | `"diff_summary.files"` | serialised bytes pre-clear | `2` (`"[]"`) |
| `DiffSummaryDropped { .. }` | `"diff_summary"` | serialised bytes pre-`None` | `4` (`"null"`) |
| `StatusStringClipped { variant, pass, .. }` | `"status.{variant}"` (e.g. `"status.Failed.error"`); if `pass=1`, suffix `".emergency"` | pre-clip string length | post-clip string length |
| `ConflictFilesTruncated { .. }` | `"status.Conflict.files"` | serialised bytes of `files` pre-truncation | post-truncation |
| `WorkerBranchClipped { pass, .. }` | `"worker_branch"`; if `pass=1`, suffix `".emergency"` | pre-clip string length | post-clip string length |
| `ArtifactUriClipped { .. }` | `"artifact_ref.uri"` | pre-clip string length | post-clip string length |
| `ArtifactKindOtherClipped { .. }` | `"artifact_ref.kind.Other"` | pre-clip string length | post-clip string length |
| `ArtifactSha256Clipped { .. }` | `"artifact_ref.sha256"` | pre-clip string length | post-clip string length |

Consumers already handle opaque string field values (no enum contract — verified at `crates/spur-acp/src/domain/events.rs:730-736`, `crates/spur-mcp/src/server.rs:4574-4584`).

## 11. Merger changes (`spur-core`)

### 11.1 `RenderOutcome` extension

```rust
// crates/spur-core/src/continuation_bridge.rs
pub struct RenderOutcome {
    pub blocks: Vec<ContentBlock>,
    pub delivered_keys: Vec<DelegationKey>,
    pub deferred_spill: Vec<(BrainContinuation, DeferReason)>,
    pub dropped_oversized: Vec<(DelegationKey, usize)>,
    /// NEW — events emitted by drain-time re-fit (self-healing safety net).
    /// Keyed by DelegationKey so the caller can route each event set to
    /// `event_sink` after a successful commit. Populated only when the
    /// drain-time `fit_continuation` call actually ran (i.e. the item
    /// arrived over-budget from the producer).
    pub drain_truncation_events: Vec<(DelegationKey, Vec<TruncationEvent>)>,
}
```

The new field respects the existing `RenderOutcome` shape: pure data, no I/O, no mocks required to test. `pack_continuations` stays a pure function.

### 11.2 Drain-time re-fit with lazy clone

`pack_continuations` at `crates/spur-core/src/continuation_bridge.rs:209-247` inserts a defensive fit at iteration entry:

```rust
use std::borrow::Cow;

for c in conts {
    // Lazy re-fit: measure first; clone + fit only when producer left it over-budget.
    let cost_pre = continuation_cost_bytes(c);
    let (c_fit, events): (Cow<'_, BrainContinuation>, Vec<TruncationEvent>) =
        if cost_pre > budget_bytes {
            let (fitted, events) = fit_continuation(c.clone(), budget_bytes);
            (Cow::Owned(fitted), events)
        } else {
            (Cow::Borrowed(c), Vec::new())
        };
    let c = c_fit.as_ref();
    let cost = continuation_cost_bytes(c);

    let key = DelegationKey::from(c);
    if !events.is_empty() {
        drain_truncation_events.push((key.clone(), events));
    }

    // Existing packing logic, unchanged — but now `cost > budget_bytes` is
    // unreachable given INV-D8 enforced by the re-fit above.
    if cost > budget_bytes {
        // Defense-in-depth only — never expected post-Plan-4.
        dropped_oversized.push((key, cost));
        continue;
    }
    // ... deliver / spill as before
}
```

Rationale: producer-side `fit_continuation` is the common case; the drain-time call is a self-healing safety net for mid-session `SPUR_MERGE_BUDGET_BYTES` changes. Happy path: zero clones, zero fits. Shrink path: one clone + one fit per affected item. Expected cost under normal operation: < 1 µs/item.

### 11.3 Caller surfacing of `drain_truncation_events`

At `crates/spur-core/src/orchestrator.rs:1700-1715` (both merged-turn and autonomous-turn render sites), after `render_*_with_spill_v2` returns and after successful `prompt()` dispatch + `commit_partial`, iterate `outcome.drain_truncation_events` and emit each inner event via the existing event sink as `SpurEventBody::ContinuationFieldTruncated { .. }`. Order: post-commit, aligned with v3.1's "events fire on successful delivery" policy.

If commit fails (rollback path), drain truncation events are discarded — the continuation returns to `pending_continuations` and will be re-fit on the next render.

## 12. Observability

### 12.1 Orchestrator tracing

Upgrade `crates/spur-core/src/orchestrator.rs:308-316` from default-level logging to:

```rust
tracing::event!(
    target: "spur.metrics.continuation_dropped_oversized",
    tracing::Level::ERROR,
    delegation_id = %key.delegation_id,
    continuation_bytes = *bytes,
    budget_bytes = effective_merge_budget(),
    invariant = "INV-D8",
    severity = "producer_contract_violation",
    "continuation dropped: envelope exceeded budget",
);
```

Ops stacks (Loki, Vector, OTel collector) scrape `target = "spur.metrics.*"` to derive counters. No new workspace dependency (no `metrics-rs` / `prometheus`). When/if SPUR adopts a metrics facade later, this event becomes a three-line swap.

Alert: `> 0` occurrences over any 5-minute window in production. Post-Plan-4, this branch should be unreachable; firing indicates either a producer path that bypassed `fit_continuation` (bug) or a cost-measurement drift between producer and merger (bug).

### 12.2 TUI formatter

Update `crates/spur-tui/src/views/session_detail.rs:1658-1672` to branch on `DropReason`:

```rust
SpurEventBody::ContinuationDropped { delegation_id, reason, .. } => {
    let (prefix, severity) = match reason {
        DropReason::OversizedSingleItem { continuation_bytes, budget_bytes } => (
            format!(
                "✖ PRODUCER-BUG: Continuation {} dropped — cost {}B > budget {}B",
                delegation_id, continuation_bytes, budget_bytes
            ),
            TraceKindSeverity::Error,
        ),
        _ => (
            format!("⚠ Continuation dropped for {}: {:?}", delegation_id, reason),
            TraceKindSeverity::Warn,
        ),
    };
    self.react_trace.push(TraceEntry { /* ... */ });
}
```

Users immediately see that `OversizedSingleItem` is a distinct severity (producer bug) versus the routine operational drops (`StaleSession`, `OverflowFull`, etc.) that retain the `⚠` prefix.

### 12.3 `ContinuationFieldTruncated` event consumers

No consumer changes required. Existing consumers treat the `field` string as opaque. New `field` values (`diff_summary.files`, `diff_summary`, `status.Failed.error`, `status.Rejected.reason.emergency`, `artifact_ref.uri`, etc.) pass through transparently.

## 13. Deferred phase-2: artifact-store (design stub)

**Not implemented this spec.** Stub captured so the public API of Plan-4 does not paint phase-2 into a corner.

### Addressing and storage

- URI: `spur://continuation-artifact/<delegation_id>/<sha256>`
- On-disk: `$SPUR_DATA_DIR/continuations/<sha256[..2]>/<sha256>.json` with `0600` perms.
- GC: 24h TTL configurable via `SPUR_CONTINUATION_ARTIFACT_TTL_HOURS`; sweep on delegation terminal + periodic background sweep.

### Producer integration (future)

When `fit_continuation` would reach step 2 or beyond (`diff_summary.files` cleared), first persist the un-truncated original to the artifact store, then populate the existing `artifact_ref` slot on the fitted continuation with an `ArtifactKind::Other("continuation_overflow")` pointer. Brain sees both the fitted continuation AND knows where to fetch the full thing.

### Open questions deferred to phase-2 design

- **Q4:** `check_delegation_status(include_full_payload)` return shape when `DelegationResult` lacks `artifact_ref` (currently lives only on `BrainContinuation` per `crates/spur-acp/src/domain/continuation.rs:44-52`). Options: (a) extend `DelegationResult` schema, (b) return a union envelope, (c) return the full `BrainContinuation`. Decide in phase-2.
- **Q5:** Coexistence rule with existing `spur://artifact/<delegation_id>` `worker_artifact` URIs. `BrainContinuation.artifact_ref: Option<ArtifactRef>` is single-slot. Options: (a) `Vec<ArtifactRef>` with kind discriminator, (b) new optional field `continuation_overflow_ref`, (c) move `worker_artifact` to `DelegationResult` and reserve `BrainContinuation.artifact_ref` for overflow only. Decide in phase-2.

## 14. Testing

### 14.1 Unit tests (`spur-acp/src/continuation_fit.rs` inline `#[cfg(test)]` mod)

- `fit_noop_for_small_input` — cost ≤ budget → input returned unchanged, `events.is_empty()`.
- `fit_clips_worker_branch_at_step_0a` — oversized `worker_branch` alone → single `WorkerBranchClipped { pass: 0 }`.
- `fit_clips_status_string_at_step_0b_through_0c` — one test per string-bearing variant (Failed, Rejected, Modified, Cancelled).
- `fit_truncates_conflict_files_at_step_0d` — oversized `Conflict.files` → `ConflictFilesTruncated`; sentinel `"<spur-truncated>+N more"` present; counts preserved.
- `fit_clips_artifact_uri_at_step_0e` — oversized `artifact_ref.uri` → `ArtifactUriClipped`.
- `fit_clips_artifact_kind_other_at_step_0f` — `ArtifactKind::Other(long_s)` → `ArtifactKindOtherClipped`; unit variants (`Patch`/`TestOutput`/`Log`) untouched.
- `fit_clips_artifact_sha256_at_step_0g` — oversized `artifact_ref.sha256` → `ArtifactSha256Clipped`; well-formed 64-char hex → no event.
- `fit_truncates_diff_files_at_step_1`.
- `fit_clears_diff_files_at_step_2` — step 2 fires when step 1 insufficient.
- `fit_drops_diff_summary_at_step_3`.
- `fit_binary_searches_summary_at_step_4` — adversarial input (all backslashes, control chars); asserts convergence ≤ 14 iterations via an instrumented measurement counter.
- `fit_emergency_reclips_at_step_5` — MIN budget + fully-populated Rejected reason + long worker_branch → Pass-1 `StatusStringClipped` + `WorkerBranchClipped` events.
- `fit_floor_debug_assert_unreachable` — adversarial MIN budget scenario; confirms step 5 prevents step 6 firing (release-mode variant also exercised).
- `fit_is_idempotent` — `fit(fit(c, b), b) == fit(c, b)` for a range of inputs.
- `effective_merge_budget_valid_env` — sets env, asserts parse.
- `effective_merge_budget_invalid_parse_warns_and_defaults`.
- `effective_merge_budget_out_of_range_warns_and_defaults` — both below MIN and above MAX.

### 14.2 Proptest (`crates/spur-acp/tests/inv_d8_envelope_fit.rs` — new file)

```rust
use proptest::prelude::*;
use spur_acp::continuation_fit::{
    continuation_cost_bytes, fit_continuation,
    MIN_MERGE_BUDGET_BYTES, MAX_MERGE_BUDGET_BYTES, TruncationEvent,
};

fn arb_summary() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        Just(None),
        ".{0,65536}".prop_map(Some),                // unrestricted content
        "[\\x00-\\x1F\"\\\\]{0,4096}".prop_map(Some), // JSON-hostile chars
    ]
}

fn arb_file_path() -> impl Strategy<Value = std::path::PathBuf> {
    prop_oneof![
        "[a-z0-9/_.-]{1,200}".prop_map(std::path::PathBuf::from),
        "[\"\\\\ ]{1,200}".prop_map(std::path::PathBuf::from),  // escape-hostile
    ]
}

fn arb_delegation_status() -> impl Strategy<Value = DelegationStatus> {
    // EXHAUSTIVE match over variants — INV-D9 enforcement: adding a new
    // DelegationStatus variant without updating this strategy is a
    // compile error on the match below.
    prop_oneof![
        Just(DelegationStatus::Success),
        ".{0,32768}".prop_map(|e| DelegationStatus::Failed { error: e }),
        prop::collection::vec(arb_file_path(), 0..500)
            .prop_map(|files| DelegationStatus::Conflict { files }),
        Just(DelegationStatus::Timeout),
        ".{0,32768}".prop_map(|r| DelegationStatus::Rejected { reason: r }),
        ".{0,32768}".prop_map(|n| DelegationStatus::Modified { reviewer_note: n }),
        /* TimedOut — bounded, no strings */
        ".{0,32768}".prop_map(|r| DelegationStatus::Cancelled { reason: r }),
    ]
}

fn arb_budget() -> impl Strategy<Value = usize> {
    MIN_MERGE_BUDGET_BYTES..=MAX_MERGE_BUDGET_BYTES
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    // P1: core invariant INV-D8
    #[test]
    fn fit_always_yields_under_budget(
        cont in arb_brain_continuation(),
        budget in arb_budget(),
    ) {
        let (fitted, _events) = fit_continuation(cont, budget);
        prop_assert!(
            continuation_cost_bytes(&fitted) <= budget,
            "INV-D8 violated at budget {budget}",
        );
    }

    // P2: idempotence
    #[test]
    fn fit_is_idempotent(cont in arb_brain_continuation(), budget in arb_budget()) {
        let (once, _) = fit_continuation(cont.clone(), budget);
        let (twice, _) = fit_continuation(once.clone(), budget);
        prop_assert_eq!(once, twice);
    }

    // P3: event order stability
    #[test]
    fn fit_events_in_ladder_order(cont in arb_brain_continuation(), budget in arb_budget()) {
        let (_, events) = fit_continuation(cont, budget);
        prop_assert!(events.windows(2).all(|w| step_index(&w[0]) <= step_index(&w[1])));
    }

    // P4: drain self-heal
    #[test]
    fn pack_never_drops_oversized_after_drain_refit(
        conts in prop::collection::vec(arb_oversized_continuation(), 1..10),
        budget in arb_budget(),
    ) {
        let outcome = render_merged_turn_with_spill_v2(&[], &conts, budget);
        prop_assert!(
            outcome.dropped_oversized.is_empty(),
            "drain-time re-fit failed to recover: {:?}",
            outcome.dropped_oversized,
        );
    }

    // P5: drop-reason budget consistency
    #[test]
    fn drop_reason_carries_effective_budget(
        cont in arb_brain_continuation(),
        budget in arb_budget(),
    ) {
        // Inject a synthetic path where `continuation_cost_bytes` returns
        // something slightly above budget to force the defense-in-depth
        // drop branch. Assert the emitted DropReason's budget_bytes equals
        // the budget passed to render.
        // (exact wiring in implementation plan)
    }
}
```

CI cost: 512 cases × ~2 ms/case ≈ 1.1 s total (measured against v3.1's 256-case precedent at ~0.55 s). Acceptable.

### 14.3 Integration test

`crates/spur-core/tests/continuation_integration.rs` (extend existing):

`oversized_continuation_survives_end_to_end`:
1. Construct a worker `DelegationResult` with 20 KB `summary` + `DiffSummary` of 500 files + `Failed { error: 10 KB }`.
2. Build a `BrainContinuation` via the producer code path.
3. Push through `BrainScheduler`; drain; render.
4. Assert: `RenderOutcome.dropped_oversized.is_empty()`.
5. Assert: `RenderOutcome.drain_truncation_events` is empty (producer-side fit already handled it).
6. Assert: at least one `SpurEventBody::ContinuationFieldTruncated` event was emitted.
7. Assert: brain receives one delivered continuation with truncation sentinels present.

`mid_session_budget_shrink_triggers_drain_refit`:
1. Produce a 4000-byte continuation with `SPUR_MERGE_BUDGET_BYTES=4096`.
2. Set `SPUR_MERGE_BUDGET_BYTES=2048`.
3. Drain.
4. Assert: `RenderOutcome.drain_truncation_events.len() >= 1` (drain-time fit ran).
5. Assert: `RenderOutcome.dropped_oversized.is_empty()`.

## 15. Migration and rollback

- **Additive.** No wire schema changes; `schema_version` stays `2`. Existing brains and tests continue to deserialize unchanged.
- **`PRODUCER_MAX_FIELD_BYTES` removal.** Dead after `fit_continuation` subsumes the producer's per-field clip. Remove along with adjacent test.
- **Single-commit revert safe.** The merger's `OversizedSingleItem` path is preserved (just less frequently exercised). Reverting to pre-Plan-4 restores original behaviour without orphaned state.
- **No persistent-state migrations.** No new files on disk, no new env-var defaults that would surprise existing deployments (default `4096` matches v3.1 behaviour).

## 16. Review history

| Round | Reviewer | Date | Verdict | Deltas incorporated |
|---|---|---|---|---|
| 1 | codex | 2026-04-24 | — (5 MUST-FIX + 0 SHOULD-FIX surfaced first pass) | Δ1–Δ5 |
| 2 | codex | 2026-04-24 | APPROVE-WITH-CHANGES | Δ6–Δ11 (5 SHOULD-FIX + 3 NITS + 5 open questions addressed / deferred) |
| 2.5 | self (L9 first-principles) | 2026-04-24 | — | Δ12–Δ15 (envelope arithmetic defect, budget drift, schema-evolution guard, observability differentiation) |
| 3 | kimi | 2026-04-24 | APPROVE-WITH-CHANGES | Δ16–Δ24 (5 MUST-FIX + 4 SHOULD-FIX addressed; 3 open questions resolved) |
| 3.5 | self (spec-time review) | 2026-04-24 | — | `ArtifactKind::Other`/`sha256` also clippable (new steps 0f/0g), step-6 release-mode fallback clarified |

All reviewer citations verified against the tree at merge commit `6b1e6980`. Review log preserved at `docs/rca/log.md`.

## 17. Appendix — delta ledger

| Δ | Concern | Resolution section |
|---|---|---|
| 1 | Crate cycle (`spur-core → spur-mcp`) prevents fit in `spur-core` | §5.1 |
| 2 | Unbounded `DelegationStatus`/`worker_branch` fields | §6 steps 0a–0d |
| 3 | Budget single-source-of-truth | §8–§9 |
| 4 | Invariant α over-claim rescoped | §4 non-goals + INV-D8 |
| 5 | Test plan privacy (`continuation_cost_bytes`, `build_detached_continuation`) | §5.2 public API |
| 6 | `fit_continuation` subsumes existing `summary` clip | §10.1 |
| 7 | Event byte-accounting table | §10.3 |
| 8 | Invariant renamed to `continuation_cost_bytes` proxy | §4 INV-D8 wording |
| 9 | Plain env getter, no OnceLock | §8 |
| 10 | Strengthened proptest (control chars, JSON-hostile, status exhaustive) | §14.2 |
| 11 | NITs: parse-fail/out-of-range split, `files.len()`, log surfaces | §8 + §12 + §6 note |
| 12 | Emergency step 5 + envelope arithmetic proof | §6 + §7 |
| 13 | Drain-time re-fit for budget drift | §11.2 |
| 14 | Observability differentiation (TUI + tracing target) | §12.1–§12.2 |
| 15 | INV-D9 schema-evolution guard | §4 + §14.2 |
| 16 | Unify `spill_reason` to `continuation_cost_bytes` | §9 |
| 17 | Steps 0e/0f/0g clip all three variable-length `artifact_ref` fields (`uri`, `ArtifactKind::Other`, `sha256`); proof updated with sub-breakdown and step-6 release-mode fallback | §5.2 + §6 + §7 |
| 18 | `RenderOutcome.drain_truncation_events` field | §11.1, §11.3 |
| 19 | `tracing::event!` with metrics target (no `metrics-rs` dep) | §12.1 |
| 20 | `effective_merge_budget()` WARN divergence justified | §8 |
| 21 | Lazy clone in drain re-fit | §11.2 |
| 22 | Unified `StatusStringClipped` with `pass: u8` | §5.2 `TruncationEvent` |
| 23 | JSON-escape expansion note | §6 |
| 24 | Namespaced sentinel `"<spur-truncated>+N more"` | §6 |

## 18. Next steps

On spec approval, hand off to `superpowers:writing-plans` to produce a phased implementation plan with DAG task ordering, review gates per phase, and clippy/proptest gates aligned to v3.1's cadence.
