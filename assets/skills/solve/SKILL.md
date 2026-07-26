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

## Tool surface

| Tool | When | Input | Output |
|---|---|---|---|
| `solve_constraints` | **default** — B′ can express it (bool/int/enum + arithmetic) | `vars[]` + `constraints[]` (tagged JSON) | sat+model / unsat / unknown / timeout |
| `solve_smt` | **escape hatch** — BitVec, Reals, Arrays, Strings, quantifiers, theories beyond B′ | raw SMT-LIB2 (command-allowlisted) | same envelope |
| `get_solve_result` | **reload** a persisted solve (brain→worker handoff) | `solve_id` (`sol_<16hex>`) | stored artifact |

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

## Pattern catalog

The two examples above are the entry points. These are the other constraint shapes a coding agent hits — one-line each, reach for them by recognition:

| Question shape | B′ sketch | Status that matters |
|---|---|---|
| Do these config / feature-flag / RBAC rules conflict? | `bool` + `enum` vars; assert all rules; check sat vs unsat | **unsat = conflict** |
| Does this layout/geometry fit? (panels, columns, gutters) | `int_range`; `add`/`sub` + `or(eq,…)` for fixed-value options; `≤` width budget | sat → px values |
| What input reaches this branch? / prove none exists | `int` for inputs; assert the predicate (or its negation) | sat → test input; **unsat → unreachable** |
| Does this bitmask/shift/index overflow? | ⚠ needs `solve_smt` (B′ has no BitVec) — `(declare-const x (_ BitVec 32))`, assert no-wrap | sat → offending input |
| What order respects these deps? (build/CI/migration) | `int_range` position per task; `lt` for before/after | sat → valid order; **unsat → cycle** |

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

## Brain→worker handoff

The persist path for sharing a solved model across the delegation boundary:

1. Brain calls `solve_constraints` with `persist: true` → receives `solve_id` (`sol_<16hex>`).
2. Brain embeds `solve_id` + key model fields in the worker task CONTEXT.
3. Worker calls `get_solve_result({solve_id})` for authoritative reload.

**Rules:**

- Worker treats the model as **authoritative**; do not re-invent constants or re-solve.
- `.spur/solver/` is gitignored; beads remains SoT for tasks; `solve_id` is a handoff cache only.
- Quota: 256 artifacts or 64 MiB per repo root (whichever first).

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
