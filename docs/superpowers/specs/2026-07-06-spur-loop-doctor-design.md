# `/spur-loop` doctor-gated natural language loop command

**Status:** design approved (interactive brainstorm), pending implementation plan
**Date:** 2026-07-06
**Owners:** Kevin Truong (kevin.truong.ds@gmail.com)
**Scope:** Add a preview-first `/spur-loop` brain command that lets users declare durable loops in natural language, while a deterministic MCP doctor tool validates and normalizes the brain-produced draft before `submit_loop` can create the loop.

---

## 1. Goal

The loop engine already has the durable substrate: `submit_loop` creates a loop issue with a `[[spur-loop v1]]` sentinel, `LoopSpec` stores goal/cadence/autonomy/template/governors/escalation, and the scheduler turns loop templates into ordinary `PlanTask`s. `PlanTask` already carries the worker control surface: `agent`, optional `profile`, optional `model`, optional `effort`, and optional config overrides.

What is missing is a user-friendly front door. Users should be able to type commands like:

```text
/spur-loop [@worker:codex](worker://codex)/gpt-5.3-spark/max L3 daily pull main, rebuild, full test, escalating failure
```

or:

```text
/spur-loop daily, 9AM [@worker:codex](worker://codex)/gpt-5.3/low research marketing about codex, claude-code then [@worker:opencode](worker://opencode)/GLM-5.2/max give out summary to @docs/marketing/
```

The user intent is natural language, but durable execution must remain deterministic. This spec adds a doctor-gated workflow:

1. the brain recognizes `/spur-loop`,
2. the brain interprets the natural language into a structured draft,
3. `spur_loop_doctor` validates and normalizes the draft,
4. the brain shows a friendly preview only,
5. the user approves,
6. the brain submits the doctor-approved canonical `SubmitLoopParams` with `submit_loop`.

## 2. Non-goals

- A second loop scheduler. The existing loop issue, sentinel, scheduler, plan generation, and reconciler remain the only durable execution path.
- A full natural-language parser in Rust. The brain interprets language; the doctor validates the structured result.
- Showing raw `LoopSpec` JSON by default. The preview is a friendly operational summary only.
- Creating a loop without explicit approval. `/spur-loop` defaults to preview-before-create.
- Packing worker/model/effort into the existing `agent` field. The canonical draft uses structured task fields.
- Changing the `LoopSpec` sentinel format unless implementation discovers a narrow shared-type need.

## 3. Existing substrate

- `submit_loop` is already exposed as an MCP tool and creates the durable loop issue.
- `LoopSpec` already carries `loop_id`, `goal`, optional `pattern`, `cadence_secs`, `autonomy`, `template`, `governors`, and optional `escalation`.
- `loop_template_to_persist_input` converts a loop template into normal `PlanTask`s.
- `PlanTask` already supports `agent`, `profile`, `model`, `effort`, `config_overrides`, task body, dependencies, and `context_files`.
- Existing `submit_loop` validation already enforces important invariants such as cadence, triage-task presence, and governor shape.

The design should reuse this substrate. `/spur-loop` is an ergonomic authoring layer over it.

## 4. Design decisions

| # | Question | Choice | Rationale |
|---|---|---|---|
| D1 | Where does natural language live? | In the brain command flow. | The brain is best positioned to interpret messy user intent, worker mentions, paths, and schedule wording. |
| D2 | What enforces determinism? | A new MCP tool named `spur_loop_doctor`. | The doctor creates a testable contract between brain-produced drafts and durable loop creation. |
| D3 | Does the doctor parse natural language? | No. It validates and normalizes a structured draft, while receiving the original text for diagnostics. | Avoids building a brittle parser while still preventing raw-NL submission. |
| D4 | What is the default UX? | Preview before create. | Durable loops can run repeatedly and spend money; silent creation is too risky. |
| D5 | What does the user see? | Friendly summary only. | Users should inspect the operational behavior, not schema JSON. |
| D6 | Can the brain call `submit_loop` directly from `/spur-loop` text? | No. It must call `spur_loop_doctor` first and submit only doctor-approved canonical params. | This preserves deterministic workflow and creates an auditable contract. |
| D7 | How is approval tied to the draft? | The doctor returns an `approval_fingerprint` over canonical params. | Any revision changes the fingerprint and forces a fresh doctor pass. |
| D8 | How should unknown model/profile/effort values behave? | Warn and pass through when structurally valid. | Worker adapters evolve faster than SPUR. Unknown values may still be valid worker-side options. |

## 5. Architecture

`/spur-loop` is a brain-facing internal command that compiles user intent into the existing loop substrate.

```text
/spur-loop natural language
  -> brain command recognizer
  -> brain-produced structured loop draft
  -> spur_loop_doctor
  -> friendly preview
  -> user approval
  -> submit_loop with doctor-approved canonical params
  -> existing loop scheduler and reconciler
```

### 5.1 Command recognizer

The brain treats messages beginning with `/spur-loop` as loop-authoring requests. It should not pass the raw text directly to `submit_loop`.

The recognizer preserves the original text and asks the brain to draft:

- goal,
- schedule/cadence,
- autonomy,
- task list,
- dependencies,
- worker/model/effort/profile bundles,
- output/context path hints,
- governor hints,
- escalation behavior.

### 5.2 Draft builder

The draft builder is brain-side. It converts natural language hints into a structured draft that is close to `SubmitLoopParams`, but not yet trusted.

Supported user hints include:

- `daily`, `weekly`, `every N hours`, `9AM` for schedule intent,
- `L1`, `L2`, `L3` for autonomy,
- `[@worker:codex](worker://codex)/model/effort` for worker bundle intent,
- `then` for sequential dependency boundaries,
- `to @path/` or `write to @path` for output/context path intent,
- natural language such as "escalating failure" for escalation intent.

### 5.3 Doctor tool

`spur_loop_doctor` is deterministic and testable. It accepts the original text and a structured draft, then returns one of:

- `ok`: canonical params are valid and no warnings were produced,
- `warnings`: canonical params are valid but preview must display warnings,
- `error`: canonical params are absent and no loop may be submitted.

The doctor validates required fields, normalizes preview data, computes the approval fingerprint, and returns canonical `SubmitLoopParams` only when valid.

### 5.4 Friendly preview

The preview is generated from the doctor output, not from the brain's unvalidated draft. It should show:

- goal,
- schedule,
- autonomy,
- tasks in dependency order,
- worker/model/effort/profile choices,
- output paths or context files,
- cost/governor hints,
- escalation behavior,
- warnings,
- a clear line that no loop has been created yet.

Raw JSON is not shown by default.

Example preview:

```text
Loop Preview

Goal
Daily marketing research and summary generation.

Schedule
Runs every day at 9:00 AM.

Autonomy
L3.

Tasks
1. codex / gpt-5.3 / low
   Research Codex and Claude Code marketing updates.

2. opencode / GLM-5.2 / max
   Summarize findings into docs/marketing/.
   Starts after task 1 completes.

Controls
No loop has been created yet.
Approve to create the durable loop and arm the first generation.
```

### 5.5 Submit bridge

After explicit approval, the brain submits exactly the doctor-approved canonical params. If the user revises anything after preview, the brain must call `spur_loop_doctor` again and discard the old fingerprint.

## 6. Tool contract

### 6.1 Request shape

The exact Rust schema can evolve during implementation, but the logical fields are:

```text
original_command: string
draft: LoopDraft
confirmation_mode: preview_first
```

`LoopDraft` should include structured equivalents of:

- goal,
- schedule intent or resolved cadence,
- autonomy,
- template tasks,
- governors,
- escalation,
- base target if present,
- user-facing assumptions made by the brain.

### 6.2 Response shape

```text
status: ok | warnings | error
friendly_preview: string or structured preview sections
warnings: list
errors: list
canonical_submit_loop_params: present only when valid
approval_fingerprint: present only when valid
```

The fingerprint is calculated from canonical submit params, not from raw user text. This lets wording changes that produce identical canonical behavior keep the same fingerprint, and behavioral changes produce a new one.

## 7. Validation rules

Blocking errors:

- missing goal,
- missing schedule/cadence,
- invalid or missing task template,
- missing loop triage task,
- invalid autonomy,
- cadence below the loop minimum,
- non-positive governor caps,
- duplicate task IDs,
- missing dependency references,
- cyclic dependencies,
- invalid worker bundle shape,
- malformed base target,
- backend or license incompatibility if detectable in the doctor layer.

Warnings:

- vague schedule normalized by assumption,
- unknown model accepted as pass-through,
- unknown effort accepted as pass-through,
- unknown profile accepted as pass-through,
- output path mentioned only in task text rather than concrete `context_files`,
- escalation intent approximated because it is not exactly representable,
- worker mention normalized from display syntax to registry name.

## 8. Data flow and errors

The only approved flow is:

```text
/spur-loop text
  -> brain draft
  -> spur_loop_doctor
  -> friendly preview
  -> user approval
  -> submit_loop
```

If the doctor returns `error`, the brain reports the errors and asks for a revision. It must not preview as if the loop is valid and must not call `submit_loop`.

If the doctor returns `warnings`, the brain may show the friendly preview, but warnings must be visible.

If `submit_loop` fails after approval, the brain reports the durable MCP error and does not claim that the loop exists. The user can revise and retry from the last draft, which still requires another doctor pass if changed.

## 9. Components

### 9.1 `crates/spur-core/src/mcp/plan.rs`

Add an MCP tool definition for `spur_loop_doctor`.

The tool description should state that this is the required validation gate for `/spur-loop` natural-language drafts and that it does not create durable loops.

### 9.2 `crates/spur-core/src/server/handlers/plan.rs`

Add the handler. It should:

- parse doctor params,
- invoke deterministic doctor logic,
- return JSON-RPC invalid params for malformed doctor requests,
- return structured doctor errors for invalid drafts,
- never create a loop issue,
- never call `submit_loop` internally.

### 9.3 `crates/spur-core/src/plan/loops/doctor.rs`

New focused module for validation and normalization. It should reuse existing loop validation functions where possible and share task normalization with `submit_plan_normalize_tasks` rather than reimplementing task graph logic ad hoc.

Responsibilities:

- validate the structured draft,
- normalize task IDs and dependency order,
- ensure a loop triage task is present,
- build canonical `SubmitLoopParams`,
- build friendly preview sections,
- compute `approval_fingerprint`,
- produce warnings and blocking errors.

### 9.4 `crates/spur-core/src/plan/loops/spec.rs`

Keep `LoopSpec` stable unless implementation needs a small shared type for doctor input/output. The loop engine remains schema-driven and scheduler-owned.

### 9.5 Documentation

Update `docs/loops.md` to explain:

- `/spur-loop` is preview-first,
- the brain interprets natural language,
- `spur_loop_doctor` validates the draft,
- approval is required before durable loop creation,
- the existing loop lifecycle tools remain the management surface.

## 10. Testing

Unit tests:

- valid daily L3 rebuild/test draft returns `ok`, friendly preview, canonical submit params, and fingerprint,
- valid daily 9AM research-then-summary draft returns dependent tasks with distinct worker/model/effort bundles,
- missing goal blocks,
- missing cadence blocks,
- missing tasks blocks,
- missing triage task blocks,
- invalid autonomy blocks,
- non-positive governor cap blocks,
- missing dependency blocks,
- cyclic dependency blocks,
- unknown model/profile/effort warn but do not block,
- fingerprint changes when canonical params change,
- fingerprint stays stable when non-behavioral raw wording changes but canonical params are identical.

Handler tests:

- `spur_loop_doctor` is registered as an MCP tool,
- malformed doctor params produce JSON-RPC invalid params,
- invalid drafts return doctor errors without creating an issue,
- valid drafts return canonical params but do not create an issue,
- `submit_loop` remains the only durable loop creation path.

Documentation/example tests:

- one example for the daily L3 pull-main/rebuild/full-test loop,
- one example for the daily 9AM marketing research then summary loop.

## 11. Rollout

Implement in three small steps:

1. Add the doctor schema, module, handler, and tests.
2. Document the `/spur-loop` brain contract and preview format.
3. Teach the brain prompt/skill path that `/spur-loop` must call `spur_loop_doctor` before preview and must call `submit_loop` only after explicit approval.

The durable loop engine itself does not change in step 1. That keeps the feature safe to add without changing scheduler semantics.

