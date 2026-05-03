# Multi-Brain Operations Guide

This guide covers operating SPUR when more than one **brain agent** is connected
to the same `.beads/` project, and how to recover plan ownership when a brain
gets stuck or dies.

The design described here is **Liveness-by-Policy (LBP)**: SPUR makes no attempt
to auto-resolve multi-brain conflicts. Sequential operation is enforced by
operator discipline; manual transfer is available via an explicit force-reclaim.

---

## 1. The single-writer rule

A SPUR plan has at most **one active owner brain at a time**. The owner is
recorded on the plan's beads epic via the label
`spur:plan-owner:<brain-session-id>`. All ownership-mutating tools
(`submit_plan`, `execute_epic`, `resume_plan`, `merge_plan`, `review_task`)
refuse to operate on a plan owned by a different brain. This is enforced by the
Tier-1 ownership gate in `crates/spur-mcp/src/server.rs`.

Multiple brains MAY coexist against one `.beads/` project — for example:

- A read-only TUI inspecting plan progress concurrently with the writing brain.
- Two brains working on **disjoint plans** within the same beads database.

What is **not** supported is two brains writing to the **same plan** at once.
SPUR does not detect liveness, does not heartbeat, and does not arbitrate
conflicting writers. The single-writer rule is a discipline you maintain at the
operator layer.

---

## 2. Inspecting plan ownership

To see who currently owns a plan, look up the plan's epic and grep for the
`spur:plan-owner:*` label:

```sh
# Discover the epic for a plan_id by its plan-id label.
br list --label spur:plan-id:<plan_id>

# Inspect the labels on the epic.
br issue show <epic-id> | grep 'spur:plan-owner:'
```

A healthy plan carries exactly one such label. **Zero** owner labels means the
plan is **Unowned** (no brain has claimed it). **More than one** owner label
means the plan is in the **Ambiguous** state — typically the residue of a
crashed handoff or a manual label edit; the Tier-1 gate refuses operation in
that state and a `force_reclaim_plan` is required to clean it up.

In the TUI, the same information is surfaced through the `PlanSnapshot` event
on the `owner_brain_session_id` field (added in commit `73c4059f`). TUI
consumers that watch `PlanSnapshot` see ownership changes without polling beads.

---

## 3. When to force-reclaim

`force_reclaim_plan` is the **operator escape hatch**. It exists because the
single-writer rule above does not solve every operational problem — sometimes
the rightful owner cannot release the plan. Legitimate scenarios:

- **Owner brain crashed or was killed.** The plan still carries the dead
  brain's `spur:plan-owner:` label, so subsequent claims by any other brain
  (including a freshly restarted same-host brain with a new session id) are
  refused.
- **Owner brain stuck or runaway.** The brain is alive but wedged or
  misbehaving and you need to take over with another brain to unblock the plan.
- **Operator-initiated takeover for governance reasons.** A human operator
  wants to step in (e.g., move a plan from a CI brain to an interactive brain
  for review) and is willing to interrupt the current owner.

Every force-reclaim writes a `PlanForceReclaimed` audit sentinel to the epic's
beads comments — including the prior owner, the new owner, a UUID token, and
the operator-supplied reason. The audit trail is the record of accountability
for these takeovers.

---

## 4. How to force-reclaim

The MCP tool is `force_reclaim_plan`. Required arguments are `plan_id` and
`confirm: true`. An optional `reason` string is recorded in the audit sentinel.

Example MCP invocation:

```json
{
  "name": "force_reclaim_plan",
  "arguments": {
    "plan_id": "plan-2026-04-30-graph-rewrite",
    "confirm": true,
    "reason": "owner brain (session 7c62...) crashed during merge_plan; taking over to integrate"
  }
}
```

Successful response shape:

```json
{
  "prior_owner": "7c6258f16a674f6aa9b45ea1ef59ff7a",
  "new_owner": "9a3b1f2c-1e44-4b71-9d8a-2f8c0a3d7e10",
  "audit_token": "5b1b9f8e-3a12-4d6c-b8e4-7f0c1d2a3b4c"
}
```

`prior_owner` is `null` when the plan was Unowned at reclaim time. The
`audit_token` matches the `token` field of the `PlanForceReclaimed` sentinel
written to the epic's comments — operators can cross-reference the response
against the audit trail by token.

The audit sentinel body in beads comments looks like:

```
[[spur-audit v1]]
{"kind":"plan-force-reclaimed","plan_id":"plan-2026-04-30-graph-rewrite","prior_owner":"7c6258f16a674f6aa9b45ea1ef59ff7a","new_owner":"9a3b1f2c-1e44-4b71-9d8a-2f8c0a3d7e10","token":"5b1b9f8e-3a12-4d6c-b8e4-7f0c1d2a3b4c","reason":"owner brain (session 7c62...) crashed during merge_plan; taking over to integrate"}
```

> **Warning.** `force_reclaim_plan` is an operator action. If you force-reclaim
> a plan from a brain that is still alive and writing, you **will** clobber its
> in-flight state. The audit trail will show the takeover, but it is still bad:
> the displaced brain may have uncommitted projector updates, in-flight
> delegations, or pending continuations that will be silently orphaned. Confirm
> the prior owner is actually stuck/dead before invoking. Don't.

Without `confirm: true`, the call returns an `invalid_params` error explaining
the safety semantics; no labels are mutated and no audit sentinel is emitted.

---

## 5. What this design does NOT do

LBP intentionally omits several mechanisms that are common in distributed
ownership systems:

- **No heartbeat.** Brains do not ping anything to prove liveness.
- **No auto-inactive detection.** A brain that crashed does not lose ownership
  automatically; an operator must call `force_reclaim_plan` to release the
  stale label.
- **No CAS (compare-and-swap) on the owner label.** The Tier-1 ownership gate
  reads, decides, then writes; there is no atomic transition.
- **No new label schema.** The same single `spur:plan-owner:<session>` label
  vocabulary is preserved.

A consequence: the **race window** between two brains simultaneously claiming
an `Unowned` plan is unresolved. If two brains call `submit_plan` /
`execute_epic` / `resume_plan` for the same plan within roughly the time a
single beads `add_label` round-trip takes (~50ms in practice), both can succeed
and both will stamp owner labels. The result is the **Ambiguous** state, which
the gate then refuses for subsequent writes — last-writer-wins, with the audit
trail recording every claim.

This is acceptable for two reasons:

1. **The audit trail records the clobber.** Every ownership write emits a
   `PlanOwnershipAcquired` (or `PlanOwnershipTransferred`, or
   `PlanForceReclaimed`) sentinel. Operators inspecting a plan after the fact
   can see exactly what happened.
2. **The scenario requires deliberate violation of the single-writer rule.**
   Following the discipline in §1, no two brains should be racing to claim the
   same plan in the first place.

If race conditions become a real operational problem — for example, multiple
CI brains contending for the same plan under heavy automation — the deferred
CAS-hardening plan at
`docs/superpowers/plans/2026-05-02-plan-ownership-cas-hardening.md` (on branch
`spur/worker/cas-hardening-plan-design`) is the path forward. It atomically
prevents the race windows described here, but at the cost of significant
implementation complexity. LBP is the lighter-weight starting point; CAS is
available if and when the operational evidence justifies the cost.
