---
name: spur-loop-authoring
description: Use when a SPUR brain receives a user message beginning with `/spur-loop` and must turn natural-language loop intent into a preview-first durable loop authoring flow.
role: brain
---

# Spur Loop Authoring

Use this skill when the user's message begins with `/spur-loop`.

## The Iron Law

**Never call `submit_loop` directly from raw `/spur-loop` text.**

`/spur-loop` is preview-first. The only approved path is:

```text
/spur-loop text
  -> brain-authored LoopDoctorDraft
  -> spur_loop_doctor
  -> doctor-produced preview
  -> explicit user approval
  -> submit_loop with doctor-approved canonical params
```

## Recognize the Full Command

Treat the whole incoming message as authoring input, not only one flattened text
string. Interactive clients may send worker mentions and paths as `ResourceLink`
content blocks plus a prepended `[UI hint]` block. Preserve the original command
text for `original_command`, and use all text, resource links, and UI hints when
building the draft.

If worker mention routing is relevant, apply `worker-mention-routing` before
choosing task `agent`, `profile`, `model`, or `effort` values.

## Draft Shape

Call `spur_loop_doctor` with:

```json
{
  "original_command": "<the user command>",
  "draft": {
    "goal": "...",
    "pattern": "...",
    "cadence_secs": 86400,
    "schedule_description": "...",
    "autonomy": "l1|l2|l3",
    "tasks": [],
    "governors": {},
    "escalation": { "after_unresolved_generations": 3 },
    "assumptions": []
  }
}
```

Omit optional fields when the user did not provide enough intent to populate
them.

`LoopDoctorDraft.tasks[]` supports these fields: `task_id`, `agent`, `profile`,
`model`, `effort`, `config_overrides`, `task`, `depends_on`, `context_files`,
`triage`, `labels`, `issue_labels`, `output_path`, and `assumptions`.

Draft the best structured interpretation of the natural-language request:

| User hint | Draft field |
|---|---|
| Goal or repeated outcome | `goal` |
| `daily`, `weekly`, `every N hours` | `cadence_secs` plus optional `schedule_description` |
| `9AM`, `09:00`, `a.m.`, `p.m.` | cadence only, with the wall-clock wording preserved in `schedule_description` |
| `L1`, `L2`, `L3` | `autonomy` |
| `then`, `after`, ordered clauses | task `depends_on` |
| Worker mention or `[UI hint]` worker | task `agent`; model/profile/effort when supplied |
| `to @path/`, `write to @path`, attached file paths | task `output_path` or `context_files` |
| "escalating failure" | `escalation` when concretely representable, otherwise `assumptions` |

At least one task must be loop triage: set `triage: true` or include the
`spur:loop-triage-task` label in `labels` or `issue_labels`.

## Doctor Response Handling

Read the doctor result before saying the loop is valid.

| `status` | Required action |
|---|---|
| `error` | Report `errors`. Do not show a valid preview and do not call `submit_loop`. |
| `warnings` | Show `friendly_preview` and visibly surface `warnings`. Do not hide them in prose. |
| `ok` | Show `friendly_preview`. |

`spur_loop_doctor` never creates a durable loop. Its valid response includes
`canonical_submit_loop_params`, `approval_fingerprint`, and
`client_idempotency_key`; invalid responses omit them.

## Approval and Revision

Approval must be explicit, such as "approve", "create it", or an equivalent
confirmation after the preview. Silence, topic changes, or follow-up edits are not
approval.

On approval, call `submit_loop` with `canonical_submit_loop_params` unchanged. The
canonical params already carry the doctor-produced `client_idempotency_key`; use
the top-level `approval_fingerprint` and `client_idempotency_key` as the audit
values for the approved preview.

If the user revises anything after preview, discard the prior
`friendly_preview`, `approval_fingerprint`, `client_idempotency_key`, and
canonical params. Build a fresh draft and call `spur_loop_doctor` again.

## Scheduling and Management

V1 does not honor exact wall-clock scheduling. Do not promise that a loop runs at
`9AM` or another local time. The doctor normalizes to cadence and warns when
wall-clock wording is present.

First generation is armed immediately after approval because `submit_loop` labels
the new loop with `spur:loop-next-run:<now>`.

After durable creation, manage loops with the existing lifecycle tools:
`get_loop_status`, `set_loop_autonomy`, `pause_loop`, `resume_loop`, and
`kill_loop`.

## Red Flags

- Calling `submit_loop` before `spur_loop_doctor`.
- Building a preview from your own draft instead of `friendly_preview`.
- Treating `warnings` as optional or hidden.
- Reusing an old fingerprint or idempotency key after the user changes anything.
- Saying exact wall-clock scheduling is supported in v1.
