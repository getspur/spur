# Code Integration Skill Design

## Purpose

Add `assets/skills/code-integration/SKILL.md`, a focused workflow for evaluating the integration seam between symbols in the current worktree and symbols in an external package. The skill must combine the worktree `code_*` MCP surface with the external-package `external_*` MCP surface to explain the end-to-end flow and produce evidence-backed review findings.

This skill complements `code-explore`. It does not replace graph-first discovery or duplicate the full tool reference. Its responsibility is the cross-graph reasoning step: establish which local symbol relies on which external symbol, trace both sides, and judge whether the integration is correct and compatible.

## Trigger and Scope

Use the skill when reviewing or explaining adapters, wrappers, trait implementations, SDK clients, serialization boundaries, plugin integrations, or other code where a worktree symbol depends on an external package contract or implementation.

The default deliverable contains both:

1. An inbound-to-outbound integration trace.
2. A findings-first evaluation of the seam.

The skill is not for reviewing two internal workspace crates, general dependency selection, or exploring an external package without a local integration boundary. Those remain `code-explore` tasks.

## Core Principle

Treat the worktree graph and external package graph as separate evidence domains. Neither graph supplies a trustworthy cross-graph edge. The reviewer must establish the seam explicitly from source evidence such as the local call expression, imported type or trait, package metadata, feature selection, and external symbol signature.

Never infer compatibility from matching symbol names alone.

## Workflow

### 1. Define the seam and exact revision

State the integration question and identify the local boundary candidate. Resolve the dependency's exact version, commit, tag, or ref from the project manifest and lockfile.

Query only that exact external revision. If it is not indexed, run `external_index`, poll `external_index_status` until terminal, and retry. Never silently substitute the latest indexed revision. If the exact source cannot be indexed, report the review as blocked or incomplete rather than presenting another revision as authoritative.

### 2. Ground the internal side

Use `knowledge_context_pack_2` for concept-level orientation when the boundary is not already known. Follow with filtered `code_symbol_search` and `code_read_symbol` to select and read the exact worktree symbol.

Use `code_callers` to identify inbound consumers and `code_callees` to identify the outbound boundary when the review needs flow or impact evidence. Apply the `code-explore` counts-first and asymmetric unresolved-edge rules. Verify suspicious common-name or cross-crate resolutions by reading source.

### 3. Ground the external side

Use `external_knowledge_context` when the upstream concept or contract is not yet known, or `external_code_search` when the symbol name is known. Carry the returned package selector into `external_code_read`.

Use `external_code_callers` or `external_code_callees` only when upstream impact or behavior beyond the public contract matters. Apply the same counts-first discipline, while recognizing that external unresolved edges represent cross-package labels.

### 4. Build the seam map

Record a compact mapping with:

- local `graph://symbol/<id>` selector;
- external `pkg:<package>@<revision>::<symbol>` selector;
- exact revision evidence;
- the source-level evidence connecting them;
- argument, return, error, ownership, lifecycle, and configuration translations;
- any assumptions that remain unverified.

This seam map is the handoff between the two MCP surfaces. Do not pass worktree selectors to external tools or package selectors to worktree tools.

### 5. Trace and evaluate

Trace the relevant path from the local caller into the boundary, through the external contract or implementation, and back into local result/error handling. Keep the trace depth-first and bounded; do not expand broad subgraphs by default.

Evaluate only applicable dimensions:

- API, type, and schema contract;
- error translation and retry semantics;
- ownership, borrowing, resource lifetime, and cleanup;
- async, cancellation, concurrency, and thread-safety assumptions;
- feature flags, configuration, defaults, and platform behavior;
- performance, allocation, batching, and backpressure;
- security, validation, trust boundaries, and unsafe behavior;
- exact-version compatibility and upgrade sensitivity.

Every finding must cite evidence from the local side, the external side, or both. A concern without enough evidence is an uncertainty, not a defect.

## Output Contract

The skill's default response has four sections:

1. **Integration trace** — a concise ordered flow that names both selectors and the exact package revision.
2. **Findings** — severity-ordered review findings with local evidence, external evidence, impact, and a concrete recommendation.
3. **Verified compatibility** — important contract points checked and found aligned, kept brief.
4. **Uncertainties** — missing index data, unresolved edges, dynamic behavior, generated code, or runtime conditions not proven by either graph.

If no defects are found, say so and retain residual risks or testing gaps. Do not manufacture findings to populate the report.

## Error Handling and Trust Rules

- Exact external revision missing: index, poll, and retry.
- Indexing fails: report the external side as ungrounded and stop compatibility claims.
- Local graph is stale for returned files: use current `code_*` reads and surface the staleness caveat.
- External knowledge retrieval is broad or weak: use exact symbol search and read the source body.
- A call edge resolves to an implausible symbol: treat it as a graph hypothesis and verify the call expression manually.
- Macros, dynamic dispatch, FFI, or generated code obscure the seam: state the limitation and use bounded source/config inspection as fallback.

## Skill Packaging

Create a concise, self-contained `assets/skills/code-integration/SKILL.md`. It will:

- declare `code-explore` as required background;
- contain the paired-seam workflow and a quick-reference table;
- contain common mistakes and red flags;
- include one concrete Serde-style example pairing a local manual `Deserialize` implementation with the exact external `Deserialize` trait contract;
- avoid duplicating the full `code_*` and `external_*` tool reference already maintained by `code-explore`.

No supporting scripts or reference files are required for the initial version.

## Validation Strategy

Develop the skill with RED–GREEN–REFACTOR scenarios:

1. Baseline an integration-review prompt without the new skill and record failures such as using the latest dependency revision, exploring only one graph, trusting a name match as a cross-graph edge, or returning a trace without findings.
2. Run the same scenario with `code-integration` and verify that it resolves the exact revision, grounds both selectors, builds the seam map, traces the flow, and produces evidence-backed findings or an explicit no-findings result.
3. Add a cold-revision or suspicious-edge variation and tighten the skill if the evaluator silently falls back or overstates graph evidence.

Run repository skill validation and the focused bundled-skill tests that prove the asset is discoverable and contains the required integration guidance. Review the final diff and commit only the new skill, its focused test changes if needed, and this approved design lineage.
