# Unified Single-Bootstrap Skills Catalog Design

**Decision date:** 2026-08-02

**Status:** Approved

**Target areas:** `spur-acp` skills configuration, `spur-core` runtime projection,
Explore serving catalog, MCP catalog tools, adapter launch integration

## Summary

SPUR will expose exactly one SPUR-managed skill at agent startup:
`skills-catalog`. The remaining nine SPUR foundation procedures, other bundled
skills, approved Explore skills, and explicitly declared host skills remain in
one verified serving catalog and are loaded on demand through `skill_navigate`
and `skill_read`.

This design also changes discovery from raw PageIndex-node ranking to
skill-level ranking. A query returns at most one best matching node per skill,
so one verbose skill cannot consume the five-result budget. Exact skill-name or
alias phrases win deterministically, stopwords do not create matches, and the
rollout gate asserts query behavior instead of merely printing metrics.

## Solver-backed decisions

Two persisted B-prime solves are authoritative for this design:

- `sol_af89bebe13494aa4` is satisfiable with
  `architecture=unified_single`, `visible_skill_count=1`,
  `hidden_policy_count=0`, `cataloged_foundation_count=10`, host coverage, and
  rollback support.
- `sol_41cd1e3d4e1d42db` is unsatisfiable for the union of three forbidden
  outcomes under the proposed retrieval contract: duplicate skills in one
  response, an exact name phrase below rank one, or results for a stopword-only
  query.

The solver artifacts validate the boolean and integer invariants. Rust tests
remain responsible for the real tokenizer, provider inventory, filesystem
confinement, adapter launch environment, and MCP serialization.

## Product contract

### Projection modes

`skills.projection_mode` has three values:

| Mode | Projected SPUR-managed skills | Purpose |
|---|---:|---|
| `catalog_only` | exactly `skills-catalog` | default progressive discovery |
| `foundation_only` | the ten bundled foundations | context-only rollback |
| `all_active` | all bundled and accepted active skills | full-materialization rollback |

Changing `catalog_only` from ten skills to one is intentional. Existing users
who require the ten-skill startup set can select `foundation_only` explicitly.
`all_active` retains its existing behavior.

The bootstrap is protected and fail-closed. A missing directory, symlinked
directory or file, invalid frontmatter, empty body, or content-integrity failure
prevents catalog-only projection. No pool, repository override, or host provider
may replace `skills-catalog`.

### Catalog scope

The catalog is a union of explicit provider snapshots, not a filesystem walk
performed by the agent:

1. `BundledProvider` supplies all valid `assets/skills` entries, including all
   ten foundations.
2. `ExploreProvider` supplies currently adopted and gate-approved repository and
   global-pool entries.
3. `HostProvider` supplies adapter/system/user/plugin roots explicitly captured
   by the launcher before native skill isolation is applied.

Every provider source has an authority, stable provider ID, canonical root,
adapter identity, and declared isolation disposition. Serving applies the same
text inventory, symlink rejection, containment, size caps, resource hashes, and
whole-skill hash reauthorization already used for Explore entries.

Foundation IDs are protected. Only the bundled provider may own
`skills-catalog`, `spur-way`, `code-explore`, `solve`, `brain-delegation`,
`brain-review-gate`, `plan-task-discipline`, `worker-signals`,
`beads-lifecycle`, or `spur-analyst`. Other collisions keep qualified provider
identity and follow the existing approved replacement policy; they cannot
silently shadow protected names.

## Provider snapshot boundary

Introduce an immutable in-memory launch value shared by projection and MCP
registration:

```rust
pub struct CatalogProviderSnapshot {
    pub schema_version: u32,
    pub adapter: Adapter,
    pub isolation: CatalogIsolation,
    pub sources: Vec<CatalogProviderSource>,
}

pub enum CatalogIsolation {
    Strict,
    SpurManagedOnly,
}

pub struct CatalogProviderSource {
    pub provider_id: String,
    pub authority: ProviderAuthority,
    pub root: PathBuf,
}
```

The snapshot is constructed once after the profile and adapter are resolved but
before projection mutates adapter targets. `SkillsCatalogMcpModule` receives the
snapshot directly; no process-global active-adapter file is used, avoiding races
between concurrent sessions with different adapters.

`ServingCatalog::load(repo_root)` remains the repository-only test and fallback
constructor. Production registries use
`ServingCatalog::load_with_providers(repo_root, snapshot)`.

## Adapter isolation

Strict one-skill mode has two requirements:

1. the adapter's repository-local projected target contains only the rendered
   `skills-catalog` entry owned by the current generation; and
2. native user/system/plugin roots captured as providers are not independently
   injected into the launched agent.

Each adapter reports an isolation capability. Codex isolation captures the
original Codex skill roots, creates an adapter home inside the immutable runtime
generation, projects only the bootstrap there, and launches with the controlled
home. Repository-local unowned skill collisions are never deleted: strict mode
returns `StrictCatalogIsolationUnavailable` with the conflicting paths.

Adapters that cannot suppress native injection must report
`CatalogIsolation::SpurManagedOnly`. A strict catalog-only launch fails with a
typed error instead of claiming a one-skill surface. `foundation_only` and
`all_active` remain usable rollback modes.

## Bootstrap routing

`assets/skills/skills-catalog/SKILL.md` is the compact always-visible router. It
does not duplicate nine full instruction bodies. It contains deterministic
exact-name routes:

| Trigger | Required catalog read |
|---|---|
| any SPUR transaction | `spur-way` |
| code navigation or impact analysis | `code-explore` |
| constants, bounds, or invariant proof | `solve` |
| delegation | `brain-delegation` |
| worker review | `brain-review-gate` |
| submitted-plan work | `plan-task-discipline` |
| worker blocker or scope change | `worker-signals` |
| issue transition | `beads-lifecycle` |
| aggregation or graph analytics | `spur-analyst` |

Exact-name routing is safe because the retrieval contract makes a normalized
exact name or alias phrase rank first and protected foundation names cannot be
shadowed.

## Retrieval contract

Search and PageIndex navigation share one query analyzer and one skill-level
ranker.

1. Normalize Unicode case and split on non-alphanumeric boundaries.
2. Remove a versioned English stopword set. If no meaningful tokens remain,
   return `invalid_query`.
3. Detect whether a normalized skill name or declared alias appears as a
   contiguous phrase in the normalized query.
4. Score PageIndex nodes using existing BM25 over meaningful tokens.
5. Select the best node for each eligible skill.
6. Sort skill candidates lexicographically by:
   exact name/alias phrase, name-token coverage, description-token coverage,
   best-node BM25, stable provider/identity key.
7. Return at most five distinct skills. `skill_navigate` includes the best
   matching node metadata and lede; `skill_search` returns the metadata card.
8. Continue within a selected skill through the existing
   `skill_navigate(root=skill_id[:node_id])` tree hop.

This uses ordering tiers instead of invented score boosts. The BM25 constants
remain the existing standard implementation values.

## Compatibility and errors

The three public MCP tool names and opaque `skill_id` format remain unchanged.
Existing clients may observe different ranking and fewer than five results when
fewer than five distinct skills match.

New typed failures are:

- `invalid_query` when normalization leaves no meaningful tokens;
- `provider_snapshot_invalid` for a duplicate provider ID, non-canonical or
  symlinked root, unsupported schema, or protected-name collision;
- `strict_catalog_isolation_unavailable` when native adapter injection or an
  unowned target prevents the exact-one contract.

Catalog unavailable behavior remains foundation-only rollback when explicitly
configured. Catalog-only startup itself stays fail-closed because its sole
bootstrap would otherwise be unusable.

## Quality gates

The fixture schema records per-case obligations rather than relying on aggregate
thresholds:

- exact name or alias phrase must be rank one;
- expected natural-language matches must occur within the first five;
- every navigate response must contain unique skill IDs;
- stopword-only input must return `invalid_query`;
- every protected foundation must be searchable and readable;
- an explicit host-provider fixture such as `systematic-debugging` must be
  searchable and readable from the same catalog revision;
- strict catalog-only projection must contain exactly `skills-catalog`;
- provider-snapshot revocation must invalidate an old opaque reference;
- existing stale-reference, gate, integrity, confinement, no-write, brain/worker
  parity, and rollback assertions continue to pass.

Recall, precision, MRR, zero-result rate, and refinement recovery remain emitted
for observability. Determinism and minimum product behavior are enforced by
assertions.

## Rollout

1. Land shared retrieval analysis, skill-level diversification, and failing
   quality fixtures.
2. Change projection semantics and add `foundation_only` rollback.
3. Add provider snapshots and protected provider merging.
4. Integrate adapter isolation, starting with Codex and the hermetic adapter;
   unsupported adapters fail strict mode explicitly.
5. Update the bootstrap router, configuration documentation, and end-to-end
   tests, then make exact-one `catalog_only` the default.

## Non-goals

- Semantic embeddings or a hosted vector database.
- Dynamic installation of a discovered skill into the worker filesystem.
- Deleting or rewriting unowned user skill directories.
- Weakening current Explore adoption, gate, integrity, revocation, or resource
  confinement rules.
- Pretending an unsupported adapter provides strict isolation.

## Acceptance criteria

1. A default Codex or hermetic launch exposes exactly one SPUR-managed skill,
   `skills-catalog`, and does not independently inject captured host roots.
2. The serving catalog contains all ten foundations, accepted Explore skills,
   and declared host-provider skills in one revision.
3. The query `systematic debugging workflow for a failing Rust integration test`
   returns the declared `systematic-debugging` host fixture at rank one.
4. No `skill_navigate` FTS response contains duplicate skill IDs.
5. Stopword-only queries produce `invalid_query` rather than unrelated results.
6. All exact-name foundation routes return the protected bundled skill first and
   `skill_read` returns verified current bytes.
7. `foundation_only` restores the ten-skill projection and `all_active` restores
   full materialization.
8. Formatting, workspace clippy, and the `spur-acp`, `spur-core`, and `spur-tui`
   suites pass through `scripts/spur-cargo` using native/default configuration.
