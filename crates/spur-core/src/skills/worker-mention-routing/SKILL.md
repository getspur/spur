---
name: worker-mention-routing
description: How the brain handles explicit worker @-mentions from the user. User intent outranks algorithmic routing. Covers validation, override conditions, ambiguity resolution, and plan-engine interaction.
role: brain
---

# Worker Mention Routing

## The Iron Law

**User @mention outranks your algorithm.**

When the user types `@codex fix the auth bug`, they have made a
delegation decision. Your job is to execute it, not second-guess it.
The `[UI hint] User-suggested workers...` block in your context is
not advice. It is a directive.

## Recognizing the hint

The TUI prepends a text block in this exact format:

  [UI hint] User-suggested workers for delegation this turn: <name>[, <name>...]
  (preference, not override; honor unless `delegation.avoid_for` clearly matches).

Scan your first `ContentBlock::Text` for this pattern on every turn.
If present, extract the worker name(s).

## Routing hierarchy

1. **User @mention** — highest priority. Honor unless the worker's
   `avoid_for` explicitly matches the task shape.
2. **Plan engine embedded preference** — if a plan task carries
   `preferred_worker` metadata, use it.
3. **Algorithmic selection** — your `delegation_plan` candidates
   and rationale. Only when (1) and (2) are absent.

## Validation: verify before delegating

**NEVER** call `delegate_to_worker(agent = "...")` for a
user-mentioned worker without first verifying it exists.

  list_available_workers() → check the `name` field.

If the mentioned name is NOT in the list:

| Scenario | Action |
|---|---|
| Typo (`@codx` for `@codex`) | Ask user to clarify: "Did you mean @codex?" |
| Deprecated worker | Inform user: "@legacy-worker is no longer available. @replacement has similar capabilities." |
| Unknown name | Inform user: "@ghost is not a known worker. Available workers: [list]." |

Do NOT silently fall back to algorithmic selection when the user
named a specific worker. That destroys trust.

## When you MAY override user preference

Only one condition: the worker's `avoid_for` field **clearly and
explicitly** matches the task.

  "avoid_for": ["multi-file coordination", "UI/UX design"]
  Task: "Redesign the settings page layout"

→ Override permitted. State your rationale aloud:
  "User mentioned @codex, but its avoid_for explicitly excludes
   UI/UX design. Delegating to @kiro instead, which lists
   `good_for: UI/UX design`."

  "avoid_for": ["complex refactoring"]
  Task: "Fix a one-line null pointer bug"

→ Override NOT permitted. `avoid_for` is a SOFT signal for
mechanical tasks. The user's explicit choice wins.

| Excuse | Reality |
|---|---|
| "The user probably didn't know codex is cheaper" | STOP. Cost is YOUR concern, not the user's when they've named a worker. |
| "Another worker scored higher in my algorithm" | STOP. Your algorithm is disabled when user intent is present. |
| "The task is simple enough for me to do myself" | STOP. Unless the task is <2 min, respect the user's delegation choice. |

## Ambiguity resolution

User mentions are fuzzy. Resolve using shortest unique prefix match
against `list_available_workers` names:

  `@claude` → matches `claude-code` and `claude-code-acp`
  → NOT unique. Ask: "Did you mean @claude-code or @claude-code-acp?"

  `@codex` → matches only `codex`
  → Unique. Proceed.

  `@c` → matches `codex`, `claude-code`, `cursor`
  → NOT unique. Ask for clarification.

Never guess when ambiguity exists.

## Multiple worker mentions

`@codex @kiro write tests and docs`

**Option A — independent subtasks:**
  Decompose into parallel tasks:
    - "write tests" → @codex
    - "write docs" → @kiro
  Call `delegate_parallel` with per-task worker selection.

**Option B — coupled work:**
  If the tasks share state or have ordering dependencies, pick the
  worker whose `good_for` best matches the dominant task shape.
  State your reasoning:
    "Both @codex and @kiro mentioned. The work is coupled (tests
     must validate doc examples). @codex's good_for includes
     test-writing; delegating the unified task to @codex."

Do NOT call `delegate_to_worker` twice in sequence for coupled
work without a plan. That wastes context and creates race conditions.

## Plan engine interaction

When using `submit_plan` / `execute_epic`:

- If the user mentioned a worker for the entire epic, embed the
  preference in the epic's first task description. The reconciler
  does not (yet) read worker preferences from beads metadata.
- If the user mentioned workers for specific subtasks, create
  separate tasks with explicit worker names in their descriptions.
  The brain must manually route each task on dispatch; the plan
  engine does not auto-route from @mentions.

## Required: delegation_plan with user mention

Even when honoring a user @mention, you MUST still pass a
`delegation_plan`. Its shape changes:

  {
    "chosen": "user-mentioned-worker-name",
    "rationale": "User explicitly requested @name via UI mention.
                  Validated against list_available_workers.
                  [Override note if applicable: avoid_for matches
                   task shape, overriding to @other with stated
                   rationale.]"
  }

The rationale is your audit trail. Reviewers and debug logs need to
see that you respected (or consciously overrode) user intent.

## Red flags — STOP and re-read this skill

- Delegating to a worker without checking `list_available_workers`
  first.
- Ignoring the `[UI hint]` block entirely.
- Overriding user preference without stating `avoid_for` match
  explicitly in your rationale.
- Using algorithmic candidate comparison when user intent is present.
- Guessing ambiguous mentions instead of asking for clarification.

## Cross-references

- `brain-delegation` — algorithmic routing when NO user mention
  is present.
- `brain-review-gate` — review the worker's output against the
  original user request (did @codex actually fix the auth bug?).
- `beads-lifecycle` — if the user-mentioned task fails, revert
  the issue status and re-offer worker choice.
