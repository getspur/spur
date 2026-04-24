# Worker Peer Mailbox Design

Date: 2026-04-25

## Summary

SPUR should support peer communication between workers in two stages.

Stage 1 ships a durable peer mailbox without relying on long-lived worker ACP
sessions. Workers can request communication, but SPUR remains the authority:
it validates the request, records the message, emits observable events, and
injects relevant mailbox context into later worker prompts.

Stage 2 may add stateful `WorkerRuntime` handles that keep worker ACP sessions
alive across turns. The live runtime is only an execution cache. Durable truth
remains beads audit records, the peer mailbox ledger, and the event stream.

This preserves SPUR's existing control plane: ACP is how SPUR controls agent
sessions, MCP remains brain-facing in v1, and workers do not receive general
MCP tools.

## First Principles

Workers should not communicate directly. SPUR owns worktree isolation,
delegation lifecycle, review gates, lineage, cost, and audit state. Therefore
peer communication must be mediated by SPUR even when it feels conversational
to workers.

The system must also survive process death. If a live ACP worker session is
lost, SPUR should be able to reconstruct the collaboration record from durable
state or explicitly mark the peer context unavailable. A worker's conversation
memory cannot become the source of truth.

## Approved Direction

Build Option 1 first: durable peer mailbox without stateful worker sessions.
Then progress toward Option 2: stateful worker runtimes backed by the same
durable mailbox protocol.

Rejected for v1:

- Direct worker-to-worker transport.
- General MCP tools exposed to workers.
- Peer messages stored only in runtime memory or only in `SpurEvent`.

## Stage 1: Durable Peer Mailbox

Stage 1 keeps the current one-shot worker execution model. A worker may emit a
structured ACP extension notification such as `_spur/peer_message`. The
orchestrator validates the request, writes durable state, and later injects
accepted mailbox context into prompts for the target worker.

The stage 1 flow is:

```text
Worker A emits _spur/peer_message
  -> orchestrator validates logical target and lifecycle state
  -> orchestrator writes a peer ledger entry
  -> orchestrator writes compact beads audit references
  -> orchestrator emits peer lifecycle events
  -> target worker receives relevant mailbox context on a later prompt
  -> target worker explicitly consumes or ignores the message
  -> review includes a peer influence summary
```

Stage 1 proves the collaboration semantics before SPUR takes on the extra risk
of keeping worker ACP sessions alive.

## Stage 2: Stateful WorkerRuntime

Stage 2 may promote delivery into live ACP sessions. A `WorkerRuntime` owns one
worker connection, one ACP protocol session, one worktree, and one serialized
mailbox driver.

The runtime is not durable truth. It is a cache of execution capability.

Required state includes:

- `executor_id`
- SPUR worker session id
- ACP protocol session id
- delegation id
- issue id
- plan task id
- worktree path
- lifecycle phase
- mailbox queue

Required lifecycle phases include at least:

- `Starting`
- `RunningTurn`
- `Draining`
- `Idle`
- `AwaitingReview`
- `ReviewedTerminal`
- `Retiring`
- `Failed`

Only one prompt may be active for a worker runtime at a time. A peer turn cannot
enter the ACP session until the active prompt has completed and notification
grace draining has finished.

If a runtime is unavailable, stale, terminal, over TTL, or over turn limits,
SPUR falls back to the stage 1 behavior: durable mailbox context is
reconstructed into a future one-shot prompt, or the message is marked
undeliverable with an auditable reason.

## Peer Message Envelope

Peer messages target logical work, not raw sessions or paths.

Minimum envelope:

```json
{
  "schema": "spur-peer-message/v1",
  "message_id": "<uuid-v4>",
  "source_delegation_id": "<delegation-id>",
  "target_delegation_id": "<delegation-id>",
  "source_issue_id": "<beads-id>",
  "target_issue_id": "<beads-id>",
  "source_plan_task_id": "<task-id>",
  "target_plan_task_id": "<task-id>",
  "kind": "question",
  "body": "Short worker-authored message",
  "causal_parent_id": null,
  "sequence": 1
}
```

Allowed `kind` values for v1:

- `question`
- `answer`
- `handoff`
- `warning`
- `constraint`

The orchestrator wraps delivered content as orchestrator-authored context. It
must not pass raw worker text as an instruction with authority over the target
worker.

## Validation

The orchestrator accepts a peer message only if all checks pass:

- Source and target delegations exist.
- Source and target issues exist.
- Source and target are in the same approved plan scope or explicitly allowed
  by the brain.
- The plan DAG permits the communication. Same lineage is not enough.
- Neither task is superseded.
- Source and target lifecycle phases allow peer communication.
- Message id has not already been accepted.
- Per-source sequence is monotonic or idempotently replayed.
- Body size is below the configured limit.
- Source worker is not terminal, cancelled, or retired.
- Target can receive now or can receive later through durable mailbox replay.

If validation fails, SPUR records a rejected peer event and, when appropriate,
a beads audit reference.

## Durable State

The peer mailbox ledger stores payload detail. Beads remains the collaboration
truth by storing compact audit references for state transitions.

Ledger states:

- `Accepted`
- `Rejected`
- `Queued`
- `Delivered`
- `Consumed`
- `Ignored`
- `Expired`
- `Dropped`
- `Undeliverable`

Beads audit references should include:

- peer send accepted
- peer send rejected
- peer delivered
- peer consumed
- peer ignored
- peer expired
- peer dropped
- peer undeliverable

The audit record may reference compact message and turn ids rather than storing
full payload text. The ledger stores full payloads and delivery metadata.

## Events

Add explicit peer lifecycle events so TUI, lineage, replay, and review can tell
the same story:

- `WorkerPeerMessageAccepted`
- `WorkerPeerMessageRejected`
- `WorkerPeerMessageQueued`
- `WorkerPeerMessageDelivered`
- `WorkerPeerMessageConsumed`
- `WorkerPeerMessageIgnored`
- `WorkerPeerMessageExpired`
- `WorkerPeerMessageDropped`
- `WorkerPeerMessageUndeliverable`

Event ordering:

- `WorkerPeerMessageAccepted` or `WorkerPeerMessageRejected` is emitted after
  durable validation state is written.
- `WorkerPeerMessageQueued` is emitted before prompt reconstruction includes
  the message.
- `WorkerPeerMessageDelivered` is emitted after the target prompt has been
  built with the message context.
- `WorkerPeerMessageConsumed` or `WorkerPeerMessageIgnored` is emitted before
  the target worker can reach review.
- `WorkerPeerMessageExpired`, `WorkerPeerMessageDropped`, and
  `WorkerPeerMessageUndeliverable` are terminal for that message.

Every new event variant needs round-trip serialization tests.

## Review Behavior

Review must surface peer influence. A worker result should include:

- inbound peer messages consumed
- inbound peer messages ignored
- outbound peer messages emitted
- undelivered peer messages that may affect completeness
- whether any peer input came from unreviewed work

Approval should require all inbound peer messages to be consumed or explicitly
ignored. Messages from unreviewed source work are advisory unless the brain
promotes them.

## Cost Behavior

Peer mailbox turns must be attributable. Costs should be tagged by:

- target delegation
- source delegation
- peer message id
- turn type
- agent

Stage 1 mostly affects prompt reconstruction cost. Stage 2 adds live
peer-turn cost and idle/runtime accounting.

## UI and Lineage

The TUI should show peer communication as collapsible lineage edges rather
than as primary task status. Task status remains driven by beads lifecycle and
delegation lifecycle.

Minimum UI states:

- sent
- accepted
- delivered
- consumed
- ignored
- rejected
- expired
- dropped
- undeliverable

Rejected, expired, and dropped messages must not look like silent stalls.

## Safety Limits

V1 should ship behind a feature flag with conservative limits:

- max peer message size
- max pending mailbox depth per target
- max messages per source delegation
- max fanout of one message
- idle TTL for stage 2 runtimes
- max turns per stage 2 runtime
- explicit downgrade behavior when the feature is disabled

No peer message is silently discarded. Drops, expirations, and truncations are
durable outcomes.

## Migration Path

1. Add the durable peer envelope, validation model, ledger, audit references,
   and events.
2. Inject accepted mailbox context into one-shot worker prompts.
3. Add review summaries and TUI lineage edges.
4. Add fallback prompt reconstruction from the ledger.
5. Only then add stateful `WorkerRuntime` delivery.
6. Keep one-shot fallback permanently available.

## V1 Defaults

- Ledger ownership starts in `spur-core` because peer routing is an
  orchestrator concern. If the implementation grows beyond routing and replay,
  it can be extracted later behind a small trait.
- Beads audit references are required for review-relevant transitions:
  accepted, rejected, delivered, consumed, ignored, expired, dropped, and
  undeliverable.
- Default routing is allowed only for direct plan DAG edges and brain-approved
  explicit peer edges. Sibling tasks under the same epic are not enough by
  themselves.
- Stage 1 requires explicit target acknowledgement through
  `_spur/peer_message_consumed` or `_spur/peer_message_ignored`. Prompt
  completion alone does not imply consumption.

## Acceptance Criteria

- Worker peer communication works without long-lived worker sessions.
- Durable state can reconstruct every accepted, delivered, consumed, rejected,
  expired, dropped, or undeliverable peer message.
- Beads contains compact audit references for review-relevant peer state.
- No worker can target raw ACP session ids, worktree paths, or peer process
  internals.
- Peer messages are visible in lineage and review context.
- One-shot worker execution remains available when peer communication is
  disabled or when a stateful runtime cannot be trusted.
- Stage 2 cannot make live ACP session memory the source of truth.
