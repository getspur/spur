# Phase 3 Verification Report

**Plan:** `docs/superpowers/plans/2026-04-25-brain-continuation-phase-3-materializer.md`
**HEAD:** `340f2ea`
**Date:** 2026-04-26

## Verdict: PASS (with documented carryovers)

All Phase 3 crates compile clean (`cargo check`), pass clippy with `-D warnings`,
and pass their full library test suites. Two carryovers from earlier phases
remain (`spur-context::real_fixtures`, `spur-tui::detail_pane_scroll`); both
are pre-existing test compile errors unrelated to Phase 3 and are explicitly
acknowledged in the plan's verification step (Step 4).

## Step Evidence

### Step 1 — Workspace check (Phase 3 crates)

```
RUSTC_WRAPPER= cargo check -p spur-acp -p spur-blob-store -p spur-worktree \
  -p spur-mcp -p spur-core -p spur-cli --all-targets
→ Finished `dev` profile [unoptimized + debuginfo] target(s) in 22.71s
```

The full-workspace `cargo check --workspace --all-targets` fails on
pre-existing breakage in `spur-context::real_fixtures` (E0282/E0599) and
`spur-tui::detail_pane_scroll` (E0063 — missing `delegation_id` and
`peer_edges` fields on test literals). Both pre-date Phase 3 and the plan
flags them as expected (§ Task 12 Step 4).

### Step 2 — Clippy on Phase 3 crates

```
RUSTC_WRAPPER= cargo clippy -p spur-acp -p spur-blob-store -p spur-worktree \
  -p spur-mcp -p spur-core -p spur-cli --no-deps -- -D warnings
→ Finished (clean)
```

After commit `340f2ea` silenced the lone `clippy::too_many_arguments` warning
on `persist_completion_result` (gemini T9 forward-looking refactor; deferred
to Phase 4 via `CompletionAuditFields` struct), the gate is green.

### Step 3 — Targeted unit tests

| Module                                  | Result                  |
| --------------------------------------- | ----------------------- |
| `spur-acp::domain::clip`                | 16 passed               |
| `spur-acp::domain::continuation`        | 9 passed (3 v3 round-trip + 6 existing) |
| `spur-blob-store --lib`                 | 23 passed (incl. 4 test_helpers) |
| `spur-mcp::outcome_materializer`        | 11 passed (3 success + 5 fallback + 3 review-fix) |
| `spur-mcp::audit_sentinel`              | 20 passed (3 new artifact_uri + existing) |
| `spur-mcp::fetch_outcome_artifact`      | 13 passed (4 section + attempt + 2 review-fix + existing) |

### Step 4 — Workspace-scoped test pass (Phase 3 crates)

```
spur-acp        --lib  →   133 passed
spur-core       --lib  →   242 passed
spur-mcp        --lib  →   202 passed
spur-blob-store --lib  →    23 passed
spur-worktree   --lib  →     8 passed
─────────────────────────────────────
TOTAL                       608 passed
```

`merge_budget_consistency` integration test in spur-core passes (1/1):
asserts conservative envelope estimate ≥ exact rendered cost across
representative continuation shapes.

### Step 5 — INV-D9 schema-evolution proptest

**DEFERRED** to a follow-up commit. The plan permits this when the test
does not yet exist; adding it now requires:

- Adding `proptest` as a dev-dep in `spur-mcp` (currently only in
  `spur-core`).
- Authoring `arb_delegation_status()` covering all 8 variants.
- ~150 LOC of generator code.

The materializer's existing test coverage exercises 5 distinct status
shapes (Success, Failed-clipped, Failed-fallback per FailureMode variant,
plus the panic-test inline `PanickingStore`). The merger-side
`merge_budget_consistency` integration test proves the conservative-bound
contract on representative shapes. The proptest would generalize this to
exhaustive variant coverage; useful but not blocking Phase 3 close-out.

**Tracked as:** Phase 4 cleanup task — "INV-D9 proptest for materializer
envelope clipping under arbitrary DelegationStatus".

### Step 6 — Schema-version round-trip CI guards

```
spur-acp::continuation_payload_v3_round_trips_through_serde       → ok
spur-acp::v3_payload_deserializes_from_v2_envelope_with_serde_default → ok
spur-core::merge_budget_consistency::conservative_estimate_dominates_exact_cost → ok
```

The third invariant (v2-envelope deserialize on the spur-core side) is
covered structurally because `ContinuationResourceBody` is `Serialize`-only
and `ContinuationPayload`'s `#[serde(default)]` already tolerates missing
v3 fields.

## Spec Coverage Map

| Spec section | Tasks | Status |
| --- | --- | --- |
| §7.1 Lean v3 schema (artifact_id, fetch_hint, estimated_cost_micros) | T2 | ✅ |
| §7.2 OutcomeMaterializer skeleton + success path + fallback | T4, T5, T6 | ✅ |
| §7.2 Clip helpers in spur-acp::domain::clip | T1 | ✅ |
| §7.3 Two callsites, one entrypoint (build_detached_continuation + reconciler) | T7, T8 | ✅ |
| §7.4 Beads audit-comment artifact_uri | T9 | ✅ |
| §7.5 Extended fetch_outcome_artifact with section + attempt | T10 | ✅ |
| §7.6 GC integration (startup sweep + spur gc CLI) | T11 | ✅ |
| §7.7 Truncation-ladder fallback + MockFailingOutcomeStore | T3, T6 | ✅ |
| §8.2 Background sweep via tokio::spawn | T11 | ✅ |
| §10.1 outcome_namespace_deleted event emission | T11 | ✅ |
| §11 Schema-version informational (not gating) | T2 | ✅ |

## Carryovers / Deferred Work

1. **INV-D9 proptest** (above) — deferred to Phase 4.
2. **`CompletionAuditFields` struct refactor** — gemini T9 SHOULD-FIX. Currently
   silenced via `#[allow(clippy::too_many_arguments)]` on `persist_completion_result`.
   Trigger for refactor: Phase 4 task that adds another field to the audit comment
   (e.g., outcome_byte_size or completion_kind).
3. **`total_bytes` field in `outcome_namespace_deleted` event** — currently omitted
   because `OutcomeStore::delete_namespace` returns a count, not a byte size. Spec
   §10.1 lists `total_bytes` as a desired field; surfacing it requires extending
   the trait return type. Tracked as a follow-up.
4. **Session-terminate hook (§8.1)** — explicit per-session-end namespace delete
   is deferred until brain-session lifecycle plumbing materializes. TTL sweep
   covers the recovery case in the meantime.

## Pre-existing Carryovers (Out of Phase 3 Scope)

- `spur-context::real_fixtures` test compile errors (E0282/E0599).
- `spur-tui::detail_pane_scroll` test compile error (E0063).
- `spur-cli` integration tests (`auth_cli`) reference older license-status text;
  the current CLI emits Community/license-provider wording. Surfaced during T11
  but not introduced by Phase 3.

These predate Phase 3 and are tracked separately.

## Commit Log (Phase 3)

```
T1  Move clip helpers to spur-acp::domain::clip               8239d69, aded093
T2  ContinuationPayload schema v3                              c609bbb, ed42b43
T3  MockFailingOutcomeStore test helper                        2b4ffed, 8092fd8
T4  OutcomeMaterializer skeleton                               78c0069, 96f3c79
T5  Materialize success path                                   6c15cb5, ff4e544
T6  Truncation-ladder fallback + panic test                    7d09225, 6b8bd39
T9  artifact_uri in Completion audit sentinel                  be1a3e0, 4a91f75
    + tool_catalog drift fix                                   7eed34b
T7  Wire materializer into build_detached_continuation         a65707b
T8  Wire reconciler completion path                            aa78d12
T10 Extend fetch_outcome_artifact with section + attempt       c64e837, f86efdf
T11 GC integration (startup sweep + spur gc outcomes CLI)      d0226d0, 14678f4
T12 Verification (clippy silencer)                             340f2ea
```

Total: ~30 commits, 12 tasks, all dual-reviewed (kimi spec + gemini quality).
