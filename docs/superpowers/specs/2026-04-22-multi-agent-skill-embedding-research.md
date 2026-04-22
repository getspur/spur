# Multi-Agent Skill Embedding: Research & Solution Design

**Date:** 2026-04-22  
**Status:** Research complete, recommendations pending implementation  
**Scope:** How SPUR skills embed into Claude Code, Kimi, Kiro, Gemini, Codex, Cursor, and OpenCode agents

---

## Executive Summary

SPUR's skill installer renders bundled skills into **7 adapter formats** across agent-specific directories (`.claude/skills/`, `.kiro/skills/`, `.cursor/rules/`, etc.). However, our research reveals **three critical architectural gaps**:

1. **Kimi is missing from the adapter matrix** — Kimi CLI is a supported worker agent but has no dedicated adapter. It accidentally picks up `.claude/skills/` fallbacks, which is unreliable.
2. **Context window blindness** — SPUR renders all 16 skills to all 7 adapters. Kimi and Codex load skill descriptions into every prompt. At ~500 tokens per skill, that's **8,000 tokens of overhead** before any user request.
3. **Role mismatch** — Brain skills (`spur-way`, `brain-review-gate`) are rendered into worker agent directories. Workers don't need brain-specific guidance; it confuses them.

This document applies **MCTS multi-branch exploration** + **Six Thinking Hats** to evaluate solutions.

---

## Phase 1: Agent Skill System Audit (White Hat — Facts)

### 1.1 Kimi Code CLI

| Property | Value |
|---|---|
| **Skill path** | `.kimi/skills/` (project), `~/.kimi/skills/` (user) |
| **Fallback paths** | `.claude/skills/`, `.codex/skills/` (reads if `.kimi/` absent) |
| **Discovery** | Scans on startup; loads `name` + `description` from frontmatter into system prompt |
| **Loading model** | **Lazy** — AI decides whether to read full `SKILL.md` body based on description match |
| **Explicit trigger** | `/skill:<name>` slash command forces load |
| **Flow skills** | Supports Mermaid/D2 diagrams with `type: flow` frontmatter; auto-executes multi-step workflows |
| **Context philosophy** | "The context window is a public good. Skills share the context window with everything else." |
| **Token sensitivity** | **HIGH** — frontmatter-only by default; body loaded on demand |

**Key insight:** Kimi's lazy loading is a *feature* we should leverage, not fight. A well-written `description` field is the gatekeeper.

### 1.2 Claude Code

| Property | Value |
|---|---|
| **Skill path** | `.claude/skills/` |
| **Discovery** | Scans on startup; loads all skills |
| **Loading model** | **Always-active** — full skill body injected when `activation: always` |
| **Frontmatter** | `name`, `description`, `role` (brain/worker), `activation` (always/conditional) |
| **Context philosophy** | Skills are session-scoped context; larger context window (~200k) |
| **Token sensitivity** | **MEDIUM** — always loads but has headroom |

### 1.3 Codex (zed-industries/codex-acp)

| Property | Value |
|---|---|
| **Skill path** | `.codex/skills/` |
| **Discovery** | Scans on startup |
| **Loading model** | **Always-active** — no lazy loading observed |
| **Frontmatter** | None in rendered output (SPUR strips it; marker + body only) |
| **Context philosophy** | Tool-calling agent; skills guide tool selection |
| **Token sensitivity** | **MEDIUM** — smaller context than Claude |

### 1.4 Gemini (gemini-cli --acp)

| Property | Value |
|---|---|
| **Skill path** | `.gemini/skills/` |
| **Discovery** | Assumed scan-on-startup (ACP wrapper) |
| **Loading model** | **Unknown/Assumed always-active** (minimal docs) |
| **Frontmatter** | Rendered with `name` + `description` |

### 1.5 Kiro

| Property | Value |
|---|---|
| **Skill path** | `.kiro/skills/` |
| **Steering** | Additional `.kiro/steering/` directory for always-on guidance |
| **Discovery** | Scans on startup |
| **Loading model** | **Always-active** via steering pointer + skill body |
| **Frontmatter** | Rendered with `name` + `description` |
| **Special** | SPUR renders a `spurpower-pointer.md` steering file |

### 1.6 Cursor

| Property | Value |
|---|---|
| **Skill path** | `.cursor/rules/*.mdc` |
| **Discovery** | Scans `.cursor/rules/` on project open |
| **Loading model** | **Conditional** — `alwaysApply: true` means always; `globs: ["*.rs"]` means file-pattern matched |
| **Frontmatter** | `description`, `alwaysApply`, `globs` |
| **Format** | `.mdc` (not `.md`); different frontmatter schema |
| **Token sensitivity** | **LOW-MEDIUM** — rules are concise by convention |

### 1.7 OpenCode

| Property | Value |
|---|---|
| **Skill path** | `.opencode/skills/` |
| **Discovery** | Assumed scan-on-startup |
| **Loading model** | **Assumed always-active** (minimal docs) |
| **Frontmatter** | Rendered with `name` + `description` |

### 1.8 SPUR Hermetic (`.spur/skills/`)

| Property | Value |
|---|---|
| **Purpose** | Override directory; user edits take precedence over bundled defaults |
| **Loading model** | Read by SPUR core at runtime; injected into brain prompts |
| **Frontmatter** | Stripped before injection |

---

## Phase 2: MCTS Branch Exploration

**Root node:** How should SPUR embed skills across heterogeneous agent skill systems?

### Branch A: Uniform Render (Status Quo)
Render all skills identically to all adapters. One body, multiple file paths.

**White Hat:** Currently implemented. 7 adapters, same content.
**Red Hat:** Simple, predictable. But wasteful.
**Black Hat:** Kimi loads all descriptions; Codex loads all bodies; workers get brain guidance they shouldn't. Context window bloat.
**Yellow Hat:** Low maintenance; one source of truth.
**Green Hat:** Could add agent-specific frontmatter but keep body uniform.
**Score:** 4/10

### Branch B: Role-Gated Render
Tag each skill with `role: brain | worker | both`. Only render brain skills to brain agent adapters, worker skills to worker agent adapters.

**White Hat:** Requires skill metadata + adapter classification. Some agents are "both" (Claude, Kiro can be brain or worker).
**Red Hat:** Clean separation of concerns. Workers don't see brain-specific invariants.
**Black Hat:** Agents that can be both roles need both skill sets. Where do we render `brain-delegation` for a Claude Code brain session vs worker session?
**Yellow Hat:** Solves the confusion problem. Workers won't try to operate review gates.
**Green Hat:** Add `role` field to `SkillPayload`; filter in `Adapter::render()`.
**Score:** 7/10

### Branch C: Adapter-Aware Compression
Render full skills to Claude (large context), compressed summaries to Kimi (lazy loading), rule-style condensations to Cursor.

**White Hat:** Requires per-adapter rendering of *content*, not just paths.
**Red Hat:** Optimal token usage per agent. Kimi descriptions become action triggers.
**Black Hat:** High maintenance; N×M complexity (N skills × M adapters). Risk of drift between versions.
**Yellow Hat:** Maximum efficiency. Kimi never loads unused skill bodies.
**Green Hat:** Generate summaries automatically from full body via frontmatter `description` expansion.
**Score:** 5/10 — too complex, maintenance burden outweighs gains.

### Branch D: Lazy-First Architecture (Kimi-Optimized)
Restructure all skills for lazy loading: frontmatter `description` is the *entire* behavioral contract; body is reference material. All agents benefit.

**White Hat:** Requires rewriting all 16 skills with terse, decision-tree style descriptions.
**Red Hat:** Elegant. Aligns with Kimi's design philosophy and helps all agents.
**Black Hat:** Massive migration cost. Testing burden for all existing skills.
**Yellow Hat:** Future-proof. As more agents adopt lazy loading, SPUR is ready.
**Green Hat:** Add `tldr:` frontmatter field — one-paragraph behavioral summary injected always; body loaded on demand.
**Score:** 8/10 — high initial cost, but correct long-term architecture.

### Branch E: Dynamic Injection (No Files)
Instead of rendering files to disk, SPUR injects skills dynamically via MCP tool calls or ACP messages at session start.

**White Hat:** No file system coupling. Skills travel with the session.
**Red Hat:** Revolutionary. Eliminates the "skills rendered but agent doesn't read them" problem.
**Black Hat:** Requires every agent to support dynamic system prompt injection. Most don't. Breaks agent-native skill UX.
**Yellow Hat:** Complete control over what each session receives.
**Green Hat:** Hybrid — render files for agent-native UX, inject critical invariants via ACP `system` message.
**Score:** 3/10 — incompatible with most agents' architectures.

---

## Phase 3: Six Thinking Hats Deep Dive on Branch D (Winner)

### White Hat — Facts & Data

- Kimi loads `name` + `description` into system prompt for *every* skill it discovers.
- Kimi loads the full `SKILL.md` body only when the AI decides it's relevant.
- Claude loads full body when `activation: always`.
- Current SPUR skill descriptions are generic: "Core philosophy skill. Establishes the Spur Way invariants..."
- A Kimi-style description should be a *behavioral contract*: "When user @-mentions a worker, honor their choice unless avoid_for matches. Validate via list_available_workers first."

### Red Hat — Feelings & Intuition

The current system feels wrong because:
- We're dumping a textbook on every agent and hoping they read the right chapter.
- Workers are getting instructions for jobs they'll never do.
- Kimi's "public good" warning resonates — we're being rude to the context window.

### Black Hat — Cautions & Risks

1. **Migration risk:** Rewriting 16 skills is error-prone. Need automated validation.
2. **Description quality risk:** A bad description means the skill is never loaded when needed.
3. **Backward compatibility:** Existing `.claude/skills/` users may have hand-edits preserved by markers. Migration must respect markers.
4. **Testing gap:** No integration test verifies that Kimi actually loads SPUR skills from `.claude/skills/` fallback.

### Yellow Hat — Benefits & Values

1. **Immediate token savings:** Kimi sessions drop from ~8k skill tokens to ~1k (descriptions only).
2. **Better worker alignment:** Workers only load skills relevant to their task.
3. **Cross-agent consistency:** A well-written description works as a behavioral contract for ALL agents.
4. **Future-proof:** As agents get smarter about lazy loading, SPUR skills become more effective.

### Green Hat — Creativity & Alternatives

1. **Auto-generated TL;DR:** Use a small model to summarize each skill body into a one-paragraph behavioral description. Store in `tldr:` frontmatter field.
2. **Skill dependency graph:** `brain-delegation` depends on `spur-way`. Render dependencies as inline references so agents load chains correctly.
3. **Flow skills for plan engine:** Convert `plan-task-discipline` into a Kimi-style flow skill with Mermaid diagram. Auto-executes plan validation.
4. **Per-agent description overrides:** `description` field in frontmatter supports adapter-specific variants:
   ```yaml
   description:
     default: "Core philosophy..."
     kimi: "Honor user @mentions. Validate workers. Beads-first always."
   ```

### Blue Hat — Process & Meta

**Decision:** Implement **Branch B (Role-Gated Render)** immediately as a quick win. Begin **Branch D (Lazy-First Architecture)** as a phased migration.

**Phase 1 (This week):** Add `role` metadata to all skills; filter by role in installer.
**Phase 2 (Next sprint):** Rewrite top 5 most-loaded skills with lazy-first descriptions.
**Phase 3 (Future):** Add Kimi adapter; test fallback behavior; consider flow skills for plan engine.

---

## Phase 4: Detailed Gap Analysis

### Gap 1: Missing Kimi Adapter (CRITICAL)

**Evidence:**
- `Adapter::all()` has 7 variants: SpurHermetic, ClaudeCode, Codex, Gemini, Kiro, OpenCode, Cursor.
- **No Kimi variant.**
- Kimi CLI reads `.claude/skills/` as fallback, but this is:
  - Unreliable (user might have `.kimi/skills/` with different content)
  - Unintentional (not explicitly designed or tested)
  - Confusing (Kimi sees `role: brain` frontmatter fields it doesn't understand)

**Impact:** Kimi workers may not receive SPUR tactical skills (TDD, debugging, verification) consistently.

**Fix:** Add `Adapter::Kimi` rendering to `.kimi/skills/spurpower-*/SKILL.md` with standard frontmatter.

### Gap 2: No Role-Based Filtering (HIGH)

**Evidence:**
- `spur-way/SKILL.md` has `role: brain` in frontmatter.
- `test-driven-development/SKILL.md` has no explicit role (assumed `both` or `worker`).
- Installer renders ALL skills to ALL adapters.
- A Codex worker loading `brain-review-gate` sees "NO APPROVAL WITHOUT BEADS VERIFICATION" — instructions for a brain role it will never perform.

**Impact:**
- Token waste.
- Worker confusion (may attempt brain-only actions).
- Violation of principle of least privilege.

**Fix:** Add `role` field to `SkillPayload`; filter in `render()` based on adapter's target role.

### Gap 3: Description Quality (MEDIUM)

**Evidence:**
- `spur-way` description: "Core philosophy skill. Establishes the Spur Way invariants..."
- This is a *category label*, not a *behavioral trigger*.
- Kimi loads this into system prompt but the AI has no idea WHEN to read the body.

**Impact:** Skills are discovered but not loaded when needed.

**Fix:** Rewrite descriptions as conditional behavioral contracts:
  ```yaml
  description: >
    TRIGGER: When making ANY beads state change OR dispatching to a worker.
    ACTION: Verify beads record exists before claiming completion.
    NEVER: Claim work is done without fresh evidence.
  ```

### Gap 4: Cursor Rules Always-Apply (MEDIUM)

**Evidence:**
- Cursor adapter renders `.cursor/rules/spurpower-*.mdc` with `alwaysApply: true`.
- All 16 skills become always-active rules in Cursor.
- Cursor rules are designed for file-pattern matching (`globs: ["*.rs"]`), not universal application.

**Impact:** Cursor users get all SPUR skills as always-on rules, potentially conflicting with project-specific rules.

**Fix:** Add `cursor_apply` field to skills:
  - `always` for universal invariants (`spur-way`)
  - `globs: ["*.rs"]` for language-specific skills (`rust-idioms`)
  - `never` for brain-only skills (don't render to Cursor at all)

### Gap 5: No Integration Test for Agent Loading (HIGH)

**Evidence:**
- `run_creates_all_seven_adapter_files_for_one_skill` verifies files are written.
- No test verifies Claude actually loads `.claude/skills/spurpower-*/SKILL.md`.
- No test verifies Kimi fallback behavior.

**Impact:** Silent failures. Skills render but agents ignore them.

**Fix:** Add MockBrain integration tests that simulate agent skill discovery.

---

## Phase 5: Recommended Implementation Plan

### Measure 1: Add Kimi Adapter

```rust
// adapters.rs
Adapter::Kimi => render_agentskills(skill, &repo_root.join(".kimi/skills"), "spurpower-"),
```

Kimi's compatibility with the `agentskills` format means `render_agentskills` works as-is. No custom renderer needed.

### Measure 2: Role-Gated Install

Add `role: brain | worker | both` to `SkillPayload` frontmatter parsing:

```rust
// skills/mod.rs or frontmatter.rs
pub enum SkillRole {
    Brain,   // Injected into brain prompts; NOT rendered to worker adapters
    Worker,  // Rendered to worker adapters; NOT injected into brain
    Both,    // Both brain and worker
}
```

Update `installer.rs` to accept a `target_role` filter:

```rust
pub fn install_for_role(
    repo_root: &Path,
    adapters: &[Adapter],
    role_filter: SkillRole,
) -> Summary { ... }
```

### Measure 3: Lazy-First Description Rewrite

Rewrite skill descriptions from category labels to behavioral triggers:

| Skill | Current Description | Lazy-First Description |
|---|---|---|
| `spur-way` | "Core philosophy skill..." | "TRIGGER: Every turn. ACTION: Verify beads record before any claim. NEVER do work outside beads." |
| `worker-signals` | "Exact format and protocol..." | "TRIGGER: When scope drifts, blocked, or discovering new work. ACTION: Emit [[spur-signal v1]] with severity + reason." |
| `brain-review-gate` | "How the brain operates..." | "TRIGGER: Before approving any worker output. ACTION: Check beads status → audit trail → signals → diff → artifacts." |
| `worker-mention-routing` | "How the brain handles..." | "TRIGGER: When [UI hint] User-suggested workers appears. ACTION: Validate name via list_available_workers, then delegate." |

### Measure 4: Cursor Conditional Rules

Extend `SkillPayload` with cursor-specific metadata:

```rust
pub struct CursorRuleMeta {
    pub always_apply: bool,
    pub globs: Option<Vec<String>>, // e.g., ["*.rs"]
}
```

Update `render_cursor` to emit conditional frontmatter:

```markdown
---
description: {desc}
alwaysApply: false
globs:
  - "*.rs"
---
```

### Measure 5: Integration Test — Agent Loading Verification

Add a test that verifies each adapter format is parseable by its target agent's skill loader:

```rust
#[test]
fn kimi_format_is_valid_agentskill() {
    let skill = sample_skill();
    let rf = Adapter::Kimi.render(&skill, &tmp);
    // Parse frontmatter
    let (fm, body) = parse_frontmatter(&rf.bytes).unwrap();
    assert!(fm.name.starts_with("spurpower-"));
    assert!(!fm.description.is_empty());
    assert!(!body.is_empty());
}
```

---

## Phase 6: MCTS Aggregate Scoring

| Branch | Token Efficiency | Maintenance | Correctness | Time to Implement | **Aggregate** |
|---|---|---|---|---|---|
| A: Uniform | 2/10 | 9/10 | 4/10 | 10/10 | **4.0** |
| B: Role-Gated | 6/10 | 7/10 | 9/10 | 7/10 | **7.0** |
| C: Compression | 9/10 | 3/10 | 6/10 | 4/10 | **5.0** |
| D: Lazy-First | 9/10 | 7/10 | 9/10 | 5/10 | **8.0** |
| E: Dynamic | 8/10 | 2/10 | 5/10 | 2/10 | **3.0** |

**Winner: Branch D (Lazy-First) with Branch B (Role-Gated) as Phase 1.**

---

## Phase 7: Risk Mitigation

| Risk | Mitigation |
|---|---|
| Migration breaks existing hand-edits | Preserve SPUR-MANAGED marker logic; only update files where marker hash matches |
| Description quality regression | Add lint: description must contain "TRIGGER:" and "ACTION:" |
| Kimi adapter untested | Add unit test for `.kimi/skills/` path rendering; follow up with e2e test |
| Role misclassification | Default to `Both` for backward compat; annotate skills explicitly over time |
| Cursor users lose skills | `alwaysApply: true` remains default for `Both` skills; only `Worker` skills get `globs` |

---

## Appendix: Agent Skill Format Matrix

| Agent | Path | Format | Frontmatter | Loading | Lazy? | Token Sensitivity |
|---|---|---|---|---|---|---|
| **Claude Code** | `.claude/skills/*/SKILL.md` | Markdown + YAML | `name`, `description`, `role`, `activation` | Always | No | Medium |
| **Kimi** | `.kimi/skills/*/SKILL.md` | Markdown + YAML | `name`, `description` | Lazy | **Yes** | **High** |
| **Codex** | `.codex/skills/*/SKILL.md` | Markdown | None (stripped) | Always | No | Medium |
| **Gemini** | `.gemini/skills/*/SKILL.md` | Markdown + YAML | `name`, `description` | Always? | Unknown | Medium |
| **Kiro** | `.kiro/skills/*/SKILL.md` | Markdown + YAML | `name`, `description` | Always | No | Medium |
| **Cursor** | `.cursor/rules/*.mdc` | Markdown + YAML | `description`, `alwaysApply`, `globs` | Conditional | No | Low |
| **OpenCode** | `.opencode/skills/*/SKILL.md` | Markdown + YAML | `name`, `description` | Always? | Unknown | Medium |
| **SPUR** | `.spur/skills/*/SKILL.md` | Markdown + YAML | `name`, `description` | Runtime | N/A | N/A |

---

## Open Questions

1. Does Gemini CLI actually load `.gemini/skills/`? Need verification.
2. Does OpenCode support the same skill format as Codex? Need verification.
3. Should SPUR support Kimi's `type: flow` skills for plan engine workflows?
4. What's the token budget for skills in each agent? (Claude: ~200k total; Kimi: unknown but explicitly warned about; Codex: ~128k?)
5. Should `brain-delegation-*` adapter skills (claude-code-acp, codex, gemini, kiro) be rendered to ALL adapters, or only to their specific agent?
