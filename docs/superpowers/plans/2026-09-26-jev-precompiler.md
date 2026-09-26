# Jev Pre-Compiler Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** Design record = beads epic `bd-1yv63` (design audits + two-phase POC evidence). The `.ipynb` design notebook was waived by user direction on 2026-09-26; the POC evidence files below are the grounding record.
**Formal @spec cells (if notebook):** none (notebook waived)
**Design epic:** `bd-1yv63`

**Goal:** Build `spur-jev`, a Jev (TypeSafe AI System One) pre-compiler stage that turns natural-language intent + fixture data into typed, validated solver requests routed through the unchanged `spur-solver` trust boundary (family compilers / `parse_and_validate` / Z3).

**Architecture:** New crate `crates/spur-jev` with five focused modules: wire types + pluggable transport, a deterministic battery generator (pure function of catalog snapshot + language-catalog version, questions embed their own evidence), weakest-link per-decision confidence gating (0.60), a deterministic compiler from typed answers to `solve_rules`/`solve_constraints` request JSON, and a diagnostic-driven repair loop. Jev output is **only ever** consumed downstream through existing validation — never trusted directly. An offline conformance harness replays recorded POC responses against solver fixtures as CI ground truth. MCP wiring is brain-side only, behind an env feature gate.

**Tech Stack:** Rust 2021, workspace `reqwest 0.12` (rustls, json), `serde`/`serde_json`, `thiserror`, `tokio`; `spur-solver` as dev-dependency for conformance; Z3 via existing `SolverService`.

**Worker routing:** all tasks → `codex` (model `gpt-5.6-sol`, effort `xhigh`) per user instruction. Every task is TDD (RED → GREEN → commit) with **pre-solve / post-solve** obligations via the solve MCP tools where constraint-shaped (tasks state when a stage is not constraint-shaped and why). Build/test always through `scripts/spur-cargo`.

**Ground-truth evidence (committed):**
- `scripts/jev_poc_probe.py`, `scripts/jev_poc_probe2.py` — the probes
- `scripts/jev_poc_results/phase1_summary.json`, `phase2_report.json` — recorded Jev answers + compiled requests + expectations
- Wire contract: `POST https://api.typesafe.ai/v1/systemone`, `{state, model, questions}` → `{model, answers, usage}`; Choice ≤ 255 options; Score ≤ 10 levels; Noul = 0–1; Choice/Score carry `confidence`, Noul does not.

**POC-derived hard rules encoded in this plan:**
1. Family mapping comes from catalog metadata (`layout.*` rules live in family `design`); never derive family from rule-id prefix.
2. Enum assertion compile rule: `eq(var, enum_label)` — a bare `enum_label` is not Bool at top level.
3. Battery questions embed their own evidence (intent sentence + pointed fields); decontextualized questions return ~0.5 nouls and stall the gate.
4. Gate on every decision's confidence (weakest link), never on routing alone; never on meta self-report nouls.
5. Pin/record the exact served model version (POC saw `jev-1.13.0`).
6. Arithmetic, structure, and expression trees are compiled in code; Jev decides only closed-set options (kind, op, label, mode, hard/soft, weight, binding).

---

### Task 1: `spur-jev` crate scaffold, wire types, transport trait, mock

**Task ID:** `jev-1`

**Files:**
- Create: `crates/spur-jev/Cargo.toml`, `crates/spur-jev/src/lib.rs`, `crates/spur-jev/src/wire.rs`, `crates/spur-jev/src/client.rs`
- Test: `crates/spur-jev/src/wire.rs` (inline `#[cfg(test)]`), `crates/spur-jev/tests/wire_roundtrip.rs`
- Modify: root `Cargo.toml` (workspace members list only)

**Depends on:** none

**Acceptance Criteria:**
- [ ] Wire types round-trip the three question kinds and three answer kinds against golden JSON fixtures extracted from `scripts/jev_poc_results/phase2_report.json`
- [ ] `MAX_CHOICE_OPTIONS = 255`, `MAX_SCORE_LEVELS = 10`, `DEFAULT_MODEL = "jev-latest"` constants; response records served `model` string
- [ ] `JevTransport` trait + `MockTransport` + `HttpJevTransport` (reqwest, rustls; `JEV_API_KEY` from env, Bearer auth; 30s timeout; never logs the key)
- [ ] `scripts/spur-cargo test -p spur-jev` green; `scripts/spur-cargo clippy -p spur-jev -- -D warnings` clean
- [ ] Redacted error type (`thiserror`): transport, HTTP status (+body prefix), decode

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: files listed above only
- OUT of scope: any battery/gating/compile logic, any `spur-solver` dependency, MCP wiring
- Touching OUT-of-scope files → emit `scope_drift` immediately

**Solve obligations:** not constraint-shaped (pure wire I/O types; no constants to derive) — golden-fixture round-trip tests stand in. State this in the task audit comment.

**Implementation (TDD):**
- [ ] Step 1 — failing round-trip test using a recorded answer set:

```rust
// tests/wire_roundtrip.rs
use spur_jev::wire::{JevResponse, Answer};

#[test]
fn recorded_choice_answer_roundtrips() {
    let raw = serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {
            "route_rule": {
                "type": "choice", "choice": "layout.containment",
                "probabilities": {"layout.containment": 1.0, "layout.non_overlap": 0.0},
                "confidence": 1.0
            },
            "data_complete": {"type": "noul", "noul": 0.95}
        },
        "usage": {"input_tokens": 829, "output_tokens": 136}
    });
    let parsed: JevResponse = serde_json::from_value(raw.clone()).expect("decode");
    assert_eq!(parsed.model, "jev-1.13.0");
    match &parsed.answers["route_rule"] {
        Answer::Choice { choice, confidence, .. } => {
            assert_eq!(choice, "layout.containment");
            assert!((*confidence - 1.0).abs() < 1e-9);
        }
        other => panic!("expected choice answer, got {other:?}"),
    }
    assert_eq!(serde_json::to_value(&parsed).expect("encode"), raw);
}
```

- [ ] Step 2 — run `scripts/spur-cargo test -p spur-jev` → FAIL (crate absent)
- [ ] Step 3 — minimal `wire.rs` with internally-tagged serde enums (`#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]`, mirroring `spur-solver/src/types.rs` style), `client.rs` with the transport trait + mock + reqwest impl
- [ ] Step 4 — tests PASS; add one more RED test: `MockTransport` returns a canned response for a given request (assert request serializes with `model` and question ids preserved)
- [ ] Step 5 — commit `feat(spur-jev): wire types, transport trait, mock transport`

---

### Task 2: Deterministic battery generator

**Task ID:** `jev-2`

**Files:**
- Create: `crates/spur-jev/src/battery.rs`, `crates/spur-jev/src/snapshot.rs`
- Test: `crates/spur-jev/tests/battery.rs`

**Depends on:** `jev-1`

**Acceptance Criteria:**
- [ ] `CatalogSnapshot` built from `spur-solver` rule-manifest registry data (rule id, family id, summary) — family comes from snapshot metadata, never rule-id prefix
- [ ] `generate_routing_battery(intent, data, snapshot, language_version) -> BTreeMap<String, Question>` is a pure function; same inputs ⇒ byte-identical serde output (test asserts this across two calls + after re-insert iteration order shuffles)
- [ ] Every generated question embeds its own evidence: `instructions` is a structured object carrying the intent sentence / pointed data fields (POC rule 3)
- [ ] Route-choice options = all snapshot rule cards; panics-free guard asserts `options.len() <= MAX_CHOICE_OPTIONS`
- [ ] Battery includes `solve_mode` choice and `data_complete` noul with criteria
- [ ] `scripts/spur-cargo test -p spur-jev` green; clippy clean

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: battery/snapshot modules + tests
- OUT of scope: transport changes, gating, compiling, solver deps beyond manifest types

**Solve obligations (pre-then-post):**
- PRE: before implementing, call `solve_constraints` (persist: true) encoding the cardinality invariant: `rules_total` int_range 0..=1000, hard `rules_total <= 255`; also `battery_questions` int_range 1..=64 with `battery_questions <= 32`. Expect `sat`. Record `solve_id`.
- POST: after implementing, re-run the same solve with `rules_total` bound set to the actual snapshot rule count discovered from the registry (via `solve_rule_spec` summary) and confirm `sat`; record the second `solve_id` and both IDs in a `[[spur-audit v1]]` comment (`kind: solve-evidence`).

**Implementation (TDD):**
- [ ] Step 1 — failing determinism + evidence-embedding test:

```rust
// tests/battery.rs
use std::collections::BTreeMap;
use spur_jev::battery::{CatalogSnapshot, RuleCard, generate_routing_battery};

fn snapshot() -> CatalogSnapshot {
    CatalogSnapshot {
        language_version: 1,
        rules: vec![
            RuleCard { rule_id: "layout.containment".into(), family: "design".into(),
                       summary: "Keep one axis-aligned rectangle inside another.".into() },
            RuleCard { rule_id: "scheduling.cumulative_capacity".into(), family: "scheduling".into(),
                       summary: "Keep concurrent demand within capacity.".into() },
        ],
    }
}

#[test]
fn battery_is_deterministic_and_embeds_evidence() {
    let a = generate_routing_battery("check the icon stays inside the panel", &serde_json::json!({}), &snapshot());
    let b = generate_routing_battery("check the icon stays inside the panel", &serde_json::json!({}), &snapshot());
    assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&b).unwrap());
    let route = &a["route_rule"];
    let instr = serde_json::to_value(&route_instructions(route)).unwrap();
    assert!(instr.to_string().contains("check the icon stays inside the panel"));
    assert!(a.contains_key("solve_mode") && a.contains_key("data_complete"));
}
```

- [ ] Step 2 — RED; Step 3 — implement pure generator; Step 4 — GREEN + add RED test that a snapshot with >255 rules returns a typed `BatteryError::TooManyOptions` instead of building an invalid question; Step 5 — commit `feat(spur-jev): deterministic routing battery generator`

---

### Task 3: Weakest-link gate + deterministic compiler

**Task ID:** `jev-3`

**Files:**
- Create: `crates/spur-jev/src/gate.rs`, `crates/spur-jev/src/compile.rs`, `crates/spur-jev/src/provenance.rs`
- Test: `crates/spur-jev/tests/gate.rs`, `crates/spur-jev/tests/compile.rs`

**Depends on:** `jev-2`

**Acceptance Criteria:**
- [ ] `GATE_THRESHOLD: f64 = 0.60`; `DecisionReview::weakest()` = min over every choice `confidence` and every gating-relevant `noul`; `Gate::Open | Gate::Blocked(AmbiguityReport)` — report lists per-decision values and the weakest link
- [ ] POC replays as tests: recorded case-2 predecessor (~0.05/0.21) and case-5 vague intent (weakest 0.32) ⇒ `Blocked`; recorded B/C/D answers ⇒ `Open`
- [ ] `compile_family` emits `solve_rules` request JSON (family from snapshot metadata; scene vs facts per family; `unknowns` passthrough) and `compile_bprime` emits `solve_constraints` request JSON; enum assertions compile as `eq(var, enum_label)` (POC rule 2); hard/soft/weight/objective passthrough
- [ ] Compiled outputs are `serde_json::Value`; downstream validation stays in `spur-solver` (purity invariant — `spur-jev` never calls Z3)
- [ ] `JevProvenance` serializes model version, per-question answers, confidences, weakest, gate — no API key material
- [ ] Phase-2 recorded cases (B1–B6, C1–C2, D1–D3 from `scripts/jev_poc_results/phase2_report.json`) all compile to requests whose JSON deep-equals the recorded `compiled` fields

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: gate/compile/provenance modules + tests
- OUT of scope: transport, battery generation, MCP wiring, any `spur-solver` source changes

**Solve obligations (pre-then-post):**
- PRE: call `solve_constraints` (persist: true) proving the gate margin over the observed POC envelope: Real vars `c ∈ [0.83, 1.0]` (correct), `w ∈ [0.05, 0.45]` (wrong/uncertain), `g = 0.60`; hard constraints `w < g`, `g <= c`. Expect `sat` (0.60 strictly separates the two clusters). Record `solve_id`.
- POST: after implementing, re-run the identical solve and additionally solve the counterexample query `not(w < 0.60 and 0.60 <= c)` under the same bounds → expect `unsat` (no overlap). Record both `solve_id`s in a `[[spur-audit v1]]` comment.

**Implementation (TDD):**
- [ ] Step 1 — failing gate test from recorded evidence:

```rust
// tests/gate.rs
use spur_jev::gate::{DecisionReview, GATE_THRESHOLD};

#[test]
fn weakest_link_blocks_recorded_vague_case() {
    let review = DecisionReview::from_confidences(&[
        ("route_rule", 0.98), ("solve_mode", 0.35), ("data_complete", 0.32),
    ]);
    assert_eq!(review.weakest, 0.32);
    assert!(matches!(review.gate, spur_jev::gate::Gate::Blocked(_)));
    assert_eq!(GATE_THRESHOLD, 0.60);
}
```

- [ ] Step 2 — RED; Step 3 — implement; Step 4 — GREEN, then RED test: `compile_bprime` with a required-label decision emits `{"kind":"op","op":"eq","args":[{"kind":"var","name":"tier"},{"kind":"enum_label","var":"tier","label":"large"}]}` (never a bare `enum_label` at top level); GREEN; Step 5 — commit `feat(spur-jev): weakest-link gate and deterministic request compiler`

---

### Task 4: Offline conformance harness (CI ground truth)

**Task ID:** `jev-4`

**Files:**
- Create: `crates/spur-jev/tests/conformance.rs`, `crates/spur-jev/tests/fixtures/` (recorded-response fixtures distilled from `scripts/jev_poc_results/`)
- Modify: `crates/spur-jev/Cargo.toml` (dev-dependency `spur-solver`)

**Depends on:** `jev-3`

**Acceptance Criteria:**
- [ ] Harness replays each recorded POC case: recorded answers → gate → compile → `spur_solver::constraint_spec::parse_and_validate` (B′ cases) or family `prepare` (rule cases) → `SolverService` solve → assert outcome/diagnostic equals fixture expectation (B1 pass, B2 fail+`design.overlap`, B3–B6 pass, C1 solution, C2 pass, D1 sat `x=4 tier=large`, D2 optimum 4 complete, D3 sat soft satisfied)
- [ ] Skips automatically (with `#[ignore]`-style env guard) when no local Z3 — does not fail CI on machines without the binary
- [ ] One persisted end-to-end solve (`persist: true` via service API or MCP handoff note) recorded in the task audit with its `solve_id`
- [ ] `scripts/spur-cargo test -p spur-jev` green on this machine

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: conformance test + fixtures + Cargo dev-deps
- OUT of scope: `spur-solver` internals, live network calls (offline only), MCP wiring

**Solve obligations:** PRE not constraint-shaped (matrix enumeration, not a derivation — state why in audit). POST: the harness itself executes the post-solve pass; additionally run one `solve_rules` verify per family represented (design, scheduling, resource, data_integrity, workflow) with `persist: true` and list the five `solve_id`s in the audit comment.

**Implementation (TDD):**
- [ ] Step 1 — failing harness skeleton asserting B2's recorded diagnostic:

```rust
// tests/conformance.rs
use spur_jev::compile::compile_family;

#[test]
fn b2_non_overlap_violation_fails_with_overlap_diagnostic() {
    let case = load_recorded_case("B2_non_overlap_violation");
    let request = compile_family(&case.answers, &case.snapshot, &case.data).expect("compile");
    // family path: exercise spur-solver execute::prepare + SolverService when Z3 present
    let outcome = solve_via_spur_solver(&request);
    assert_eq!(outcome.diagnostic.as_deref(), Some("design.overlap"));
}
```

- [ ] Step 2 — RED; Step 3 — implement fixture loader + solve helper; Step 4 — GREEN for all recorded cases; Step 5 — commit `test(spur-jev): offline conformance harness over recorded POC evidence`

---

### Task 5: Diagnostic repair loop

**Task ID:** `jev-5`

**Files:**
- Create: `crates/spur-jev/src/repair.rs`
- Test: `crates/spur-jev/tests/repair.rs`

**Depends on:** `jev-3`

**Acceptance Criteria:**
- [ ] `build_repair_battery(diagnostic, rejected_expr, candidates) -> Question map` embeds the diagnostic (`code`, `path`, `message`, `found`) and each candidate repair as Choice options with literal descriptions
- [ ] `apply_repair` maps the chosen candidate to a compile-rule action; the recorded G-suite case (diagnostic `top_level_not_boolean` at `constraints[1].expr`, candidates `eq_wrap|drop|bool_var|label_only`, recorded answer `eq_wrap` @ 1.0) must recompile to the `eq(var, enum_label)` shape and pass `parse_and_validate`
- [ ] Gate: repair decisions are weakest-link gated like any other decision; blocked repair ⇒ `AmbiguityReport`, no silent fallback
- [ ] Post-solve: the repaired request solves `sat` with model `x=4, tier=large` (recorded expectation)

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: repair module + tests
- OUT of scope: transport, battery, gate internals changes (consume only), MCP wiring

**Solve obligations:** PRE not constraint-shaped (no constants to derive). POST: run `solve_constraint_check` then `solve_constraints` on the repaired recorded request via the solver service/MCP; expect `valid: true` then `sat x=4 tier=large`; record `solve_id` (persist) in the audit comment.

**Implementation (TDD):**
- [ ] Step 1 — failing repair replay test (recorded G fixture: choice `eq_wrap`, confidence 1.0):

```rust
// tests/repair.rs
#[test]
fn recorded_enum_diagnostic_repairs_to_eq_wrap() {
    let fix = spur_jev::repair::apply_recorded_g_fixture();
    let expr = fix.expect_recompiled_constraint("tier_required");
    assert_eq!(expr["kind"], "op");
    assert_eq!(expr["op"], "eq");
    assert_eq!(expr["args"][1]["kind"], "enum_label");
}
```

- [ ] Step 2 — RED; Step 3 — implement; Step 4 — GREEN; Step 5 — commit `feat(spur-jev): diagnostic-driven repair loop`

---

### Task 6: MCP wiring behind env gate + provenance plumbing

**Task ID:** `jev-6`

**Files:**
- Create: `crates/spur-jev/src/mcp.rs`
- Modify: `crates/spur-core/src/mcp/mod.rs` (registry entry only, behind `SPUR_JEV_ENABLED=1` env; `catalog_only()` module otherwise), `crates/spur-jev/README.md`

**Depends on:** `jev-4`

**Acceptance Criteria:**
- [ ] `JevMcpModule` exposes one tool `jev_compile` (intent + data + optional family hint) → runs battery via `HttpJevTransport` when `JEV_API_KEY` present, else typed `solver_unavailable`-style error; returns `{gate, ambiguity?|request, provenance}` — the `request` is documented to be consumed only via `solve_rule_spec`/`solve_constraint_check`/`solve_rules`/`solve_constraints`
- [ ] Registered in the brain MCP registry only when `SPUR_JEV_ENABLED=1`; workers never get the live module (egress policy)
- [ ] `solver_registry_exposes_solver_tools`-style registry test extended: 7 solver tools + `jev_compile` behind the flag, 7 without
- [ ] README documents the pipeline, invariants, env vars, and the POC evidence paths
- [ ] Post-solve: end-to-end offline replay through the module (mock transport) → `solve_constraint_check` → `solve_constraints` `sat`; persisted `solve_id` in audit

**Suggested Worker:** codex (model gpt-5.6-sol, effort xhigh)

**Scope Boundary:**
- IN scope: `spur-jev/src/mcp.rs`, one registry block in `spur-core/src/mcp/mod.rs`, README
- OUT of scope: `spur-solver` sources, worker registry, skills assets

**Solve obligations:** PRE not constraint-shaped. POST: mock-transport end-to-end compile of the recorded containment case → check `valid: true` → solve `pass`; record `solve_id` (persist) in the audit comment.

**Implementation (TDD):**
- [ ] Step 1 — failing tool-listing test (flag off → 7 tools; flag on → 8 incl. `jev_compile`)
- [ ] Step 2 — RED; Step 3 — implement module + gated registration; Step 4 — GREEN + mock end-to-end; Step 5 — commit `feat(spur-jev): jev_compile MCP tool behind env gate`

---

## Dependency DAG

```text
jev-1 (wire+client) ─▶ jev-2 (battery) ─▶ jev-3 (gate+compile) ─▶ jev-4 (conformance) ─▶ jev-6 (MCP wiring)
                                                    │
                                                    └─────────────▶ jev-5 (repair loop)
```

jev-4 and jev-5 are parallelizable after jev-3. No cycles. Terminal: jev-6.

## Self-Review (executed)

1. **Spec coverage:** POC phases 1–2 → tasks 1–6 cover wire, battery, gate, compile, conformance, repair, wiring, provenance. Deferred deliberately (out of scope, tracked in bd-1yv63): live CI Jev calls, worker exposure, design notebook.
2. **Placeholders:** none — every code step shows real test bodies; every task names exact files.
3. **Type consistency:** `JevRequest/JevResponse/Answer` (jev-1) consumed by battery (jev-2) and gate/compile (jev-3); `DecisionReview/Gate/AmbiguityReport` produced in jev-3, consumed in jev-4/5/6.
4. **DAG:** valid, widest-possible given interface needs.
5. **beads compatibility:** unique IDs, explicit deps, brain-verifiable acceptance criteria, hard scope boundaries per task.
