# Review: bd-cpf.7 correctness (gemini)

## Verdict
**LGTM.** The implementation is correct, precise, and cleanly fulfills the bd-cpf.7 specification without introducing scope creep or regressions. Behavior preservation during refactoring is perfect.

## Correctness Assessment

1. **Scope:** **Pass.** The diff strictly implements `DrainStarted` and `DrainTimedOut`. There are no changes to `DrainCappedOut` structures, nor are there unrelated events like `AckReceived` or `LateAckDropped`.
2. **Helper Deduplication:** **Pass.** The new `candidate_set_for_target` properly uses `retain` with a `HashSet` to return a strictly deduplicated `Vec`. The previous code iterated a raw extended list and maintained two inline HashSets for skipping duplicates. Migrating to a pre-deduplicated list means those inline sets were redundant and correctly removed. Behavior is exactly preserved.
3. **Mutual Exclusivity:** **Pass.** The emission logic uses `if cap_hit { ... } else if remaining_messages > 0 { ... }`. Since `cap_hit` and `!cap_hit` are mutually exclusive, a drain can never emit both exit events. Symmetry is maintained, and clean exits appropriately emit nothing.
4. **Elapsed Time Semantics:** **Pass.** `actual_elapsed_ms` is computed immediately after the loop exits and before the second asynchronous ledger snapshot. This preserves the semantic that the elapsed time measures only the orchestrator's wait loop, not the overhead of subsequent state queries.
5. **DrainStarted Async Window:** **Pass.** The orchestrator awaits the candidate snapshot, counts it, and synchronously passes that count into the `DrainStarted` event. The event strictly reflects the queried state. This is standard and fully acceptable; any message arriving right after the snapshot is handled correctly by the subsequent loop and exit snapshot.
6. **Clean Exit Test:** **Pass.** `drain_timed_out_not_emitted_on_clean_exit` correctly sets `quiet_window < max_total` (100ms < 60s) and advances time by `quiet_window`. Because the bundle has no pending messages for the target, it cleanly exits the quiet window with `remaining_messages == 0` and correctly asserts no `DrainTimedOut` event fires.
7. **Cap Hit Test:** **Pass.** `drain_cap_hit_emits_only_drain_capped_out` sets `max_total < quiet_window` (100ms < 10s) and advances by `max_total`. This guarantees `cap_hit` triggers first. The test verifies that only `DrainCappedOut` is emitted, explicitly proving mutual exclusivity.
8. **Consumer Exhaustiveness:** **Pass.** `crates/spur-core/src/lineage/projection.rs` is indeed the only workspace consumer matching exhaustively over these events (aside from tests). Grep confirms no other crate (like `spur-tui` or `spur-bot`) requires an arm extension.