# Skills Catalog MCP Independent Final Review

**Review date:** 2026-07-31 UTC / 2026-08-01 Asia/Ho_Chi_Minh

**Plan:** `c178305c-74a4-4a76-b93e-5f8b2544f049`

**Reviewed range:** `dca45c3dc56b83539c6b2b246f14de81c5896823..95ab98241fe77e2fa285fcea40f2d4e63d211f66`

**Verdict:** `block_rollout`

Do not enable `catalog_only` outside controlled development/evaluation, do not make it the default, and do not retire `all_active`. The integrated code has substantial fail-closed and rollback coverage, and a live Notebook MCP parse confirms all four native `ns_mermaid` cells, 20/20 matched obligations, and the four required report hashes. Two mandatory acceptance gates remain unsatisfied: the retrieval evaluation has no enforced baseline thresholds, and four of the five exact fresh review commands did not reach Cargo because repository synchronization failed. The Notebook MCP check parsed and confirmed the persisted proof results; it did not freshly re-execute the cells, and worker-reported Rust test summaries do not replace the missing independent Cargo checks.

## Findings

### Blocker — the exact fresh Rust quality gate did not run

`scripts/spur-cargo fmt --all -- --check` passed. The required remote clippy command and all three required test commands failed during source synchronization, before Cargo or rustc ran:

```text
rsync: .claude/skills/marketing-ab-testing: stat: No such file or directory
remote cargo exited 23
```

The path is a tracked dangling symlink to `../../marketing/marketingskills/skills/ab-testing`; the target tree is absent. This is outside the feature diff, but it still prevents independent final verification. The same infrastructure limitation was recorded during the earlier SC6 review, so prior worker summaries are not a substitute for fresh evidence.

Required before re-review: make `scripts/spur-cargo` synchronize a worktree containing tracked dangling symlinks safely (or restore the intended tracked target), then rerun every exact command below to a Cargo-produced result. No coverage report was produced, so the repository-level `>80%` coverage criterion is not confirmed. No independent cyclomatic-complexity measurement was produced either.

### High — SC6 measures retrieval quality but never gates a regression

The plan requires SC6 to “Assert documented release thresholds; if a new threshold is required, persist a solver result and record its ID.” The implementation calculates recall@5, precision@5, MRR, zero-result rate, and refinement recovery in [`crates/spur-core/tests/skills_catalog_mcp.rs`](../../../crates/spur-core/tests/skills_catalog_mcp.rs#L263-L373), then only prints the resulting object at [lines 837-850](../../../crates/spur-core/tests/skills_catalog_mcp.rs#L837-L850). There is no frozen expected-metrics object, approved baseline file, minimum threshold, or assertion against any of those values. No worker recorded a threshold solve ID.

This is not merely missing reporting. For positive fixture cases, the evaluator increments counters when an acceptable result is present but does not assert that one is present or that its rank meets a bound. A non-empty but irrelevant result set can satisfy `expected_zero_results=false` while recall, precision, and MRR fall. Refinement recovery is also counted without an acceptance assertion. Consequently the integration test may stay green through a retrieval-quality regression.

The fixture covers exact, conceptual, source-filtered, refinement, no-match, revoked, and shadow cases, but it does not establish the approved frozen baseline, activation precision, or downstream task outcome required before changing defaults. The user documentation correctly says missing retrieval or observation evidence keeps `all_active`; the executable gate must enforce the same rule.

Required before re-review: approve and persist a baseline, solve and record any newly selected numeric threshold as required by the plan, encode the expected metric values/thresholds in the fixture or a versioned baseline artifact, and fail the test when any gated metric regresses. Runtime activation precision, downstream outcomes, token use, and the observation window remain later operational evidence; they cannot be inferred from this unit fixture.

### Medium — filesystem validation and use are not atomic

The serving read path checks `symlink_metadata`, size, canonical containment, and then performs a separate path-based `std::fs::read` in [`read_verified_text`](../../../crates/spur-core/src/explore/serving.rs#L1019-L1087). A mutable path can be replaced after the metadata/canonicalization checks but before the open. The resource SHA and whole-directory hash prevent mismatched bytes from being returned under ordinary collision resistance, which meaningfully limits impact, but they do not prevent the process from opening or reading a replaced outside file, FIFO, or unexpectedly large file before rejecting it. That leaves a local race/denial-of-service window and means confinement is not enforced on the exact opened object.

The catalog-only projection has a related provenance race. [`resolve_catalog_bootstrap`](../../../crates/spur-core/src/skills/projection/resolver.rs#L140-L267) validates and reads `SKILL.md`, then `resolve_candidate` hashes the source directory separately. If the bootstrap changes between those operations, the parsed payload and recorded source digest can describe different versions; generation renders from the earlier payload while copying supporting assets from the later path.

Required hardening: open the resource once with no-follow semantics, validate metadata/containment for that opened handle where the platform permits, cap the actual read to `MAX_TEXT_CONTENT_BYTES + 1`, and hash/decode those same bytes. For projection, bind the parsed payload and source digest to one stable snapshot or recheck that the source digest still matches immediately before publishing. Add deterministic mutation/race regressions around the seam.

### Low — gate-verdict policy is duplicated across production paths

The approved verdict set `clean | overridden | replaced-bundled` is repeated in:

- `crates/spur-core/src/explore/mod.rs:42`;
- `crates/spur-core/src/explore/materialize.rs:585`;
- `crates/spur-core/src/skills/projection/resolver.rs:89-92`;
- `crates/spur-core/src/explore/serving.rs:780-782`.

The strings currently agree, and the TUI consumes `ServingCatalog::decision` rather than reimplementing eligibility, so no present cross-surface exposure was found. The duplication is nevertheless security-policy drift risk: a future verdict change could make Explore adoption, projection, and MCP serving disagree. Move the decision to one typed/pure helper and exercise it from each consumer.

### Low — ranking and precedence knobs need explicit rationale

The externally visible limits are handled well: search default/max `5` are named and documented, `MAX_TEXT_CONTENT_BYTES=262144` is named and backed by `sol_ece03f4a166e4004`, and the TUI width `5` is named and backed by `sol_26a4532a782743e3`. The BM25 parameters (`K1=1.2`, `B=0.75`) are named but not tied to the frozen retrieval baseline, while local/global/bundled/replacement precedence is encoded with raw `u8` values `0..3`. Document these choices and replace precedence numbers with a typed ordering before tuning the policy. Also consider a query-length and aggregate resource-count/byte bound; current per-resource limits do not bound total scan cost.

## Mandatory proof evidence

On the SC8 retry, a fresh live Notebook MCP check called `notebook_context_pack` and then `notebook_get_notebook` for `/Volumes/Projects/spur/docs/superpowers/specs/2026-07-31-skills-catalog-mcp-design.ipynb`. The returned notebook model parsed four native `ns_mermaid` cells and confirmed 20/20 matched obligations, 0 mismatched, 0 inconclusive, with every required report hash. This is live MCP parsing and confirmation of the proof outputs, not fresh cell re-execution; the proof relations establish the encoded policy, not Rust conformance or operational rollout readiness.

| Contract | Cell / version | Confirmed source hash | MCP-confirmed result | Required report hash | Live Notebook MCP |
|---|---|---|---|---|---|
| `SKILLS-CATALOG-ARCHITECTURE` | `cfb2feda-f45d-4f8c-b693-f29f175bcccf` / 31 | `5f47c400ba20633ecbb36dc12704d7d9c2a681d5d290554382073d293ea091e6` | 5/5, verified | `bcf1a8f905a3b125a5f1d87a3de2da9187cf4bbde1509d5a839494ea566faa20` | **parsed and confirmed** |
| `SKILL-ELIGIBILITY-POLICY` | `21dee2ad-636a-4c03-8219-b314099ef1c7` / 3 | `642ea8d67d8feac11646d386c00dd0878535ed33e212403d92a97f8d87cf82da` | 5/5, verified | `680e7574ba00239081f67fb8bc6cd11cabacf926921c264dcd0f5dfdb1cfc60d` | **parsed and confirmed** |
| `SKILL-READ-DECISION` | `381669f4-3b9d-4d99-9b8d-7575f14a906c` / 29 | `9a91a6fd5e04817f501573d8042e3a8b8ad2346c2d265d60e3c8b1ea6d0a9497` | 5/5, verified | `90842fabcb998497f4bbe0f766d729e7da4aca6a1a84924f9f578e82e3c7fdbe` | **parsed and confirmed** |
| `CATALOG-ROLLOUT-GATE` | `0d159255-a4e7-4781-99ba-7c55013e0a95` / 29 | `76ce92d199bfd19283b6fa795c74666c87334c52683ba58aedf3ca9775c0d4fb` | 5/5, verified | `d6b1e57cf466b9750c6ecfbf22ac513890c002e4d1e06e237977e5c5a8913082` | **parsed and confirmed** |

## Solver evidence

All plan-mandated and worker-recorded solve IDs were reloaded through `get_solve_result` rather than read directly from storage:

| Solve ID | Status | Review use |
|---|---|---|
| `sol_46039afe656a4dff` | `sat` | Witnesses one bootstrap, approval-gated shared catalog, context-only delivery, lazy text resources, repeated lexical search, and no embedding/agentic retriever. Its result limit `3` is only a feasibility witness, not the API contract. |
| `sol_b57f4c1096ef4a0c` | `unsat` | Excludes an unapproved successful read or task-specific filesystem write in the encoded policy. |
| `sol_677cfaa405324970` | `sat` | Witnesses a valid task order: serving, bootstrap, projection, MCP, TUI, integration, docs, review. |
| `sol_ece03f4a166e4004` | `sat` | Records `max_text_content_bytes=262144`. |
| `sol_26a4532a782743e3` | `sat` | Records `agent_column_width=5`. |

No additional solve ID was present for retrieval thresholds, BM25 tuning, query limits, or precedence.

## Invariant-to-implementation map

| Invariant / review concern | Implementation evidence | Test or proof evidence | Assessment |
|---|---|---|---|
| Single bootstrap | `SelectionPolicy::CatalogOnly` returns `resolve_catalog_bootstrap`; the resolver requires the canonical `skills-catalog` directory, real `SKILL.md`, valid frontmatter, `role: both`, and returns one element (`resolver.rs:70-73`, `140-267`). | Integration assertions at `skills_catalog_mcp.rs:852-871`; MCP-confirmed architecture report; SC1 review correction explains why the conceptual underscore name is invalid. | Code conforms; fresh test unavailable. |
| Context-only delivery / no task-specific writes | `ServingCatalog` and `SkillsCatalogMcpModule` contain no filesystem write API; MCP success and error data use `write_effect=none` (`skills_catalog.rs:50-175`, `285-351`). Projection writes only the one configured bootstrap through the existing reconciler. | Tree snapshots cover successful and failed reads (`skills_catalog_mcp.rs:594-698`, `898-911`); `sol_b57...` and MCP-confirmed architecture/read reports. | Static conformance found; fresh test unavailable; TOCTOU residual above. |
| Eligibility on search and read | Exact pure formula at `serving.rs:36-45`; search uses `eligible_indices` (`356-449`); every read reloads catalog state and checks current eligibility (`452-512`). | Full 32-row truth table (`serving.rs:1384`), revocation integration (`775-790`), MCP-confirmed eligibility report. | Conforms; duplicated verdict helper is a maintenance risk. |
| Unapproved local/global shadowing | Global and merged/local identities are retained separately, same-identity selection preserves an eligible state over an ineligible state, and name selection chooses only eligible candidates (`serving.rs:295-339`, `767-802`). | Unit regressions at `serving.rs:1481-1559`; integration global-approved/local-unapproved assertion at `skills_catalog_mcp.rs:736-752`. | No exposure found. |
| Pinned identity and stale reference | Opaque reference derives from source, relative path, pin, and content hash (`serving.rs:151-171`, `1162-1189`); read reloads and distinguishes stale lineage; descriptor and whole-directory hashes are rechecked (`452-546`, `1019-1087`). | Stale concurrent revision and integrity cases at `skills_catalog_mcp.rs:754-835`; MCP-confirmed read report. | Logical fail-closed behavior found; path operations are not atomic. |
| Path/symlink/resource confinement | Pool components are validated and canonicalized (`serving.rs:719-765`); directory scans reject symlinks, scripts, binary/unsupported/non-UTF-8/oversized content (`840-993`); requested paths reject absolute, traversal, backslash, scripts, and unsupported media (`996-1017`). | Unit/integration traversal, cross-skill, symlink, script, binary, UTF-8, size, and undeclared-resource cases (`skills_catalog_mcp.rs:671-734`). | Broad negative coverage; TOCTOU gap remains. |
| Fail closed / stable errors | Unrooted calls require authority root; malformed inputs map to `-32602`; catalog failures and domain denials return stable error kinds with `write_effect=none` (`skills_catalog.rs:30-47`, `50-157`, `274-351`). | Adapter and rooted-registry tests cover malformed/empty reads, unavailable catalog, and no-write responses. SC3 retry fixed the original wrong `skill_not_found` mapping. | Conforms statically; fresh tests unavailable. |
| Authority preservation | Bootstrap explicitly states normal authority ordering and forbids bypass, filesystem discovery, installation, materialization, script execution, and invented references (`assets/skills/skills-catalog/SKILL.md`). | Integration string checks at `skills_catalog_mcp.rs:912-920`; MCP-confirmed architecture report. | Conforms. |
| Bounded disclosure | Search returns only metadata, filters the eligible view, and takes at most `MAX_SEARCH_LIMIT=5`; full text requires explicit opaque-reference read (`serving.rs:12-14`, `356-449`). | Metadata-no-leak and five-result assertions at `skills_catalog_mcp.rs:594-669`. | API bound conforms; retrieval-quality gate does not. |
| No raw query/content logging | Search telemetry records source filter, revision, count, latency, error kind, and write effect, but not `input.query`; read logs opaque ID/hash/resource, not returned content (`skills_catalog.rs:50-175`, `323-352`). | Logging field tests in `skills_catalog.rs`; user docs enumerate the same fields. | No raw query or instruction-body logging found. |
| Brain/worker registry symmetry | Both rooted production builders compose the same module with the repository root (`mcp/mod.rs:107-132`, `213-242`); worker HTTP routes both tools through the exact registry (`worker_server.rs:1126-1229`, `1872-1931`). | Production registry test at `mcp/mod.rs:529-686`, advertised-name symmetry at `718-727`, live HTTP test at `worker_server.rs:3882-3952`. | No asymmetry found; fresh tests unavailable. |
| Explore/MCP shared policy | TUI imports `ServingCatalog` and caches `ServingDecision`; it does not call MCP or duplicate the eligibility formula (`manage.rs:8-110`, `248-293`, `480-493`). | TUI cases cover eligible, disabled, rejected, and incompatible states plus refresh caching (`manage.rs:612-741`). | Shared facade is correct; fresh TUI test unavailable. |
| Default and rollback | `SkillsProjectionMode::AllActive` is the serde/default variant (`config/mod.rs:503-529`); runtime roles read layered config, while `Init` stays all-active (`projection/mod.rs:170-184`); legacy resolver remains present. | Integration asserts default, explicit opt-in, exact-one projection, and multi-skill rollback (`skills_catalog_mcp.rs:852-896`). User docs give layered opt-in and explicit rollback. | Static rollback path intact; do not change default. |
| No dependency cycle/new crate | The adapter remains in `spur-core` over the existing `spur-core -> spur-mcp` direction. | `git diff --quiet` over all `Cargo.toml` files and `Cargo.lock` returned 0; no dependency manifest changed. | No new dependency or cycle introduced by this range. |
| Rollout four-gate decision | Documentation explicitly keeps `all_active` when any gate lacks evidence and says this release neither changes the default nor retires legacy. | MCP-confirmed rollout report has 5/5 obligations; retrieval thresholds, observation evidence, and fresh Rust results are absent. | `keep_legacy`; rollout blocked. |

## Integrated diff, commits, and beads audit

The integrated range contains 14 files, 4,758 insertions, and 48 deletions. Every SC1-SC7 current task diff and each recorded prior attempt was inspected. Historical rejected attempts had no retained diff; review-feedback comments and current integrated bytes were checked instead.

| Task | Integrated commit | Attempts / audit result |
|---|---|---|
| SC2 serving | `2c580109afc149ccbde9504e000dcfc9d90d808c` | 2; first retry added exact name-token preference and correct unsafe-media denial. |
| SC5 TUI | `e654334be477da78d345c348cf13784cc7efaf54` | 2; first retry removed per-frame filesystem loading and added explicit refresh caching. |
| SC1 bootstrap | `5c1b9e602cd8e637d250ed2bcfaf545bcd4e8696` | 2; plan-wide correction established canonical `skills-catalog` because underscore IDs fail the existing validator. |
| SC3 MCP | `6fc058a3f5f1f3a307874048a28f316acd304e6a` | 3; malformed/empty read mapping fixed. Scope-drift signal `974e34ed-828f-4893-8844-0dfc39638371` was persisted and mutation `257a4d20-e650-4eb4-95ca-9b4568c2a4e2` expanded the worker-server/tool-catalog scope before approval. |
| SC4 projection | `69a7add429787bfd73cea93873eb9d5af12e0cf9` | 2; retry rejected symlinked bootstrap directory/document and added integrity validation. |
| SC6 integration | `409fe7233317e116b850590a13f17158cbd00d89` | 2; retry changed the main-document check to exact byte equality. Retrieval thresholds remained unreviewed and unenforced. |
| SC7 docs | `95ab98241fe77e2fa285fcea40f2d4e63d211f66` | 1; approved. |

Beads currently reports 7/8 reviewed, SC1-SC7 approved, SC8 dispatched, and verified plan-projection freshness. Every approved task has completion and approval audit sentinels. The numerous `potential_clobber` signals were compared against the linear integrated commit range; no missing task file or conflict marker was found, and `git diff --check` passes. The only substantive scope-drift signal was processed before SC3 approval.

The plan/spec conceptual name `skills_catalog` remains textually stale in places, but the persisted plan correction and implementation consistently use the validator-compatible canonical ID `skills-catalog`. This should be reconciled in a later source-spec maintenance change; it does not justify broadening the global ID grammar.

## Fresh command evidence

Commands were rerun from the reviewed worktree during the SC8 retry on `2026-07-31` UTC / `2026-08-01` Asia/Ho_Chi_Minh. Exit 23 results occurred before Cargo and are **not** test failures or passes.

| Command | Result |
|---|---|
| `scripts/spur-cargo fmt --all -- --check` | **PASS**, exit 0, no output. |
| `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-acp -p spur-core -p spur-tui -- -D warnings` | **BLOCKED BEFORE CARGO**, exit 23: rsync could not stat tracked dangling `.claude/skills/marketing-ab-testing`. |
| `scripts/spur-cargo test -p spur-acp config` | **BLOCKED BEFORE CARGO**, exit 23, same rsync failure. |
| `scripts/spur-cargo test -p spur-core --test skills_catalog_mcp` | **BLOCKED BEFORE CARGO**, exit 23, same rsync failure. |
| `scripts/spur-cargo test -p spur-tui views::explore::manage` | **BLOCKED BEFORE CARGO**, exit 23, same rsync failure. |
| `git diff --check dca45c3dc..HEAD` | **PASS**, exit 0. |
| `git diff --quiet dca45c3dc..HEAD -- Cargo.toml Cargo.lock 'crates/*/Cargo.toml'` | **PASS**, exit 0; dependency manifests unchanged. |

No plain `cargo` fallback was used. A local build would create a new heavy target with only about 6.8 GiB free and would not satisfy the exact remote clippy command in any event.

## Rollout recommendation and exit criteria

The encoded `CATALOG-ROLLOUT-GATE` has a concrete `keep_legacy` witness when retrieval is false while the other inputs are true. Current evidence is weaker than that witness: retrieval thresholds are absent, observation is absent, and the fresh security/integration commands did not reach Cargo. The live Notebook MCP proof confirmation satisfies the executable-contract inspection requirement but cannot turn those missing rollout inputs true. The only defensible operational decision is therefore:

```text
retrieval_gate_pass   = false
security_gate_pass    = unverified
integration_gate_pass = unverified
observation_gate_pass = false
decision              = keep_legacy
verdict               = block_rollout
```

Re-review only after all of the following are attached:

1. an approved frozen retrieval baseline with executable threshold assertions and any required persisted solve ID;
2. green results from all five exact SC8 commands after the remote-sync problem is fixed;
3. a decision on the two filesystem TOCTOU seams, preferably with handle-bound/bounded-read hardening and regressions;
4. for any later default change, the separately approved activation/downstream/token/latency/error observation window required by the rollout contract.

Until then, retain `all_active` as both the default and rollback path.
