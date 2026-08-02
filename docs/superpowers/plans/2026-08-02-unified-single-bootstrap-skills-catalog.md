# Unified Single-Bootstrap Skills Catalog Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `catalog_only` expose exactly one `skills-catalog` bootstrap while a unified, verified MCP catalog discovers all foundations, Explore entries, and explicit host-provider skills with enforceable retrieval quality.

**Architecture:** Use one immutable provider snapshot per launched agent, shared by runtime projection and the skills-catalog MCP module. Rank PageIndex results at the skill level with exact-phrase precedence and one best node per skill; preserve `foundation_only` and `all_active` rollback paths and fail strict isolation closed.

**Tech Stack:** Rust 2021, Tokio, Serde/TOML/JSON, existing BM25 PageIndex, SHA-256 inventory verification, SPUR projection generations, MCP JSON tools, `scripts/spur-cargo`.

**Design:** `docs/superpowers/specs/2026-08-02-unified-single-bootstrap-skills-catalog-design.md`

**Solver artifacts:** `sol_af89bebe13494aa4`, `sol_41cd1e3d4e1d42db`

---

## File and responsibility map

- `crates/spur-core/src/explore/serving.rs`: shared query analysis, skill-level ranking, provider-aware serving build, read reauthorization.
- `crates/spur-core/src/explore/providers.rs`: immutable provider snapshot, authority validation, protected-name rules.
- `crates/spur-core/src/explore/mod.rs`: provider module export.
- `crates/spur-core/src/mcp/skills_catalog.rs`: inject provider snapshots into production catalog loads.
- `crates/spur-core/src/skills/adapters.rs`: adapter skill roots and strict-isolation capability.
- `crates/spur-core/src/skills/projection/{mod.rs,resolver.rs,generation.rs,reconcile.rs}`: exact-one/default selection, rollback selection, controlled adapter view.
- `crates/spur-acp/src/config/mod.rs`: `foundation_only` configuration value and strict catalog option.
- `crates/spur-core/src/orchestrator/connection.rs`: brain/adhoc provider capture and launch environment.
- `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`: worker provider capture and launch environment.
- `crates/spur-core/src/worker_server.rs`: rooted worker catalog registry with provider snapshot.
- `assets/skills/skills-catalog/SKILL.md`: compact foundation routing contract.
- `crates/spur-core/tests/{skills_catalog_mcp.rs,fixtures/skills_catalog_queries.json}`: product-level retrieval, provider, projection, and rollback gates.
- `crates/spur-core/tests/tool_catalog.rs`: production MCP registration coverage.
- `docs/user-docs/05-configuration.md`: exact-one semantics, provider scope, isolation failures, rollback.

## Dependency DAG

```text
USC1 retrieval contract
  → USC2 exact-one projection modes
    → USC3 unified provider snapshot
      → USC4 adapter isolation and launch integration
        → USC5 bootstrap routing and end-to-end gates
```

The sequence is intentional: later tasks reuse files and contracts from earlier
tasks. Do not dispatch siblings in parallel.

### Task USC1: Enforce skill-level retrieval quality

**Files:**
- Modify: `crates/spur-core/src/explore/serving.rs`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs`
- Modify: `crates/spur-core/tests/fixtures/skills_catalog_queries.json`

- [ ] **Step 1: Add failing query-contract fixtures and tests**

Extend the fixture cases with explicit obligations:

```rust
#[derive(Debug, Deserialize)]
struct RetrievalCase {
    id: String,
    query: String,
    acceptable_skill_ids: BTreeSet<String>,
    expected_rank_one: Option<String>,
    expected_error: Option<String>,
    require_distinct_skills: bool,
    source: Option<String>,
}
```

Add cases for the long systematic-debugging query, exact foundation names,
duplicate PageIndex sections from one skill, and `"a and for with"`. Assert
rank one, top-five inclusion, unique `skill_id` values, or `invalid_query` per
case. Make `evaluate_retrieval` exercise both `search` and `navigate`; retain
metrics only as diagnostics.

- [ ] **Step 2: Run the focused test and record the expected failure**

Run:

```bash
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp -- --nocapture
```

Expected: failure showing duplicate navigate skill IDs and/or stopword-derived
results under the current raw-node ranking.

- [ ] **Step 3: Commit the failing regression**

```bash
git add crates/spur-core/tests/skills_catalog_mcp.rs crates/spur-core/tests/fixtures/skills_catalog_queries.json
git commit -m "test(spur-core): USC1 lock catalog retrieval invariants"
```

- [ ] **Step 4: Implement shared meaningful-token analysis and skill-level ranking**

Refactor `search` and `navigate` around shared private types shaped as:

```rust
struct AnalyzedQuery {
    normalized: String,
    meaningful_tokens: Vec<String>,
}

struct RankedSkillNode {
    state_index: usize,
    node_index: usize,
    exact_name_phrase: bool,
    name_token_matches: usize,
    description_token_matches: usize,
    bm25: f64,
}
```

The analyzer must remove a versioned static English stopword set and reject an
empty meaningful-token vector. Ranking must retain only the best node per
`state_index`, then sort by exact contiguous normalized name/alias phrase,
name-token coverage, description-token coverage, BM25, identity key, and node
ID. Apply `take(limit)` only after per-skill collapse. `search` uses the same
skill order and omits node details; `navigate_root` remains unchanged.

- [ ] **Step 5: Re-run focused tests and commit the implementation**

Run:

```bash
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-core explore::serving::tests
```

Expected: all selected tests pass, including unique skill IDs, exact-name rank
one, and stopword-only invalidation.

```bash
git add crates/spur-core/src/explore/serving.rs
git commit -m "feat(spur-core): USC1 rank catalog results by distinct skill"
```

### Task USC2: Make catalog-only exact-one and add foundation rollback

**Depends on:** USC1

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-core/src/skills/projection/mod.rs`
- Modify: `crates/spur-core/src/skills/projection/resolver.rs`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs`
- Modify: `docs/user-docs/05-configuration.md`

- [ ] **Step 1: Add failing configuration and resolver tests**

Add `SkillsProjectionMode::FoundationOnly` round-trip/default tests and resolver
tests that assert:

```rust
assert_eq!(
    resolve_with_policy(SelectionPolicy::CatalogOnly)
        .iter()
        .map(|skill| skill.payload.id.as_str())
        .collect::<Vec<_>>(),
    vec!["skills-catalog"],
);
assert_eq!(
    resolve_with_policy(SelectionPolicy::FoundationOnly).len(),
    FOUNDATION_SKILL_IDS.len(),
);
```

Keep existing bootstrap symlink/frontmatter/integrity failures under
`CatalogOnly`; move ten-foundation inclusion assertions to `FoundationOnly`.

- [ ] **Step 2: Run and commit the failing tests**

Run:

```bash
scripts/spur-cargo test -p spur-acp skills_projection_mode
scripts/spur-cargo test -p spur-core skills::projection::resolver::tests
```

Expected: failure because `foundation_only` is not accepted and catalog-only
still resolves ten entries.

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-core/src/skills/projection/resolver.rs crates/spur-core/tests/skills_catalog_mcp.rs
git commit -m "test(skills): USC2 specify exact-one catalog projection"
```

- [ ] **Step 3: Implement the three-mode selection contract**

Add the enum variants and mapping:

```rust
pub enum SelectionPolicy {
    AllActive,
    FoundationOnly,
    CatalogOnly,
}

pub enum SkillsProjectionMode {
    AllActive,
    FoundationOnly,
    #[default]
    CatalogOnly,
}
```

Split resolver behavior into `resolve_required_bootstrap` for `CatalogOnly` and
the existing ten-entry loop for `FoundationOnly`. Preserve strict bootstrap
validation and the complete `FOUNDATION_SKILL_IDS` constant for catalog
protection and rollback.

- [ ] **Step 4: Document semantics and verify**

Update configuration examples and accepted-value lists. Run:

```bash
scripts/spur-cargo test -p spur-acp skills_projection_mode
scripts/spur-cargo test -p spur-core skills::projection
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
```

Expected: all selected tests pass and the default projection resolves exactly
one skill.

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-core/src/skills/projection/mod.rs crates/spur-core/src/skills/projection/resolver.rs docs/user-docs/05-configuration.md
git commit -m "feat(skills): USC2 project one catalog bootstrap by default"
```

### Task USC3: Build the unified provider snapshot

**Depends on:** USC2

**Files:**
- Create: `crates/spur-core/src/explore/providers.rs`
- Modify: `crates/spur-core/src/explore/mod.rs`
- Modify: `crates/spur-core/src/explore/serving.rs`
- Modify: `crates/spur-core/src/mcp/skills_catalog.rs`
- Modify: `crates/spur-core/src/worker_server.rs`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs`
- Modify: `crates/spur-core/tests/tool_catalog.rs`

- [ ] **Step 1: Add failing provider and MCP parity tests**

Construct a test snapshot with one explicit host root containing a
`systematic-debugging` fixture. Assert brain and worker registries return it at
rank one for the long query, use the same revision, read exact bytes, reject a
protected `spur-way` collision, reject symlinked provider roots, and revoke an
old opaque reference when the provider disappears.

- [ ] **Step 2: Run and commit the failing tests**

Run:

```bash
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-core --test tool_catalog
```

Expected: compile/test failure because provider snapshots and provider-aware
registry constructors do not exist.

```bash
git add crates/spur-core/tests/skills_catalog_mcp.rs crates/spur-core/tests/tool_catalog.rs
git commit -m "test(spur-core): USC3 specify unified catalog providers"
```

- [ ] **Step 3: Implement immutable provider validation**

Create the approved types:

```rust
pub const PROVIDER_SNAPSHOT_SCHEMA_VERSION: u32 = 1;

pub struct CatalogProviderSnapshot {
    pub schema_version: u32,
    pub adapter: Adapter,
    pub isolation: CatalogIsolation,
    pub sources: Vec<CatalogProviderSource>,
}

pub enum CatalogIsolation { Strict, SpurManagedOnly }
pub enum ProviderAuthority { HostSystem, HostUser, HostPlugin }
```

Validate unique provider IDs, canonical non-symlink directory roots, schema
version, and protected-name ownership. Do not accept arbitrary request-time
paths from MCP callers.

- [ ] **Step 4: Merge providers into serving and MCP construction**

Add `ServingCatalog::load_with_providers` and provider-aware brain/worker module
constructors. Reuse `build_state`, verified inventories, eligibility, opaque
references, revision hashing, and `read_verified_text`; include provider ID and
authority in the revision and deterministic identity key. Keep
`ServingCatalog::load` as the repository-only fallback used by existing tests.

- [ ] **Step 5: Verify and commit**

Run:

```bash
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p spur-core --test tool_catalog
scripts/spur-cargo test -p spur-core explore::serving::tests
```

Expected: provider discovery/read/revocation tests pass without weakening
existing Explore gate and confinement tests.

```bash
git add crates/spur-core/src/explore/providers.rs crates/spur-core/src/explore/mod.rs crates/spur-core/src/explore/serving.rs crates/spur-core/src/mcp/skills_catalog.rs crates/spur-core/src/worker_server.rs
git commit -m "feat(spur-core): USC3 serve explicit host skill providers"
```

### Task USC4: Enforce strict adapter isolation at launch

**Depends on:** USC3

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`
- Modify: `crates/spur-core/src/skills/projection/mod.rs`
- Modify: `crates/spur-core/src/skills/projection/generation.rs`
- Modify: `crates/spur-core/src/skills/projection/reconcile.rs`
- Modify: `crates/spur-core/src/orchestrator/connection.rs`
- Modify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
- Modify: `crates/spur-core/src/worker_server.rs`
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs`

- [ ] **Step 1: Add failing isolation capability tests**

Add tests proving that Codex and `SpurHermetic` catalog-only launches build a
strict provider snapshot before projection, expose only the bootstrap in the
controlled adapter target, preserve captured host roots in the MCP catalog, and
pass the controlled launch environment to the ACP process. Add a conflicting
unowned project skill target and assert typed
`StrictCatalogIsolationUnavailable` without deleting it. Assert unsupported
adapter capability cannot report `Strict`.

- [ ] **Step 2: Run and commit the failing tests**

Run:

```bash
scripts/spur-cargo test -p spur-core strict_catalog
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
```

Expected: failure because provider capture and strict launch environment are not
connected to projection or MCP registration.

```bash
git add crates/spur-core/tests/skills_catalog_mcp.rs crates/spur-core/src/orchestrator/connection.rs crates/spur-core/src/orchestrator/delegation/worker_attempt.rs
git commit -m "test(orchestrator): USC4 lock strict catalog isolation"
```

- [ ] **Step 3: Implement the adapter isolation contract**

Add adapter APIs returning captured native provider roots, isolation
capability, and controlled environment overrides. Capture roots before
reconciliation. For Codex, construct an immutable runtime home for the
generation and set `CODEX_HOME` for the launched process; its skills directory
contains only the rendered bootstrap. For `SpurHermetic`, use the generation's
`.spur/skills` target. Never delete unowned targets; strict mode reports their
paths and aborts.

Add a default-on strict flag scoped to catalog-only mode:

```toml
[skills]
projection_mode = "catalog_only"
strict_catalog_surface = true
```

`foundation_only` and `all_active` ignore strict exact-one enforcement and
remain rollback paths.

- [ ] **Step 4: Thread one snapshot through launch and MCP registration**

Brain, adhoc, and worker launch paths must build one snapshot, pass it into
projection/isolation, and construct the rooted catalog MCP module with the same
value. Do not write a process-global active-adapter manifest. Preserve existing
projection ordering: profile and adapter resolution, provider capture,
projection, MCP/ACP construction, initialize/session creation.

- [ ] **Step 5: Verify and commit**

Run:

```bash
scripts/spur-cargo test -p spur-core strict_catalog
scripts/spur-cargo test -p spur-core orchestrator::connection::tests
scripts/spur-cargo test -p spur-core orchestrator::delegation::worker_attempt::tests
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
```

Expected: strict Codex/hermetic tests pass, collision tests preserve user bytes,
and unsupported adapters report an explicit capability failure.

```bash
git add crates/spur-core/src/skills/adapters.rs crates/spur-core/src/skills/projection crates/spur-core/src/orchestrator/connection.rs crates/spur-core/src/orchestrator/delegation/worker_attempt.rs crates/spur-core/src/worker_server.rs crates/spur-acp/src/config/mod.rs
git commit -m "feat(orchestrator): USC4 isolate catalog-only skill surfaces"
```

### Task USC5: Route foundations and restore the full quality gate

**Depends on:** USC4

**Files:**
- Modify: `assets/skills/skills-catalog/SKILL.md`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs`
- Modify: `crates/spur-core/tests/fixtures/skills_catalog_queries.json`
- Modify: `docs/user-docs/05-configuration.md`

- [ ] **Step 1: Update the compact bootstrap routing contract**

Replace ten-foundation preload assumptions with the exact-name routing table
from the design. Require `skill_navigate` before `skill_read`, prohibit treating
ledes as instructions, require exact opaque IDs from current hits, and describe
the `foundation_only` and `all_active` rollback modes. Keep the bootstrap small;
do not inline the nine instruction bodies.

- [ ] **Step 2: Add end-to-end acceptance assertions**

In one rooted brain and one rooted worker fixture, assert:

1. catalog-only projection renders exactly `skills-catalog`;
2. all ten protected foundations are exact-name rank one and readable;
3. the host `systematic-debugging` fixture is rank one for the long query;
4. navigate IDs are unique and stopword-only input is invalid;
5. search, navigate, and read report one catalog revision;
6. `foundation_only` renders ten bundled foundations;
7. `all_active` renders the full accepted set;
8. removing a provider or changing bytes revokes/stales prior references;
9. catalog operations create no files in provider or worker skill roots.

- [ ] **Step 3: Run formatting and focused suites**

Run:

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-acp
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp -- --nocapture
scripts/spur-cargo test -p spur-core --test tool_catalog
```

Expected: all commands pass and emitted metrics accompany enforceable per-case
assertions.

- [ ] **Step 4: Run the repository quality gate with native/default parameters**

Run exactly through the wrapper:

```bash
scripts/spur-cargo fmt --all -- --check
SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings
scripts/spur-cargo test -p spur-acp
scripts/spur-cargo test -p spur-core
scripts/spur-cargo test -p spur-tui
```

Expected: formatting, workspace clippy, and all three crate suites pass. A
remote failure is authoritative and must be fixed rather than re-run locally.

- [ ] **Step 5: Commit the completed rollout**

```bash
git add assets/skills/skills-catalog/SKILL.md crates/spur-core/tests/skills_catalog_mcp.rs crates/spur-core/tests/fixtures/skills_catalog_queries.json docs/user-docs/05-configuration.md
git commit -m "feat(skills): USC5 enable unified single-bootstrap catalog"
```

## Plan self-review checklist

- Every design acceptance criterion maps to USC1-USC5.
- Shared files occur only along the declared dependency chain.
- Every behavior change starts with a failing test commit.
- All build/test commands use `scripts/spur-cargo`; no bare Cargo command appears.
- Strict isolation never deletes unowned user files.
- Unsupported adapters fail honestly instead of claiming exact-one behavior.
- The existing read authorization, integrity, revocation, and confinement model
  remains required.
