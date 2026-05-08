# PR1 Data Lane — Second-Opinion Review (Kimi)

**Verdict:** ship-with-amendments

## Top 3 Risks

### 1. Stale detail overwrite without epoch protection (HIGH)
- **Files:** `data_loop.rs:115`, `issue_browser.rs:1196-1208`
- **Scenario:** `GetIssueDetail{X}` is in-flight in the data lane. An `IssueUpdated{X}` event arrives via the brain-ordered `user_rx` stream and mutates the TUI's `tracked_issues` and (if loaded) `issue_focus`. When the data-lane response finally arrives, `IssueDetailFetched` blindly transitions `IssueFocus::Loading → Loaded` with the now-stale PM data. Graph queries received epoch protection; detail queries did not.
- **Mitigation:** Add `detail_data_epoch` + `detail_request_epochs` analogous to the graph cache epoch, or stop coalescing detail queries (they're cheaper than graph calls and the coalescing benefit is marginal).

### 2. `graph_request_epochs` leak on graph errors (MEDIUM)
- **File:** `issue_browser.rs:1253-1263`
- **Scenario:** On `IssueCommandError` for `GetIssueGraph`, the TUI clears `graph_loading` but never removes the ID from `graph_request_epochs`. Each failed or stale graph query leaks an entry; over a long session the map grows without bound.
- **Mitigation:** `self.graph_request_epochs.remove(&id)` in the `GetIssueGraph` error arm.

### 3. Diagnostic waterfall loses PM-call granularity (MEDIUM)
- **Files:** `data_loop.rs:199`, `orchestrator.rs:4065-4084`
- **Scenario:** The legacy `user_rx` path emits `orch_pm_get_issue_ok` with a `pm_get_issue_ms` breakdown. The data-lane path only emits `data_loop_query_done` with total elapsed. When traffic migrates to the data lane, `RUST_LOG=issue_probe=info` still gives a clean end-to-end trace (`data_send` → `data_loop_query_done` → `tui_event_received`), but PM-side latency regressions in `PmService::get_issue` or `issue_subgraph_json` become invisible.
- **Mitigation:** Add `data_loop_pm_start` / `data_loop_pm_ok` / `data_loop_pm_err` probes inside `handle_get_issue_detail` and `handle_get_issue_graph`, mirroring the legacy orchestrator probes.

## What claude-code likely missed

- **The `graph_request_epochs` leak on error paths** (not just epoch-mismatch drops).
- **PR1 improves the mid-stream scheduler-drop hazard.** By removing `GetIssueDetail`/`GetIssueGraph` from the brain-ordered `user_rx` channel entirely, they can no longer arrive mid-stream, be pushed to the scheduler's post-turn queue, and then silently dropped at `orchestrator.rs:4172`. PR1 makes this hazard *less* likely, not worse.
- **"Born stale" graph epoch is a non-issue.** `get_issue_graph_action` captures the epoch synchronously (`issue_browser.rs:498-501`) and the action is dispatched in the same UI poll cycle; there is no async window between epoch capture and `send_data_query`.
- **TUI preserves `IssueFocus::Loading{X}` across `IssuesLoaded`** (`issue_browser.rs:1083-1086`), so `IssueDetailFetched{X}` arriving after a refresh that no longer contains X still transitions to `Loaded{X}`. This was possible pre-PR1 with deterministic ordering; PR1 only makes the timing non-deterministic, not the outcome worse.
- **Fail-open behavior is correct.** `IssueCommandError` for `GetIssueDetail` clears `IssueFocus::Loading` to `None` (`issue_browser.rs:1272`), so the TUI does not stay stuck in `Loading` forever.

## Final Recommendation

Ship after adding detail-query epoch protection and cleaning up `graph_request_epochs` on error. Backfill data-lane PM probes in a fast follow so the diagnostic patch stays useful as traffic shifts to the new lane.
