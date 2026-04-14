# Brain-Worker Communication — Phase 1 Refinement

**Status:** design
**Date:** 2026-04-14
**Reference architecture:** `docs/spur/brain-worker-architecture.md`
**Area:** `spur-core` orchestrator · `spur-mcp` callback server · `spur-acp` domain types

## Problem

`docs/spur/brain-worker-architecture.md` identifies three real gaps:

1. **Brain gets impoverished feedback.** `DelegationResult.summary` is truncated to 500 chars; errors are the string `"Worker reported errors"`; no diff stats. The brain makes its next planning decision nearly blind.
2. **Brain session identity not threaded.** `McpCallbackServer` receives the brain's `SessionId` but doesn't persist it; `DelegationRequest` can't carry it; events attribute delegations to the worker session. Lineage cannot reconstruct the brain → worker tree.
3. **Retry loop is open.** Each retry worker is a blank slate — the augmented task drops previous attempts' summaries, diffs, and reviewer feedback. Violates the Reflexion pattern that every surveyed framework implements.

The architecture doc's proposed Phase 1 (three targeted changes, ~130 LoC) moves in the right direction. A multi-round evaluation against the actual code surfaced seven refinements that belong in the same change set:

| # | Issue | Evidence | Consequence if unfixed |
|---|---|---|---|
| R1 | `&output_text[..500]` at `orchestrator.rs:2492` is a byte slice | Multi-byte UTF-8 boundary slice | Panic on any worker output containing em-dashes, non-Latin text, code with unicode identifiers |
| R2 | Head-weighted 2:1 truncation loses the conclusion | LLM workers restate the task at the head; the decision-relevant summary is at the tail | Brain reads restatement + middle tool output, misses the worker's conclusion |
| R3 | Regex-based diff-stat parser is brittle | Binary diffs, rename markers, mode changes, `/dev/null` paths | Miscounts, wrong file lists on common diff shapes |
| R4 | `ReviewPayload.diff_summary` already exists but is passed `None` (`orchestrator.rs:1814`) | Review gate type has the field; orchestrator never populates it | Humans reviewing workers see no diff stats either |
| R5 | Error string is a generic literal (`orchestrator.rs:2503`) | `"Worker reported errors".into()` regardless of actual failure | Brain cannot distinguish "retry with more context" from "abandon, nothing will help" |
| R6 | Retry accumulator as tuple loses structure | Doc proposes `Vec<(u32, String, String)>` | Readability, and the diff — the most concrete artifact — is dropped |
| R7 | Existing comment deliberately chose non-accumulating retry text (`orchestrator.rs:2016-2018`) | "Prevents compounding constraint text across N retries" | Change 3 reverses this choice without engaging the tradeoff |

## Goals

1. **Thread brain session identity end-to-end** so lineage, cost attribution, and future session resumption can all pivot on `brain_session_id`.
2. **Make `DelegationResult` decision-grade** — widen summary, make it UTF-8-safe, preserve the conclusion, attach diff stats, and stop handing the brain a generic error string.
3. **Close the retry-level Reflexion loop** so retry workers see what earlier attempts tried and why they were rejected.
4. **Reuse the same enrichment helper at both DelegationResult and ReviewPayload sites** — humans and the brain deserve symmetric information density.

## Non-goals

- `ErrorKind` taxonomy enum. Requires worker-side structured exit the workers don't emit today. Phase 2.
- `expected_output` field on `DelegationRequest` (CrewAI pattern). Changes the brain prompt contract. Deserves its own spec.
- Brain-level retry history (retry attempts surfaced in `DelegationResult`). Inner loop (this spec) closes between retries; outer loop across delegations remains Phase 2.
- Executor abstraction / worker cancellation / split broadcast bus / WORKER_REPORT.md filesystem handoff / async delegation model. All legitimately Phase 2.

## Design

Four changes across the same four files the architecture doc already identified. Each change is additive — no existing fields removed, no method signatures re-ordered.

### Change 1 — Thread `brain_session_id`

**Closes gap P5** (orchestrator identity).

**Files:** `crates/spur-mcp/src/server.rs`, `crates/spur-mcp/src/tools.rs`, `crates/spur-core/src/orchestrator.rs`.

**Propagation path:**

1. `McpCallbackServer::new(&session_id)` persists the brain session on the server struct (currently discards after building the socket path).
2. `DelegationRequest` gains `pub brain_session_id: SessionId`.
3. Every request-construction site in `server.rs` stamps the field. This includes the eight tool handlers: `delegate_to_worker`, `delegate_parallel` (per-task), `get_issue`, `update_issue`, `create_pr`, `report_progress`, `get_session_cost`. PM and session-cost handlers share the same brain; uniform propagation keeps the mental model simple.
4. `handle_delegations` destructures the new field and forwards it to `execute_delegation`.
5. `execute_delegation` uses `brain_session_id` when emitting:
   - `DelegationRequested.from` (currently `worker_session`)
   - `DelegationDispatched.from` (currently `worker_session`)

**Non-breaking:** `DelegationRequest` is internal to the spur-mcp ↔ spur-core boundary. No JSON contract crosses.

**Estimated size:** ~30 LoC.

### Change 2 — Enrich `DelegationResult` and `ReviewPayload`

**Closes gaps P1** (structured results) **and P2** (rich feedback).

**Files:** `crates/spur-acp/src/domain/delegation.rs`, `crates/spur-core/src/orchestrator.rs`.

**Struct diff** (`delegation.rs`):

```rust
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
+   pub diff_summary: Option<DiffSummary>,
    pub summary: Option<String>,                 // widened cap, see below
    pub estimated_cost_usd: f64,
}
```

`DiffSummary` is reused from `crates/spur-acp/src/domain/events.rs:29` — already has `files_changed`, `insertions`, `deletions`, `files`.

**Smart truncation (`truncate_summary`)** — tail-weighted, char-boundary-safe, env-configurable:

```text
cap   = env("SPUR_SUMMARY_MAX_BYTES").unwrap_or(4000)
head  = cap / 4                     // 1000 bytes by default
tail  = cap - head                  // 3000 bytes by default

if text.len() <= cap           → return text
else:
    h = char_boundary_floor(text, head)          // first h bytes, aligned to char boundary
    t = char_boundary_ceil (text, text.len() - tail)   // last (len - t) bytes
    return "{text[..h]}\n\n[... N chars omitted ...]\n\n{text[t..]}"
```

Rationale for tail-weight: LLM worker output typically opens with task restatement and closes with a crisp summary + file list. The middle holds tool-call transcripts with high token count but low decision value. Brain-relevant information is concentrated at the tail.

Rationale for env var: lets us widen in production without a recompile when a truncated summary is observed in the wild.

**DiffSummary computation (`build_diff_summary`)**: call `git diff --numstat` against the worktree (separate call from the existing `collect_diff`). Parse the tab-delimited output into `DiffSummary`. ~15 LoC, no regex, no dependency on exact unified-diff shape.

**Dual populate:** the same helper feeds two sites:

- `run_one_worker_attempt` → `WorkerAttemptOutcome` → `DelegationResult.diff_summary`
- `execute_delegation`'s review-gate payload construction (currently `diff_summary: None` at `orchestrator.rs:1814`) → `ReviewPayload.diff_summary`

**Error-string replacement (covers R5):** at `orchestrator.rs:2502-2505`, replace the literal `"Worker reported errors"` with `tail_of(output_text, 500)` when worker_success == false. Uses the same char-boundary-safe truncation helper. Brain now sees the last 500 chars of the worker's output as the error — almost always the actual compiler error, test failure, or panic message.

**Estimated size:** ~70 LoC (50 in orchestrator for truncation + numstat helper + dual-site wiring; ~20 in delegation.rs for struct + serde).

### Change 3 — Retry History Accumulator

**Closes gap P3** (Reflexion loop).

**Files:** `crates/spur-core/src/orchestrator.rs` (execute_delegation retry arm only).

**New private type** (module-local):

```rust
struct RetryAttempt {
    attempt_n: u32,
    summary: String,                    // already truncated by Change 2
    diff_summary: Option<DiffSummary>,  // concrete artifact, not just prose
    feedback: String,                   // reviewer's new_constraints verbatim
}
```

**Accumulator:** `let mut retry_history: Vec<RetryAttempt> = Vec::new();` in `execute_delegation`, populated on each `ReviewDecision::Retry` before re-entering the loop.

**Augmented task template:**

```text
{original_task}

--- Previous attempts ---

Attempt 1:
  What was tried: {summary_1}
  Files touched: {files_changed_1} changed, +{ins_1}/-{del_1}
  Reviewer feedback: {feedback_1}

Attempt 2:
  ...

--- Your task ---

Address the reviewer's most recent feedback above. Do NOT repeat
approaches that were rejected earlier — the reviewer sees the same
history and will reject a repeat.
```

**Bloat cap (addresses R7):** the original designer's comment at `orchestrator.rs:2016-2018` flags that compounding constraint text risks prompt bloat. Counter-measures in this design:

1. **Per-attempt summary already capped** by Change 2 (4 KB default).
2. **Total retry context cap** of 2 KB enforced — if accumulated history exceeds 2 KB, drop the OLDEST attempts first (keep the most recent, which are most relevant to the current feedback).
3. **`max_review_retries`** already bounds the loop (spur-acp config).

With `max_review_retries = 3` and summaries capped at 4 KB, worst-case accumulated history is ~12 KB before the 2 KB cap kicks in — so the cap is usually non-binding, present only as a safety belt.

**Trade-off documented:** accumulation costs tokens; not accumulating costs the worker's ability to avoid repeating failed approaches. The surveyed frameworks (Reflexion, LangGraph, Anthropic) agree the information is worth the tokens. This change inverts the original comment's choice, with a bloat cap as mitigation.

**Estimated size:** ~60 LoC.

Note: the generic error-string replacement (R5) lives at the same call site as Change 2's truncation and uses the same helper — it's included in Change 2's scope, not a separate change.

## File touch summary

| File | Changes |
|---|---|
| `crates/spur-mcp/src/server.rs` | Store `brain_session_id` on struct; stamp onto 8 handlers (Change 1) |
| `crates/spur-mcp/src/tools.rs` | Add `brain_session_id` to `DelegationRequest` (Change 1) |
| `crates/spur-acp/src/domain/delegation.rs` | Add `diff_summary` to `DelegationResult` (Change 2) |
| `crates/spur-core/src/orchestrator.rs` | Thread `brain_session_id`; tail-weighted UTF-8-safe truncation helper; `build_diff_summary` helper; populate both `DelegationResult` and `ReviewPayload`; replace generic error string; retry accumulator loop (Changes 1, 2, 3) |

Total: ~160 LoC across 4 files. Matches the architecture doc's file set; 30 LoC above its 130-line estimate.

## Testing

**Unit:**

- `truncate_summary` — table-driven tests for (a) under-cap passthrough, (b) exact-cap passthrough, (c) over-cap with head+tail + omission marker, (d) UTF-8 boundary cases (em-dash at byte 499, 500, 501), (e) env var override.
- `build_diff_summary` — golden tests against synthetic `git diff --numstat` output: normal, binary file (`-\t-\tpath`), rename (`old → new` path).
- `RetryAttempt` accumulation — build a fake history and assert the augmented-task template renders exactly (string-equality test).
- `RetryAttempt` bloat cap — feed oversized summaries, assert oldest attempts drop first.

**Integration (uses existing test harness in spur-core):**

- `test_brain_session_id_threads_to_events` — spawn a brain session, invoke `delegate_to_worker` via the MCP server, subscribe to the event bus, assert `DelegationRequested.from == brain_session_id` (not worker_session).
- `test_delegation_result_round_trip` — send a delegation that produces a real diff, assert `diff_summary.files_changed > 0` in the result returned to the brain's MCP tool call.
- `test_review_payload_has_diff_summary` — trigger a review gate on a delegation with a diff, assert `ReviewPayload.diff_summary` is `Some`.
- `test_retry_history_in_augmented_task` — two-retry run, assert the third attempt's task text contains both prior attempts' summaries.

**Manual smoke:**

- Spawn a brain, have it delegate a small task (e.g., "add a hello-world function") with `review_required=true`. Reject once with feedback. Observe the retry worker's prompt in the TUI and verify it contains the first attempt's summary.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Widened summary (4 KB) pressures brain context window under heavy parallel delegation | `SPUR_SUMMARY_MAX_BYTES` env var allows narrowing without recompile; default stays well under 5% of a 128 KB window even with 5 parallel delegations. |
| `git diff --numstat` adds ~50ms per delegation | Await Point Analysis already budgets 10-100ms for git operations in the happy path; the second call is in the same budget. |
| Retry context bloat across many retries | 2 KB total cap + per-attempt summary cap + `max_review_retries` bound. Worst-case math documented above. |
| Accumulated retry context may confuse the worker if reviewer feedback contradicts itself across attempts | The prompt template explicitly frames history as "what NOT to repeat" rather than "requirements to satisfy simultaneously". Current designer's comment about compounding text engaged directly in Change 3's design. |
| UTF-8 boundary helper is easy to get wrong | Use `floor_char_boundary` (stable since 1.80) rather than hand-rolling `char_indices` logic. Rust's stdlib handles the edge cases. |

## What this does NOT change

- The four-channel architecture (MCP bridge, delegation pipeline, event bus, review gate). All remain.
- `DelegationStatus` variants. `#[non_exhaustive]` on the enum means additive `ErrorKind` remains an open Phase 2 option.
- Brain prompt contracts. The brain already deserializes `DelegationResult`; new fields are additive `Option`s.
- ACP / MCP protocol surface. No new tools, no new methods.

## Phase 2 backlog (unchanged from architecture doc, flags added)

| Item | Blocker |
|---|---|
| `ErrorKind` taxonomy | Needs worker-side structured exit signal (worker SDK → orchestrator contract) |
| `expected_output` on `DelegationRequest` | Needs brain prompt contract change; own spec |
| Brain-level retry history surfaced in `DelegationResult` | Tackle after inner-loop Reflexion is validated in production |
| Executor abstraction (cancel + tracking) | Cost-of-orphan measurement first; may turn out to be non-issue |
| Split broadcast bus | Defer until TUI lag is observed |
| Async delegation model | Protocol-level change; separate design cycle |
| WORKER_REPORT.md filesystem handoff | Only if truncation proves insufficient |
