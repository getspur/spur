# PR1 Data Lane — Code Review

**Date:** 2026-05-08
**Reviewer:** Claude Opus 4.7 (1M context)
**Branch base:** `main` @ `9dcb34d2` (T1 channel-types already on main)
**Worker commits under review:**

| SHA | Task |
|-----|------|
| `27016aee` | T2: pr1-drain-loop (`crates/spur-interactive/src/data_loop.rs`, `host.rs` data lane wiring, orchestrator `event_funnel_handle`) |
| `0cda546b` | T3: pr1-cli-routing (`crates/spur-cli/src/main.rs::route_tui_input_to_host`) |
| `e950bfe7` | T4: pr1-graph-cache-epoch (`crates/spur-tui/src/views/issue_browser.rs`) |
| `108435e2` | T5: pr1-orch-cleanup (legacy-handler `tracing::debug!` probes + drop dead `probe_label` arms) |

## Verdict

**Pass — ship with two amendments.** The data lane achieves its design goals (read/brain decoupling, same-id coalescing, bounded graph concurrency, minimal TUI test churn, epoch-defended cache). Amendments below are tightenings, not blockers.

---

## Critical issues

None.

---

## Significant issues (should fix before merge)

### S1. `graph_request_epochs` leaks on error and dispatch-failure paths
**File:** `crates/spur-tui/src/views/issue_browser.rs:498`–`502`, `1216`, `1246`–`1263`

`get_issue_graph_action` inserts `(id → epoch)` on every dispatch, but the entry is only removed in the `IssueSubgraphLoaded` branch (line 1216). The `IssueCommandError { operation: "GetIssueGraph" | "get_graph", … }` branch (line 1253) never removes the entry, so a long-lived TUI session that hits N graph errors accumulates N permanent map entries. Likewise, if the orchestrator never produces any response (e.g. host shutdown before the data loop drains), the entry persists.

This is bounded by issue-id cardinality — not a runaway leak — but it is unbounded in principle, and trivially fixable: remove the entry at the top of the `GetIssueGraph` error arm too. While there, also drop the same-id stale-epoch check from the error arm so a stale error cannot clobber `graph_error` for an unrelated active request.

```rust
// In IssueCommandError handler, GetIssueGraph branch:
if let Some(id) = id {
    if let Some(req_epoch) = self.graph_request_epochs.remove(id) {
        if req_epoch < self.graph_data_epoch {
            return; // stale error — discard
        }
    }
    // … existing handling …
}
```

### S2. `'r'` keybinding refresh does not bump the data epoch
**File:** `crates/spur-tui/src/views/issue_browser.rs:741`–`744`

```rust
KeyCode::Char('r') if key.modifiers.is_empty() => {
    self.invalidate_graph_cache();
    Some(Action::RefreshIssues)
}
```

Every other mutation site (`seed_issues` at line 280, `IssuesLoaded` at line 1064, `IssueUpdated` at line 1142) bumps the epoch, but the user-initiated refresh dispatcher does not. Window: between pressing `r` and `IssuesLoaded` arriving, an in-flight graph response from before the refresh is treated as fresh and re-cached. After `IssuesLoaded` arrives, the cache is invalidated again — so the *cache* is fine — but `pending_action` may have already been replaced and `graph_loading` cleared based on a graph the user has just signalled they no longer trust. Cheap fix: add `self.bump_graph_data_epoch();` next to `invalidate_graph_cache()`.

---

## Minor concerns (consider)

### M1. `data_loop_graph_concurrency_bounded` only asserts the upper bound
**File:** `crates/spur-interactive/src/data_loop.rs:649`–`653`

```rust
assert!(pm.max_graph_in_flight() <= 4, …);
```

This passes if the semaphore is set to 1, 2, 3, or 4 — only the regression direction "bumped semaphore above 4" is caught. Add a lower-bound assertion (e.g. `>= 2` to allow scheduler slack, or set `graph_delay` long enough that all four parallel slots demonstrably fill: `assert_eq!(pm.max_graph_in_flight(), 4)`). The current 100 ms delay × 8 queries on a single-thread runtime will reach 4 deterministically because all four spawned futures park on `tokio::time::sleep` before the next yields.

### M2. `tui_input_to_interactive` retains dead arms for `GetIssueDetail` / `GetIssueGraph`
**File:** `crates/spur-cli/src/main.rs:1145`–`1150`

`route_tui_input_to_host` short-circuits these variants to `data_tx` before reaching `tui_input_to_interactive`, so the arms at 1145–1150 are unreachable for the only caller. Mirror the `SubmitReview` pattern (`unreachable!`) so a future refactor cannot silently route reads back onto `user_tx`. PR3 is planned to introduce a typed `BrainScheduler::push_user_message` API anyway, so this aligns with that direction.

### M3. Legacy orchestrator probe wording is misleading for non-TUI callers
**File:** `crates/spur-core/src/orchestrator.rs:4040`–`4046`, `4100`–`4106`

The `tracing::debug!(site = "orch_legacy_handler", "GetIssueDetail handled via legacy user_rx path — TUI should be on data_rx")` message implies any firing is a TUI bug. But this handler legitimately serves CLI direct-invocation callers and tests that build an `Orchestrator` without going through `InteractiveFrontendHost`. The probe is debug-level and harmless, but the phrasing will produce noisy false-positive alerts if anyone hooks it into a SLO. Consider rewording to "non-data-lane GetIssueDetail dispatch" or gating with a marker that's only set when the host has a data lane attached.

### M4. `wrapping_add` epoch is correct but unnecessary
**File:** `crates/spur-tui/src/views/issue_browser.rs:495`

`u64::wrapping_add(1)` overflows after ~2^64 mutations — physically unreachable. `checked_add` panicking on overflow would be a clearer signal that overflow is a bug. Not worth a code change on its own.

---

## Test coverage assessment

**Strong**

- `data_loop_dispatches_independently` (data_loop.rs:501): correctly demonstrates that 5 concurrent `GetIssueDetail` queries all start before any completes. ✓
- `data_loop_coalesces_same_id` (data_loop.rs:559): correctly demonstrates a duplicate `GetIssueDetail` for the same id is dropped while the first is pending. Also correctly verifies the `pm.detail_calls` count is 1, not 2. ✓
- `tui_get_issue_detail_uses_data_lane` / `tui_get_issue_graph_uses_data_lane` (main.rs:1313, 1336): verify routing AND that no spillover to user_rx/review_rx occurs. ✓
- `tui_refresh_issues_stays_on_command_lane` (main.rs:1359): explicitly locks in the routing decision for a non-data variant. ✓
- `graph_cache_epoch_drops_stale_response` / `graph_cache_epoch_keeps_fresh_response` (issue_browser.rs:1754, 1780): cover both directions of the epoch comparison. ✓
- `data_channel_send_recv_roundtrip` (host.rs:219): smoke test of the new `send_data_query` + `take_data_rx` plumbing. ✓

**Gaps**

- See M1 — concurrency bound test is one-sided.
- No test exercises the `acquire_owned()` failure path (closed semaphore). Hard to construct in practice; OK to skip.
- No test verifies `graph_request_epochs` is cleaned up on the error path (would catch S1 if added).

---

## Per-design-goal verdict

| Goal | Verdict | Notes |
|---|---|---|
| 1. Pure read, no brain dependency | ✅ | `DataQueryProvider` trait is read-only; `run_data_query_loop_with_provider` never touches the brain scheduler. |
| 2. Concurrent dispatch with same-id coalescing | ✅ | `pending: HashSet<QueryKey>` + `completed_rx` is correct. `pending` removes via the unbounded `completed_tx` regardless of success/error/semaphore-closed (data_loop.rs:206 unconditional `completed_tx.send(key)`). |
| 3. Bounded concurrency on `GetIssueGraph` (sem 4) | ✅ (with M1) | Implementation correct; test under-asserts. |
| 4. Split point at CLI bridge | ✅ | `route_tui_input_to_host` is the only TUI→host hop; existing `tui_input_to_interactive` callers in tests still work. |
| 5. Graph-cache epoch defense | ✅ (with S1, S2) | Bumped in `seed_issues`, `IssuesLoaded`, `IssueUpdated`. Missing on `'r'` keybinding (S2). |
| 6. TUI hangs eliminated | ✅ qualitatively | Read queries no longer queue behind brain-stream user_rx traffic; `tokio::spawn` per query (~hundreds of ns) is dwarfed by the actual PM service call (DB read in milliseconds). No measurable regression risk on idle-time issue opens. |

---

## Performance regression risk

Per-query `tokio::spawn` adds ~100–500 ns of overhead vs an inline await. The PM service call (`spur_pm::PmService::get_issue` → SQLite/JSON read) is at least 100 µs cold and typically a few ms. Spawn overhead is < 0.5 % of end-to-end latency. The win — eliminating head-of-line blocking against brain-stream traffic — is worth multiple orders of magnitude more on the streaming-active path. **No regression concern.** The added probe `tracing::info!` calls in `host.rs::send_data_query` and `data_loop.rs::spawn_data_query_handler` are also cheap (`elapsed()` + format) and can be downgraded to `debug!` if/when the lane proves itself in production.

---

## Out-of-scope reminders (per task brief)

The following were explicitly deferred to later PRs and are correctly absent from this PR:
- `pending_post_turn` chronological queue for non-Message variants → PR2.
- Typed `BrainScheduler::push_user_message` + `unreachable!()` → PR3.
- SQLite reader pool / persistent connection.
- `RefreshIssues` placement (intentionally on user_rx).
- Variant audit table.

The **design rationale** for keeping `RefreshIssues` / `RefreshPlans` / plan commands on `user_rx` is **not currently documented in code** at the routing decision site (`crates/spur-cli/src/main.rs::route_tui_input_to_host`). The doc-comment on `DataQuery` (host.rs:5–6) explains the data-lane purpose but does not call out the brain-coupled vs read-only criterion. A 2-line comment above the `match input` in `route_tui_input_to_host` ("only variants with no brain ordering dependency go on data_tx; everything else preserves user_rx FIFO with brain-stream traffic") would make the next reader's life easier and prevent accidental routing-rule drift in PR2/PR3.

---

## Final recommendation

**Ship with amendments S1 and S2 fixed.** M1–M4 can land as a follow-up. No correctness blockers. The architecture is clean, the trait abstraction (`DataQueryProvider`) is the right shape, and the test fixture (`FakeDataPm`) is reusable for future read-side queries.
