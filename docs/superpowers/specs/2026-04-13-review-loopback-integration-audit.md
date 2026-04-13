# Review Loopback — Brain↔Executor Integration Audit

**Date:** 2026-04-13
**Status:** Findings (no implementation gate — recommended fixes listed but not designed)
**Audits:** `2026-04-13-orchestrator-review-loopback-design.md` + the merged
implementation (commits `a72fec5..c90b040`).
**Method:** MCTS-style multi-round walk over 12 brain↔executor user
journeys, cross-checked against `spur-tui` integration code.

## TL;DR

The Rust core is type-correct end-to-end. The two seams that touch
non-Rust modalities — the **TUI input flow** (human → ReviewDecision)
and the **MCP tool-result framing** (DelegationStatus → brain LLM) —
are missing. Out of 4 review-decision verbs, **only `Approve` carries
usable signal end-to-end today**. `Reject`, `Modify`, and `Retry` are
wired type-correctly but degrade to placeholder strings because the
TUI provides no path to type free text alongside a hotkey. The
brain-facing tool result is a raw JSON dump with no schema documentation
in the tool description, so the spec's central distinction (`Rejected`
vs `TimedOut`) is invisible to the LLM that consumes it.

## P0 — The loop is half-broken

### F1. TUI cannot supply free-text reason / note / constraints

**Evidence:** `crates/spur-tui/src/views/dashboard.rs:347`
```rust
crate::components::review_card::decision_for_key(ch, None)
```
The `prompt_answer` argument is **always `None`**. Per
`crates/spur-tui/src/components/review_card.rs:60-74`, this means:
- `'d'` → `Reject { reason: "(no reason given)" }`
- `'m'` → `Modify { note: "(no note)" }`
- `'R'` → `Retry { new_constraints: "(no constraints)" }`

**Impact on brain↔executor loop:**
- Brain receives `Rejected.reason = "(no reason given)"` — spec
  line 173 says `Rejected.reason` is "actionable feedback the brain
  can address on a retry." It cannot address a placeholder. Brain
  will either (a) re-delegate the same task verbatim (doom loop),
  (b) hallucinate what the human meant, or (c) bail.
- `Retry` loop respawns the worker with task =
  `"{original}\n\n## Additional constraints\n(no constraints)"`. Worker
  re-runs with no actual additional input. Same diff. Operator hits
  Retry again. By the 4th iteration the retry-limit backstop fires
  with `Failed { error: "retry limit exceeded after 3 attempts" }`.
- `Modify` is identically empty — brain has no caveat to incorporate.

**Net effect:** 3 of 4 review verbs are functionally unreachable from
the TUI. The whole `ReviewDecision` enum richness (the spec's "novel
synthesis" over OpenAI's binary needs_approval) is decorative.

**Fix shape (not designed):** Two-mode review input. After 'd'/'m'/'R'
the input bar is repurposed as a labeled prompt ("Reject reason:",
"Modify note:", "Retry constraints:") and Enter on that submits with
the typed text. Or: a modal dialog (analogous to `QuitConfirmDialog`
at `crates/spur-tui/src/components/quit_confirm.rs`) with labeled
fields plus diff preview.

### F6. Orchestrator drops `outcome.diff` when building the review payload

**Evidence:** `crates/spur-core/src/orchestrator.rs:1713-1718`
```rust
let review_payload = ReviewPayload {
    summary: outcome.summary.clone().unwrap_or_default(),
    diff_summary: None,        // ← outcome.diff exists but is ignored
    pr_url: None,
    error: None,
};
```
`outcome.diff` is `Option<String>` containing the unified diff the
worker produced. It is threaded into `apply_worktree_cleanup` and
`finalize` but never folded into the `DiffSummary` the operator sees
in the review card. `crates/spur-tui/src/components/review_card.rs:26-30`
only renders the diff line if `diff_summary` is `Some`.

**Impact on brain↔executor loop:** Operator approves/rejects/modifies
**without seeing what changed**. The review gate is decorative as a
correctness check — the operator can only trust the worker's
self-summary. A worker that introduces a backdoor in a file the
operator doesn't even know was touched will sail through. This
compounds with F1: operator can't see the code AND can't articulate
why they're rejecting.

**Fix shape (not designed):** Compute `DiffSummary { files_changed,
insertions, deletions }` from `outcome.diff` (parse unified diff
hunks; or thread a `DiffStat` from the worker's git operation
directly). Stretch: render a diff preview in the review card itself,
or in a dedicated detail-pane tab.

### F12. Brain receives raw JSON with no schema documentation

**Evidence A — tool description silent on review semantics:**
`crates/spur-mcp/src/tools.rs:44`
```rust
description: "Delegate a task to a worker agent. Blocks until the worker completes.".into(),
```
The tool definition exposed to the brain LLM says nothing about:
possible status variants, the difference between worker `Timeout` and
review `TimedOut`, that `Rejected.reason` is human feedback to act on,
that `TimedOut.fallback` records what was *applied* (not what to do
next), or that `Modified.reviewer_note` is a caveat alongside an
accepted diff.

**Evidence B — result is unframed JSON dump:**
`crates/spur-mcp/src/server.rs:344-362`
```rust
let result_json = serde_json::to_value(&result)?;
JsonRpcResponse::success(id, json!({
    "content": [{
        "type": "text",
        "text": serde_json::to_string_pretty(&result_json)
            .unwrap_or_else(|_| result_json.to_string())
    }]
}))
```
The brain reads pretty-printed JSON like:
```json
{ "status": { "Rejected": { "reason": "(no reason given)" } },
  "diff": "diff --git ...",
  "summary": "..." }
```

**Impact on brain↔executor loop:** The whole spec rationale for
splitting `Rejected` (human-issued) from `TimedOut` (system-applied)
— see spec lines 172-177 — assumes the brain can distinguish them
and treat them differently. With no tool-description schema and no
result framing, the LLM is guessing. Two failure modes are
near-certain:
- Brain confuses worker-`Timeout` with review-`TimedOut` — the names
  are deliberately close in the type system but indistinguishable in
  prose to a non-domain LLM.
- Brain treats `TimedOut { fallback: Reject }` as if it were
  `Rejected` — exactly what the split was designed to prevent.

The regression test at `crates/spur-acp/tests/delegation_status_display.rs`
verifies the JSON renders distinguishably *to a string-pattern matcher*.
That is necessary but not sufficient — it does not verify the LLM
actually does the right thing.

**Fix shape (not designed):** Two layered changes.
1. Rewrite `delegate_to_worker_def()` description to enumerate status
   vocabulary and explain semantics. Same for the JSON-schema's
   response shape (or move to a separate `output_schema` field if MCP
   permits).
2. Add a `DelegationResult` → prose formatter on the spur-mcp side
   (not raw `to_string_pretty`). Variants frame themselves:
   - `Rejected` → `"Human reviewer REJECTED with reason: \"{reason}\". Address this when retrying. Worker's diff (preserved):\n{diff}"`
   - `TimedOut { fallback: Reject {reason} }` → `"Review timed out after {N}s with no human response. Default fallback: Reject (\"{reason}\"). NOT human feedback — treat as 'unable to confirm.'"`
   - `Modified` → `"Human reviewer APPROVED with this caveat: \"{note}\". Incorporate the caveat alongside the accepted diff:\n{diff}"`

## P1 — Works but degrades

### F4. Review card omits `attempt_n`

**Evidence:** `crates/spur-tui/src/components/review_card.rs:20-23`
```rust
out.push(Line::from(Span::styled(
    format!("── Review requested: {} ──", kind_label(&req.kind)),
    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
)));
```
The whole `attempt_n` supersession infrastructure (spec lines
258-281; ReviewSink check, end-to-end propagation through
`UserInput::SubmitReview` and `InteractiveInput::SubmitReview`) exists
to disambiguate stale review cards after `Retry`. But **the operator
has no visual cue which attempt they are reviewing**. The first
visible signal that supersession kicked in is the `tracing::warn!` in
`ReviewSink::submit` — operator sees their decision silently dropped.

**Fix shape:** Add `attempt {n}/{max}` to the review card header.
Trivial — `req.attempt_n` is already on `ReviewRequest`.

### F8. Review card omits original task and agent name

**Evidence:** `crates/spur-tui/src/components/review_card.rs:9-47` —
the render only shows `summary`, optional `diff_summary`, optional
`pr_url`, optional `error`, and the hotkey legend. No
`agent`/`task`/`session_id`.

**Impact on parallel-N journey (J5):** Brain calls
`delegate_parallel({tasks: [3 tasks]})`. Three review cards stack.
Operator pressing 'r' (JumpToReview) cycles through them — all titled
identically `"── Review requested: completion ──"`. Operator has no
way to distinguish "the auth refactor" from "the migration script"
from "the doc update" without correlating tree-pane state. Any
free-text reason the operator types under F1's eventual fix would
target an ambiguous worker.

**Fix shape:** Show `agent` + first 80 chars of `task` in the review
header. Both are available from the `ExecutorNode` already.

### F11. `Action::SubmitReview` dispatch is fire-and-forget

**Evidence:** `crates/spur-tui/src/app.rs:637-643`
```rust
if let Some(ref tx) = self.user_input_tx {
    let _ = tx.try_send(UserInput::SubmitReview { ... });
}
// Optimistically reflect resolution locally...
self.lineage.apply(&spur_acp::SpurEvent::now(
    spur_acp::SpurEventBody::ExecutorReviewResolved { ... }
));
```
- `try_send` returns `Err(Full)` if the channel (size 32, cli
  `main.rs:370`) is at capacity. The `let _ =` swallows that.
- The optimistic apply runs unconditionally, clearing
  `pending_review` from the projection.

**Impact:** UI shows the card disappear (resolved). The dispatch
silently never landed. `ReviewSink` still has the registered sender.
Brain hangs until `review_timeout` fires (default 30 min). Operator
has no card to retry the action through.

This is not hot-path likely (32 buffer absorbs reasonable bursts), but
the failure mode is silent and unrecoverable from the operator's POV.

**Fix shape:** Use `send().await` (translator task is async); on
`Err`, surface an Action::ShowError or activity-log entry. Defer the
optimistic apply until the await completes. Or keep optimistic apply
but reconcile when the authoritative `ExecutorReviewResolved` /
`ExecutorReviewCancelled` arrives — currently it does reconcile
(F15 below) but only after timeout, which is too late.

### F15. Optimistic UI doesn't surface reconciliation when it diverges

**Evidence:** `crates/spur-tui/src/app.rs:644-650`. Optimistic apply
fires `ExecutorReviewResolved { decision: <whatever the user chose> }`.
If the orchestrator's authoritative outcome differs (e.g., timeout
already fired and emitted `ExecutorReviewCancelled` first; or
brain-cancel arrived; or F11's silent dispatch loss happened), the
projection's clear-pending logic just runs again — but the UI never
informs the operator that **their decision was overwritten or
dropped**. They saw "Approved" briefly; the lineage will end at
`TimedOut`; nobody told them.

**Fix shape:** Make the optimistic apply tag the resolution with a
`provisional: bool` flag, and on receiving the authoritative event
diff against it. If diverged → push a warning into activity log: "Your
Approve was not applied — review timed out before dispatch."

## P2 — Polish

- **F7.** `Esc` on dashboard with no focused node = `Action::Quit`
  (`dashboard.rs:464`). The `QuitConfirmDialog` mitigates accidental
  exit, but operators who mistype Esc out of a review card go to a
  modal quit prompt. Soft. Consider Esc → close-detail-pane only.
- **F-extra-1.** `ReviewKind::Failure | Conflict | Checkpoint` are
  rendered (`review_card.rs:50-55`) but never emitted (orchestrator
  hardcodes `ReviewKind::Completion` at `orchestrator.rs:1722`). Dead
  code. Either delete from the enum or expand the spec to actually
  use them.
- **F-extra-2.** Brain-cancellation (`ExecutorReviewCancelled` from
  brain side) clears the review card silently. Operator who was
  reviewing sees the card vanish with no toast/log entry explaining
  why. Add a short activity-log entry.

## Journey scoring summary

| Journey | (a) Brain signal | (b) Human info | (c) UI consistency | (d) Graceful degradation |
|---|---|---|---|---|
| J1 Approve happy path | ✅ Success unambiguous | ❌ no diff visible (F6) | 🟡 try_send fragile (F11) | 🟡 silent failures (F11/F15) |
| J2 Reject with reason | ❌ placeholder reason (F1) | ❌ no diff (F6) | 🟡 same as J1 | 🟡 same as J1 |
| J3 Modify with note | ❌ placeholder note (F1) | ❌ no diff (F6) | 🟡 | 🟡 |
| J4 Retry with constraints | ❌ no constraints (F1) → doom loop | ❌ no diff (F6) | 🟡 | 🟡 |
| J5 Parallel-N | ✅ correlation-id routing works | ❌ cards indistinguishable (F8) | 🟡 | 🟡 |
| J6 Brain-cancel mid-review | ✅ ExecutorReviewCancelled emitted | 🟡 silent disappear (F-extra-2) | 🟡 | ✅ |
| J7 Review timeout | ✅ TimedOut emitted | n/a | ✅ | ✅ |
| J8 TUI restart mid-review | n/a | n/a | n/a | ❌ (durability disclaimed) |
| J9 Stale-attempt double-submit | ✅ guarded by attempt_n | ❌ no attempt_n shown (F4) | ✅ supersession works | ✅ |
| J11 Channel-full silent drop | ❌ brain hangs (F11) | ❌ UI lies | ❌ | ❌ |
| **Brain interpretation of statuses** | ❌ unframed JSON (F12) | n/a | n/a | ❌ |

## Architectural observation

The implementation is type-correct end-to-end. Every variant flows
through ReviewSink, projection, lineage, and back. The seams that
break the loop are at the **edges where the typed Rust core meets
non-Rust modalities**:
- TUI input flow (no path to type free text with a hotkey decision)
- MCP tool framing (no schema in description, no prose in result)

This is not a "the design was wrong" finding. It's a "we built the
rails; we never built the cars." The next investment isn't in
orchestrator surgery — it's in two narrow, focused features:
1. TUI review-input UX redesign (likely a modal review dialog
   component; analogous to QuitConfirmDialog).
2. MCP brain-facing semantic layer (tool-description rewrite +
   variant-aware result formatter).

Each is its own brainstorm/spec/plan cycle.

## Open questions

- Should the diff preview live in the review card itself (cramped) or
  in a dedicated detail-pane tab the review card links to?
- Should the brain-facing prose framing live in spur-mcp (couples MCP
  to UI semantics) or in a new `spur-acp::brain_format` module that
  spur-mcp consumes (clean separation, but more crates touched)?
- Is `delegate_parallel` worth its own batch-review UX (one decision
  card listing N children, single Approve covers all), or do operators
  prefer per-task review even in batches? Speculative — wait for
  signal.
