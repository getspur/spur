# bd-arch.23 Correctness Review (Gemini)

**Verdict:** LGTM-with-NITs

The implementation in commit `b77ba09f` successfully addresses Architecture Risk #23 according to the Alt G synthesis spec (cancellable permit acquire + heartbeat watchdog, default-off). It is technically sound, thread-safe, and exhaustive in its matching logic. The new `DelegationAbortReason` signaling pattern is highly extensible for Stage-2 resource limits, without adding overhead or complexity to the current system.

## Findings

- **NIT (crates/spur-acp/src/domain/delegation.rs):** There are no production callers of the old `CancellationControl::register()` method (only test files like `crates/spur-core/tests/cancellation.rs` and `crates/spur-mcp/src/server.rs`). Consider deprecating or removing `register()` entirely in favor of `register_with_abort_handle()` to prevent future engineers from accidentally using it and dropping the abort handle.
- **NIT (crates/spur-core/tests/delegation_watchdog.rs):** Consider adding a direct unit test for `DelegationAbortHandle` asserting that concurrent `request_abort()` calls respect the first-writer-wins invariant.

## Correctness Verification

**1. Did the diff match the synthesis spec? Any deviation?**
Yes, it matches perfectly. The diff implements the typed `DelegationAbortReason` enum with `BrainRequested` and `WorkerHeartbeatTimeout`, uses `tokio::select!` for cancellable permit acquire with `biased;` ordering, exposes the default-off config toggles (`worker_heartbeat_watchdog_enabled`, `worker_heartbeat_timeout_secs`, `worker_heartbeat_initial_grace_secs`), and spawns a per-delegation heartbeat watchdog broadcast subscription.

**2. The `handle_delegations` signature changed to accept `worktree_config` and `event_tx`. Verify each call site passes the right values.**
Verified. All three call sites (`orchestrator.rs:1233`, `2635`, `2907`) pass `self.config.worktree.clone()` and `self.event_tx.clone()`.

**3. Verify the lifetime of abort handles: are they still reachable via CancellationControl for `cancel_delegation` MCP calls?**
Verified. `CancellationControl` stores the `DelegationAbortHandle` directly in an `Arc<Mutex<HashMap<String, DelegationAbortHandle>>>`. The orchestrator inserts the handle on dispatch. When `handle_delegations` finishes spawning the task, the handle remains in the `HashMap` indefinitely until the task completes normally, aborts, or is cancelled by MCP (which calls `remove`). Thus, `cancel_delegation` correctly retrieves the active handle.

**4. Verify the match at orchestrator.rs:3650+ is exhaustive and that the wildcard match forms a clean partition.**
Verified. The match is fully exhaustive. It explicitly covers `Some(...)` across both enum variants (with guards for partitioning) and `None`. The first arm consumes `WorkerHeartbeatTimeout` *only* if `executor_id != "<not-dispatched>"`. The second arm safely sweeps up the `"<not-dispatched>"` case, `BrainRequested`, and `None`, assigning `None` to `executor_id_opt`. This is idiomatic Rust, preventing runtime drift and extracting the `executor_id` accurately.

**5. `drop(heartbeat_watchdog_stop)` after the select! — verify this triggers the watchdog's `&mut stop_rx` arm to exit cleanly.**
Verified. Dropping the `oneshot::Sender` immediately causes the `oneshot::Receiver` (`stop_rx`) to resolve with `Err(RecvError::Closed)`. Because the `select!` arm pattern is `_ = &mut stop_rx => return;`, it ignores the returned value/error and cleanly breaks the loop, destroying the task.

**6. `DelegationAbortHandle` uses `Arc<tokio::sync::Mutex<Option<DelegationAbortReason>>>`. Verify there's no deadlock potential.**
Verified. The mutex is solely used to set the reason `if guard.is_none()` and to clone the reason. Waiters block on the *cancellation token's internal synchronization* via `cancel_token.cancelled()`, not on the mutex. Lock hold times are strictly instantaneous memory operations without embedded `.await` boundaries. There is no deadlock risk.

**7. `cancel_with_reason` returns `NotFound` if the token entry was already removed. Is this a correctness issue?**
This is expected behavior, not an issue. If the delegation already finished, its cleanup logic removes it from the hash map. An MCP client attempting to cancel it should correctly receive a `NotFound` semantic because there is no longer an active task to cancel.

**8. Are there any production callers that use the old `register()` and lose the abort_handle?**
Verified. A global `grep -rn '\.register('` across the `crates/` directory reveals that all remaining callers of `cc.register()` are inside `tests/` files. There are no production callers of the old API losing the abort handle.

**9. Test coverage looks complete. Are there missing scenarios?**
The test suite is highly comprehensive, effectively simulating the edge cases. It misses only two minor scenarios around `DelegationAbortHandle`'s atomicity:
- (a) A concurrent watchdog race where `request_abort` is fired by two threads near-simultaneously.
- (b) A direct unit test of `request_abort` ensuring its `if guard.is_none()` lock ensures first-writer-wins behavior.
The code's lock usage guarantees correctness here, but explicit unit testing of the atomic write is good hygiene.