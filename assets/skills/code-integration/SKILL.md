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
   contract/behavior → local result, error, retry, or cleanup. Name the selector
   and source evidence at each side of the seam. Stay depth-first and bounded.
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
belong under uncertainties, not findings. A narrower supported capability is a
defect only when local callers, tests, or a public contract prove the broader
capability is required; otherwise report it as a verified constraint or
uncertainty without severity.

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
