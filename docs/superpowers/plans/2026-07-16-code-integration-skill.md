# Code Integration Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bundled `code-integration` skill that combines worktree `code_*` and package `external_*` evidence to trace and evaluate exact-version integration seams.

**Architecture:** Keep `code-explore` as the graph-navigation substrate and add one self-contained skill for the cross-graph seam. A focused `spur-core` test will prove that the asset is bundled and contains the non-negotiable workflow/output markers; fresh-agent scenarios will prove that the prose changes behavior rather than merely satisfying string checks.

**Tech Stack:** Markdown agent skill, SPUR bundled-skill catalog, Rust unit tests, `knowledge_context_pack_2`, worktree `code_*` MCP tools, external-package `external_*` MCP tools.

---

## File Structure

- Create `assets/skills/code-integration/SKILL.md`: the paired-seam workflow, exact-revision gate, evidence rules, output contract, quick reference, mistakes, and Serde example.
- Modify `crates/spur-core/src/skills/mod.rs`: add one focused bundled-asset contract test beside the existing `code-explore` skill tests.

Do not add supporting scripts, references, creation logs, or agent UI metadata. Repository-bundled skills are discovered from `assets/skills/<id>/SKILL.md`, and this workflow is small enough to remain self-contained.

### Task 1: RED — Baseline the Integration-Review Failure

**Files:**
- Read: `docs/superpowers/specs/2026-07-16-code-integration-skill-design.md`
- Read: `assets/skills/code-explore/SKILL.md`
- No repository files changed

- [ ] **Step 1: Run a fresh-agent baseline without the new skill**

Use a fresh agent with no forked conversation context. Do not mention `code-integration` and prohibit reading repository skill/spec/plan files so the test measures default behavior:

```text
Review whether crates/spur-core/src/delegation_types.rs::BaseTarget's manual
Deserialize implementation integrates correctly with the exact serde revision
used by this worktree. Use the available code_* and external_* MCP tools.
Give an inbound-to-outbound trace, then severity-ordered findings. Work quickly;
do not read assets/skills, .*/skills, docs/superpowers/specs, or
docs/superpowers/plans.
```

- [ ] **Step 2: Record the baseline behavior in the active task notes**

Record exact excerpts for each observed failure. Look specifically for:

- skipping `Cargo.lock` and using an unverified/latest revision;
- inspecting only the local or only the external symbol;
- treating matching names or a graph resolution as a proven cross-graph edge;
- omitting the return/error path from the trace;
- reporting concerns without paired evidence;
- returning a trace without findings, verified compatibility, or uncertainties.

Do not create a repository artifact for these notes. The purpose is to identify the minimal wording the skill must teach.

- [ ] **Step 3: Confirm RED**

Expected: the baseline exhibits at least one missing behavior from the approved design. If it already satisfies every requirement, strengthen the scenario with this pressure and rerun it without the skill:

```text
The exact revision is not indexed. Finish now using the latest indexed revision
and do not spend time on package indexing.
```

Expected: the unskilled agent either accepts revision drift or fails to explain why it must stop/index/retry.

### Task 2: RED — Add the Bundled-Skill Contract Test

**Files:**
- Modify: `crates/spur-core/src/skills/mod.rs:1213`
- Test: `crates/spur-core/src/skills/mod.rs`

- [ ] **Step 1: Add the failing test beside the `code-explore` skill tests**

Insert this test after `code_explore_skill_documents_external_package_tools`:

```rust
    #[test]
    fn code_integration_skill_documents_paired_graph_review() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("code-integration", &fake).unwrap();

        for keyword in [
            "code-explore",
            "knowledge_context_pack_2",
            "code_read_symbol",
            "code_callers",
            "code_callees",
            "external_knowledge_context",
            "external_code_search",
            "external_code_read",
            "external_code_callers",
            "external_code_callees",
            "external_index",
            "external_index_status",
            "graph://symbol/<id>",
            "pkg:<package>@<revision>::<symbol>",
            "counts_by_kind",
            "Integration trace",
            "Findings",
            "Verified compatibility",
            "Uncertainties",
        ] {
            assert!(
                body.contains(keyword),
                "code-integration must document paired graph review via `{keyword}`"
            );
        }

        let raw = all_bundled_raw().get("code-integration").unwrap();
        let parsed = frontmatter::parse_source(raw);
        let desc = parsed.description.as_deref().unwrap_or("");
        for keyword in ["integration seam", "dependency", "upstream package"] {
            assert!(
                desc.contains(keyword),
                "code-integration description must trigger on `{keyword}`"
            );
        }
    }
```

- [ ] **Step 2: Format the Rust test**

Run:

```bash
scripts/spur-cargo fmt --all
```

Expected: exit 0; only the new test may be reformatted.

- [ ] **Step 3: Run the focused test and verify it fails for the missing asset**

Run:

```bash
scripts/spur-cargo test -p spur-core code_integration_skill_documents_paired_graph_review -- --nocapture
```

Expected: FAIL in `skills::tests::code_integration_skill_documents_paired_graph_review` because `load_skill("code-integration", ...)` returns `None` before the asset exists. A compile failure or unrelated test failure is not valid RED; fix the test and rerun until it fails for the missing skill.

- [ ] **Step 4: Commit the failing test**

```bash
git add crates/spur-core/src/skills/mod.rs
git commit -m "test(spur-core): code-integration require bundled skill"
```

Expected: one commit containing only the failing contract test.

### Task 3: GREEN — Add the Minimal `code-integration` Skill

**Files:**
- Create: `assets/skills/code-integration/SKILL.md`
- Test: `crates/spur-core/src/skills/mod.rs`

- [ ] **Step 1: Create the skill with the approved paired-seam workflow**

Create `assets/skills/code-integration/SKILL.md` with exactly this initial content, adding only wording required by the Task 1 baseline failures:

```markdown
---
name: code-integration
description: Use when reviewing or explaining an integration seam between a current-worktree symbol and a dependency or upstream package symbol, especially adapters, wrappers, trait implementations, SDK calls, serialization boundaries, FFI, or version-sensitive external APIs.
---

# Code Integration — Paired Graph Review

## Overview

Evaluate the seam, not two isolated codebases. Ground the local symbol with
`code_*`, ground the exact dependency revision with `external_*`, explicitly
prove how the symbols connect, then trace and review the contract in both
directions.

**REQUIRED BACKGROUND:** Use `code-explore` for graph-first discovery,
counts-first edge inspection, selector handling, and staleness rules.

<HARD-GATE>
Resolve the dependency's exact version, tag, ref, or commit from the manifest
and lockfile before judging compatibility. If that revision is cold, run
`external_index`, poll `external_index_status`, and retry. Never silently
substitute the latest indexed revision.
</HARD-GATE>

## Paired-Seam Workflow

1. **Name the seam.** State the local boundary and the integration question.
2. **Ground local code.** Orient with `knowledge_context_pack_2` when needed,
   then select with `code_symbol_search` and read with `code_read_symbol`. Use
   `code_callers` for inbound flow and `code_callees` for outbound behavior.
   Read `counts_by_kind` first; verify suspicious common-name resolutions.
3. **Ground external code.** Use `external_knowledge_context` for a concept or
   `external_code_search` for a known symbol, always pinned to the exact
   revision. Read the selected contract with `external_code_read`. Use
   `external_code_callers` or `external_code_callees` only when upstream impact
   or implementation behavior matters, and read `counts_by_kind` first.
4. **Prove the bridge.** Record the local `graph://symbol/<id>`, external
   `pkg:<package>@<revision>::<symbol>`, revision evidence, and the call/import,
   trait, type, feature, or configuration evidence connecting them. Matching
   names are not a cross-graph edge.
5. **Trace both directions.** Follow caller → local boundary → external
   contract/behavior → local result, error, retry, or cleanup. Stay depth-first
   and bounded.
6. **Evaluate.** Check only applicable contract, schema, error, ownership,
   lifetime, async/cancellation, concurrency, configuration, feature, platform,
   performance, security, and version assumptions.

## Seam Map

Before reporting, capture this compact ledger:

| Evidence | Required content |
|---|---|
| Local | Worktree selector and current source body |
| External | Package selector, exact revision, and source body |
| Bridge | Source-level proof connecting the two symbols |
| Translation | Arguments, returns, errors, ownership, lifecycle, config |
| Unknowns | Dynamic, generated, runtime, or unresolved behavior |

Do not pass worktree selectors to `external_*` tools or package selectors to
`code_*` tools. The seam map—not a synthetic graph edge—joins the evidence.

## Review Output

### Integration trace

Give a concise ordered flow naming both selectors and the exact revision.

### Findings

List severity-ordered defects. Each finding includes local evidence, external
evidence, impact, and a concrete recommendation. Insufficiently proven concerns
belong under uncertainties, not findings.

### Verified compatibility

Briefly name important contract points checked and found aligned. Do not invent
findings when the integration is sound.

### Uncertainties

State missing index data, unresolved or suspicious edges, macro/dynamic/FFI
boundaries, generated code, runtime assumptions, and tests still needed.

## Serde Example

For a local manual `Deserialize` implementation:

1. Read the local impl as `graph://symbol/<id>` and verify its inbound/outbound
   path from source; do not trust a common-name `deserialize` edge by itself.
2. Read `Cargo.lock`; if it pins Serde 1.0.228, search and read
   `pkg:serde@1.0.228::Deserialize`, not an arbitrary latest version.
3. Map the local `D: serde::Deserializer<'de>` signature, value conversion,
   `D::Error` translation, and returned local type to the external trait
   contract, then trace how local callers handle success and failure.

## Common Mistakes

| Mistake | Correction |
|---|---|
| Review local usage only | Ground the exact upstream contract too. |
| Use latest because it is warm | Index the locked revision and retry. |
| Treat a name match as the bridge | Cite the local call/import/type evidence. |
| Expand both graphs broadly | Trace one bounded seam depth-first. |
| Trust a surprising resolved edge | Read the source body and mark uncertainty. |
| Report a plausible concern as a defect | Require paired evidence or downgrade it. |
```

- [ ] **Step 2: Validate the skill metadata and directory shape**

Run:

```bash
python3 /Users/kevintruong/.codex/skills/.system/skill-creator/scripts/quick_validate.py assets/skills/code-integration
```

Expected: validation succeeds with no frontmatter or naming errors.

- [ ] **Step 3: Run the focused test and verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-core code_integration_skill_documents_paired_graph_review -- --nocapture
```

Expected: PASS for `skills::tests::code_integration_skill_documents_paired_graph_review`.

- [ ] **Step 4: Commit the skill**

```bash
git add assets/skills/code-integration/SKILL.md
git commit -m "feat(skills): code-integration add paired graph reviews"
```

Expected: one implementation commit containing only the new skill asset.

### Task 4: REFACTOR — Forward-Test the Skill

**Files:**
- Modify if needed: `assets/skills/code-integration/SKILL.md`

- [ ] **Step 1: Run the original scenario with the new skill**

Use a fresh agent with no forked conversation context:

```text
Use $code-integration at assets/skills/code-integration to review whether
crates/spur-core/src/delegation_types.rs::BaseTarget's manual Deserialize
implementation integrates correctly with the exact serde revision used by this
worktree. Use the available code_* and external_* MCP tools. Give an
inbound-to-outbound trace, then severity-ordered findings. Do not read the
design spec or implementation plan.
```

Expected behavior:

- reads the exact Serde revision from `Cargo.lock`;
- grounds a local `graph://symbol/...` selector with `code_read_symbol`;
- grounds `pkg:serde@1.0.228::Deserialize` with `external_code_read`;
- explicitly proves the bridge rather than claiming the two graphs connect;
- traces local entry through return/error handling;
- produces Integration trace, Findings, Verified compatibility, and Uncertainties;
- does not manufacture a defect when evidence shows compatibility.

- [ ] **Step 2: Run the exact-revision pressure scenario with the skill**

Use a fresh agent and a hypothetical raw-artifact constraint so it cannot substitute a warm revision:

```text
Use $code-integration at assets/skills/code-integration. Review a local adapter
whose lockfile evidence pins dependency `example-pkg` at revision `abc123`.
The external query reports that `abc123` is not indexed, while `def456` is warm.
The deadline is immediate and a teammate says latest is close enough. Explain
the next tool actions and what compatibility claims are permitted. Do not read
the design spec or implementation plan.
```

Expected: chooses `external_index` → `external_index_status` → retry for `abc123`; refuses to evaluate against `def456`; if indexing fails, reports incomplete external grounding and makes no compatibility claim.

- [ ] **Step 3: Close only observed gaps and rerun both scenarios**

If either scenario fails, amend the smallest relevant section of `SKILL.md`. Do not add a broad tool reference already covered by `code-explore`. Rerun both prompts until both satisfy their expected behavior.

- [ ] **Step 4: Commit refinements only if the forward test changed the skill**

```bash
git add assets/skills/code-integration/SKILL.md
git commit -m "docs(skills): code-integration harden seam evidence rules"
```

Expected: either a focused refinement commit or no commit when the initial skill passes unchanged.

### Task 5: Verify the Bundled Skill

**Files:**
- Verify: `assets/skills/code-integration/SKILL.md`
- Verify: `crates/spur-core/src/skills/mod.rs`

- [ ] **Step 1: Run skill validation again**

```bash
python3 /Users/kevintruong/.codex/skills/.system/skill-creator/scripts/quick_validate.py assets/skills/code-integration
```

Expected: success.

- [ ] **Step 2: Run the focused bundled-skill test**

```bash
scripts/spur-cargo test -p spur-core code_integration_skill_documents_paired_graph_review -- --nocapture
```

Expected: PASS with zero failures.

- [ ] **Step 3: Run the complete skill-catalog test group**

```bash
scripts/spur-cargo test -p spur-core skills::tests -- --nocapture
```

Expected: all `skills::tests` pass with zero failures.

- [ ] **Step 4: Check formatting and diff integrity**

```bash
scripts/spur-cargo fmt --all -- --check
git diff --check
git status --short
```

Expected: formatting and diff checks exit 0. `git status --short` shows no uncommitted changes to `assets/skills/code-integration/SKILL.md` or `crates/spur-core/src/skills/mod.rs`; unrelated pre-existing worktree changes remain untouched.

- [ ] **Step 5: Review the commits**

```bash
git log -3 --oneline -- assets/skills/code-integration/SKILL.md crates/spur-core/src/skills/mod.rs
```

Expected: the failing-test commit precedes the implementation commit, with an optional refinement commit after it.
