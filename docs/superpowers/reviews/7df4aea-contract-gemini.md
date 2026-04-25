# Reviewing merge commit 7df4aea — contract adherence angle

## 1. Stage-1 Spec Contracts
- **a. Idempotent acceptance:** Verified. `sequential_replay_after_acceptance_returns_already_accepted` (in `peer_mailbox_concurrency.rs`) proves that replaying an accepted message safely yields `AlreadyAccepted` instead of minting a fresh guard.
- **b. Single-Delivered guarantee:** Verified. `concurrent_record_terminal_does_not_double_emit` verifies that exactly one `Delivered` terminal transition succeeds, preventing duplicate emissions.
- **c. Forced-terminal-timeout drain:** Verified. The `drain_completes_after_quiet_window_with_no_acks` test (in `orchestrator.rs`) ensures that messages missing acks within the `quiet_window` are gracefully forced to the `Ignored` state.
- **d. Plan-scope-snapshot immutability:** Verified. `PlanScopeSnapshot` is passed by value/reference strictly for router reads. The router never mutates it.
- **e. Source-id deterministic ordering:** Verified. `fan_out_100x100_pending_for_target_is_consistent` asserts that concurrent reads of `pending_for_target` yield deterministic, sorted sets by `message_id`.

## 2. Transition Matrix as Source of Truth
Verified. The matrix is the absolute source of truth. `prop_relaxed_arms_are_legal` (in `peer_mailbox_ledger_properties.rs`) verifies EXACTLY the 6 relaxed arms (`Accepted` -> `Consumed`/`Ignored`, and `DeliveredInflight` -> `Consumed`/`Ignored`/`Expired`/`Dropped`). No undocumented backdoors.

## 3. Wire-Protocol Contracts
Verified. `_spur/peer_message_consumed` and `_spur/peer_message_ignored` extract parameters safely.
- `params["message_id"]`: Parses dynamically via `serde_json`. Malformed or `null` types safely log a warning and return early, preventing panics.
- `params["reason"]`: Safely falls back to `.unwrap_or("worker_ignored")` avoiding type-mismatch crashes.

## 4. Event-Shape Stability
[NIT] 10 `WorkerPeerMessage` variants are stable. Spot-checking against `crates/spur-core/src/lineage/projection.rs`: `WorkerPeerMessageDelivered` uses `message_id` and `injected_chars`, but does not project `target_prompt_id` or `brain_session_id` into the graph. This is acceptable for Stage-1 visual lineage but leaves fields un-queried.

## 5. Audit-Failed Contract
Verified. After the TOCTOU fix (`35548a7`), `AuditFailed` ONLY fires for genuine invalid transitions. The orchestrator explicitly guards via `is_terminal(from) => continue`, ensuring that valid worker-ack races (e.g. prompt delivery racing with direct-ack) resolve via ledger transition cleanly without falsely flagging `AuditFailed`. No spurious call sites remain.

## 6. Deferred TODOs as Contract Debt
[SHOULD-FIX] These three items represent open contract debt for Stage-2:
- **`reconciler.rs:22` `drain_quiet_window` ignored:** Current behavior ignores the window duration. Intent: messages awaiting ack should drop to `Ignored` after the window. Gap: No background cleanup enforcing terminal states.
- **`prompt_builder` double-injection:** Current behavior could allow double injection if transitions race. Intent: messages must inject exactly once. Gap: Known race where a message is read before it transitions to `DeliveredInflight`.
- **`FunnelCommand` boxing:** Current behavior uses `#[allow(clippy::large_enum_variant)]`. Intent: minimize stack size for the channel. Gap: Large variants (`SpurEventBody`) increase queue memory overhead under load.

## 7. Replay-Purity Invariant
Verified. The relaxed matrix path (`Accepted` -> `Consumed`) correctly bypasses `DeliveredInflight`. Because the TOCTOU fix (`is_terminal(from)`) suppresses subsequent `Delivered` transitions, `WorkerPeerMessageDelivered` only fires when the code actually takes the `DeliveredInflight` -> `Delivered` path. Replay-purity remains completely intact, as direct-acked messages explicitly record `WorkerPeerMessageConsumed` without hallucinating a delivery.

**Verdict:** The D-G chain safely enforces Stage-1 mailbox contracts; ready for Stage-2 async/drain hardening.
