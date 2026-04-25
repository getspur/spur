# Reviewing merge commit 7df4aea — operational/on-call angle

## 1. Page-worthy failure modes

**Weakest link: silent swallow of malformed terminal acks.** `spur_ext_interp.rs:115` parses `params["message_id"]` with `serde_json::from_value`. On malformed JSON it logs `tracing::warn!` and returns silently — no event, no ack. A worker that believes it acked a message will see it forced to `Ignored` after drain timeout, creating a phantom "message lost" alert pointing at the wrong subsystem.

**Reconciler racing orchestrator cleanup.** `guard.rs:124` does `ledger.get` after transition. If the orchestrator restarts between transition and get, the audit event is never emitted. Transition and event emission are not atomic.

**Post-prompt `ledger.get` returns None.** No `ledger.get` exists in the immediate post-prompt path (`orchestrator.rs:4906-5004`); the analogous pattern is `guard.rs:124`. The same atomicity gap applies if a persistent ledger cleans up entries.

**Severity: BLOCKER** for malformed-JSON silent path — operators have no funnel signal to page on.

## 2. Observability audit — `WorkerPeerMessage*` events

Coverage is complete (Accepted → Rejected → Queued → Delivered → terminal variants + AuditFailed + Reconciled). SLO derivability is good: delivery, consumption, and audit-failure rates are all computable from the funnel.

`tracing::warn!` levels:
- `spur_ext_interp.rs:118` (malformed `message_id`): **warn-spam risk at 100 msgs/sec**. Downgrade to `debug!` or rate-limit.
- `orchestrator.rs:4938` (delivered-inflight transition failure): legitimate — signals ledger inconsistency.
- `orchestrator.rs:5245` (drain forced-terminal failure): legitimate.

**`interpret_peer_message_terminal` failure path** (`spur_ext_interp.rs:138-145`): `record_terminal` failure logs warn but `ack_tx.send(())` still fires. The caller thinks the ack succeeded; the ledger disagrees. Split-brain.

**Post-prompt audit-failed emit** (`orchestrator.rs:4946`): correctly emits `WorkerPeerMessageAuditFailed`. Good.

**Drain forced-Ignored** (`orchestrator.rs:5239`): hard-coded `"drain_timeout"` surfaces in funnel. Good for dashboard grouping.

## 3. Drain timeout semantics

Default 2s (`limits.rs:17`). `drain_peer_acks_with_timeout` (`orchestrator.rs:5207`) loops with `tokio::time::timeout(quiet_window, ack_rx.recv())` — the timer **resets on every ack**, not a total cap. A chatty worker sending one ack per 1.9s keeps the drain alive indefinitely.

CPU-bound worker scenario: a healthy worker processing a large diff may not ack within 2s. Drain forces `Ignored`; the late ack then fails because the message is already terminal.

**Severity: SHOULD-FIX** — add an absolute max-drain cap (e.g., 30s) alongside the per-ack quiet window.

## 4. Reconciler unused param (`reconciler.rs:22`)

`drain_quiet_window` is ignored; boot reconcile forces all `DeliveredInflight` to `Queued` or `Delivered` immediately.

Production scenario: orchestrator restarts mid-flight. Worker finishes and sends `_spur/peer_message_consumed`. The reconciler already reverted the message to `Queued`. The ack path attempts `Queued → Consumed`, which is **invalid per the matrix** (`ledger.rs:94-98`). The ack is swallowed; the message is stranded in `Queued`.

**Real bug: yes. Promote to beads issue: yes.** Severity: SHOULD-FIX.

## 5. Cardinality explosion

`WorkerPeerMessageIgnored.reason` is a `String` from worker-supplied `params["reason"]` (`spur_ext_interp.rs:130`). Workers can inject timestamps, stack traces, or unique IDs. Dashboards grouping by `reason` will explode cardinality.

**Severity: SHOULD-FIX** — cap to 128 bytes and validate against an allow-list, or hash unknowns to `"other:<prefix>"`.

## 6. Test-environment fidelity

Concurrency tests use `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]` (`peer_mailbox_concurrency.rs`). Prod schedulers have more threads and different contention. Tests exercise `InMemoryLedger` (tokio::sync::Mutex), not a persistent store with network latency. No Loom or Miri coverage.

**OPS-NOTE:** logic races are covered; scheduler-dependent deadlocks and memory-ordering bugs are not.

## 7. Rollback safety

`ReplayBody` (`replay_compat.rs:6`) is `#[serde(untagged)]` with `Known/Unknown` fallthrough. New `WorkerPeerMessage*` variants are additive — old replays deserialize them as `Known`. No breaking shape changes.

Persistent state: Stage 1 uses `InMemoryLedger` only. **OPS-NOTE:** safe to roll back.

## 8. Deferred TODOs — scenario & likelihood

| TODO | Production scenario | Likelihood |
|---|---|---|
| `reconciler.rs:22` drain_quiet_window unused | Orchestrator restart swallows worker acks | **med** |
| `orchestrator.rs:4806` hard-coded 200k context window | Agent with smaller window gets overfull prompts | **high** (week 1) |
| `orchestrator.rs:4969` Task 14 durable audit path | Crash loses peer-mailbox events (mitigated by in-memory ledger today) | **low** (high once persistence lands) |
| `event_funnel.rs:22` FunnelCommand boxing | Minor perf hit on hot emit path | **low** |
| prompt_builder double-injection race (Stage 2) | Concurrent builds duplicate prompt blocks, wasting context window | **low** (graceful via `AlreadyInjected`) |

---

**On-call readiness verdict:** Stage-1 hardening is structurally sound for single-node in-memory use, but the 2s reset-timer and reconciler boot-race are real pager risks during deploys — land the beads issue for reconciler graceful window before production load.
