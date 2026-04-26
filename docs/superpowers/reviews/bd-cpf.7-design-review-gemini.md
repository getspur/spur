# bd-cpf.7 Design Review (Gemini)

## Recommendation

**Scope:**
- **SHOULD-DO:** `WorkerPeerMessageDrainStarted` and `WorkerPeerMessageDrainTimedOut`. These are cheap additive events that directly address the Stage-2 observability gaps around multi-drain amplification and replay floods.
- **DEFER:** `WorkerPeerMessageLateAckDropped`. Structural analysis shows that late acks are impossible under the current architecture. The `ack_tx` sender drops when the worker connection shuts down, which happens before/during the drain.
- **DEFER:** `WorkerPeerMessageAckReceived`. The volume is too high and there is no concrete downstream consumer for per-ack events right now.

**Patch-size estimate for preferred scope:** ~25 LoC (including `SpurEventBody` variant additions), plus ~50 LoC of tests.

## Q1: Scope

The proposed minimal scope of `DrainStarted` + `DrainTimedOut` passes the cost/value check. Both provide critical pager value for Stage-2 (persistent ledger drain pressure) while remaining completely wire-compatible and strictly additive. We endorse deferring both `LateAckDropped` and `AckReceived`.

## Q2: `DrainTimedOut` field shape

**Verdict:** Full payload.

Symmetry with `DrainCappedOut` provides the most value, preventing consumers from having to perform stateful joins with `DrainStarted` just to understand the context of the timeout.

Shape should be:
- `brain_session_id: String`
- `target_delegation_id: DelegationId`
- `acks_received: u32`
- `remaining_messages: u32`
- `quiet_window_ms: u64`
- `cap_ms: u64`
- `actual_elapsed_ms: u64`

## Q3: `DrainStarted` field shape

**Verdict:** Proposed shape is correct and complete.

Shape:
- `brain_session_id: String`
- `target_delegation_id: DelegationId`
- `candidates_at_start: u32`
- `cap_ms: u64`
- `quiet_window_ms: u64`

This provides the exact denominator (`candidates_at_start`) needed to evaluate the success of the drain when paired with the exit events.

## Q4: `LateAckDropped` lifetime question

**Verdict:** The sender is definitively dropped; there are no late acks.

**Analysis:**
Tracing `ack_rx` reveals that the `ack_tx` sender is cloned and moved into the `ext_rx` handling task (spawned via `tokio::spawn` inside `run_one_worker_attempt`). This task is bound to the lifecycle of the worker connection's external notification channel.
When the worker prompt finishes, `run_one_worker_attempt` explicitly calls `connection.shutdown().await`. Thus, by the time the orchestrator calls `drain_peer_acks_with_timeout`, the worker connection is already tearing down. The `ext_rx` task drains its remaining notifications, drops its `ack_tx` clone, and exits. The main loop's `ack_tx` clone is also explicitly dropped right before the drain call.
Because the channel closes, `drain_peer_acks_with_timeout` breaks cleanly via `Ok(None)`. Furthermore, `ack_rx` is re-created per worker attempt. There is no structural path for an ack to arrive late on an already-closed, un-reused receiver. `LateAckDropped` observability would be dead code.

## Q5: `DrainTimedOut` vs `DrainCappedOut` on the no-remaining path

**Verdict:** Emit `DrainTimedOut` ONLY when `remaining_messages > 0`.

**Analysis:**
Because the `ack_rx` channel closes when the worker connection terminates, the typical successful drain exits the loop via `Ok(None)`. This sets `cap_hit = false` and currently labels any remaining messages as `"drain_timeout"`.
If we emitted `DrainTimedOut` unconditionally on the `!cap_hit` path, we would emit it for every single clean drain (where `remaining_messages == 0`). This would be high volume and semantically confusing. Emitting only when `remaining > 0` cleanly flags the anomaly: the worker shut down or went quiet while leaving messages stranded in its mailbox.

## Q6: Volume control

**Verdict:** Accept the volume for `DrainStarted`.

Given that `DrainStarted` is scoped to one event per worker prompt, the volume is acceptable. In Stage-2, multi-drain amplification is a specific risk we need to monitor. Aggregating or sampling at the source risks hiding the exact behavior we are trying to observe. Volume control should be handled by downstream dashboard sampling, not by complexifying the emitter.

## Q7: Tests

**Verdict:** BLOCKER.

Any patch must include:
1. Wire-compat roundtrip serialization tests for `WorkerPeerMessageDrainStarted` and `WorkerPeerMessageDrainTimedOut` (testing the `Unknown` fallback behavior).
2. A functional test verifying `DrainStarted` is emitted on entry, and `DrainTimedOut` is emitted when `remaining_messages > 0` and the timeout is hit.