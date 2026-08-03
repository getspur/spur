# Spec Document Reviewer Prompt Template

Use this template when dispatching a spec document reviewer subagent.

**Purpose:** Verify the spec is complete, consistent, and ready for implementation planning.

**Dispatch after:** Spec document is written to `docs/superpowers/specs/` (prefer `.ipynb` design notebooks).

```
Task tool (general-purpose):
  description: "Review spec document"
  prompt: |
    You are a spec document reviewer. Verify this spec is complete and ready for planning.

    **Spec to review:** [SPEC_FILE_PATH]
    **Artifact kind:** notebook (.ipynb) | markdown (.md)

    ## What to Check

    | Category | What to Look For |
    |----------|------------------|
    | Completeness | TODOs, placeholders, "TBD", incomplete sections |
    | Consistency | Internal contradictions, conflicting requirements |
    | Clarity | Requirements ambiguous enough to cause someone to build the wrong thing |
    | Scope | Focused enough for a single plan — not covering multiple independent subsystems |
    | YAGNI | Unrequested features, over-engineering |

    ### Extra checks when the spec is a notebook (.ipynb)

    | Category | What to Look For |
    |----------|------------------|
    | Authority | Design lives in the notebook; no claim that a generated .md is authoritative |
    | Formal cells | Decision partitions, lifecycles, release/eligibility gates use native `ns_mermaid` code cells (`metadata.spur.code_type` / `code_type: ns_mermaid`), not Markdown mermaid fences |
    | Same-cell contract | Each formal unit is one annotated Mermaid source (no second formal source: Python/Z3, persistent AST, duplicate constraint blocks) |
    | Proof freshness | Formal cells have execution outputs (`application/vnd.spur.ns-proof+json` or equivalent); mandatory obligations match expected statuses; deliberate negative fixtures document expected failure |
    | Profile / evidence | Intro or beads notes pin profile/dialect; key source/IR/report hashes or solve IDs are recorded for handoff |
    | Handoff | Clear list of `@spec` ids / cell ids implementation must respect |

    Normative reference for notebook formal specs:
    `docs/superpowers/specs/ns-spec-v0.2-design.ipynb` (NS-Spec v0.3).
    Practical template: `docs/superpowers/specs/2026-07-31-skills-catalog-mcp-design.ipynb`.

    ## Calibration

    **Only flag issues that would cause real problems during implementation planning.**
    A missing section, a contradiction, a formal claim without an executable gate,
    or a requirement so ambiguous it could be interpreted two different ways —
    those are issues. Minor wording improvements, stylistic preferences, and
    "sections less detailed than others" are not.

    Approve unless there are serious gaps that would lead to a flawed plan.

    ## Output Format

    ## Spec Review

    **Status:** Approved | Issues Found

    **Artifact:** notebook | markdown

    **Formal cells (if notebook):** [list @spec ids or "none — trivial prose only"]

    **Issues (if any):**
    - [Section / cell X]: [specific issue] - [why it matters for planning]

    **Recommendations (advisory, do not block approval):**
    - [suggestions for improvement]
```

**Reviewer returns:** Status, Artifact kind, Formal cells, Issues (if any), Recommendations
