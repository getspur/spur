# Phase 4 Verification Report

**Plan:** `docs/superpowers/plans/2026-04-26-brain-continuation-phase-4-cleanup.md`
**HEAD:** `aa1bf3b`
**Date:** 2026-04-26

## Verdict: PASS

All three Phase 4 cleanup tasks (T1 INV-D9 proptest, T2 CompletionAuditFields refactor, T3 DeleteNamespaceReport) landed cleanly with no regressions. Phase 3 baseline test count of 608 → 618 (T1 + T3 each added one regression test; T1 also exercises 256 proptest cases).

## Step Evidence

### Step 1 — Phase 3 + 4 lib tests

```
spur-acp        --lib  →   134 passed (was 133)
spur-core       --lib  →   250 passed (was 242)
spur-mcp        --lib  →   202 passed (was 202)
spur-blob-store --lib  →    24 passed (was 23, +1 from T3)
spur-worktree   --lib  →     8 passed (was 8)
─────────────────────────────────────
TOTAL                       618 passed
```

### Step 2 — Clippy gate

```
RUSTC_WRAPPER= cargo clippy -p spur-acp -p spur-blob-store -p spur-worktree \
  -p spur-mcp -p spur-core -p spur-cli --no-deps -- -D warnings
→ Finished (clean)
```

The `#[allow(clippy::too_many_arguments)]` silencer that was added during Phase 3 T12 verification (commit `340f2ea`) on `persist_completion_result` is GONE — T2 brought the arg count down from 8 to 6 by extracting `CompletionAuditFields`.

### Step 3 — INV-D9 proptest

```
RUSTC_WRAPPER= cargo test -p spur-mcp --test inv_d9_proptest
→ test inv_d9_arb_delegation_status_clips_under_budget ... ok
→ test result: ok. 1 passed; 0 failed
```

256 cases per run. The post-T1-review hardened version actually exercises the materializer's clip helpers — generators include multi-byte UTF-8 codepoints and produce inputs that exceed the 512-byte status / 256-byte branch / etc caps. Runtime: ~3s (vs ~2.3s for the original vacuous version).

### Step 4 — Workspace check (Phase 3 + 4 crates)

```
RUSTC_WRAPPER= cargo check -p spur-acp -p spur-blob-store -p spur-worktree \
  -p spur-mcp -p spur-core -p spur-cli --all-targets
→ Finished (clean)
```

Pre-existing failures in `spur-context::real_fixtures` and `spur-tui::detail_pane_scroll` remain out of scope (acknowledged in Phase 3 verification report).

## Spec Coverage / Carryover Status

| Phase 3 carryover | Phase 4 task | Status |
| --- | --- | --- |
| INV-D9 proptest for materializer envelope clipping | T1 | ✅ closed |
| `CompletionAuditFields` struct refactor | T2 | ✅ closed |
| `total_bytes` in `outcome_namespace_deleted` | T3 | ✅ closed |
| Session-terminate hook (§8.1) | — | DEFERRED to Phase 5 |
| Legacy ref purge (spec line 431) | — | DEFERRED to Phase 5 |

## Commit Log (Phase 4)

```
T0  Phase 4 cleanup plan                                    1363b23
T1  INV-D9 proptest (initial)                               1fef8ab
    + T1 review fix: actually exercise clipping             c76ea1c
T2  CompletionAuditFields struct                            97fbaa2
    + T2 review fix: by-value emit + default + field docs   096023a
T3  DeleteNamespaceReport with total_bytes                  3c2e697
    + T3 review fix: best-effort ref sizing                 aa1bf3b
T4  Verification (this report)                              <pending>
```

7 commits, 4 tasks, all dual-reviewed (kimi spec + gemini quality). Both review streams found real issues in Phase 4:

- **T1** vacuous proptest (gemini caught: regex hardcap on arb_text + ASCII-only generators). Fixed via UTF-8-aware generators that scale to max_len.
- **T2** unnecessary 3× clone in emit_completion_audit (gemini caught). Fixed via by-value parameter.
- **T3** early-abort on corrupt ref breaking best-effort deletion (gemini caught). Fixed via `if let Ok(size)` pattern matching the legacy-ref handling.

## Notes

- Phase 4 deliberately did NOT add the `outcome_namespace_deleted` event for the orchestrator's startup TTL sweep — the existing emission lives in the CLI path. A follow-up could surface namespace-level events from the `MeasuredOutcomeStore` wrapper for sweep-driven deletes.
- The `MockFailingOutcomeStore` mock now returns `DeleteNamespaceReport` errors via the regular failure path; no test fixture changes required beyond signature update.
- `spur-mcp::outcome_materializer` had an inline OutcomeStore test fixture that needed updating for the trait signature change (codex flagged this as a deviation).

## Next Phase Candidates

1. **Legacy ref purge** (spec line 431): post-Plan-5 cleanup of `refs/spur/artifacts/<session-id>` writes/reads. The new GitBlobOutcomeStore already cleans up legacy refs on `delete_namespace`; what remains is removing the legacy-ref READ path from any straggling consumers.
2. **Session-terminate hook (§8.1)**: explicit per-session-end namespace delete. Currently only TTL sweep covers cleanup. Trigger needs lifecycle plumbing in `spur-core` (brain-session lifecycle events).
3. **Batch git size queries** (gemini T3 SHOULD-FIX): replace per-ref `rev-parse` + `cat-file -s` with a single `git for-each-ref --format='%(refname) %(objectsize)'`. Performance optimization for large namespaces.
4. **`bytesize` formatting in CLI output** (gemini T3 NIT): human-friendly byte counts (1.2 MiB) instead of raw integer.
