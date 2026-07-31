# Skills Catalog MCP Implementation Plan

> **For the SPUR orchestrator:** Execute this as a beads-backed DAG. Implementation tasks use `codex` with profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`. The terminal review task uses `codex` with profile `code-reviewer`, model `gpt-5.6-sol`, effort `xhigh`.

**Source spec:** `docs/superpowers/specs/2026-07-31-skills-catalog-mcp-design.ipynb`

**Spec commit:** `2ae2abf07`

**Scope:** opt-in catalog-only runtime, Explore-backed discovery/read MCP tools, rollout evidence, and independent review

**Out of scope:** semantic/vector retrieval, executing skill scripts, serving binary assets, removing the legacy all-active projection, or introducing a new crate

## Outcome

Coding-agent sessions can opt into a runtime that projects one bootstrap skill, `skills_catalog`. The bootstrap directs the agent to repository-scoped MCP tools:

- `skill_search` returns at most five eligible metadata results from the merged Explore catalog.
- `skill_read` returns the exact currently eligible `SKILL.md` or one approved UTF-8 resource.
- Neither tool writes task-specific skills into the worker filesystem.
- The existing all-active projection remains the default and rollback path until all four rollout gates pass.

## Authoritative contracts

Every worker must read the notebook spec and reload these persisted solver results before editing:

- `sol_46039afe656a4dff` is `sat`: one bootstrap skill, shared approval-gated catalog, deterministic lexical/repeated search, lazy text resources, and context-only delivery are jointly feasible. Its model value `search_result_limit=3` is only a witness inside the allowed range; the notebook API contract remains default/max `5`.
- `sol_b57f4c1096ef4a0c` is `unsat`: under approval-gated read and context-only delivery, an unapproved external skill cannot be returned and a task-specific filesystem write cannot occur.
- `sol_677cfaa405324970` is `sat`: the task dependency constraints below admit a total order, while independent ready tasks can still execute concurrently.

If a worker introduces a new buffer, byte cap, result limit, timeout, or rollout threshold, it must run `solve_constraints` with `persist=true`, use a named constant or configuration field, and record the returned solve ID in its beads outcome. `unknown` and `timeout` are never evidence of `unsat`.

The executable NS-Mermaid cells are the implementation oracle:

| Contract | Required behavior | Verified report |
|---|---|---|
| `SKILLS-CATALOG-ARCHITECTURE` | eligible reads return context; every branch has `write_effect=none` | `bcf1a8f905a3b125a5f1d87a3de2da9187cf4bbde1509d5a839494ea566faa20` |
| `SKILL-ELIGIBILITY-POLICY` | `enabled ∧ compatible ∧ (bundled ∨ (adopted ∧ gate_approved))` | `680e7574ba00239081f67fb8bc6cd11cabacf926921c264dcd0f5dfdb1cfc60d` |
| `SKILL-READ-DECISION` | content only when search hit, current eligibility, version, and resource all validate | `90842fabcb998497f4bbe0f766d729e7da4aca6a1a84924f9f578e82e3c7fdbe` |
| `CATALOG-ROLLOUT-GATE` | retire legacy only after retrieval, security, integration, and observation gates pass | `d6b1e57cf466b9750c6ecfbf22ac513890c002e4d1e06e237977e5c5a8913082` |

## File structure

| Path | Purpose |
|---|---|
| `assets/skills/skills_catalog/SKILL.md` | single projected bootstrap protocol |
| `crates/spur-core/src/explore/serving.rs` | shared eligibility, index, search, exact read, references, revision, and compatibility logic |
| `crates/spur-core/src/explore/mod.rs` | expose the Explore serving facade |
| `crates/spur-core/src/mcp/skills_catalog.rs` | `ToolModule` adapter and stable MCP schemas/errors |
| `crates/spur-core/src/mcp/mod.rs` | compose tools into rooted brain and worker registries |
| `crates/spur-acp/src/config/mod.rs` | backward-compatible projection-mode configuration |
| `crates/spur-core/src/skills/projection/mod.rs` | choose all-active or catalog-only policy |
| `crates/spur-core/src/skills/projection/resolver.rs` | resolve exactly the bootstrap under catalog-only policy |
| `crates/spur-tui/src/views/explore/manage.rs` | show the same agent-serving eligibility decision in Explore |
| `crates/spur-core/tests/skills_catalog_mcp.rs` | cross-surface and rollback integration tests |
| `crates/spur-core/tests/fixtures/skills_catalog_queries.json` | deterministic retrieval evaluation corpus |
| `docs/user-docs/05-configuration.md` | opt-in, observability, and rollback documentation |
| `docs/superpowers/reviews/2026-07-31-skills-catalog-mcp.md` | independent final review evidence |

## Dependency DAG

```text
SC1 bootstrap ────────> SC4 projection ─────┐
       └─────────────────────────────────────┤
SC2 serving ──> SC3 MCP ────────────────────┼─> SC6 integration ──┐
       └──────> SC5 Explore TUI ────────────┤                    │
SC3 MCP ──────> SC7 docs <──── SC4 projection                    ├─> SC8 review
SC5 Explore TUI ──────────────────────────────────────────────────┤
SC7 docs ─────────────────────────────────────────────────────────┘
```

`sol_677cfaa405324970` witnesses one valid order as serving, bootstrap, projection, MCP, TUI, integration, docs, review. This order is not a serialization requirement; only the edges above are authoritative.

## Task SC1 — Author the one-skill bootstrap

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** none

**Planned writes:** `assets/skills/skills_catalog/SKILL.md`

**NS-Mermaid contract:** **Implement `SKILLS-CATALOG-ARCHITECTURE` report `bcf1a8...faa20`: the bootstrap routes discovery through `skill_search` and exact loading through `skill_read`; it never enumerates the catalog or asks for task-specific filesystem installation.**

### TDD steps

1. Inspect the current bundled skill frontmatter conventions and the `SkillCatalog::discover` asset root.
2. Add `skills_catalog/SKILL.md` with:
   - concise trigger language for specialized workflow discovery;
   - repeated natural-language search guidance;
   - exact `skill_id` handoff from search to read;
   - optional approved text-resource reads;
   - fail-closed behavior for every stable error kind;
   - base-agent fallback when MCP is unavailable;
   - explicit prohibition on inventing skill IDs, bypassing approval, executing resources, or materializing task-specific skills.
3. Verify the asset is discoverable as exactly `skills_catalog` and contains no embedded inventory of other skills.
4. Run:

```bash
scripts/spur-cargo test -p spur-core skills::tests
```

### Acceptance

- The bootstrap is self-contained protocol documentation.
- It names only `skill_search` and `skill_read` as the discovery/read path.
- It does not enumerate current skill names or promise installation.
- Commit: `feat(skills): SC1 add catalog bootstrap protocol`

## Task SC2 — Build the Explore serving facade

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** none

**Planned writes:** `crates/spur-core/src/explore/serving.rs`, `crates/spur-core/src/explore/mod.rs`

**NS-Mermaid contract:** **Implement `SKILL-ELIGIBILITY-POLICY` report `680e75...fc60d` and `SKILL-READ-DECISION` report `90842f...fdbe`; both eligible and denied paths must be total, exclusive, deterministic, and filesystem-write-free.**

### TDD steps

1. Add failing unit tests for:
   - the full eligibility truth table;
   - bundled inclusion and adopted/approved pool inclusion;
   - rejection of synced-only, rejected, disabled, removed, and incompatible skills;
   - local/global merge behavior where an unapproved local shadow cannot expose or replace an approved global entry;
   - deterministic catalog revision from sorted eligible identity and policy state;
   - opaque version-pinned reference round trip and stale-reference rejection;
   - text-only compatibility plus script, binary, symlink, unsafe path, undeclared resource, and oversized content denial;
   - deterministic lexical/BM25 ranking, exact-token preference, stable identity tie-break, source filter, repeated queries, and limit validation `1..=5`.
2. Implement a repository-scoped serving facade that reuses `Catalog::load_merged`, `Manifest::load_layered`, bundled skill discovery, pool loading, and existing SHA-256 helpers. Do not scrape TUI state or duplicate Explore storage roots.
3. Derive opaque IDs from `source`, `rel_path`, `pinned_commit`, and `content_sha256`; callers must not parse or construct them.
4. Build an immutable text-resource inventory from the pinned source. Canonicalize before reading, reject absolute/traversal/cross-skill/symlink escapes, allow UTF-8 text only, and recheck identity, integrity, compatibility, and current eligibility on every read.
5. Keep the service read-only. Do not call projection/materialization APIs and do not create cache files in the worker tree.
6. Emit stable domain error kinds matching the notebook contract.
7. Run:

```bash
scripts/spur-cargo test -p spur-core explore::serving
scripts/spur-cargo clippy -p spur-core -- -D warnings
```

### Acceptance

- Search returns metadata only, default/max five.
- Read returns exact verified text or one stable fail-closed error.
- No new crate dependency is added unless the task records explicit evidence that the standard-library/current-workspace implementation is insufficient.
- Commit: `feat(explore): SC2 add agent-serving skill catalog`

## Task SC3 — Expose `skill_search` and `skill_read` through MCP

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC2

**Planned writes:** `crates/spur-core/src/mcp/skills_catalog.rs`, `crates/spur-core/src/mcp/mod.rs`

**NS-Mermaid contract:** **Bind the verified `SKILLS-CATALOG-ARCHITECTURE` (`bcf1a8...faa20`) and `SKILL-READ-DECISION` (`90842f...fdbe`) exactly: search returns versioned metadata, read reauthorizes and returns context, and all outcomes have `write_effect=none`.**

### TDD steps

1. Add failing module tests for exact JSON schemas, request deserialization, default/max limit, source filter, metadata-only search output, read response serialization, and every stable error kind.
2. Implement `SkillsCatalogMcpModule` in `spur-core`, using the public `spur_mcp::ToolModule` contract. Do not move Explore logic into `spur-mcp`; that would reverse the existing dependency.
3. Give the module an optional repository root so tool schemas remain stable in unrooted catalog tests while calls without an authority root fail clearly.
4. Compose the module into both rooted brain and worker registries. Preserve existing tool ordering, alias behavior, worker deny lists, and context-service composition.
5. Add structured tracing fields for tool name, catalog revision, result count/error kind, source, and latency; never log returned instruction content.
6. Run:

```bash
scripts/spur-cargo test -p spur-core mcp::skills_catalog
scripts/spur-cargo test -p spur-core --test tool_schema_stability
scripts/spur-cargo test -p spur-core --test tool_catalog
```

### Acceptance

- Both registries advertise `skill_search` and `skill_read`.
- Tool calls use only the repository-scoped Explore facade.
- JSON-RPC errors are stable and content is never logged.
- Commit: `feat(mcp): SC3 serve Explore skills lazily`

## Task SC4 — Add opt-in catalog-only projection with rollback

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC1

**Planned writes:** `crates/spur-acp/src/config/mod.rs`, `crates/spur-core/src/skills/projection/mod.rs`, `crates/spur-core/src/skills/projection/resolver.rs`

**NS-Mermaid contract:** **Implement the pre-retirement side of `CATALOG-ROLLOUT-GATE` report `d6b1e5...13082`: catalog-only is explicit and reversible; all-active remains the default until every rollout gate passes.**

### TDD steps

1. Add failing config round-trip tests for `skills.projection_mode = "all_active" | "catalog_only"`, with absent configuration defaulting to `all_active`.
2. Add failing resolver tests proving:
   - catalog-only selects exactly bundled `skills_catalog`;
   - pool or repository overrides cannot replace the bootstrap;
   - missing/integrity-invalid bootstrap fails closed;
   - all-active behavior is unchanged and remains the rollback path.
3. Add `SelectionPolicy::CatalogOnly` and choose it from layered repository configuration for brain/worker runtime reconciliation without widening the two large orchestrator call sites.
4. Keep `Init` semantics explicit and tested. Do not retire `AllActive`.
5. Run:

```bash
scripts/spur-cargo test -p spur-acp config
scripts/spur-cargo test -p spur-core skills::projection
```

### Acceptance

- New sessions opt in through layered config.
- Catalog-only projections contain exactly one bootstrap skill.
- Removing the opt-in restores the legacy projection path.
- Commit: `feat(skills): SC4 add catalog-only projection mode`

## Task SC5 — Surface agent eligibility in Explore

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC2

**Planned writes:** `crates/spur-tui/src/views/explore/manage.rs`

**NS-Mermaid contract:** **Render the same `SKILL-ELIGIBILITY-POLICY` decision proven by report `680e75...fc60d`; the TUI is the governance plane and must not invent a second eligibility formula.**

### TDD steps

1. Add failing render tests for agent-visible, unapproved, disabled/rejected, and context-incompatible pool rows.
2. Reuse the Explore serving facade to add a compact agent-availability column or detail marker in the existing Manage/Pool lens.
3. Preserve current navigation, removal, status findings, selection clamping, and narrow-terminal behavior.
4. Do not add search/read controls to the TUI and do not call MCP from the TUI.
5. Run:

```bash
scripts/spur-cargo test -p spur-tui views::explore::manage
```

### Acceptance

- Humans can distinguish adopted content from agent-eligible content.
- Displayed decisions match MCP eligibility for the same merged view.
- Existing Explore snapshots and interactions remain stable except for the intentional marker.
- Commit: `feat(spur-tui): SC5 show agent skill eligibility`

## Task SC6 — Add integration and retrieval gates

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC1, SC3, SC4

**Planned writes:** `crates/spur-core/tests/skills_catalog_mcp.rs`, `crates/spur-core/tests/fixtures/skills_catalog_queries.json`

**NS-Mermaid contract:** **Exercise both branches of `SKILL-READ-DECISION` (`90842f...fdbe`) and collect the retrieval/security/integration evidence required by `CATALOG-ROLLOUT-GATE` (`d6b1e5...13082`); tests do not authorize legacy retirement.**

### TDD steps

1. Create a representative fixture with task-intent queries, expected relevant skill IDs, negative/unapproved cases, repeated refinements, and source-filter cases.
2. Add integration tests for:
   - rooted brain and worker registry search/read round trip;
   - exact `SKILL.md` and one declared UTF-8 resource;
   - metadata-only search and at-most-five results;
   - revocation between search and read;
   - stale version reference and integrity mismatch;
   - local/global unapproved shadowing;
   - concurrent catalog revisions without mixed content;
   - path traversal, absolute path, symlink, cross-skill, script, binary, undeclared resource, and size denial;
   - unchanged worker filesystem before/after successful and failed reads;
   - catalog-only exact-one projection and all-active rollback;
   - MCP-unavailable bootstrap fallback behavior at the protocol boundary.
3. Compute deterministic retrieval metrics from the fixture: recall@5, precision@5, MRR, zero-result rate, and refinement recovery. Assert documented release thresholds; if a new threshold is required, persist a solver result and record its ID.
4. Run:

```bash
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-core
```

### Acceptance

- Security tests prove context-only, fail-closed behavior.
- Retrieval metrics are deterministic and visible in test failure output.
- The integration suite validates rollback but leaves all-active as the default.
- Commit: `test(spur-core): SC6 gate skills catalog rollout`

## Task SC7 — Document opt-in, evidence, and rollback

**Worker:** `codex`, profile `rust-engineer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC3, SC4

**Planned writes:** `docs/user-docs/05-configuration.md`

**NS-Mermaid contract:** **Document `CATALOG-ROLLOUT-GATE` report `d6b1e5...13082` as a four-gate operational decision: any failed gate keeps legacy; only all four passing permits later retirement.**

### Steps

1. Document the exact layered config for opt-in and rollback.
2. Explain the one-bootstrap flow, tool inputs, stable errors, source/integrity semantics, and context-only limitations.
3. Document which metrics/traces supply retrieval, security, integration, and observation evidence.
4. State that this change does not make catalog-only the default and does not remove all-active projection.
5. Run:

```bash
git diff --check -- docs/user-docs/05-configuration.md
```

### Acceptance

- An operator can enable, observe, and roll back the feature without reading source.
- Retirement criteria exactly match the notebook, with no implied auto-retirement.
- Commit: `docs(config): SC7 explain skills catalog rollout`

## Task SC8 — Independently review implementation against proofs

**Worker:** `codex`, profile `code-reviewer`, model `gpt-5.6-sol`, effort `xhigh`

**Depends on:** SC5, SC6, SC7

**Planned writes:** `docs/superpowers/reviews/2026-07-31-skills-catalog-mcp.md`

**NS-Mermaid contract:** **Review all four verified reports—architecture `bcf1a8...faa20`, eligibility `680e75...fc60d`, read `90842f...fdbe`, and rollout `d6b1e5...13082`—as mandatory acceptance evidence, not illustrative diagrams.**

### Review procedure

1. Reload `sol_46039afe656a4dff`, `sol_b57f4c1096ef4a0c`, `sol_677cfaa405324970`, and any solve IDs recorded by implementation tasks.
2. Inspect every task diff and beads audit/signal state. Verify each claimed commit and test result instead of trusting summaries.
3. Use Notebook MCP to confirm the active source spec still has four `ns_mermaid` cells, 20/20 matched obligations, and the four report hashes above.
4. Review for:
   - duplicated or divergent Explore/MCP/TUI eligibility;
   - unapproved local/global shadow exposure;
   - stale-reference or TOCTOU authorization gaps;
   - canonicalization/symlink/path traversal mistakes;
   - accidental task-specific filesystem writes;
   - instruction content in logs;
   - registry asymmetry between brain and worker;
   - projection default/rollback regressions;
   - undocumented magic numbers or thresholds;
   - new dependency cycles or unjustified crate dependencies.
5. Run:

```bash
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-acp -p spur-core -p spur-tui -- -D warnings
scripts/spur-cargo test -p spur-acp config
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-tui views::explore::manage
```

6. Write the review artifact with severity-ordered findings, proof/test evidence, residual risks, rollout recommendation, and a clear verdict: `approve`, `request_changes`, or `block_rollout`. Do not modify implementation files. A blocking finding must remain blocking; do not soften it to complete the plan.
7. Commit: `docs(review): SC8 audit skills catalog MCP`

### Acceptance

- The artifact maps every notebook invariant to implementation and test evidence.
- It confirms the default remains all-active unless a separate approved retirement change exists.
- Any unresolved high/critical finding yields `request_changes` or `block_rollout`.

## Plan-level verification

After all implementation changes are approved, the brain must verify fresh evidence before closing the epic:

```bash
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-acp -p spur-core -p spur-tui -- -D warnings
scripts/spur-cargo test -p spur-acp config
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-tui views::explore::manage
git diff --check
```

The plan is complete only when the final reviewer approves, the brain independently inspects task diffs and beads audit state, and the required tests pass. Passing this plan enables opt-in rollout; it does not itself authorize default migration or legacy retirement.
