# `solve` Skill Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create `assets/skills/solve/SKILL.md` — a self-contained, reference-grade skill documenting the `solve` MCP tools (`solve_constraints`, `solve_smt`, `get_solve_result`) for SPUR brain and worker agents.

**Architecture:** Single-file documentation artifact assembled from the approved spec at `docs/superpowers/specs/2026-07-26-solve-skill-design.md`. The spec contains the complete content for all 11 sections verbatim; this plan sequences the transcription into SKILL.md with formatting guidance, verification, and commits. No code, no separate reference files (per the "self-contained" decision). RED/GREEN skill-testing is documented as a verification task rather than a heavy upfront subagent exercise, since the failure modes are already enumerated in the spec's research grounding.

**Tech Stack:** Markdown, YAML frontmatter. No build step. Verification is `wc -l`, manual CSO check, and cross-reference resolution.

**Spec:** [`docs/superpowers/specs/2026-07-26-solve-skill-design.md`](../specs/2026-07-26-solve-skill-design.md) — all section content below is transcribed from the spec's "Full section content" blocks unless otherwise noted.

---

## File Structure

- **Create:** `assets/skills/solve/SKILL.md` (the only file; self-contained per design decision)
- **No supporting files** — the hybrid encoding decision keeps everything inline; no `reference.md`, no examples directory.

## Conventions

- **Commit scope:** `feat(skills)` (new skill asset). Example: `feat(skills): solve S1 scaffold + discovery`.
- **Sub-id scheme:** `S<task-number>` (S for solve-skill), to avoid collision with the spur-tui `sbhf` series.
- **Line budget:** ~300–400 lines total (peer parity with `spur-analyst` 352, `code-explore` 330). Run `wc -l` after Task 5.
- **Tool names:** bare (`solve_constraints`), never `spur-mcp:solve_constraints` (in-repo peer convention).
- **Paths:** forward slashes only (Anthropic anti-pattern: avoid Windows-style paths).

---

### Task 1: Scaffold + sections 1–3 (frontmatter, HARD-GATE, why)

**Files:**
- Create: `assets/skills/solve/SKILL.md`

- [ ] **Step 1: Create the skill directory**

```bash
mkdir -p assets/skills/solve
```

- [ ] **Step 2: Write the frontmatter + title + sections 1–3**

Create `assets/skills/solve/SKILL.md` with this exact content (transcribed from spec §1–§3):

```markdown
---
name: solve
description: Use when about to write a magic number, buffer/pool/retry size, layout dimension, or config constant into code without proof it satisfies the rules; when needing to prove a code invariant, clamp, or bound is sound for all inputs; when checking whether feature-flag, RBAC, or resource-limit rules conflict; or when finding (or proving none exists) an input that triggers a branch/condition.
role: both
---

# solve — Constraint Solving for Code Constants & Invariants

The `solve` MCP tools hand a coding agent a **constraint model-finder** (Z3, subprocess) so work starts from **solved values** and **proven invariants** instead of invented constants and guessed feasibility. LLMs propose constraints; Z3 returns `sat` + a concrete model (or `unsat` / `unknown` / `timeout`).

<HARD-GATE>
Before writing a magic number, unverified constant, or layout/config value
into code — or before claiming a clamp/invariant "should be safe" — check
whether the problem is constraint-shaped. If two or more rules/relations
must hold simultaneously (a budget AND a minimum, a width AND a floor, a
flag matrix), encode them with solve_constraints and let Z3 return sat
(a concrete feasible model) or unsat (proof of impossibility) instead of
inventing a value or guessing. Reach for solve BEFORE the value lands in
code, not after as a check.
</HARD-GATE>

**Example prompts that should trigger solve:**

- "Size a worker pool to fit a 512 MiB budget with ≥4 workers"
- "Prove this hint-floor clamp prevents status-bar overflow for all widths"
- "Do these feature-flag rules ever contradict?"
- "Find an input that overflows this index, or prove none exists"
- "What replica/memory values satisfy HA≥3 and total≤4096?"

## Why: solve is your verifiable intermediate output

`solve` is the validation step in plan-validate-execute. Instead of writing a constant and hoping it satisfies the rules, solve first — it returns a **verifiable artifact** you act on before any code is written.

| status | meaning | what you do |
|---|---|---|
| `sat` + model | feasible | bake the model's values into code constants; write a test asserting the constraints |
| `unsat` | impossible / sound | report the impossibility, OR (for invariant checks) ship with confidence — unsat is a proof |
| `unknown` | solver incomplete | do NOT treat as unsat; tighten encoding or raise timeout within the 60s cap |
| `timeout` | wall clock exceeded | do NOT treat as unsat; simplify the problem or split it |

**The load-bearing rule: never collapse `unknown`/`timeout` into `unsat`.** `unsat` is a proof; `unknown`/`timeout` are "I don't know."
```

- [ ] **Step 3: Verify it parses as valid markdown + frontmatter**

Run:
```bash
head -5 assets/skills/solve/SKILL.md
```
Expected: YAML frontmatter with `name: solve`, `description:`, `role: both`, then `---`.

- [ ] **Step 4: Commit**

```bash
git add assets/skills/solve/SKILL.md
git commit -m "feat(skills): solve S1 scaffold + frontmatter + discovery frame"
```

---

### Task 2: Section 4 (tool surface) + section 5 (encoding)

**Files:**
- Modify: `assets/skills/solve/SKILL.md` (append)

- [ ] **Step 1: Append section 4 (tool surface)**

Append to `assets/skills/solve/SKILL.md`:

```markdown

## Tool surface

| Tool | When | Input | Output |
|---|---|---|---|
| `solve_constraints` | **default** — B′ can express it (bool/int/enum + arithmetic) | `vars[]` + `constraints[]` (tagged JSON) | sat+model / unsat / unknown / timeout |
| `solve_smt` | **escape hatch** — BitVec, Reals, Arrays, Strings, quantifiers, theories beyond B′ | raw SMT-LIB2 (command-allowlisted) | same envelope |
| `get_solve_result` | **reload** a persisted solve (brain→worker handoff) | `solve_id` (`sol_<16hex>`) | stored artifact |
```

- [ ] **Step 2: Append section 5 (encoding)**

Append to `assets/skills/solve/SKILL.md`:

```markdown

## Encoding (B′ typed JSON)

**Vars:**

| `type` | fields | example |
|---|---|---|
| `bool` | `name` | `{name:"use_cache",type:"bool"}` |
| `int` | `name` (prefer `int_range` when bounds known) | `{name:"x",type:"int"}` |
| `int_range` | `name`,`min`,`max` (`min≤max`) | `{name:"workers",type:"int_range",min:1,max:16}` |
| `enum` | `name`,`values[]` (non-empty, unique) | `{name:"mode",type:"enum",values:["fast","safe"]}` |

**Ops:**

| class | ops | arity |
|---|---|---|
| compare | `eq`,`ne`,`lt`,`le`,`gt`,`ge` | 2 → Bool |
| arith | `add`,`sub`,`mul` (no `div`) | add/mul ≥2; sub 2 → Int |
| bool | `and`,`or` / `not` | and/or ≥1; not 1 → Bool |

**Every ConstraintExpr node is a tagged object** — bare strings/numbers are invalid: `{kind:"var",name:"x"}`, `{kind:"int",value:48}`, `{kind:"enum_label",var:"mode",label:"fast"}`, `{kind:"op",op:"le",args:[...]}`. Full type rules (enum≠arith, bare-leaf rejection, nest/size caps) → §Anti-patterns below; exhaustive grammar in the [tool spec §ConstraintExpr](../../docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md#b-types-and-constraint-ast).

One full end-to-end encoding is shown in Example 1 below — copy and adapt.
```

- [ ] **Step 3: Verify the relative cross-reference resolves**

The spec link `../../docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md` must resolve from `assets/skills/solve/SKILL.md`. Confirm the anchor fragment `#b-types-and-constraint-ast` matches a heading in the target spec (it does — the spec's §"B′ types and constraint AST" heading slugifies to that fragment).

- [ ] **Step 4: Commit**

```bash
git add assets/skills/solve/SKILL.md
git commit -m "feat(skills): solve S2 tool surface + B′ encoding reference"
```

---

### Task 3: Section 6 (canonical pair — full worked examples)

**Files:**
- Modify: `assets/skills/solve/SKILL.md` (append)

- [ ] **Step 1: Append section 6 with both worked examples**

Append to `assets/skills/solve/SKILL.md`:

````markdown

## Canonical examples

### Example 1 — derive constants from a budget (sat → model)

Fit a worker pool in 512 MiB; ≥4 workers; batch 8–128; each worker costs `48 + 2*batch` MiB.

```json
{
  "vars": [
    {"name":"workers","type":"int_range","min":1,"max":16},
    {"name":"batch","type":"int_range","min":8,"max":128}
  ],
  "constraints": [
    {"kind":"op","op":"ge","args":[{"kind":"var","name":"workers"},{"kind":"int","value":4}]},
    {"kind":"op","op":"le","args":[
      {"kind":"op","op":"mul","args":[
        {"kind":"var","name":"workers"},
        {"kind":"op","op":"add","args":[
          {"kind":"int","value":48},
          {"kind":"op","op":"mul","args":[{"kind":"int","value":2},{"kind":"var","name":"batch"}]}
        ]}
      ]},
      {"kind":"int","value":512}
    ]}
  ],
  "timeout_ms": 30000
}
```

→ `sat`, model `{workers:4, batch:40}`. The worker bakes `const WORKERS: u32 = 4; const BATCH: usize = 40;` plus a test asserting `workers * (48 + 2*batch) <= 512`. This is a **feasibility predicate, not a golden model** — any sat assignment is valid; Z3 may return any.

### Example 2 — prove an invariant sound (unsat → proof)

A read into a fixed buffer never overflows: `pos` and `len` are both independently bounded `[0, 512]`; the unsafe condition is `pos + len > 1024`. To *prove* safety, assert the **negation** (the unsafe condition) and discharge `unsat`:

```json
{
  "vars": [
    {"name":"pos","type":"int_range","min":0,"max":512},
    {"name":"len","type":"int_range","min":0,"max":512}
  ],
  "constraints": [
    {"kind":"op","op":"gt","args":[
      {"kind":"op","op":"add","args":[{"kind":"var","name":"pos"},{"kind":"var","name":"len"}]},
      {"kind":"int","value":1024}
    ]}
  ]
}
```

→ `unsat` = proof that `pos + len > 1024` can never hold under the bounds; ship the read with confidence (and/or a test asserting the bound). If `sat`, the model is a counterexample → failing test + fix.

**This is the assert-negation pattern: to prove P, assert ¬P and discharge `unsat`.** A sat model is a counterexample; an unsat result is a proof. (Real-world instance: the `status_bar.rs` `HINT_FLOOR` clamp — the encoding asserts the overflow condition over the full layout arithmetic; `unsat` proves the clamp sound for all terminal widths.)
````

- [ ] **Step 2: Verify the JSON blocks are valid (balanced braces, tagged nodes)**

Read Example 1's JSON and confirm: every node has a `kind` tag; no bare strings/numbers; ops match the §5 table. Repeat for Example 2.

- [ ] **Step 3: Commit**

```bash
git add assets/skills/solve/SKILL.md
git commit -m "feat(skills): solve S3 canonical pair (budget/sat + invariant/unsat)"
```

---

### Task 4: Sections 7–9 (pattern catalog, escape hatch, handoff)

**Files:**
- Modify: `assets/skills/solve/SKILL.md` (append)

- [ ] **Step 1: Append section 7 (pattern catalog)**

Append to `assets/skills/solve/SKILL.md`:

```markdown

## Pattern catalog

The two examples above are the entry points. These are the other constraint shapes a coding agent hits — one-line each, reach for them by recognition:

| Question shape | B′ sketch | Status that matters |
|---|---|---|
| Do these config / feature-flag / RBAC rules conflict? | `bool` + `enum` vars; assert all rules; check sat vs unsat | **unsat = conflict** |
| Does this layout/geometry fit? (panels, columns, gutters) | `int_range`; `add`/`sub` + `or(eq,…)` for fixed-value options; `≤` width budget | sat → px values |
| What input reaches this branch? / prove none exists | `int` for inputs; assert the predicate (or its negation) | sat → test input; **unsat → unreachable** |
| Does this bitmask/shift/index overflow? | ⚠ needs `solve_smt` (B′ has no BitVec) — `(declare-const x (_ BitVec 32))`, assert no-wrap | sat → offending input |
| What order respects these deps? (build/CI/migration) | `int_range` position per task; `lt` for before/after | sat → valid order; **unsat → cycle** |
```

- [ ] **Step 2: Append section 8 (solve_smt escape hatch)**

Append to `assets/skills/solve/SKILL.md`:

````markdown

## `solve_smt` escape hatch

Escalate from `solve_constraints` when the problem needs:

- **BitVec** / machine integers / bitmask reasoning (B′ has no BitVec sort)
- **Reals** / floating point (B′ is integer-only)
- **Arrays, Strings, Datatypes, quantifiers** (B′ is QF_LIA + enums + bool)
- A specific `(set-logic …)` the B′ encoder won't emit

**Guards (non-negotiable):**

- Command allowlist: `set-logic`, `set-option` (restricted keys), `declare-const`, `declare-fun`, `assert`, `check-sat`, `get-model`, `get-value`, `push`, `pop`. Any other top-level command → entire script rejected.
- 256 KiB size cap. Fixed `z3` argv; agents never pass flags.
- Same status envelope (sat/unsat/unknown/timeout).

**Pattern:** declare consts → assert constraints → end with `(check-sat)`:

```smt
(declare-const x (_ BitVec 32))
(assert (bvult x #x00010000))
(check-sat)
```

The runner appends `(get-value (…))` for declared vars on sat. Prefer `(get-value)` over `(get-model)` when you know the var list (token-lighter).
````

- [ ] **Step 3: Append section 9 (brain→worker handoff)**

Append to `assets/skills/solve/SKILL.md`:

```markdown

## Brain→worker handoff

The persist path for sharing a solved model across the delegation boundary:

1. Brain calls `solve_constraints` with `persist: true` → receives `solve_id` (`sol_<16hex>`).
2. Brain embeds `solve_id` + key model fields in the worker task CONTEXT.
3. Worker calls `get_solve_result({solve_id})` for authoritative reload.

**Rules:**

- Worker treats the model as **authoritative**; do not re-invent constants or re-solve.
- `.spur/solver/` is gitignored; beads remains SoT for tasks; `solve_id` is a handoff cache only.
- Quota: 256 artifacts or 64 MiB per repo root (whichever first).
```

- [ ] **Step 4: Commit**

```bash
git add assets/skills/solve/SKILL.md
git commit -m "feat(skills): solve S4 pattern catalog + smt escape + handoff"
```

---

### Task 5: Sections 10–11 (anti-patterns, TL;DR)

**Files:**
- Modify: `assets/skills/solve/SKILL.md` (append)

- [ ] **Step 1: Append section 10 (anti-patterns)**

Append to `assets/skills/solve/SKILL.md`:

```markdown

## Anti-patterns

1. **Treating `unsat` as failure.** `unsat` is a *result*, not an error. For invariant checks, unsat = proof of soundness. For feasibility, unsat = "report impossibility, don't invent." Never retry-on-unsat or suppress it.
2. **Collapsing `unknown`/`timeout` into `unsat`.** They mean "I don't know." Tighten encoding, raise timeout (≤60s cap), or simplify. Do NOT conclude impossibility.
3. **Inventing a value instead of solving.** About to write `const BUFFER_SIZE: usize = ???` with ≥2 constraints on it → solve first. (Anthropic: "no voodoo constants.")
4. **Enum as arithmetic.** Enums are not Int operands. Only `eq`/`ne` vs `enum_label` (or another enum of the same values). `add`/`mul`/`lt` on enums → `invalid_params`. Encode "mode is fast" as `{kind:"enum_label",var:"mode",label:"fast"}`, never as an int index.
5. **Bare leaf nodes.** Every ConstraintExpr is a tagged object. `42` and `"workers"` are invalid → `{kind:"int",value:42}`, `{kind:"var",name:"workers"}`.
6. **No `div`.** B′ has no division. Encode ratios by cross-multiplying (`a/b = c/d` → `a*d = c*b`).
7. **Re-inventing constants in the worker.** Brain passed a `solve_id` → reload via `get_solve_result`, treat as authoritative.
8. **Direct file reads of `.spur/solver/`.** Always `get_solve_result`. Path/format is an implementation detail.
9. **`solve_smt` when B′ suffices.** Escape hatch is for theories B′ can't express. If bool/int/enum + arithmetic covers it → `solve_constraints`.
```

- [ ] **Step 2: Append section 11 (TL;DR)**

Append to `assets/skills/solve/SKILL.md`:

````markdown

## TL;DR

```
0. Recognize: about to write a magic number / invariant / config value with
   ≥2 constraints? → solve.
1. Encode: vars (bool|int|int_range|enum) + tagged ConstraintExpr
   (every node is {kind:…}).
2. solve_constraints (default); solve_smt only for BitVec/Reals/theories beyond B′.
3. Status: sat → bake model into code + test; unsat → proof/impossibility;
   unknown/timeout → NOT unsat.
4. Hand off: persist:true + solve_id → worker get_solve_result (authoritative).
5. Gotchas: enum≠arith, no div, bare leaves rejected, never unknown→unsat.
```
````

- [ ] **Step 3: Commit**

```bash
git add assets/skills/solve/SKILL.md
git commit -m "feat(skills): solve S5 anti-patterns + TL;DR"
```

---

### Task 6: Verify (CSO, length, cross-refs, lint)

**Files:**
- Verify: `assets/skills/solve/SKILL.md`

- [ ] **Step 1: Length check**

Run:
```bash
wc -l assets/skills/solve/SKILL.md
```
Expected: 280–400 lines (peer parity target). If >400, trim redundancy in the pattern catalog or escape-hatch section. If <250, the content is incomplete — re-check against the spec.

- [ ] **Step 2: CSO check (description = triggers only, no workflow)**

Read the `description:` line in the frontmatter. Verify:
- Starts with "Use when" ✓
- Third person (no "I", no "you") ✓
- No workflow summary (does NOT describe the skill's process) ✓
- Keyword-rich: magic number, buffer, pool, retry, layout, config, invariant, clamp, bound, feature-flag, RBAC, resource-limit, branch, input, prove, conflict, sound ✓

If any check fails, rewrite the description per the spec §1.

- [ ] **Step 3: Cross-reference resolution**

Confirm every internal reference resolves:
- The spec link `../../docs/superpowers/specs/2026-07-25-z3-constraint-solver-design.md#b-types-and-constraint-ast` (in §5) — run `ls` on the resolved path from the skill directory.
- The three tool names (`solve_constraints`, `solve_smt`, `get_solve_result`) appear in §4 and are used consistently in §6–§10.

- [ ] **Step 4: Placeholder / lint scan**

Run:
```bash
rg -n "TBD|TODO|FIXME|XXX|<placeholder>|fill in|TBC" assets/skills/solve/SKILL.md
```
Expected: no matches.

Also verify no Windows-style backslash paths:
```bash
rg -n '\\\\' assets/skills/solve/SKILL.md
```
Expected: no matches.

- [ ] **Step 5: Section-count check**

Run:
```bash
rg -c "^## " assets/skills/solve/SKILL.md
```
Expected: 6 top-level `##` sections (Why, Tool surface, Encoding, Canonical examples, Pattern catalog, `solve_smt` escape hatch, Brain→worker handoff, Anti-patterns, TL;DR) — count should be 9. (The HARD-GATE and example prompts are under the `#` title, not a `##`.) Adjust if the count diverges from the spec's 11-section spine (some sections share a `##` heading).

- [ ] **Step 6: Commit if any fixes were made**

If Steps 1–5 required edits:
```bash
git add assets/skills/solve/SKILL.md
git commit -m "fix(skills): solve S6 verification fixes (CSO/length/xrefs)"
```

---

### Task 7: Baseline testing note (RED/GREEN documentation)

**Files:**
- Modify: `assets/skills/solve/SKILL.md` (no change — this task documents the testing approach per `writing-skills`, run separately)

- [ ] **Step 1: Document the expected RED baseline (from the spec's research)**

Record (in a commit message or a scratch note, not in SKILL.md itself — the skill is reference content, not a test log) the five baseline failure modes the spec enumerated, which an agent WITHOUT this skill is expected to exhibit:

1. *"Size a thread pool for this memory budget"* → invents a number instead of solving.
2. *"Prove this index clamp is safe for all inputs"* → writes a fuzz test / guesses, instead of encode + discharge unsat.
3. *"Do these feature flags conflict?"* → reasons manually instead of solving.
4. Receives `unsat` → treats as failure instead of as proof.
5. Receives `unknown` → collapses to unsat instead of reporting honestly.

- [ ] **Step 2: (Deferred) Run RED then GREEN when a subagent harness is available**

Per `writing-skills` Iron Law ("no skill without a failing test first"), the full RED→GREEN→REFACTOR cycle should be run with fresh subagents before considering the skill production-ready. This is deferred beyond this plan because (a) the failure modes are already documented from the research phase, (b) the user requested inline implementation, and (c) the skill is Reference-type (writing-skills: "Test with retrieval/application scenarios") rather than a discipline skill needing pressure-testing. Track this as a follow-up: run the five scenarios with a fresh agent pre-skill and post-skill, verify ≥4/5 compliance.

- [ ] **Step 3: Commit a tracking note**

```bash
git commit --allow-empty -m "docs(skills): solve S7 baseline-testing approach documented (RED/GREEN deferred)"
```

---

## Self-Review

**1. Spec coverage:** Every section of the spec's 11-section spine maps to a task:
- §1 Frontmatter → Task 1 Step 2 ✓
- §2 HARD-GATE + prompts → Task 1 Step 2 ✓
- §3 Why → Task 1 Step 2 ✓
- §4 Tool surface → Task 2 Step 1 ✓
- §5 Encoding → Task 2 Step 2 ✓
- §6 Canonical pair → Task 3 Step 1 ✓
- §7 Pattern catalog → Task 4 Step 1 ✓
- §8 solve_smt escape → Task 4 Step 2 ✓
- §9 Handoff → Task 4 Step 3 ✓
- §10 Anti-patterns → Task 5 Step 1 ✓
- §11 TL;DR → Task 5 Step 2 ✓
- Testing plan → Task 7 ✓
- Success criteria → Task 6 (verification) ✓

No spec section is unimplemented.

**2. Placeholder scan:** No TBD/TODO/FIXME in the plan. Every step contains the actual markdown content to write (transcribed from the spec). JSON examples are complete. Commands are exact with expected output.

**3. Consistency:** Tool names (`solve_constraints`, `solve_smt`, `get_solve_result`) used consistently across tasks. Sub-id scheme `S1`–`S7` consistent. Commit scope `feat(skills)` consistent. The assert-negation example in Task 3 matches the spec's corrected §6 Example 2 (buffer overflow, not the withdrawn status_bar sketch).

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-26-solve-skill.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
