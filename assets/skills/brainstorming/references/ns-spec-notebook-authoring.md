# NS-Spec Notebook Spec Authoring

Normative design: `docs/superpowers/specs/ns-spec-v0.2-design.ipynb` (NS-Spec v0.3 —
self-verifying notebook specifications). Practical template:
`docs/superpowers/specs/2026-07-31-skills-catalog-mcp-design.ipynb`.

This reference is the compiler-backed loop for writing design specs as notebooks
with native `ns_mermaid` cells through Notebook MCP. Do not invent Mermaid
annotation syntax from general knowledge.

## Artifact contract

| Role | Path / form | Authority |
|---|---|---|
| **Authoritative design** | `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.ipynb` | Sole source of truth |
| **Formal unit** | One native `ns_mermaid` code cell | Diagram + annotations + proofs + ports share one cell identity |
| **Prose** | Markdown cells in the same notebook | Narrative only; not a second formal source |
| **Optional export** | Generated `.md` snapshot | Review convenience only; never authoritative |

**Forbidden as formal contracts:** Markdown ` ```mermaid ` fences, hand-authored
Python/Z3 cells, persistent ASTs, or a second constraint block beside the Mermaid
source.

## Notebook MCP tool map

Call these tools only (no filesystem hand-edits of formal cells when MCP is available):

| Tool | When |
|---|---|
| `notebook_context_pack` | Orient on the open notebook |
| `notebook_new` / `notebook_open` / `notebook_save` | Create or load the design notebook; persist path under `docs/superpowers/specs/` |
| `notebook_insert_cell` | Add markdown or code cells (`kind`, `source`; for code: `code_type: "ns_mermaid"`) |
| `notebook_write_cell` / `notebook_edit_cell` | Replace or patch cell source with `expected_version` |
| `notebook_read_cell` / `notebook_get_notebook` | Inspect source, outputs, proof mime bundles |
| `notebook_run_cell` | Execute one `ns_mermaid` cell; publishes diagram + proof ports |
| `notebook_ns_mermaid_spec` | Load versioned profile + proof-kind syntax (pin dialect before authoring) |
| `notebook_ns_mermaid_check` | Preflight parse/type/obligations **without** solving or mutating the notebook |
| `notebook_ns_mermaid_explain` | Repair guidance for diagnostics, obligations, counterexamples |

`mutation_id` on insert is a client idempotency key. Always pass `expected_version`
on writes.

## When a cell must be `ns_mermaid`

Add a native formal cell when the design asserts any of:

- decision / release / eligibility / routing **partitions** (mutually exclusive branches)
- **determinism** of outputs from declared inputs
- state-machine **lifecycle** (initiation / preservation / transitions)
- **witnesses** that branches or status values are reachable
- any property you want implementation workers bound by later

Plain prose or informal architecture sketches stay in markdown. If a diagram is
only illustrative, say so in the surrounding markdown and do not add `@verify`.

## Profile binding (required before formal cells)

1. Choose diagram kind: `flowchart` / `relational_lia`, `stateDiagram-v2`,
   `sequenceDiagram`, etc. — only if the live registry marks it `implemented`.
2. Call `notebook_ns_mermaid_spec` with that `profile` (and `proof_kind` when
   needed; `include: "valid_example"` or `"all"` for fixtures + full entry).
3. Pin `profile_id`, `profile_version`, and capability `state` from the registry
   response in the design epic comment (beads) and in the notebook's intro markdown.
4. Never fall back to remembered Mermaid dialect or a nearby profile.
5. Empty `notebook_ns_mermaid_spec` request → `list_profiles` (authoritative
   availability matrix). Header aliases (e.g. `flowchart`) resolve to stable IDs
   (e.g. `relational_lia`).

### Live registry ground truth (`registry_schema_version: 1`)

Re-query the tool before authoring; this table is a snapshot of what the
code-owned registry returned at skill-update time, not a second dialect source.

**Theory for all implemented formal profiles:** `z3` / `qf_lia_bool_int_enum`
(Bool, mathematical Int, enum, linear arithmetic only — no reals, BitVec,
quantifiers, collections, or Optimize).

| Stable ID | Mermaid headers | State | Binding | Use in design specs |
|---|---|---|---|---|
| `relational_lia` | `flowchart`, `graph` | **implemented** | `inline_annotations` | **Default** for decision partitions, eligibility/routing/release gates |
| `state_invariant_lia` | `stateDiagram`, `stateDiagram-v2` | **implemented** | `inline_annotations` | Lifecycles: initiate + preserve invariants |
| `sequence_trace` | `sequenceDiagram` | **implemented** | `inline_annotations` | Ordered message protocols (alt/opt only) |
| `flow_conservation` | `sankey` | **implemented** | data port / metadata / companion | Exact integer flow nonneg + conservation |
| `numeric_series` | `pie`, `xychart`, `xychart-beta` (**only these**) | **implemented** (partial) | data port / metadata / companion | Nonneg slices/series; optional pie total |
| `numeric_series` | `radar`, `quadrant`, `treemap` | **capability_unavailable** | — | Visualization claim only; no formal cell |
| `conceptual_diagrams` | `mindmap`, `journey`, `ishikawa`, `wardley`, `cynefin` | **visualization_only** | none | Render only; never claim verified |
| `architecture_policy` | `architecture`, `architecture-beta`, `C4`, `block` | **capability_unavailable** | — | Do not use as formal gates |
| `bounded_reachability`, `transition_bmc` | (none wired) | **capability_unavailable** | — | No BMC / property-bearing unrolling |
| `schedule_feasibility`, `schedule_optimization`, `kanban_policy` | gantt/timeline/kanban | **capability_unavailable** | — | No scheduling proofs |
| `finite_sets`, `bounded_relational`, `packet_*`, `git_policy`, `event_trace` | various | **capability_unavailable** | — | Treat as unverified |

**Design-spec default:** `relational_lia` + partition proofs. Use
`state_invariant_lia` for state machines. Use `sequence_trace` only when the
protocol is the contract. Prefer markdown for architecture/C4 sketches until
`architecture_policy` is implemented.

### Implemented proof kinds (inline formal profiles)

| Proof kind ID | Syntax | Expected status | Profiles |
|---|---|---|---|
| `witness_non_vacuity` | `@verify <id>: witness non_vacuity` | `sat` | relational, state, sequence |
| `witness_consistency` | `@verify <id>: witness consistency` | `sat` | relational, state, sequence |
| `prove_determinism` | `@verify <id>: prove determinism` | `unsat` | relational, state, sequence |
| `prove_partition_coverage` | `@verify <id>: prove partition_coverage` | `unsat` | relational, state, sequence |
| `prove_partition_exclusive` | `@verify <id>: prove partition_exclusive` | `unsat` | relational, state, sequence |
| `witness_branch` | `@verify <id>: witness branch <branch_id>` | `sat` | relational, state, sequence |
| `witness_each_status` | `@verify <id>: witness each status` | `sat_per_enum_member` | requires enum **output named literally `status`** |
| `explicit_witness` | `@witness <predicate>` | `sat` | relational, state, sequence |
| `prove_initiate` | `@verify <id>: prove initiate <invariant_id>` | `sat_consistency_then_unsat` | **state only** |
| `prove_preserve` | `@verify <id>: prove preserve <inv> on <transition>` | `sat_enabledness_then_unsat` | **state only** |
| `prove_sequence_protocol` | `@verify <id>: prove sequence_protocol` | `sat` | **sequence only** |

**Data profiles (implicit obligations, not free-form `@verify` menus):**

- `flow_conservation`: nonneg flow + conserve internal nodes (+ nonvacuity)
- `numeric_series`: nonneg slices/series values; pie `@ns-total` exact total when declared

### Naming / expression rules (shared)

- Expression IDs: `[A-Za-z_][A-Za-z0-9_]*`; reserved: `Int`, `Bool`, `and`, `or`, `not`, `true`, `false`
- Stable IDs: letter/underscore first, then `[A-Za-z0-9_.-]`
- Next-state: `@update <state_var>' = <expression>` (state profile)
- Compiler namespace: names starting `__ns_` are reserved
- **Unsupported everywhere in QF_LIA profiles:** reals/rationals, BitVec, collections, quantifiers, optimization, unbounded model checking

### Preflight vs run

- `notebook_ns_mermaid_check` → stage `obligation_preview`, `published_verified: false`, `solver_verdicts: null`. Confirms parse/bind/type and **expected** obligation IDs only.
- `notebook_run_cell` → actual solver evidence and publish. Never treat preflight as verified.

Registry valid examples for `relational_lia`, `state_invariant_lia`, and
`sequence_trace` preflight-clean (`ok: true`, empty diagnostics) against the
live checker at skill-update time.

## Compiler-backed authoring loop (per formal cell)

From NS-Spec §13.5 — brain authoring during brainstorming follows the same
gates as workers:

1. **Intent first** — record semantic behavior, branches, invariants, and required
   checks in markdown or beads *without* inventing syntax.
2. **Pin profile** — `notebook_ns_mermaid_spec`.
3. **Draft NS-Mermaid source** — stable node IDs; `@spec`, `@type`, `@input`,
   `@output`, `@branch`/`@when`/`@ensures`, and named `@verify` points.
4. **Preflight** — `notebook_ns_mermaid_check(source=...)`. On diagnostics:
   `notebook_ns_mermaid_explain` → span-local repair → re-check. Do not full-cell
   rewrite for a local error.
5. **Insert/write** — `notebook_insert_cell(kind="code", code_type="ns_mermaid",
   source=...)` or `notebook_write_cell` with matching `expected_version`.
   Persist **only** NS-Mermaid source (never AST / Z3 / second constraint block).
6. **Run** — `notebook_run_cell(cell_id)`. Expect mime outputs such as
   `application/vnd.spur.ns-mermaid+text` and schema-v2
   `application/vnd.spur.ns-proof+json`.
7. **Accept only when fresh** — all mandatory obligations match expected statuses;
   `verified` aggregate agrees with intent; source/IR/report hashes are present
   for beads audit. Counterexamples are semantic evidence: do **not** weaken
   `@requires`, delete `@invariant`, widen branches, or remove `@verify` to go
   green without an explicit intent change and user approval.

## Minimal formal cell shape (relational flowchart)

Positive pattern used in design notebooks (skills-catalog architecture gate):

```text
flowchart TD
    CTX["`@spec YOUR-SPEC-ID
@type Decision = enum[accept, reject]
@input gate_a: Bool
@input gate_b: Bool
@output decision: Decision`"]

    ACCEPT["`@branch ACCEPT
@when gate_a and gate_b
@ensures DECISION_ACCEPT: decision = accept`"]

    REJECT["`@branch REJECT
@when not (gate_a and gate_b)
@ensures DECISION_REJECT: decision = reject`"]

    CHECK["`@verify DEC_DETERMINISTIC: prove determinism
@verify DEC_COVERAGE: prove partition_coverage
@verify DEC_EXCLUSIVE: prove partition_exclusive
@verify ACCEPT_REACHABLE: witness branch ACCEPT
@verify REJECT_REACHABLE: witness branch REJECT`"]

    CTX --> ACCEPT --> CHECK
    CTX --> REJECT --> CHECK
```

Load exact proof syntax and expected solver statuses from
`notebook_ns_mermaid_spec(profile=..., proof_kind=...)` — the registry is
authoritative over this sketch.

## Recommended notebook outline

Scale to complexity. Skills-catalog style is the default for product designs:

1. **Title / status / scope** (markdown)
2. **Architecture decision** (`ns_mermaid` gate)
3. **Goals and non-goals** (markdown)
4. **Core policies / state / protocols** (one `ns_mermaid` cell per formal unit,
   interleaved with markdown that explains human intent)
5. **Migration / seams / risks** (markdown)
6. **Proof evidence table** (markdown) — after runs: cell id, obligation match
   counts, source/IR/report hashes for beads and review

Meta-runtime designs may follow the denser structure of
`ns-spec-v0.2-design.ipynb` (§ same-cell architecture, lifecycle, authoring gate).

## Diagnostic-driven repair

| Evidence | Response | Auto semantic change? |
|---|---|---:|
| Parse / placement / type / unresolved ID | Span-local edit from allowed profile forms; re-preflight | no |
| Unsupported construct | Supported equivalent or escalate profile change | no |
| Missing required obligation | Add manifest-required `@verify` | no |
| Proof `sat` (refuted) | Attach counterexample; revise **intent** with user | only with approval |
| Witness `unsat` (unreachable) | Report unreachable branch/requirement; revise intent | only with approval |
| `unknown` / timeout / cancel | Simplify within approved semantics or escalate; never accept | no |
| Hash mismatch vs preflight | Stale or runtime drift; re-run or report defect | no |

## Self-review extras for notebook specs

In addition to the skill's general placeholder / consistency / scope checks:

1. Every formal claim that needs proof has a native `ns_mermaid` cell (not a fence).
2. Each formal cell was preflighted and run; proof JSON is present in outputs.
3. Mandatory obligations match; deliberate negative fixtures document expected
   `verified=false`.
4. Intro markdown pins profile IDs used.
5. Beads epic links the `.ipynb` path and records key proof hashes / solve
   evidence IDs where independent `solve` was used.
6. No second formal source (AST, Python Z3, duplicate constraint markdown).

## Handoff to writing-plans

Pass:

- **Source spec:** `docs/superpowers/specs/YYYY-MM-DD-<topic>-design.ipynb`
- **Design epic:** closed issue id
- **Formal cells:** list of `@spec` ids / cell ids that implementation must
  respect (immutable contract surface)

writing-plans treats the notebook as the approved spec; plan tasks may cite
individual `@spec` cells as acceptance oracles.
