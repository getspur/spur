# bd-arch.21 Correctness Review (Gemini)

**Verdict:** LGTM-with-NITs

## Classifications

* **NIT (`crates/spur-core/src/orchestrator.rs:927`)**: `peer_mailbox_reconciler_abort_handle()` uses `self.background_tasks.last()`. This is fragile. If another background task is pushed in `Orchestrator::new` later, this helper will silently return the wrong abort handle. Consider storing the `AbortHandle` directly on `PeerMailboxBundle` or `Orchestrator`.

## Specific Correctness Questions

**1. Did the diff match the synthesis spec? Any deviation?**
Yes, the diff perfectly matches the "Alt H" synthesis spec. The production wire-up is in `Orchestrator::new`, gated by `peer_mailbox_enabled`, with `Limits::default()`. The reconciler is tracked via `background_tasks`. The Q3 `brain_session_id` refactor uses the exact codex hybrid shape (pass-per-emit on router, `RwLock` resolver on reconciler). Exhaustive matching on `TerminalOutcome` is implemented. All 5 required integration tests (3 SHOULD-DO + 2 from kimi) are present. There are no deviations.

**2. Fragility of `peer_mailbox_reconciler_abort_handle`:**
Yes, relying on `self.background_tasks.last()` is fragile.
*Robust alternative:* Add a `peer_mailbox_reconciler: Option<tokio::task::AbortHandle>` field directly to `Orchestrator` or store it within `PeerMailboxBundle`. This ensures the helper explicitly grabs the correct handle independent of the insertion order in `background_tasks`.

**3. `"<no-active-session>"` fallback and guard dropping:**
The fallback is correct as a defensive default. In practice, a worker guard is only created when `interpret_peer_message` successfully accepts a peer message, which requires an active worker to emit the message. A worker is only running when an active session exists. Furthermore, the `InMemoryLedger` starts empty on boot, so there are no stranded messages from previous runs to trigger the reconciler loop before the first session starts. Therefore, a guard cannot be dropped before the slot is written, and this fallback will never fire in normal execution.

**4. Slot update vs `run_startup_reconcile` call site:**
The slot update is placed immediately *before* the call to `run_startup_reconcile` inside the session boundaries (e.g., orchestrator.rs:1190). This sequence is correct: any stranded messages recovered by `run_startup_reconcile` will be emitted under the *new* session ID, appearing in the active session's event stream. Since no workers have been spawned for the new session yet (this happens later via `handle_delegations`), no *new* guards can be dropped concurrently to race with this update.

**5. `record_terminal` in `drain_peer_acks_with_timeout`:**
In `orchestrator.rs:3967`, the `drain_peer_acks_with_timeout` function is called inside `run_worker` and explicitly passed the `brain_session_id` from the active worker's context (`WorkerAttemptCtx`). This correctly passes the active session ID to `record_terminal` (at line 5415), ensuring it resolves to the current active session ID, not a baked-in stale value.

**6. Leftover `self.brain_session_id` references:**
The refactor was clean. All 13 internal `funnel.emit` sites in `router.rs` (7 in `accept_or_reject` rejection paths via the `reject()` helper, 1 `WorkerPeerMessageAccepted`, and 5 in `record_terminal` outcomes) correctly use the newly passed `brain_session_id` parameter. `grep "self.brain_session_id" crates/spur-core/src/peer_mailbox/router.rs` confirms zero leftovers.

**7. Test coverage missing scenarios:**
The 5 integration tests cover the primary lifecycles well, including concurrency races. Two edge cases remain:
- **Reconciler panics:** The "Alt H" spec explicitly deferred panic-restart supervision for the reconciler. If the reconciler panics, the `JoinHandle` fails, and stranded messages will accumulate in the MPSC channel without emitting events. This is an accepted Stage-1 limitation, but an unverified scenario in testing.
- **Concurrent guard drops vs Slot update:** Handled correctly by `RwLock`, preventing torn reads, though the exact session ID logged depends on thread scheduling. A test verifying behavior under heavy drop load during a slot write is technically missing but behavior is guaranteed by standard synchronization primitives.

**8. CHANGELOG accuracy:**
The CHANGELOG states the reconciler is aborted on shutdown via `Orchestrator::drop`. Verification of `orchestrator.rs:720` shows `impl Drop for Orchestrator` iterates `self.background_tasks.drain(..)` and calls `handle.abort()`. Since the reconciler handle is pushed to this vector at orchestrator boot, the CHANGELOG statement is factually accurate.
