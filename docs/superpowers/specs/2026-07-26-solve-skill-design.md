# `solve` Skill — Design

**Status:** Approved (brainstorming)  
**Date:** 2026-07-26  
**Skill (proposed):** `assets/skills/solve/SKILL.md`  
**Tool surface documented:** `crates/spur-solver` MCP module (`solve_constraints`, `solve_smt`, `get_solve_result`)

**Related:**
- [2026-07-25 Z3 Constraint Solver — Design](./2026-07-25-z3-constraint-solver-design.md) — the tool this skill documents
- [2026-07-25 Z3 Constraint Solver — Plan](../plans/2026-07-25-z3-constraint-solver.md) — names this skill as Phase 6 deliverable ("`solve_smt` allowlist gate + docs/skill note")

## Purpose

Give SPUR brain and worker agents a discoverable, reference-grade skill for the `solve` MCP tools so that coding work starts from **solved values** and **proven invariants** instead of invented constants and guessed feasibility.

**Product thesis (mirrors the tool spec):** the skill teaches agents *when* a coding problem is constraint-shaped and *how* to encode it correctly in B′, so they reach for `solve_constraints` / `solve_smt` before writing a magic number, claiming a clamp is "probably safe," or asserting that rules don't conflict.

**Gap this fills:** the tool spec anticipated a "skill note" (Phase 6) but did not specify its structure. No prominent standalone Z3-for-coding-agents skill exists in the wild (GitHub searches for `z3`/`smt`/`constraint solver` + `mcp` + `skill` return empty). The space is nascent; this skill sets the pattern for the repository.

## Research grounding

Design decisions were validated against four adjacent sources rather than invented:

| Source | Role | What it validated |
|---|---|---|
| **z39** (`alejandroqh/z39`) — MCP server over subprocess Z3 for AI agents | Closest *functional* analog | Domain-encoder framing (schedule/logic/config/safety); `unsat`-as-answer hero narrative ("instead of inventing one that looks plausible"); feasibility-under-constraints as the dominant entry point. Content source, not structural model. |
| **Sequential Thinking** (`@modelcontextprotocol/server-sequential-thinking`) — official MCP reasoning peer | Closest *structural* peer | Discovery-led structure (Features → Tool → Usage → example prompts → verification); the example-*prompts*-that-trigger pattern, which bridges recognition and encoding. Validates discovery-led opening. |
| **Z3 Guide / "Programming Z3"** (microsoft.github.io/z3guide) | Canonical Z3 reference | Theory-first organization (`Logic → Theories → Strategies`). **Anti-model** for an agent skill — agents learn by problem-shape, not theory. Confirms problem-shape-first organization. |
| **Anthropic `anthropic-best-practices.md`** (local) | Authoritative skill-authoring guidance | Conciseness ("Claude is already very smart"); degrees-of-freedom split; Examples pattern; **verifiable-intermediate-outputs** (plan-validate-execute = solve's value prop); **"no voodoo constants"** (Ousterhout's law = solve's raison d'être); build-evaluations-before-docs. Strongest structural authority. |
| **In-repo peers** (`spur-analyst`, `code-explore`) | Repository convention | Discovery-led, reference-grade, ~330–352 lines; HARD-GATE → tool surface → templates/encoding → anti-patterns → TL;DR. Structural model for this skill. |

### Coding-agent use-case taxonomy (synthesized)

The skill must signal breadth without burning tokens on seven full worked examples. The taxonomy below, drawn from z39's domains, MSR's Z3 application list (verification, symbolic execution, model-based dev, network verification), the Z3 guide's difference-arithmetic lead, and the symbolic-execution/refinement-types tradition, organizes the field:

| # | Code question shape | Industry grounding | B′ fit | Outcome that matters |
|---|---|---|---|---|
| 1 | "What constants satisfy all the rules?" (derive buffer/pool/retry sizes from a budget) | z39 `config/find_valid`; MSR "model-based development" | perfect (int_range + arith) | **sat → model** baked into code |
| 2 | "Is this invariant/bound sound?" (does this clamp/index/guard ever fail?) | z39 `logic/find_counterexample`/`always_true`; MSR "program verification"; refinement types | perfect (assert negation) | **unsat → proof** (or sat counterexample) |
| 3 | "Do these config/rules conflict?" (feature gates, RBAC, resource limits) | z39 `config/validate`; MSR "network verification" | perfect (bool + enum) | sat=consistent / **unsat=conflict** |
| 4 | "Does this layout/geometry fit?" (panels, columns, gutters) | z39 `schedule` (no-overlap is same shape); Z3 guide difference arithmetic | perfect (int_range + disjunctions) | sat → concrete px values |
| 5 | "What input reaches this branch?" (edge-case / test generation) | MSR "testing, fuzzing, dynamic symbolic execution" (KLEE/SAGE/angr) | good | sat → test case / **unsat → unreachable** |
| 6 | "Does this bitmask/shift/index overflow?" (bit-level) | MSR bitvector theory; exploit analysis | **needs `solve_smt`** (no BitVec in B′) | sat → offending input |
| 7 | "What order respects these deps?" (build/CI/migration sequencing) | z39 `schedule` (lead domain) | good (int positions + before/after) | sat → ordering / **unsat → cycle** |

Use-cases #1 and #2 become the two full canonical worked examples (cover sat + unsat, the two highest-value coding patterns). #3–#7 become a one-line-each pattern catalog. #6 is the `solve_smt` escape-hatch trigger.

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Skill emphasis | **C — Integrated** (~40% reference / ~60% technique) | Discovery ("is this a constraint problem?") mirrors `code-explore`/`spur-analyst`; encoding is the failure mode unique to solve. |
| Canonical examples | **Pair**: #1 budget/sat (indexer memory) + #2 invariant/unsat (`status_bar.rs` `HINT_FLOOR`) | Industry-dominant entry point (#1, per z39/MSR/Z3 guide) + highest-value least-discovered use (#2, where solve's edge over "just try it" is sharpest). Both industry-grounded. |
| Encoding depth | **C — Hybrid** | Inline vars/ops tables (scannable) + one full ConstraintExpr encoding via example #1 (copyable) + type-rule gotchas in anti-patterns + spec link for exhaustive rules. Matches `spur-analyst`'s space allocation (templates, not BNF). |
| Document spine | **Approach 1 — Discovery-led**, with the failure-mode frame promoted to the HARD-GATE | Peer parity; Anthropic's "no voodoo constants" is their own name for solve's failure mode → discovery hook. Plus Sequential Thinking's example-prompts pattern in §2. |
| Length budget | ~300–400 lines (peer parity: `spur-analyst` 352, `code-explore` 330) | Anthropic guidance: "SKILL.md body under 500 lines." |
| `role` frontmatter | `both` | Spec: brain and workers both have tool access; peer convention. |
| Tool name style | Bare names (`solve_constraints`), not `spur-mcp:solve_constraints` | In-repo peer convention (`spur-analyst`, `code-explore` use bare names). |

## Non-goals

- **Teaching SMT theory.** Anthropic: "Claude is already very smart." No re-explanation of satisfiability, models, or what a solver is. Only add what Claude lacks: the B′ wire form, the unsat-as-proof doctrine, the SPUR handoff path.
- **Documenting the `spur-solver` crate internals** (encoder, mangling, process lifecycle, semaphore). That belongs in the tool spec and rustdoc, not the skill.
- **Full type-rule enumeration inline.** Exhaustive ConstraintExpr type rules live in the [tool spec §ConstraintExpr](./2026-07-25-z3-constraint-solver-design.md#b-types-and-constraint-ast); the skill links there and surfaces only the gotchas agents hit.
- **νZ optimization doctrine beyond a one-line mention.** Spec defers optimization; skill mirrors ("re-query with tighter bounds, do not claim optimality").
- **A TUI surface for solves.** Out of scope (tool spec non-goal).
- **Replacement for typecheckers or full program verification.** Solve is model-finding + proof-by-unsat for discrete coding questions, not Dafny/F*.

## Document spine (11 sections)

| # | Section | Purpose | Source |
|---|---|---|---|
| 1 | Frontmatter | CSO-correct description (pure triggers, no workflow) | Anthropic CSO rules |
| 2 | HARD-GATE + example prompts | Discovery checkpoint: "is this a solve problem?" | Anthropic discovery + Sequential Thinking prompts |
| 3 | Why: verifiable-intermediate-output | sat/unsat/unknown/timeout semantics; plan-validate-execute frame | Anthropic verifiable-outputs |
| 4 | Tool surface | 3-tool table | tool spec §MCP tool surface |
| 5 | Encoding (hybrid) | vars/ops tables + one full ConstraintExpr + spec link | approved decision (hybrid) |
| 6 | Canonical pair | budget/sat + invariant/unsat, full worked | approved; industry-grounded |
| 7 | Pattern catalog | use-cases #3–#7, one-line sketches | taxonomy above |
| 8 | `solve_smt` escape hatch | BitVec/Reals/quantifier trigger + guards | tool spec §solve_smt guards |
| 9 | Brain→worker handoff | persist + solve_id + get_solve_result | tool spec §persistence |
| 10 | Anti-patterns | enum≠arith, bare-leaf, unsat-as-failure, inventing | tool spec type rules + Anthropic |
| 11 | TL;DR | 5-step recipe | peer parity (`spur-analyst`) |

## Full section content (implementation reference)

### §1 — Frontmatter

```yaml
---
name: solve
description: Use when about to write a magic number, buffer/pool/retry size, layout dimension, or config constant into code without proof it satisfies the rules; when needing to prove a code invariant, clamp, or bound is sound for all inputs; when checking whether feature-flag, RBAC, or resource-limit rules conflict; or when finding (or proving none exists) an input that triggers a branch/condition.
role: both
---
```

**CSO check:** pure triggers (no workflow summary), third person, ~370 chars, keyword-rich (magic number, buffer, pool, retry, layout, config, invariant, clamp, bound, feature-flag, RBAC, resource-limit, branch, input, prove, conflict, sound). `role: both` per peer convention.

### §2 — HARD-GATE + example prompts

```
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
```

**Example prompts that should trigger solve** (recognition bridge, from Sequential Thinking):

- "Size a worker pool to fit a 512 MiB budget with ≥4 workers"
- "Prove this hint-floor clamp prevents status-bar overflow for all widths"
- "Do these feature-flag rules ever contradict?"
- "Find an input that overflows this index, or prove none exists"
- "What replica/memory values satisfy HA≥3 and total≤4096?"

### §3 — Why: verifiable-intermediate-output

Maps solve onto Anthropic's plan-validate-execute: solve IS the validation step before the execute (writing code).

| status | meaning | what you do |
|---|---|---|
| `sat` + model | feasible | bake the model's values into code constants; write a test asserting the constraints |
| `unsat` | impossible / sound | report the impossibility, OR (for invariant checks) ship with confidence — unsat is a proof |
| `unknown` | solver incomplete | do NOT treat as unsat; tighten encoding or raise timeout within the 60s cap |
| `timeout` | wall clock exceeded | do NOT treat as unsat; simplify the problem or split it |

**The load-bearing rule: never collapse `unknown`/`timeout` into `unsat`.** `unsat` is a proof; `unknown`/`timeout` are "I don't know."

### §4 — Tool surface

| Tool | When | Input | Output |
|---|---|---|---|
| `solve_constraints` | **default** — B′ can express it (bool/int/enum + arithmetic) | `vars[]` + `constraints[]` (tagged JSON) | sat+model / unsat / unknown / timeout |
| `solve_smt` | **escape hatch** — BitVec, Reals, Arrays, Strings, quantifiers, theories beyond B′ | raw SMT-LIB2 (command-allowlisted) | same envelope |
| `get_solve_result` | **reload** a persisted solve (brain→worker handoff) | `solve_id` (`sol_<16hex>`) | stored artifact |

### §5 — Encoding (hybrid)

**Vars table (inline, scannable):**

| `type` | fields | example |
|---|---|---|
| `bool` | `name` | `{name:"use_cache",type:"bool"}` |
| `int` | `name` (prefer `int_range` when bounds known) | `{name:"x",type:"int"}` |
| `int_range` | `name`,`min`,`max` (`min≤max`) | `{name:"workers",type:"int_range",min:1,max:16}` |
| `enum` | `name`,`values[]` (non-empty, unique) | `{name:"mode",type:"enum",values:["fast","safe"]}` |

**Ops quick-reference (inline, scannable):**

| class | ops | arity |
|---|---|---|
| compare | `eq`,`ne`,`lt`,`le`,`gt`,`ge` | 2 → Bool |
| arith | `add`,`sub`,`mul` (no `div`) | add/mul ≥2; sub 2 → Int |
| bool | `and`,`or` / `not` | and/or ≥1; not 1 → Bool |

**Every ConstraintExpr node is a tagged object** — bare strings/numbers are invalid (`{kind:"var",name:"x"}`, `{kind:"int",value:48}`, `{kind:"enum_label",var:"mode",label:"fast"}`, `{kind:"op",op:"le",args:[...]}`). Full type rules (enum≠arith, bare-leaf rejection, nest/size caps) → §Anti-patterns + [tool spec §ConstraintExpr](./2026-07-25-z3-constraint-solver-design.md#b-types-and-constraint-ast).

**One full encoding** shown end-to-end in example #1 (§6) — copy and adapt.

### §6 — Canonical pair (full worked)

**Example 1 — derive constants from a budget (sat → model):**

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

→ `sat`, model `{workers:4, batch:40}`. Worker bakes `const WORKERS: u32 = 4; const BATCH: usize = 40;` + a test asserting `workers * (48 + 2*batch) <= 512`. (Feasibility predicate, not a golden model — any sat assignment is valid; Z3 may return any.)

**Example 2 — prove an invariant sound (unsat → proof):**

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

**This is the assert-negation pattern: to prove P, assert ¬P and discharge `unsat`.** A sat model is a counterexample; an unsat result is a proof.

### §7 — Pattern catalog (remaining use-cases, one-line each)

| Question shape | B′ sketch | Status that matters |
|---|---|---|
| Do these config / feature-flag / RBAC rules conflict? | `bool` + `enum` vars; assert all rules; check sat vs unsat | **unsat = conflict** |
| Does this layout/geometry fit? (panels, columns, gutters) | `int_range`; `add`/`sub` + `or(eq,…)` for fixed-value options; `≤` width budget | sat → px values |
| What input reaches this branch? / prove none exists | `int` for inputs; assert the predicate (or its negation) | sat → test input; **unsat → unreachable** |
| Does this bitmask/shift/index overflow? | ⚠ needs `solve_smt` (B′ has no BitVec) — `(declare-const x (_ BitVec 32))`, assert no-wrap | sat → offending input |
| What order respects these deps? (build/CI/migration) | `int_range` position per task; `lt` for before/after | sat → valid order; **unsat → cycle** |

### §8 — `solve_smt` escape hatch

**Escalate from `solve_constraints` when** the problem needs:

- **BitVec** / machine integers / bitmask reasoning (B′ has no BitVec sort)
- **Reals** / floating point (B′ is integer-only)
- **Arrays, Strings, Datatypes, quantifiers** (B′ is QF_LIA + enums + bool)
- A specific `(set-logic …)` the B′ encoder won't emit

**Guards (per tool spec, non-negotiable):**

- Command allowlist: `set-logic`, `set-option` (restricted keys), `declare-const`, `declare-fun`, `assert`, `check-sat`, `get-model`, `get-value`, `push`, `pop`. Any other top-level command → entire script rejected.
- 256 KiB size cap. Fixed `z3` argv; agents never pass flags.
- Same status envelope (sat/unsat/unknown/timeout).

**Pattern:** declare consts → assert constraints → end with `(check-sat)`. Runner appends `(get-value (…))` for declared vars on sat. Prefer `(get-value)` over `(get-model)` when you know the var list (token-lighter).

### §9 — Brain→worker handoff

The persist path for sharing a solved model across the delegation boundary:

1. Brain calls `solve_constraints` with `persist: true` → receives `solve_id` (`sol_<16hex>`).
2. Brain embeds `solve_id` + key model fields in the worker task CONTEXT.
3. Worker calls `get_solve_result({solve_id})` for authoritative reload.

**Rules:**

- Worker treats the model as **authoritative**; do not re-invent constants or re-solve.
- `.spur/solver/` is gitignored; beads remains SoT for tasks; `solve_id` is a handoff cache only.
- Quota: 512 artifacts or 64 MiB per repo root (whichever first); oldest-first ring eviction when full.

### §10 — Anti-patterns

1. **Treating `unsat` as failure.** `unsat` is a *result*, not an error. For invariant checks, unsat = proof of soundness. For feasibility, unsat = "report impossibility, don't invent." Never retry-on-unsat or suppress it.
2. **Collapsing `unknown`/`timeout` into `unsat`.** They mean "I don't know." Tighten encoding, raise timeout (≤60s cap), or simplify. Do NOT conclude impossibility.
3. **Inventing a value instead of solving.** About to write `const BUFFER_SIZE: usize = ???` with ≥2 constraints on it → solve first. (Anthropic: "no voodoo constants.")
4. **Enum as arithmetic.** Enums are not Int operands. Only `eq`/`ne` vs `enum_label` (or another enum of the same values). `add`/`mul`/`lt` on enums → `invalid_params`. Encode "mode is fast" as `{kind:"enum_label",var:"mode",label:"fast"}`, never as an int index.
5. **Bare leaf nodes.** Every ConstraintExpr is a tagged object. `42` and `"workers"` are invalid → `{kind:"int",value:42}`, `{kind:"var",name:"workers"}`.
6. **No `div`.** B′ has no division. Encode ratios by cross-multiplying (`a/b = c/d` → `a*d = c*b`).
7. **Re-inventing constants in the worker.** Brain passed a `solve_id` → reload via `get_solve_result`, treat as authoritative.
8. **Direct file reads of `.spur/solver/`.** Always `get_solve_result`. Path/format is an implementation detail.
9. **`solve_smt` when B′ suffices.** Escape hatch is for theories B′ can't express. If bool/int/enum + arithmetic covers it → `solve_constraints`.

### §11 — TL;DR

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

## Skill's own testing plan (TDD, per `writing-skills`)

The Iron Law: **no skill without a failing test first.** RED-GREEN-REFACTOR applied to documentation.

**RED (baseline — run WITHOUT the skill, document failures verbatim):**

1. *"Size a thread pool for this memory budget"* → does the agent invent a number, or encode + solve?
2. *"Prove this index clamp is safe for all inputs"* → does the agent write a fuzz test / guess, or encode + discharge unsat?
3. *"Do these feature flags conflict?"* → manual reasoning, or solve?
4. Agent receives `unsat` → treats as failure, or as proof?
5. Agent receives `unknown` → collapses to unsat, or reports honestly?

Five scenarios cover the distinct failure modes (recognition-sat, recognition-unsat, recognition-conflict, status-misreading-unsat, status-misreading-unknown). Writing-skills floor is three; five is justified by the skill's dual failure surface (recognition + status-reading).

**GREEN:** write the skill (the 11 sections above), re-run the same scenarios, verify compliance.

**REFACTOR:** plug rationalizations surfaced in GREEN. Expected rationalizations and their counters:

- *"Too small to bother solving"* → counter: "≥2 constraints = solve, regardless of size."
- *"I'll just test it instead"* → counter: tests check what you tried; solve proves what's possible/impossible. They compose, not substitute.
- *"unsat means the tool broke"* → counter: §3 status table; unsat is a success result.
- *"The encoding is verbose, I'll simplify the JSON"* → counter: bare leaves rejected (§10 #5); the verbosity is load-bearing.

## Success criteria

1. SKILL.md written at `assets/skills/solve/SKILL.md`, ~300–400 lines, YAML frontmatter per §1.
2. All 11 sections present with the content specified above.
3. Description passes CSO check (pure triggers, third person, no workflow summary, keyword-rich).
4. Two canonical worked examples are copy-paste-runnable against the live `solve_constraints` tool (the B′ JSON is valid per the encoder).
5. Every cross-reference resolves (tool spec §ConstraintExpr, §solve_smt guards, §persistence).
6. Baseline scenarios (RED) documented; post-skill (GREEN) shows compliance on ≥4 of 5.
7. Lint clean: no `TBD`/`TODO`/placeholder, no internal contradictions, no ambiguous requirements, single-skill scope (no decomposition needed).
8. Conventions matched: bare tool names (not `spur-mcp:` prefixed), forward-slash paths, `role: both`, peer-parity structure.

## Naming and location

- **Skill directory:** `assets/skills/solve/`
- **Main file:** `assets/skills/solve/SKILL.md` (self-contained; no supporting files needed — all content fits inline per the hybrid encoding decision)
- **Spec (this document):** `docs/superpowers/specs/2026-07-26-solve-skill-design.md`
- **Implementation plan (next):** `docs/superpowers/plans/2026-07-26-solve-skill.md` (produced by `writing-plans`)

## Open questions (resolved for v1)

| Question | Resolution |
|---|---|
| Reference vs technique emphasis? | Integrated (C), ~40/60. |
| Which canonical examples? | Pair: budget/sat + invariant/unsat. Industry-grounded. |
| Encoding depth inline? | Hybrid (C): vars/ops inline, one worked encoding, gotchas in anti-patterns, spec link. |
| Document spine? | Discovery-led (Approach 1), failure-mode promoted to HARD-GATE, example-prompts in §2. |
| `role` frontmatter? | `both` (brain + workers both have access; peer convention). |
| Tool name prefixing? | Bare names (in-repo convention). |
| Skill length? | ~300–400 lines (peer parity; Anthropic <500). |
| Supporting files? | None — self-contained SKILL.md. |

## References

- [2026-07-25 Z3 Constraint Solver — Design](./2026-07-25-z3-constraint-solver-design.md) — the documented tool
- [2026-07-25 Z3 Constraint Solver — Plan](../plans/2026-07-25-z3-constraint-solver.md) — Phase 6 names this skill
- `assets/skills/spur-analyst/SKILL.md` — structural peer (DuckDB graph SQL)
- `assets/skills/code-explore/SKILL.md` — structural peer (retrieval stack)
- `assets/skills/writing-skills/SKILL.md` + `anthropic-best-practices.md` — authoring guidance
- [z39](https://github.com/alejandroqh/z39) — functional analog (MCP over subprocess Z3)
- [Sequential Thinking MCP server](https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking) — structural peer (official reasoning tool)
- [Z3 Guide](https://microsoft.github.io/z3guide/) — canonical Z3 reference (anti-model for structure)
- `crates/spur-solver/` — the implemented tool surface
