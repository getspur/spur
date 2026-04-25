# Worker Peer Mailbox Design Review

Date: 2026-04-25
Spec: `docs/superpowers/specs/2026-04-25-worker-peer-mailbox-design.md`
Reviewer: Kimi Code CLI
Method: MCTS multi-round first-principles evaluation + industry research grounding

---

## Executive Summary

The spec is architecturally sound and correctly identifies the critical boundary: **durable truth lives in SPUR, not in worker session memory**. The two-stage approach (durable mailbox first, stateful runtime second) is the right risk ordering.

However, several areas need strengthening before implementation:

1. **Message delivery semantics are underspecified** — the spec implies at-least-once but does not explicitly choose a guarantee or defend the choice.
2. **The causal_parent_id / sequence mechanism is underdeveloped** — it introduces a partial ordering concept without clear conflict resolution rules.
3. **Stage 2 WorkerRuntime conflates concerns** — the proposed lifecycle phases overlap with existing executor `LifecycleState` more than the spec acknowledges.
4. **Cost attribution model lacks precision** — "Stage 1 mostly affects prompt reconstruction cost" ignores the token multiplier of injecting peer context into prompts.
5. **Review gate integration has a timing hazard** — the bounded post-prompt ack drain creates a race between terminal events and consumption acknowledgements.

Overall verdict: **Approved with revisions**. The spec should address items 1, 4, and 5 before implementation begins. Items 2 and 3 can be refined during Stage 1 implementation but must be resolved before Stage 2.

---

## MCTS Round 1: Selection — Key Decision Nodes

The spec contains five critical decision branches that determine the design's correctness under failure:

| Node | Decision | Spec Choice | Alternatives Rejected |
|---|---|---|---|
| A | Mediation authority | SPUR mediates all peer traffic | Direct worker-to-worker transport |
| B | Durability boundary | Ledger + beads audit refs | Runtime memory only, Event log only |
| C | Worker session model | One-shot attempts (Stage 1) | Stateful runtime (deferred to Stage 2) |
| D | Routing validation | Plan DAG + explicit brain edges | Same-epic siblings allowed |
| E | Delivery guarantee | Implicit at-least-once? | Not explicitly chosen |

Node E is the spec's weakest selection. Every other node has a clear documented choice; delivery semantics are scattered across sections without an explicit guarantee statement.

---

## MCTS Round 2: Simulation — Consequence Analysis

### Branch A: Mediated vs Direct Transport

**First principle:** SPUR owns worktree isolation, delegation lifecycle, review gates, lineage, cost, and audit state. If workers communicated directly, SPUR would lose observability and control over cross-worker dependencies.

**Simulation:**
- Direct transport: Workers would need to discover each other's ACP session IDs or worktree paths. This leaks execution internals into the collaboration layer. A worker retry would change the target session, breaking in-flight messages. Review gates would have no visibility into peer influence.
- Mediated transport: SPUR can validate, record, inject context, and emit events. The spec correctly notes that "workers should not communicate directly."

**Industry grounding:**
- CrewAI uses hub-and-spoke orchestration where "all inter-agent communication flows through the orchestrator" — this matches the spec's approach.
- AutoGen's peer-to-peer model enables dynamic negotiation but creates an attack surface where "attackers can manipulate transfer decisions, potentially bypassing the orchestrator's routing logic."
- Service mesh architectures (Istio, Linkerd) explicitly separate control plane (policy, discovery) from data plane (traffic forwarding). The spec's `PeerMailboxRouter` + `EventFunnel` maps cleanly to control-plane mediation.

**Score:** +1.0. Correct decision, well-justified.

### Branch B: Durability Boundary

**First principle:** If a live ACP worker session is lost, SPUR should reconstruct collaboration from durable state or explicitly mark context unavailable.

**Simulation:**
- The spec places durability at Layer 4 (`PeerMailboxLedger`) with beads storing compact audit references. This matches the existing SPUR pattern where `SpurEvent` is replayable but not the primary audit source for review-relevant transitions.
- The three-step rule (persist ledger → write beads → emit event) is correct but incomplete. It does not specify what happens if beads write succeeds but event emission fails, or if the process crashes between steps 2 and 3.

**Industry grounding:**
- The Outbox pattern (used by Stripe, Shopify, LinkedIn) solves exactly this dual-write problem: business state and message emission must be atomic. The spec's ledger is effectively an outbox table.
- Best practice: make ledger transitions idempotent and let a background relay drain un-emitted events. The spec partially addresses this ("All ledger transitions are idempotent by `message_id` plus transition kind") but does not specify the relay mechanism.
- Event sourcing systems (Akka Persistence, EventStoreDB) use the event stream as source of truth. The spec inverts this: ledger is source of truth, events are secondary. This is defensible for SPUR because the event bus is a broadcast channel, not a log-backed store.

**Score:** +0.7. Good direction, needs atomicity clarification and relay specification.

### Branch C: One-shot vs Stateful Runtime

**First principle:** The live runtime is only an execution cache. Durable truth remains beads audit records, the peer mailbox ledger, and the event stream.

**Simulation:**
- Stage 1 (one-shot) preserves all existing invariants. Peer context is injected into `PromptRequest` construction. This is safe because `run_one_worker_attempt` already handles setup, prompt, drain, and teardown as an atomic unit.
- Stage 2 (stateful `WorkerRuntime`) introduces new failure modes: stale sessions, TTL expiration, turn limit exhaustion. The spec correctly requires fallback to Stage 1 behavior.
- However, the proposed `WorkerRuntimePhase` enum (`Starting`, `RunningTurn`, `Draining`, `Idle`, `AwaitingReview`, etc.) overlaps heavily with existing `LifecycleState`. The spec claims Stage 2 "should introduce a dedicated `WorkerRuntimePhase` instead of overloading the existing executor `LifecycleState`" but does not justify why a separate phase space is better than extending the existing one.

**Industry grounding:**
- Temporal's worker model keeps workers stateless; workflow state lives in the Temporal server. The spec's Stage 1 aligns with this.
- Akka's actor runtime is stateful but its mailbox is an implementation detail, not durable truth. The spec's Stage 2 would be closer to Akka if the mailbox driver were ephemeral, but the spec requires Stage 2 to "call the same router and ledger APIs" — which is correct.
- Claude Code agent teams use full sessions per teammate, but "targeted teammate-to-teammate messages add tokens to both sending and receiving contexts" — cost scales linearly. SPUR's mediated model can bound this cost at the router layer.

**Score:** +0.8 for Stage 1; +0.5 for Stage 2 (needs phase-space justification).

### Branch D: Routing Validation

**First principle:** The plan DAG permits communication. Same lineage is not enough.

**Simulation:**
- The validation rules are comprehensive: DAG edges, supersession state, lifecycle phases, sequence monotonicity, body size limits.
- The spec correctly rejects "sibling tasks under the same epic" as insufficient.
- Missing: What happens when the brain adds an explicit peer edge after a worker has already started? The DAG is static at plan submit time, but the brain can modify plans via mutation. The router needs a dynamic `PlanScopeProvider` refresh mechanism.

**Industry grounding:**
- Service mesh control planes (Istiod, Linkerd control plane) dynamically push routing rules to proxies. The spec's `PlanScopeProvider` is analogous but does not specify push vs pull.
- LangGraph's state machine uses explicit edges: "nodes represent agent actions or LLM calls, and edges define how control flows." The spec's DAG validation follows the same principle.

**Score:** +0.8. Rules are correct, needs dynamic plan update handling.

### Branch E: Delivery Guarantee

**First principle:** In distributed systems, assume every message will arrive zero or more times, never exactly once.

**Simulation:**
- The spec has states for `Accepted`, `Rejected`, `Queued`, `Delivered`, `Consumed`, `Ignored`, `Expired`, `Dropped`, `Undeliverable` — implying a state machine that tracks delivery progress.
- But it never explicitly states "at-least-once delivery with idempotent transitions."
- The idempotency rule ("All ledger transitions are idempotent by `message_id` plus transition kind") is stated but not enforced in the design.
- Missing: deduplication window size. If a worker retries a `_spur/peer_message` after a crash, how long does the router retain the `message_id` to reject duplicates?

**Industry grounding:**
- Akka's core semantics are at-most-once delivery, with optional at-least-once via "Reliable Delivery" feature requiring ACK-RETRY.
- Stripe's payment infrastructure: "In distributed systems, assume every message will arrive zero or more times, never exactly once."
- The spec should explicitly choose at-least-once (preferred for collaboration correctness) and specify the dedup window.

**Score:** +0.3. Critical gap. Must be addressed before implementation.

---

## MCTS Round 3: Backpropagation — Invariant Check

Check the spec against SPUR's established invariants:

| Invariant | Status | Notes |
|---|---|---|
| INV-C3: UI-visible event precedes model-visible continuation | ⚠️ Partial | Peer events flow through `EventFunnel`, but prompt injection happens in `run_one_worker_attempt` before `PromptRequest` construction. Need to verify `WorkerPeerMessageQueued` precedes `PromptDispatched`. |
| S2 funnel: monotonic seq ordering | ✅ Preserved | Peer events explicitly flow through `EventFunnel` after durable writes. |
| S1.d broadcast: 4096 cap, lagged detection | ✅ Preserved | No change to broadcast sizing. |
| Beads label grammar: `[A-Za-z0-9_:-]+` | ⚠️ Needs check | Audit references use compact UUIDs (32 hex chars). The label `spur:peer-message-id:{compact_uuid}` would be 54 chars — over the 50-char `br create` cap. Use `br label add` path or shorter prefix. |
| Review gate: supersession guard | ⚠️ Risk | If a peer message targets a task that gets superseded mid-flight, the router must transition the message to `Undeliverable` or redirect. Spec does not address this. |
| Worker `_spur/*` allowlist | ✅ Preserved | Explicit allowlist of `_spur/peer_message`, `_spur/peer_message_consumed`, `_spur/peer_message_ignored`. No generic passthrough. |
| Replay-purity: lineage projection | ✅ Preserved | Peer events become edges, not primary task status. Projection remains deterministic. |

---

## MCTS Round 4: Consolidated Findings

### Finding 1: Delivery Semantics Must Be Explicit (HIGH)

**Problem:** The spec describes a state machine without declaring the delivery guarantee or deduplication window.

**Recommendation:**
- Add a "Delivery Guarantees" section declaring **at-least-once delivery** as the v1 target.
- Specify a `peer_message_dedup_ttl` (suggested default: 24 hours, matching typical worker attempt windows).
- Require the ledger to store `(message_id, source_delegation_id, accepted_at)` for the TTL duration so replays can reject duplicates.
- Document that exactly-once is intentionally not targeted because "in distributed systems, duplicates are fixable; data loss is not."

### Finding 2: Beads Label Length Hazard (MEDIUM)

**Problem:** The spec's audit reference labels may exceed beads' 50-character create-path cap.

**Recommendation:**
- Use `spur:peer:{compact_uuid}` (22 + 32 = 54 chars) only via `br label add`, not `br create --label`.
- Or shorten to `spur:pm:{compact_uuid}` (19 + 32 = 51 chars) — still over.
- Or use a base58-encoded truncated ID: `spur:pm:{22-char-base58}` = 28 chars total.
- Document this explicitly in the spec and add a label constructor to `crates/spur-mcp/src/plan/labels.rs` following existing patterns.

### Finding 3: Causal Ordering Is Underdeveloped (MEDIUM)

**Problem:** The `causal_parent_id` and `sequence` fields suggest a partial order, but the spec does not define:
- What happens when `causal_parent_id` references a message the target has not yet seen?
- What happens when `sequence` gaps are detected?
- Is sequence per-source-delegation or per-source-executor?

**Recommendation:**
- Clarify that `sequence` is per-`(source_delegation_id, target_delegation_id)` pair.
- Define gap behavior: router emits `WorkerPeerMessageDropped` with reason `sequence_gap` if `sequence > expected` and `allow_out_of_order = false`.
- Define `causal_parent_id` resolution: if the referenced message is not `Accepted` or `Delivered` in the target's mailbox, the router may either queue the dependent message or reject it with `causal_parent_unresolved`.
- Consider removing `causal_parent_id` from v1 entirely if the use case is not immediate. YAGNI: the existing `sequence` field provides sufficient ordering for question/answer/handoff patterns.

### Finding 4: Stage 2 Phase Space Overlap (MEDIUM)

**Problem:** The proposed `WorkerRuntimePhase` (`Starting`, `RunningTurn`, `Draining`, `Idle`, `AwaitingReview`, `ReviewedTerminal`, `Retiring`, `Failed`) maps closely to `LifecycleState` + attempt status.

**Recommendation:**
- Either justify the separate phase space with a table showing why `LifecycleState` cannot express runtime mailbox state (e.g., `Idle` means "connected but no active prompt" — this has no equivalent in `LifecycleState`).
- Or fold runtime state into existing structures: add `mailbox_phase: Option<MailboxPhase>` to the executor node rather than creating a parallel phase enum.
- The spec should also address how `WorkerRuntime` interacts with the existing `run_one_worker_attempt` retry loop — does the runtime survive retries, or is it per-attempt?

### Finding 5: Cost Attribution Precision (MEDIUM)

**Problem:** "Stage 1 mostly affects prompt reconstruction cost" understates the token impact.

**Recommendation:**
- Quantify the cost model: each accepted peer message injected into a prompt adds `len(message_body) + overhead` tokens. With a default `max_peer_message_size` of, say, 2KB, and `max_pending_mailbox_depth` of, say, 8, a single worker prompt could carry up to 16KB of peer context.
- Specify that cost tracking must attribute peer-injected tokens separately from task tokens so the brain can see peer communication cost in review.
- Add a `PeerCostUpdate` event or extend `CostUpdate` with a `peer_bytes` field.

### Finding 6: Review Gate Timing Hazard (HIGH)

**Problem:** The spec requires "a bounded post-prompt acknowledgement drain before review is requested." If delivered inbound messages remain unacknowledged after the drain, the router must record them as terminal before review proceeds.

**Simulation of the hazard:**
1. Worker B completes its task.
2. Orchestrator begins post-prompt drain (e.g., 5-second grace window).
3. Worker B emits `_spur/peer_message_consumed` for a message from Worker A.
4. The ext notification consumer is async; the event is in flight.
5. Drain timeout fires; orchestrator marks unacknowledged messages as `Ignored`.
6. The consumption event arrives late.
7. Router sees `message_id` already terminal. What happens?

**Recommendation:**
- Define late-arrival behavior explicitly: if a consumption/ignore event arrives after the message is already terminal, emit `WorkerPeerMessageLateArrival` (or use the existing `signal:late-arrival` label pattern) and do NOT transition the message.
- Ensure the review gate reads the final state AFTER the drain completes, not during.
- Consider making the drain duration configurable and separate from the existing notification grace window used for file-touch dedup.

### Finding 7: Supersession During Peer Flight (MEDIUM)

**Problem:** If Worker A sends a peer message to Worker B, and Worker B's task is superseded before the message is delivered, the spec does not define the outcome.

**Recommendation:**
- Add a rule: if the target task is superseded while messages are queued for it, those messages transition to `Undeliverable` with reason `target_superseded`.
- If the target task is retried (not superseded), queued messages should be carried forward to the new attempt. This requires the ledger to key mailbox entries by `issue_id` or `plan_task_id`, not just `delegation_id`, because retries get new delegation IDs.

### Finding 8: Missing Migration Step — Rollback / Disable (LOW)

**Problem:** The migration path does not mention how to disable the feature or recover from a buggy peer mailbox deployment.

**Recommendation:**
- Add step 0: feature flag default-off.
- Add rollback step: when disabled, the router rejects all new peer messages with `feature_disabled`, but existing queued messages still deliver.
- Document that Stage 1 fallback (one-shot prompt reconstruction) must remain available permanently, even after Stage 2 ships.

---

## Industry Research Summary

### Reference Designs Evaluated

| System | Pattern | Relevance to SPUR |
|---|---|---|
| **CrewAI** | Hub-and-spoke orchestration, no direct agent communication | Validates mediated model. Hierarchical delegation matches SPUR's brain-worker model. |
| **AutoGen** | Peer-to-peer conversation, GroupChat manager | Contrast: direct communication creates emergent behavior but loses auditability. SPUR's mediated approach is safer for production. |
| **LangGraph** | Explicit state machine, directed graph edges | Validates DAG-based routing. Nodes = tasks, edges = peer communication permissions. |
| **Temporal** | Stateless workers, server-side workflow state | Validates Stage 1 one-shot approach. Workers are ephemeral; state lives in the orchestrator. |
| **Akka** | Actor model with optional durable mailbox (Persistence) | At-most-once by default; durable delivery requires explicit ACK-RETRY. SPUR should adopt similar opt-in reliability. |
| **Service Mesh (Istio)** | Control plane + data plane sidecar | `PeerMailboxRouter` = control plane. `EventFunnel` = telemetry plane. Worker ACP sessions = data plane. |
| **AIDevOps** | SQLite-backed mailbox with supervisor pulse | Practical precedent for mediated messaging in agent systems. SQLite WAL mode handles concurrency. |

### Key Insight from Research

The most robust pattern across all evaluated systems is **separation of durability from delivery**:
- The Outbox pattern (Transactional Outbox + Message Relay) ensures at-least-once without requiring the event bus to be durable.
- The control plane / data plane split (service mesh) ensures policy enforcement without embedding policy in workers.
- The spec follows both patterns correctly at the architectural level but needs to tighten operational details.

---

## Recommended Spec Revisions

### Must Have (blocks implementation)

1. **Add explicit delivery guarantee section** — at-least-once, idempotent transitions, dedup TTL.
2. **Resolve review gate timing hazard** — define late-arrival behavior and drain/review sequencing.
3. **Fix beads label length** — choose a label format under 50 chars or document `br label add` path.

### Should Have (refine during Stage 1)

4. **Clarify causal ordering or defer it** — either fully specify `causal_parent_id` / `sequence` or remove from v1.
5. **Add cost attribution precision** — token overhead quantification, separate peer cost tracking.
6. **Define supersession behavior** — queued message fate when target task is superseded or retried.

### Nice to Have (before Stage 2)

7. **Justify or merge WorkerRuntimePhase** — separate enum vs extension of existing `LifecycleState`.
8. **Add rollback / disable path** — feature flag behavior, graceful degradation.

---

## Conclusion

The spec's architectural foundation is correct: mediated peer communication with durable state owned by SPUR. The two-stage rollout is appropriately conservative. The primary gaps are operational semantics (delivery guarantee, timing hazards, label constraints) rather than structural flaws. Addressing the "Must Have" items will yield an implementation-ready spec that preserves SPUR's existing invariants while adding worker collaboration safely.
