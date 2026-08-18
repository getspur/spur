---
name: brainstorming
description: Use when brainstorming a feature, designing a component, exploring requirements, creating a spec, or deciding how to build something. Triggers on phrases like "brainstorm", "design this", "how should we build", "create a spec", "figure out the approach", or "plan this feature". In SPUR, produces a beads-tracked design with an epic, design notebook (ns-mermaid), and approved spec before any code is written.
role: brain
---

# SPUR Brainstorming — Ideas Into Beads-Tracked Designs

## Overview

In SPUR, design work is not ephemeral chat. It is work product that must be discoverable, reviewable, and retryable. Every design decision lives in beads or it does not exist.

**Core principle:** INTENT → DESIGN → RECORD. The design process itself is a beads epic.

Help turn ideas into fully formed designs and specs through natural collaborative dialogue. The output is an approved **design notebook** (with native `ns_mermaid` formal cells where contracts matter) **and** a beads epic that captures design rationale, decisions, and approval state.

**Normative NS-Spec design:** `docs/superpowers/specs/ns-spec-v0.2-design.ipynb` (v0.3 — notebook cell as authoritative self-verifying unit).  
**Practical product-spec template:** `docs/superpowers/specs/2026-07-31-skills-catalog-mcp-design.ipynb`.  
**Authoring procedure:** `references/ns-spec-notebook-authoring.md` (required when writing formal cells).

## The Iron Law

```
NO IMPLEMENTATION WITHOUT AN APPROVED SPEC AND A BEADS EPIC
```

Do NOT invoke any implementation skill, write any code, scaffold any project, or dispatch any worker until:
1. A design has been presented and the user has approved it
2. A beads epic exists tracking the design
3. The approved spec is written under `docs/superpowers/specs/` (default: design `.ipynb`)

This applies to EVERY project regardless of perceived simplicity.

## Anti-Pattern: "This Is Too Simple To Need A Design"

Every project goes through this process. A todo list, a single-function utility, a config change — all of them. "Simple" projects are where unexamined assumptions cause the most wasted work. The design can be short (a few sentences for truly simple projects), but you MUST present it, get approval, and record it in beads.

## Spec Artifact Format

| Complexity | Authoritative artifact | Notes |
|---|---|---|
| **Default (any non-trivial design)** | `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.ipynb` | Written **only** via Notebook MCP. Markdown cells for prose; native `ns_mermaid` code cells for formal gates. |
| **Trivial only** (no partitions, state machines, or proof obligations) | Same path as a short notebook **or** legacy `…-design.md` | Prefer notebook for consistency. If markdown is used, say so in the beads epic. |
| **Portable review export** | Optional generated `.md` | Non-authoritative snapshot only. |

**The notebook is the specification.** Generated Markdown is never the source of truth when an `.ipynb` exists.

### Formal cells vs prose

- **Markdown cells** — goals, non-goals, rationale, migration, risks, human narrative.
- **Native `ns_mermaid` cells** (`code_type: "ns_mermaid"`) — architecture decisions, eligibility/routing/release gates, lifecycles, and any claim that must be self-verifying through `solve` inside the cell runner.
- **Never** put formal contracts in Markdown Mermaid fences, Python/Z3 cells, or a second authored constraint AST. One cell = one annotated Mermaid source = one verification identity (NS-Spec same-cell architecture).

## SPUR Design Checklist

Complete these in order. Each step updates beads.

1. **Explore project context** — check files, docs, recent commits, existing beads issues
2. **Create beads epic** — `create_issue(title: "Design: <feature>", type: "epic", labels: ["spur:plan-id:<id>"])`. Status: `open`. This is the design's source of truth.
3. **Ask clarifying questions** — one at a time; record key constraints as beads comments with `spur-audit v1` kind: `plan-submit`
4. **Propose 2-3 approaches** — with trade-offs and your recommendation; record the chosen approach in a beads comment
5. **Present design** — in sections scaled to complexity; get user approval after each section
6. **Write design notebook** — create/save `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.ipynb` via Notebook MCP; author formal units as `ns_mermaid` cells using the compiler-backed loop in `references/ns-spec-notebook-authoring.md`; link the path in the beads epic
7. **Spec self-review** — placeholder scan, internal consistency, scope check, ambiguity check, **plus** ns-mermaid proof freshness
8. **User reviews written spec** — ask user to review the notebook (and proof evidence) before proceeding
9. **Transition to writing-plans** — invoke the `writing-plans` skill to create a beads-backed implementation plan

## Process Flow

```dot
digraph spur_brainstorming {
    "Explore context" [shape=box];
    "Create beads epic" [shape=box];
    "Ask questions" [shape=box];
    "Propose approaches" [shape=box];
    "Present design" [shape=box];
    "User approves?" [shape=diamond];
    "Write design notebook\n(Notebook MCP + ns_mermaid)" [shape=box];
    "Spec self-review\n(+ proof freshness)" [shape=box];
    "User reviews spec?" [shape=diamond];
    "Update beads epic\n(link notebook, mark approved)" [shape=box];
    "Invoke writing-plans" [shape=doublecircle];

    "Explore context" -> "Create beads epic";
    "Create beads epic" -> "Ask questions";
    "Ask questions" -> "Propose approaches";
    "Propose approaches" -> "Present design";
    "Present design" -> "User approves?";
    "User approves?" -> "Present design" [label="no, revise"];
    "User approves?" -> "Write design notebook\n(Notebook MCP + ns_mermaid)" [label="yes"];
    "Write design notebook\n(Notebook MCP + ns_mermaid)" -> "Spec self-review\n(+ proof freshness)";
    "Spec self-review\n(+ proof freshness)" -> "User reviews spec?";
    "User reviews spec?" -> "Write design notebook\n(Notebook MCP + ns_mermaid)" [label="changes"];
    "User reviews spec?" -> "Update beads epic\n(link notebook, mark approved)" [label="approved"];
    "Update beads epic\n(link notebook, mark approved)" -> "Invoke writing-plans";
}
```

**The terminal state is invoking writing-plans.** Do NOT dispatch workers, invoke test-driven-development, or take any implementation action. The ONLY next step is writing-plans.

## The Process

### Understanding the Idea

- Check out the current project state first (files, docs, recent commits, existing beads issues)
- Before asking detailed questions, assess scope: if the request describes multiple independent subsystems (e.g., "build a platform with chat, file storage, billing, and analytics"), flag this immediately. Don't spend questions refining details of a project that needs decomposition first.
- If the project is too large for a single spec, help the user decompose into sub-epics: what are the independent pieces, how do they relate, what order should they be built? Each sub-epic gets its own design epic → spec → plan → implementation cycle.
- For appropriately-scoped projects, ask questions one at a time to refine the idea
- Prefer multiple choice questions when possible
- Focus on understanding: purpose, constraints, success criteria

### beads-First Design Tracking

**At step 2 (Create beads epic):**

```
create_issue(
  title: "Design: <feature name>",
  body: "Design epic for <feature>. Tracks design decisions, approach selection, and spec approval.",
  type: "epic",
  labels: ["spur:plan-id:<generate-uuid>"]
)
```

**During steps 3-5 (Questions, approaches, design):**

Record significant decisions as comments on the epic:

```
[[spur-audit v1]]
{
  "kind": "plan-submit",
  "plan_id": "<id>",
  "decision": "Chosen approach: <name>",
  "rationale": "<why>"
}
```

**At step 8 (Spec approved):**

Update the epic to mark design complete:

```
update_issue(
  status: "closed",
  comment: "Design approved. Spec: docs/superpowers/specs/<filename>.ipynb. Formal @spec cells: <ids>. Ready for implementation plan."
)
```

### Exploring Approaches

- Propose 2-3 different approaches with trade-offs
- Present options conversationally with your recommendation and reasoning
- Lead with your recommended option and explain why
- Consider SPUR-specific constraints: which approach minimizes cross-task coupling? Which produces the clearest task boundaries for parallel workers?

### Presenting the Design

- Once you believe you understand what you're building, present the design
- Scale each section to its complexity
- Ask after each section whether it looks right so far
- Cover: architecture, components, data flow, error handling, testing
- **SPUR-specific:** Explicitly call out task decomposition boundaries. Which components can be built in parallel? Which have strict ordering dependencies? This informs the plan DAG later.
- Flag which decisions will become **formal `ns_mermaid` gates** in the notebook (partitions, lifecycles, release/eligibility rules) vs prose-only sections
- Be ready to go back and clarify if something doesn't make sense

### Design for Isolation and Worker Context

- Break the system into smaller units that each have one clear purpose
- For each unit, you should be able to answer: what does it do, how do you use it, and what does it depend on?
- **SPUR-specific:** Each unit should map to a plausible plan task. If a unit is too large for a single worker session, split it further.
- Can someone understand what a unit does without reading its internals? Can you change the internals without breaking consumers? If not, the boundaries need work.
- Prefer smaller, well-bounded units — they are easier to reason about and produce more reliable worker outputs.

### Working in Existing Codebases

- Explore the current structure before proposing changes. Follow existing patterns.
- Where existing code has problems that affect the work, include targeted improvements as part of the design
- Don't propose unrelated refactoring. Stay focused on what serves the current goal.

## After the Design

### Documentation — write the design notebook

**Hard gate:** Author the approved design through Notebook MCP, not by dumping a giant hand-edited `.ipynb` JSON blob unless MCP is unavailable (then note the fallback in beads).

1. `notebook_new` or create/open the target path under `docs/superpowers/specs/`.
2. Persist as `YYYY-MM-DD-<topic>-design.ipynb`.
3. Insert **markdown** cells for narrative sections (title, goals, non-goals, risks, migration).
4. For each formal unit, follow **`references/ns-spec-notebook-authoring.md`**:
   - pin dialect with `notebook_ns_mermaid_spec`
   - preflight with `notebook_ns_mermaid_check`
   - repair with `notebook_ns_mermaid_explain` (span-local only)
   - insert/write with `code_type: "ns_mermaid"`
   - execute with `notebook_run_cell`
   - accept only when mandatory obligations match and hashes are fresh
5. Record notebook path, `@spec` ids, and key proof hashes in the beads epic.
6. Commit the design notebook to git.

**Do not** rely on general Mermaid knowledge for NS-Mermaid annotations. Profile registry + preflight are the dialect source of truth (NS-Spec §13).

### Spec Self-Review

After writing the spec document, look at it with fresh eyes:

1. **Placeholder scan:** Any "TBD", "TODO", incomplete sections, or vague requirements? Fix them.
2. **Internal consistency:** Do any sections contradict each other? Does the architecture match the feature descriptions?
3. **Scope check:** Is this focused enough for a single implementation plan, or does it need decomposition into multiple epics?
4. **Ambiguity check:** Could any requirement be interpreted two different ways? If so, pick one and make it explicit.
5. **beads-check:** Is every major decision recorded in the beads epic comments? If not, add them.
6. **Notebook / NS-Spec check** (when `.ipynb`):
   - Formal claims use native `ns_mermaid` cells, not Markdown fences
   - Each formal cell has been run; proof outputs are present
   - Expected obligation statuses match (or deliberate negative fixtures document failure)
   - Profile pins and proof hashes are recorded for review
   - No second formal source (AST, Python Z3, duplicate constraint prose)

Fix any issues inline. No need to re-review — just fix and move on.

Optional: dispatch a reviewer with `spec-document-reviewer-prompt.md` against the notebook path.

### User Review Gate

After the spec review loop passes, ask the user to review the written spec before proceeding:

> "Spec written and committed to `<path>.ipynb`. Formal cells: `<@spec ids>`. The design epic `<issue_id>` tracks all decisions and proof hashes. Please review the notebook (diagram + proof badges) and let me know if you want changes before we write the implementation plan."

Wait for the user's response. If they request changes, make them (re-run formal cells after source edits) and re-run the spec review loop. Only proceed once the user approves.

### Transition to Implementation Planning

- Invoke the `writing-plans` skill to create a detailed, beads-backed implementation plan
- Pass the **notebook** spec path, closed design epic issue_id, and list of formal `@spec` cell ids that bind implementation
- Do NOT invoke any other skill. writing-plans is the next step.

## Key Principles

- **One question at a time** — Don't overwhelm with multiple questions
- **Multiple choice preferred** — Easier to answer than open-ended when possible
- **YAGNI ruthlessly** — Remove unnecessary features from all designs
- **Explore alternatives** — Always propose 2-3 approaches before settling
- **Incremental validation** — Present design, get approval before moving on
- **Beads-first** — Every design decision is recorded in beads before implementation begins
- **Notebook-first formal specs** — Authoritative design is `.ipynb`; `ns_mermaid` cells are self-verifying contracts
- **Compiler-backed NS-Mermaid** — Profile pin → check → write → run → accept; never prompt-only annotations
- **Be flexible** — Go back and clarify when something doesn't make sense

## Anti-Patterns (Spec Authoring)

| Excuse | Reality |
|---|---|
| "Markdown Mermaid fence is fine for the architecture gate" | Fences are not executable NS-Spec units. Use native `ns_mermaid` cells. |
| "I'll write formal cells after the plan" | The approved spec is the contract. Formal gates belong in the design notebook before writing-plans. |
| "I know Mermaid; skip the profile registry" | NS-Mermaid is a versioned dialect. Use `notebook_ns_mermaid_spec` / `check`. |
| "Proof failed — delete the @verify to ship the design" | Counterexamples are semantic. Revise intent with the user; do not weaken the contract to go green. |
| "Markdown export is the real spec" | `.ipynb` is authoritative when both exist. |

## Cross-References

- **`references/ns-spec-notebook-authoring.md`** — Notebook MCP + ns-mermaid authoring loop (required for formal cells)
- **`docs/superpowers/specs/ns-spec-v0.2-design.ipynb`** — Normative NS-Spec v0.3 design (same-cell architecture, authoring gates, lifecycle)
- **spur-way** — beads-first invariant and the three primitives
- **beads-lifecycle** — Status state machine and label semantics for the design epic
- **writing-plans** — Next skill in the chain; converts approved spec (notebook or md) to beads-backed plan
- **solve** — Constraint substrate reached only through the `ns_mermaid` cell runner / solve MCP (never raw Z3 in the notebook)
- **brain-delegation** — How the brain dispatches workers after the plan is approved
- **plan-task-discipline** — DAG rules and task boundaries that the design must respect
