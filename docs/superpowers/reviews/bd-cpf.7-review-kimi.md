# bd-cpf.7 Operational Review — Kimi

**Commit under review:** `c7c2691` — `feat(spur-core,spur-acp): bd-cpf.7 add DrainStarted + DrainTimedOut events`
**Reviewer:** kimi
**Date:** 2026-04-26
**Scope:** Operational review of the synthesized Alt B implementation (DrainStarted + DrainTimedOut). Focus: pager risk, replay compat, CHANGELOG accuracy, emission policy re-evaluation, helper refactoring correctness, and test naming.

---

## Verdict

**LGTM-with-NITs**

---

## Pager-risk classification

**Low** — purely additive observability. No behavioral change to drain logic, message-loss accounting, or lineage mutation. New events are diagnostic-only and explicitly no-opped in `lineage/projection.rs`.

---

## Issues

### NIT

#### 1. CHANGELOG phrasing — awkward parenthetical
- **File:** `CHANGELOG.md`
- **Lines:** `## Unreleased` → `### Added` (lines 14–25)
- **Problem:** The phrase "(with `quiet_window_ms` replacing nothing — both fields are present so dashboards can reuse panel queries)" is jarring. An operator skimming the CHANGELOG at 3 AM will pause on "replacing nothing" and re-read. The parenthetical fights against itself: it says something is replaced, then immediately denies it.
- **Minimal-edit suggestion:** Replace the parenthetical with a direct statement of symmetry:

  ```markdown
  - **Peer mailbox drain lifecycle events.** `WorkerPeerMessageDrainStarted`
    and `WorkerPeerMessageDrainTimedOut` add symmetric observability to the
    post-prompt ack drain. `DrainStarted` carries the candidate-set size
    and the cap/quiet-window limits in effect; `DrainTimedOut` carries the
    same payload shape as `WorkerPeerMessageDrainCappedOut` plus
    `quiet_window_ms`, so dashboards can reuse panel queries across both
    exit events. `DrainTimedOut` is emitted only when the quiet-window exit
    leaves remaining non-terminal messages; clean-exit drains
    (`remaining_messages == 0`) emit no exit event. Diagnostic-only —
    message loss continues to be tracked per-message via
    `WorkerPeerMessageIgnored`. (bd-cpf.7)
  ```

  *Acceptable alternative:* leave as-is if the CHANGELOG editor prefers the original voice; this is a readability nit, not a correctness issue.

---

## Direct answers to operational questions

### 1. Pager risk overall: any behavioral change to existing message-loss accounting?

**No behavioral change. Confirmed low pager risk.**

The per-message `WorkerPeerMessageIgnored` events are still emitted from the same `record_terminal` loop (`orchestrator.rs:5350–5374`) with the same `"drain_timeout"` / `"drain_capped"` reasons. `DrainStarted` and `DrainTimedOut` are emitted via `funnel.emit` only; they do not interact with the ledger, the router, or the `record_terminal` path. Message-loss dashboards that count `WorkerPeerMessageIgnored { reason = "drain_timeout" }` will see identical counts before and after this commit.

### 2. Forward-replay test: is codex's decision to skip deserialize-with-missing-fields for new variants acceptable?

**Yes — acceptable and correct.**

Codex's discipline is: `#[serde(default)]` is for fields added to *existing* variants, not for brand-new variants. This is sound because:

- A pre-bd-cpf.7 JSONL replay contains no `DrainStarted` or `DrainTimedOut` events at all.
- During replay, those unknown variant names hit `SpurEventBody`'s `#[non_exhaustive]` + `Known/Unknown` fallthrough (or `replay_compat` mapping) and deserialize as `Unknown` variants.
- The "missing-field-on-new-variant" scenario is therefore unreachable in practice: a `DrainTimedOut` event in the JSONL implies the replay was produced by a post-bd-cpf.7 binary, which always writes all seven fields.

The two round-trip tests (`worker_peer_message_drain_started_round_trips`, `worker_peer_message_drain_timed_out_round_trips`) cover the serde contract adequately. No additional replay test is needed.

### 3. CHANGELOG entry: is it accurate? Suggest clearer phrasing?

**Accurate in content; one phrasing nit (see Issue 1 above).**

The entry correctly describes:
- `DrainStarted` fields (`candidates_at_start`, `cap_ms`, `quiet_window_ms`)
- `DrainTimedOut` conditional emission (`remaining_messages > 0`)
- Clean-exit behavior (`remaining_messages == 0` emits no exit event)
- Diagnostic-only nature and per-message `WorkerPeerMessageIgnored` tracking

The only operational friction is the "replacing nothing" parenthetical. See Issue 1 for a suggested rewrite.

### 4. Conditional emission re-evaluated: does the synthesis override cost any dashboard or alert?

**No — conditional emission is operationally acceptable. Endorse the synthesis decision.**

My original design review argued for unconditional `DrainTimedOut` to support the alerting algebra:

```
rate(DrainStarted) - rate(DrainTimedOut) - rate(DrainCappedOut) = clean exits
```

With the conditional implementation, this algebra no longer holds directly because clean exits are implicit (no exit event). However, the dashboard use cases I care about are still satisfiable:

- **Drain-timeout rate:** `rate(DrainTimedOut)` — unchanged.
- **Drain-cap rate:** `rate(DrainCappedOut)` — unchanged.
- **Drain-start denominator:** `rate(DrainStarted)` — provides the total drain count for ratio computation.
- **Clean-exit rate:** `rate(DrainStarted) - rate(DrainTimedOut) - rate(DrainCappedOut)` — still computable; the result is the implicit clean-exit count. The only loss is that clean exits are not *explicitly* typed.

The volume argument from the synthesis holds: unconditional emission would double per-prompt event volume on the common clean path, and the event name "TimedOut" is semantically misleading for a clean exit. If a future dashboard genuinely needs an explicit clean-exit signal, the right follow-up is `WorkerPeerMessageDrainFinished { exit_reason: Clean | TimedOut | Capped }` (as codex noted as NICE-TO-HAVE). Defer that until a concrete consumer exists.

### 5. `cap_ms` inclusion on `DrainTimedOut`: verified?

**Yes — confirmed present.**

`orchestrator.rs:5339` sets `cap_ms: max_total.as_millis() as u64` on the `WorkerPeerMessageDrainTimedOut` emit. This enables the misconfig-detection dashboard I argued for: if `quiet_window_ms >= cap_ms`, the operator will see `actual_elapsed_ms ≈ cap_ms` on `DrainTimedOut` events and can flag the misconfiguration.

### 6. Refactoring correctness: `record_terminal` loop now iterates deduplicated `candidate_set_for_target` output. Any operational regression?

**No regression — behavior is preserved and simplified.**

The old code (pre-refactor) presumably built a deduplicated candidate set inline or used an inner `HashSet` during the `record_terminal` loop. The new `candidate_set_for_target` helper (`orchestrator.rs:5240–5256`) deduplicates via `HashSet::insert` in a single `retain` pass:

```rust
let mut seen = std::collections::HashSet::new();
candidates.retain(|entry| seen.insert(entry.envelope.message_id));
```

The `record_terminal` loop (`orchestrator.rs:5350–5374`) then iterates over `candidates`, which now contains exactly one entry per unique `message_id`. The `WorkerPeerMessageIgnored` events are emitted once per unique message, matching the pre-refactor behavior. Removing the inner dedup is a pure simplification with no operational change.

### 7. Test naming: operational clarity improvements?

**Names are clear; one minor suggestion.**

| Current name | Assessment |
|---|---|
| `drain_started_emits_with_candidates_at_start` | Good — describes the operational signal and the key field. |
| `drain_timed_out_emits_when_quiet_window_exits_with_remaining` | Good — explicitly ties emission to the quiet-window exit + remaining condition. |
| `drain_timed_out_not_emitted_on_clean_exit` | Good — negates cleanly. |
| `drain_cap_hit_emits_only_drain_capped_out` | Good — mutual-exclusivity assertion is explicit. |

**Minor suggestion:** `drain_cap_hit_emits_only_drain_capped_out` could be shortened to `drain_cap_hit_emits_only_capped_out` for consistency (the other tests drop the `drain_` prefix from the event name in the test name: they say `drain_started...` and `drain_timed_out...`, not `drain_drain_started...`). However, this is cosmetic — the current name is unambiguous.

All four tests correctly exercise the four operational cases: (a) drain starts with candidates, (b) quiet-window timeout with remaining work, (c) quiet-window timeout with no remaining work, (d) cap-hit exit. Coverage is complete.

---

## Summary

The implementation matches the synthesized Alt B design exactly: two additive diagnostic events, conditional `DrainTimedOut` emission, `cap_ms` included on both exit variants, deduplicated helper extraction, and four functional tests covering the operational matrix. The only item worth addressing is the CHANGELOG parenthetical (NIT). Once that is cleaned up or consciously retained, this is ready to land.
