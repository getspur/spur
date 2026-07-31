# Port Mermaid Fences to NS-Mermaid Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace every remaining Markdown Mermaid fence in `ns-spec-v0.2-design.ipynb` with an executable native `ns_mermaid` cell while preserving one intentional negative transfer fixture.

**Architecture:** Keep prose in the existing Markdown cells, remove all eleven fenced diagrams, and place executable NS-Mermaid cells immediately after the Markdown cell that explains each diagram. Process diagrams become relational accept/block contracts, the sequence diagram becomes an equivalent verified flowchart, the lifecycle remains a transition-system cell, and the broken transfer remains an expected `verified=false` counterexample fixture.

**Tech Stack:** nbformat 4 notebook, SPUR Notebook MCP, native `ns_mermaid` cells, schema-v2 NS proof JSON, Z3 through the notebook runner's existing `solve` integration.

---

### Task 1: Capture the migration inventory and expected gate

**Files:**
- Modify: `docs/superpowers/specs/ns-spec-v0.2-design.ipynb`

- [ ] **Step 1: Confirm the active notebook and fence inventory**

Use Notebook MCP `notebook_context_pack`, then read the notebook and confirm:

```text
11 Markdown Mermaid fences
9 flowchart
1 sequenceDiagram
1 stateDiagram-v2
3 existing native ns_mermaid cells
```

- [ ] **Step 2: Record the expected final shape**

```text
0 Markdown Mermaid fences
13 native ns_mermaid cells
12 positive fixtures with verified=true
1 negative transfer fixture with verified=false
52 cell-namespaced logical ports
```

- [ ] **Step 3: Preserve the negative-fixture policy**

The `TRANSFER-ORIGINAL` cell must keep output-dependent branch guards and must return a determinism counterexample. Do not weaken the source merely to make the aggregate notebook green.

### Task 2: Port architecture and verification pipeline diagrams

**Files:**
- Modify Markdown cells: `c03-arch`, `c05-decision`, `v03-decision`
- Insert native NS-Mermaid cells after each modified Markdown cell.

- [ ] **Step 1: Remove each fenced diagram with targeted Notebook MCP edits**

Replace each fence with prose that says the following native cell is authoritative and executable.

- [ ] **Step 2: Insert the architecture gate**

Use this relational shape:

```text
@spec NS-SPEC-SAME-CELL-ARCHITECTURE
@input source_present: Bool
@input parser_pass: Bool
@input obligations_complete: Bool
@input solver_terminal: Bool
@input ports_published: Bool
@output decision: enum[ready, blocked]
```

`READY` is the conjunction of all five inputs; `BLOCKED` is its negation. Require determinism, coverage, exclusivity, and witnesses for both branches: 5/5 matched.

- [ ] **Step 3: Insert the per-cell verification gate**

Use inputs for Mermaid validity, type validity, obligation completeness, lowering support, and solver expectation match. Emit `verified` or `failed` with the same five proof/witness points.

- [ ] **Step 4: Insert the one-cell authority gate**

Use inputs for source presence, parse/type success, obligation completeness, solver completion, and proof-identity match. Emit `accept` or `reject`; require the standard five obligations.

### Task 3: Port the transfer pair

**Files:**
- Modify Markdown cell: `c06-determinism`
- Insert two native NS-Mermaid cells after `c06-determinism`.

- [ ] **Step 1: Remove both transfer fences**

Retain the prose that explains why the first declaration is invalid and why the second is corrected.

- [ ] **Step 2: Insert the negative transfer fixture**

Keep:

```text
@spec TRANSFER-ORIGINAL
@when status = insufficient_balance
@when status = success
@verify DET_ORIGINAL: prove determinism
```

Expected result: `DET_ORIGINAL` returns `sat`, the report contains one mismatch and a concrete model, and `verified=false`.

- [ ] **Step 3: Insert the corrected transfer fixture**

Use input-partition guards for invalid, insufficient, and success. Replace unsupported `witness each status` shorthand with three explicit `witness branch` obligations. Expected result: determinism, coverage, exclusivity, and all three witnesses match, for 6/6.

### Task 4: Port conformance, solve sequence, and lifecycle diagrams

**Files:**
- Modify Markdown cells: `c08-conformance`, `c09-solve`, `c10-lifecycle`
- Insert native NS-Mermaid cells after each modified Markdown cell.

- [ ] **Step 1: Insert the conformance-release gate**

Use inputs `proof_verified`, `vectors_present`, `adapter_ready`, and `implementation_match`; emit `conformance_passed` or `blocked`. Require the standard five obligations.

- [ ] **Step 2: Translate the sequence diagram into a verified flowchart**

Preserve the Author → runner → solve → outputs → downstream order visually. Formalize inputs `source_valid`, `typing_valid`, `obligations_complete`, `solver_terminal`, `hashes_match`, and `ports_published`; emit `publish` or `block`. Require the standard five obligations.

- [ ] **Step 3: Insert the lifecycle transition-system cell**

Use state variables:

```text
fresh: Bool
proof_complete: Bool
approved: Bool
```

Use transitions `VERIFY`, `APPROVE`, `EDIT`, and `RERUN`, each in its own note with explicit `@from` and `@to`. Prove initiation and preservation of `APPROVAL_VALID: not approved or (fresh and proof_complete)` across all four transitions: 5/5 matched.

### Task 5: Port trust, implementation repair, and authoring-loop diagrams

**Files:**
- Modify Markdown cells: `50d1a03d-7b1e-4090-9005-0206f8b60288`, `5ac1ea71-960c-4149-a93e-ab8fcf376a1e`, `20084d37-b4c3-4855-9d7f-84878d1e0988`
- Insert two native NS-Mermaid cells; reuse existing cell `a33a56fe-01c9-4dc7-a5ef-be365cb2a20b` for the authoring loop.

- [ ] **Step 1: Insert the runtime-trust gate**

Use inputs `profile_pinned`, `native_oracle_match`, `ports_match`, and `brain_review_pass`; emit `trusted` or `untrusted`. Require the standard five obligations.

- [ ] **Step 2: Insert the implementation-release gate**

Use the six normative release inputs already listed in the prose:

```text
runtime_trusted
spec_approved
spec_hash_unchanged
native_proof_pass
conformance_pass
brain_review_pass
```

Emit `release` or `block`; require the standard five obligations.

- [ ] **Step 3: Reuse the existing authoring-gate cell**

Remove the large authoring-loop Mermaid fence and replace it with prose identifying the following existing native cell as the executable formalization. Do not duplicate the twelve-input authoring gate.

### Task 6: Verify zero fences and execute every native cell

**Files:**
- Verify: `docs/superpowers/specs/ns-spec-v0.2-design.ipynb`

- [ ] **Step 1: Scan the loaded notebook**

Expected:

```text
Markdown Mermaid fence count: 0
Native ns_mermaid cell count: 13
Unique cell IDs: true
```

- [ ] **Step 2: Run all thirteen native cells through Notebook MCP**

Expected:

```text
12 positive cells: verified=true
TRANSFER-ORIGINAL: verified=false with one matched runtime execution and one expected proof mismatch
No parser/type/unsupported/inconclusive result
Each cell emits Mermaid MIME plus schema-v2 proof JSON
```

- [ ] **Step 3: Verify ports and persistence**

Expected:

```text
52 namespaced NS-Mermaid ports
No producer collision
Close/open preserves every source/IR/report hash
```

- [ ] **Step 4: Validate notebook JSON and diff**

Run:

```bash
jq -e '.nbformat == 4 and ([.cells[].id] | length == (unique | length))' docs/superpowers/specs/ns-spec-v0.2-design.ipynb
git diff --check -- docs/superpowers/specs/ns-spec-v0.2-design.ipynb
```

Expected: both commands exit 0.

### Task 7: Commit the migration

**Files:**
- Modify: `docs/superpowers/specs/ns-spec-v0.2-design.ipynb`
- Add: `docs/superpowers/plans/2026-07-31-port-mermaid-fences-to-ns-mermaid.md`

- [ ] **Step 1: Stage only the plan and design notebook**

```bash
git add docs/superpowers/plans/2026-07-31-port-mermaid-fences-to-ns-mermaid.md
git add docs/superpowers/specs/ns-spec-v0.2-design.ipynb
```

- [ ] **Step 2: Commit the completed migration**

```bash
git commit -m "docs(specs): NS6 port Mermaid diagrams to NS-Mermaid"
```

- [ ] **Step 3: Confirm the target paths are clean**

```bash
git diff --exit-code HEAD -- docs/superpowers/plans/2026-07-31-port-mermaid-fences-to-ns-mermaid.md
git diff --exit-code HEAD -- docs/superpowers/specs/ns-spec-v0.2-design.ipynb
```

Expected: both commands exit 0.
