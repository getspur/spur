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
- Adding wall-clock or timezone scheduling in v1. Current loop execution is cadence-based and first-arms immediately; exact phrases like `9AM` are normalized with an explicit warning unless a later scheduler change adds cron/timezone fields.

## 3. Existing substrate

- `submit_loop` is already exposed as an MCP tool and creates the durable loop issue.
- `LoopSpec` already carries `loop_id`, `goal`, optional `pattern`, `cadence_secs`, `autonomy`, `template`, `governors`, and optional `escalation`.
- `loop_template_to_persist_input` converts a loop template into normal `PlanTask`s.
- `PlanTask` already supports `agent`, `profile`, `model`, `effort`, `config_overrides`, task body, dependencies, and `context_files`.
- Loop templates are raw JSON before generation. They may carry `labels` or `issue_labels` that are consumed by `submit_loop` validation, including `spur:loop-triage-task`; those fields are not part of `PlanTask` and are discarded when the scheduler later deserializes tasks into `PlanTask`.
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
| D7 | How is approval tied to the draft? | The doctor returns an `approval_fingerprint` over normalized canonical params, excluding server-minted fields such as `loop_id`, `issue_id`, and `next_run`. | `submit_loop` currently mints `loop_id` and arms `next_run` itself, so those values cannot be part of pre-approval equality. Any behavioral revision still changes the fingerprint and forces a fresh doctor pass. |
| D8 | How should unknown model/profile/effort values behave? | Warn and pass through when structurally valid. | Worker adapters evolve faster than SPUR. Unknown values may still be valid worker-side options. |
| D9 | What is the doctor task type? | A dedicated doctor draft task type, not plain `PlanTask`. | The draft must preserve loop-only metadata such as `labels`/`issue_labels` or a `triage` marker so canonical template JSON can satisfy existing `submit_loop` validation. |
| D10 | Where should loop validation live? | Shared loop validation under `plan::loops`, called by both `submit_loop` and `spur_loop_doctor`. | Existing validation helpers are private inside `server/handlers/plan.rs`; duplicating them would split invariants. |
| D11 | What happens for explicit L2/L3 creation? | Allow only when the user explicitly asks for it, and surface a warning that creation starts at that autonomy directly. | `set_loop_autonomy` enforces ratcheted promotion after creation, but `submit_loop` currently accepts the submitted autonomy. The preview must make that difference visible. |
| D12 | How are wall-clock phrases handled? | V1 canonicalizes to cadence and warns that exact local time is not honored. | `LoopSpec` has `cadence_secs`, not cron or timezone fields. A user may approve the cadence normalization, but the preview must not imply exact 9AM execution. |
| D13 | Is the doctor gated? | Yes at the handler layer: it checks the same loop feature/backend prerequisites as `submit_loop`. | A successful preview followed by a guaranteed license/backend failure would be poor UX. The pure doctor module remains testable; the handler supplies environment checks. |
| D14 | How is duplicate approval handled? | The doctor returns a `client_idempotency_key` derived from the fingerprint; `/spur-loop` submission must pass it through to `submit_loop`. | Approval retries after reconnect or repeated user action should not create duplicate durable loops. This requires adding idempotency support to `submit_loop`, mirroring the submit-plan TTL/dedup storage mechanism only; the typed `SubmitLoopParams.client_idempotency_key` field in section 9.3 is authoritative, not `submit_plan`'s raw-`Value` parameter parsing style. |

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

No TUI router change is required for the command itself: unknown slash commands already pass through to the brain as normal text. Worker mentions are not guaranteed to appear as one flat string, though. In interactive clients they may arrive as structured `ResourceLink` content blocks plus a prepended `[UI hint]` text block. The brain command recognizer must use the full message content, including resource links and UI hints, when drafting worker bundles.

The recognizer preserves the original command intent and asks the brain to draft:

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

The draft builder is brain-side. It converts natural language hints into a structured `LoopDoctorDraft`, but not yet trusted. The draft is intentionally richer than `PlanTask` because it must retain loop-authoring metadata that `PlanTask` cannot represent.

Supported user hints include:

- `daily`, `weekly`, `every N hours`, `9AM` for schedule intent,
- `L1`, `L2`, `L3` for autonomy,
- `[@worker:codex](worker://codex)/model/effort` for worker bundle intent,
- `then` for sequential dependency boundaries,
- `to @path/` or `write to @path` for output/context path intent,
- natural language such as "escalating failure" for escalation intent.

Each draft task carries the normal execution fields (`task_id`, `agent`, `profile`, `model`, `effort`, `config_overrides`, `task`, `depends_on`, `context_files`) plus loop-only fields:

- `triage: bool`, or an equivalent `issue_labels`/`labels` list,
- optional user-facing output path hints,
- optional assumptions that must be rendered in the preview.

When the doctor builds canonical `SubmitLoopParams`, it emits raw template task JSON with `labels` or `issue_labels` containing `spur:loop-triage-task` for the triage task. Only after `submit_loop` accepts the raw template does the scheduler deserialize tasks into `PlanTask`.

### 5.3 Doctor tool

`spur_loop_doctor` is deterministic and testable. It accepts the original text and a structured draft, then returns one of:

- `ok`: canonical params are valid and no warnings were produced,
- `warnings`: canonical params are valid but preview must display warnings,
- `error`: canonical params are absent and no loop may be submitted.

The doctor validates required fields, normalizes preview data, computes the approval fingerprint and submit idempotency key, and returns canonical `SubmitLoopParams` only when valid.

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
- first-run behavior,
- warnings,
- a clear line that no loop has been created yet.

Raw JSON is not shown by default.

Example preview:

```text
Loop Preview

Goal
Daily marketing research and summary generation.

Schedule
Runs every 24 hours.
First generation will be armed immediately after approval.

Autonomy
L3. This creates the loop directly at L3.

Tasks
1. codex / gpt-5.3 / low
   Research Codex and Claude Code marketing updates.

2. opencode / GLM-5.2 / max
   Summarize findings into docs/marketing/.
   Starts after task 1 completes.

Controls
No loop has been created yet.
Approve to create the durable loop and arm the first generation.

Warnings
- Exact 9:00 AM wall-clock scheduling is not represented in v1; this preview uses a 24-hour cadence.
```

### 5.5 Submit bridge

After explicit approval, the brain submits exactly the doctor-approved canonical params plus the doctor-produced idempotency key. If the user revises anything after preview, the brain must call `spur_loop_doctor` again and discard the old fingerprint and idempotency key.

## 6. Tool contract

### 6.1 Request shape

The exact Rust schema can evolve during implementation, but the logical fields are:

```text
original_command: string
draft: LoopDoctorDraft
```

Preview-first behavior is the `/spur-loop` command contract, not a configurable doctor mode.

`LoopDoctorDraft` should include structured equivalents of:

- goal,
- schedule intent or resolved cadence,
- autonomy,
- template tasks,
- governors,
- escalation,
- base target if present,
- user-facing assumptions made by the brain.
- doctor draft tasks with loop-only triage metadata.

### 6.2 Response shape

```text
status: ok | warnings | error
friendly_preview: string or structured preview sections
warnings: list
errors: list
canonical_submit_loop_params: present only when valid
approval_fingerprint: present only when valid
client_idempotency_key: present only when valid
```

The fingerprint is calculated from normalized canonical submit params, not from raw user text. The normalization must:

- exclude `spec.loop_id`, `issue_id`, `next_run`, and any other server-minted fields,
- sort JSON object keys before hashing,
- preserve task order and dependency lists after doctor normalization,
- use SHA-256 and return lowercase hex.

This lets wording changes that produce identical canonical behavior keep the same fingerprint, while behavioral changes produce a new one. The `client_idempotency_key` is `spur-loop:<approval_fingerprint>` and must be accepted by `submit_loop` before `/spur-loop` is exposed.

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
- exact wall-clock scheduling when the user refuses cadence normalization.

Warnings:

- vague schedule normalized by assumption,
- unknown model accepted as pass-through,
- unknown effort accepted as pass-through,
- unknown profile accepted as pass-through,
- output path mentioned only in task text rather than concrete `context_files`,
- escalation intent approximated because it is not exactly representable,
- worker mention normalized from display syntax to registry name.
- explicit L2/L3 creation starts at that autonomy directly rather than using the post-create ratchet.
- first generation will be armed immediately by `submit_loop`.

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

If approval submission is retried with the same `client_idempotency_key`, `submit_loop` must return the existing loop result rather than creating a duplicate loop.

## 9. Components

### 9.1 `crates/spur-core/src/mcp/plan.rs`

Add an MCP tool definition for `spur_loop_doctor`.

The tool description should state that this is the required validation gate for `/spur-loop` natural-language drafts and that it does not create durable loops.

### 9.2 `crates/spur-core/src/server/handlers/plan.rs`

Add the handler. It should:

- parse doctor params,
- check `PM_PRO_BEADS_ADVANCED` and beads backend availability, matching `submit_loop`,
- invoke deterministic doctor logic,
- return JSON-RPC invalid params for malformed doctor requests,
- return structured doctor errors for invalid drafts,
- never create a loop issue,
- never call `submit_loop` internally.

Keep the new handler code in this file as a thin dispatch shim that parses and routes only; all real doctor, normalization, and validation logic must live in `plan::loops::doctor` and `plan::loops::validation` rather than growing inline logic in `server/handlers/plan.rs`.

### 9.3 `crates/spur-core/src/tool_schemas.rs`

Add the public MCP schemas:

- `SpurLoopDoctorParams`,
- `LoopDoctorDraft`,
- `LoopDoctorDraftTask`,
- `SpurLoopDoctorOutput`,
- optional `client_idempotency_key` on `SubmitLoopParams`.

These should follow sibling loop schemas: `serde(deny_unknown_fields)`, `Serialize`/`Deserialize`, and `JsonSchema`.
For `SubmitLoopParams.client_idempotency_key`, this typed schema field is authoritative; do not mirror `submit_plan`'s raw-`Value` argument parsing style for this parameter.

### 9.4 `crates/spur-core/src/plan/loops/validation.rs`

Move or share existing loop submit validation out of `server/handlers/plan.rs` so both `submit_loop` and `spur_loop_doctor` call one implementation. This shared module owns:

- goal/cadence/governor checks,
- triage-task checks over raw template JSON,
- task graph validation,
- base-target validation helpers if needed.

### 9.5 `crates/spur-core/src/plan/loops/doctor.rs`

New focused module for validation and normalization. It should reuse existing loop validation functions where possible and share task normalization with `submit_plan_normalize_tasks` rather than reimplementing task graph logic ad hoc.

Responsibilities:

- validate the structured draft,
- normalize task IDs and dependency order,
- ensure a loop triage task is present,
- emit raw template task JSON that preserves loop labels before `PlanTask` deserialization,
- build canonical `SubmitLoopParams`,
- build friendly preview sections,
- compute `approval_fingerprint`,
- compute `client_idempotency_key`,
- produce warnings and blocking errors.

### 9.6 `crates/spur-core/src/plan/loops/spec.rs`

Keep `LoopSpec` stable unless implementation needs a small shared type for doctor input/output. The loop engine remains schema-driven and scheduler-owned.

### 9.7 `crates/spur-core/src/server/handlers/plan.rs` submit-loop idempotency

Extend `submit_loop` to accept `client_idempotency_key` and de-duplicate repeated create requests for the same key, following the existing `submit_plan` idempotency precedent. The exact storage/replay mechanism belongs in the implementation plan, but `/spur-loop` must not be exposed without a duplicate-create guard.
The precedent lives in `crates/spur-core/src/submit_plan_dedup.rs`, the TTL-based, beads-label-backed registry with `REGISTRY_LABEL`, `KEY_LABEL_PREFIX`, `TTL`, `key_label()`, `lookup()`, `record()`, and `registry_epic()`. Generalize or reuse that registry, such as by parameterizing it over feature/kind or extracting a shared helper, rather than forking a parallel loop-specific copy. Any `server/handlers/plan.rs` changes for this idempotency path must also follow section 9.2's thin-shim rule and delegate real logic out to `plan::loops::doctor` / `plan::loops::validation` or the shared dedup helper.

### 9.8 Documentation

Update `docs/loops.md` to explain:

- `/spur-loop` is preview-first,
- the brain interprets natural language,
- `spur_loop_doctor` validates the draft,
- approval is required before durable loop creation,
- exact wall-clock scheduling is not honored in v1,
- first generation is armed immediately after approval,
- the existing loop lifecycle tools remain the management surface.

## 10. Testing

Unit tests:

- valid daily L3 rebuild/test draft returns `ok`, friendly preview, canonical submit params, and fingerprint,
- valid daily 9AM research-then-summary draft returns dependent tasks with distinct worker/model/effort bundles and a wall-clock normalization warning,
- missing goal blocks,
- missing cadence blocks,
- missing tasks blocks,
- missing triage task blocks,
- doctor draft with `triage: true` emits canonical raw task JSON containing `spur:loop-triage-task`,
- invalid autonomy blocks,
- non-positive governor cap blocks,
- missing dependency blocks,
- cyclic dependency blocks,
- unknown model/profile/effort warn but do not block,
- fingerprint changes when canonical params change,
- fingerprint stays stable when non-behavioral raw wording changes but canonical params are identical.
- fingerprint excludes server-minted `loop_id` and remains stable before/after `submit_loop` fills it.
- exact `9AM` wording produces a cadence-normalization warning.
- explicit L3 creation produces a direct-autonomy warning.
- escalation preview shows the concrete `after_unresolved_generations` value.

Handler tests:

- `spur_loop_doctor` is registered as an MCP tool,
- malformed doctor params produce JSON-RPC invalid params,
- ungated or non-beads environments fail at doctor time the same way they fail at submit-loop time,
- invalid drafts return doctor errors without creating an issue,
- valid drafts return canonical params but do not create an issue,
- `submit_loop` remains the only durable loop creation path.
- repeated `submit_loop` calls with the same `client_idempotency_key` do not create duplicate loop issues.

Documentation/example tests:

- one example for the daily L3 pull-main/rebuild/full-test loop,
- one example for the daily 9AM marketing research then summary loop.

## 11. Rollout

Implement in five small steps:

1. Add the doctor schema, module, handler, and tests.
2. Move loop validation into a shared module and have `submit_loop` keep using that shared validation.
3. Add `submit_loop` idempotency support keyed by `client_idempotency_key`.
4. Document the `/spur-loop` brain contract and preview format.
5. Teach the brain prompt/skill path that `/spur-loop` must call `spur_loop_doctor` before preview and must call `submit_loop` only after explicit approval.

The scheduler semantics do not change in v1. `submit_loop` does gain shared validation and idempotency support so the preview-first command can be operationally safe.
