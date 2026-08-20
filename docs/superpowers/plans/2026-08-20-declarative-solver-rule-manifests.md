# Declarative Solver Rule Manifests Implementation Plan

**Source spec:** `docs/superpowers/specs/2026-08-20-declarative-solver-rule-manifests-design.ipynb`
**Formal @spec:** `SOLVER-RULE-MANIFEST-ROUTING`
**Design epic:** `bd-2bc` (approved and closed)
**Implementation epic:** `bd-29b`
**Plan label:** `spur:plan-id:587cb35a-08df-426e-9595-a3d957e9462d`

**Goal:** Move built-in solver rule catalog metadata, contracts, public examples, and conformance vectors into strict versioned YAML manifests while retaining Rust-native semantic compilation, fact normalization, solver lowering, diagnostics, and persistence.

**Architecture:** A shared `manifest_format.rs` defines strict v1 DTOs and closed enums. `build.rs` discovers and validates sorted YAML sources, emits a canonical JSON bundle to `OUT_DIR`, and runtime code converts the embedded bundle into the existing `RuleRegistry`. A closed `NativeHandlerV1` selects exhaustive Rust dispatch; a shared validator handles manifest-representable request contracts before native semantic checks. Public catalog examples remain byte-for-byte compatible with current `solve_rule_spec` output, while separate conformance vectors drive executable tests.

**Tech stack:** Rust 2021, Serde/serde_json at runtime, serde_yml as build/dev dependency only, YAML source manifests, existing `ConstraintExpr`/Z3 execution path, `scripts/spur-cargo` for all build and test commands.

---

## Task 1: Freeze the current serialized built-in catalog

**Task ID:** `catalog-golden`
**Beads issue:** `bd-qve`
**Files:**

- Create: `crates/spur-solver/tests/rule_manifest_equivalence.rs`
- Create: `crates/spur-solver/tests/fixtures/builtin_rule_catalog_v1.json`

**Depends on:** none
**Suggested worker:** `codex`

**Scope boundary:** Capture only the existing public catalog projection. Do not introduce manifest types or change production code. Drift checkpoint: if the current registry cannot serialize deterministically, stop and report the unstable fields before normalizing anything.

**Acceptance criteria:**

- The fixture contains the complete sorted `builtin_registry()` family/profile/rule catalog JSON.
- The test compares semantic `serde_json::Value` equality and also verifies stable family/profile/rule ordering.
- The fixture includes all 18 current rules, including catalog-only `rbac.minimum_privilege`.

**Implementation steps:**

1. Add a test that loads `include_str!("fixtures/builtin_rule_catalog_v1.json")` and compares it with `serde_json::to_value(builtin_registry())`; leave the fixture absent and run:

   ```bash
   scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence
   ```

   Expected RED: compilation fails because the fixture does not exist.
2. Generate the fixture once from the current Rust registry, review it for all families/profiles/rules, then keep it immutable through the migration.
3. Re-run the command and require GREEN.
4. Commit with `test(spur-solver): bd-qve freeze built-in rule catalog`.

## Task 2: Define strict v1 manifest DTOs and static validation

**Task ID:** `manifest-format`
**Beads issue:** `bd-2w3`
**Files:**

- Create: `crates/spur-solver/src/rules/manifest_format.rs`
- Create: `crates/spur-solver/tests/rule_manifest_format.rs`
- Create: `crates/spur-solver/tests/support/mod.rs`
- Modify: `crates/spur-solver/src/rules/mod.rs`
- Modify: `crates/spur-solver/Cargo.toml`
- Modify: `Cargo.lock`

**Depends on:** none
**Suggested worker:** `claude-code-acp`

**Scope boundary:** Model and validate source documents only; do not discover files, emit build artifacts, construct `RuleRegistry`, or dispatch handlers. Add `serde_yml` under dev-dependencies here; build-dependency wiring belongs to Task 7. Drift checkpoint: manifests must not contain Rust paths or a generic expression language.

**Acceptance criteria:**

- Strict Serde DTOs reject unknown fields and unsupported `schema_version` values.
- Closed enums cover availability, strength, parameter kinds, subject cardinality, `NativeObjectValidatorV1`, and every `NativeHandlerV1` handler required by the 17 implemented-hard rules.
- `ParameterKindV1` supports integer, boolean, string, string enum, string array, and closed-validator native object.
- Static validation rejects duplicate IDs/handlers, missing owners, availability-handler mismatches, invalid defaults/bounds/enum values, and missing valid/invalid conformance vectors on implemented-hard rules.
- The formal routing truth table is encoded directly by validation: implemented-hard plus handler is executable, non-executable plus no handler is catalog-only, all mismatches reject.

**Implementation steps:**

1. Write table-driven failing tests for strict parsing and each invariant. Include this routing test shape:

   ```rust
   for (availability, strength, handler, expected_ok) in routing_cases() {
       let rule = rule_fixture(availability, strength, handler);
       assert_eq!(validate_rule(&rule).is_ok(), expected_ok);
   }
   ```

2. Run `scripts/spur-cargo test -p spur-solver --test rule_manifest_format` and confirm RED because the DTO/validator API is missing.
3. Implement `ManifestBundleV1`, `FamilyManifestV1`, `RuleManifestV1`, `ParameterContractV1`, `ConformanceVectorsV1`, the closed enums, and pure static validators. Add `NativeHandlerV1::ALL` and `NativeObjectValidatorV1::ALL` constants.
4. Keep `manifest_format` free of runtime registry and YAML-discovery dependencies; expose it only as narrowly as integration-test support requires.
5. Re-run the targeted test and require GREEN.
6. Commit with `feat(spur-solver): bd-2w3 define v1 rule manifest format`.

## Task 3: Extract accessibility manifests

**Task ID:** `accessibility-yaml`
**Beads issue:** `bd-2uy`
**Files:**

- Create: `crates/spur-solver/src/rules/families/accessibility/family.yaml`
- Create: `crates/spur-solver/src/rules/families/accessibility/rules/focus_not_obscured.yaml`
- Create: `crates/spur-solver/src/rules/families/accessibility/rules/reflow.yaml`
- Create: `crates/spur-solver/src/rules/families/accessibility/rules/target_size.yaml`
- Create: `crates/spur-solver/src/rules/families/accessibility/rules/text_contrast.yaml`
- Create: `crates/spur-solver/tests/accessibility_rule_manifests.rs`

**Depends on:** `manifest-format`
**Suggested worker:** `codex`

**Scope boundary:** Transcribe accessibility catalog data and static request contracts only. Do not edit Rust family/compile code. Drift checkpoint: preserve every existing public example and guidance string exactly; model the `exception` object with the closed native validator.

**Acceptance criteria:**

- The family/profile documents and four rules parse and pass v1 static validation.
- Each implemented-hard rule has a unique native handler and valid/invalid conformance vectors.
- Public examples and existing stable IDs match the Rust catalog exactly.

**Implementation steps:**

1. Add a test that expects the accessibility family plus the exact IDs `a11y.focus_not_obscured`, `a11y.reflow`, `a11y.target_size`, and `a11y.text_contrast`; run `scripts/spur-cargo test -p spur-solver --test accessibility_rule_manifests` and confirm RED for missing files.
2. Add the strict YAML manifests, including subject cardinality, parameter contracts, authorities, requirements, public examples, conformance vectors, handler keys, and native-object validator selection.
3. Re-run the targeted test and require GREEN.
4. Commit with `feat(spur-solver): bd-2uy extract accessibility rule manifests`.

## Task 4: Extract design manifests

**Task ID:** `design-yaml`
**Beads issue:** `bd-3qk`
**Files:**

- Create: `crates/spur-solver/src/rules/families/design/family.yaml`
- Create: `crates/spur-solver/src/rules/families/design/rules/axis_capacity.yaml`
- Create: `crates/spur-solver/src/rules/families/design/rules/containment.yaml`
- Create: `crates/spur-solver/src/rules/families/design/rules/non_overlap.yaml`
- Create: `crates/spur-solver/src/rules/families/design/rules/aspect_ratio.yaml`
- Create: `crates/spur-solver/tests/design_rule_manifests.rs`

**Depends on:** `manifest-format`
**Suggested worker:** `codex`

**Scope boundary:** Transcribe design catalog data and static contracts only; retain scene normalization and geometric semantics in Rust. Drift checkpoint: public examples are catalog projections, not full conformance requests.

**Acceptance criteria:**

- The family/profiles and exact rules `layout.axis_capacity`, `layout.containment`, `layout.non_overlap`, and `media.aspect_ratio` validate.
- Handler keys are unique and conformance vectors carry executable scene/fact inputs.
- Existing guidance, authorities, requirements, IDs, versions, and public examples are unchanged.

**Implementation steps:**

1. Add the expected-ID parsing test and run `scripts/spur-cargo test -p spur-solver --test design_rule_manifests`; confirm RED for missing YAML.
2. Add the family and rule manifests with exact catalog values plus separate conformance vectors.
3. Re-run the targeted test and require GREEN.
4. Commit with `feat(spur-solver): bd-3qk extract design rule manifests`.

## Task 5: Extract policy manifests

**Task ID:** `policy-yaml`
**Beads issue:** `bd-3jr`
**Files:**

- Create: `crates/spur-solver/src/rules/families/policy/family.yaml`
- Create: `crates/spur-solver/src/rules/families/policy/rules/dynamic_separation_of_duty.yaml`
- Create: `crates/spur-solver/src/rules/families/policy/rules/minimum_privilege.yaml`
- Create: `crates/spur-solver/src/rules/families/policy/rules/permission_reachable.yaml`
- Create: `crates/spur-solver/src/rules/families/policy/rules/role_hierarchy_acyclic.yaml`
- Create: `crates/spur-solver/src/rules/families/policy/rules/static_separation_of_duty.yaml`
- Create: `crates/spur-solver/tests/policy_rule_manifests.rs`

**Depends on:** `manifest-format`
**Suggested worker:** `codex`

**Scope boundary:** Transcribe policy metadata/contracts only; do not move graph reachability, hierarchy closure, or assignment semantics out of Rust. Drift checkpoint: `rbac.minimum_privilege` remains catalog-only/advisory with no handler and no executable conformance requirement.

**Acceptance criteria:**

- All five exact policy rule IDs validate under the policy family/profile ownership rules.
- The four implemented-hard rules have unique handlers and conformance vectors.
- `rbac.minimum_privilege` has its current unavailability reason and no native handler.

**Implementation steps:**

1. Add the expected-ID and catalog-only routing test; run `scripts/spur-cargo test -p spur-solver --test policy_rule_manifests` and confirm RED.
2. Add strict YAML preserving existing catalog output and encoding executable contracts only for implemented-hard rules.
3. Re-run the targeted test and require GREEN.
4. Commit with `feat(spur-solver): bd-3jr extract policy rule manifests`.

## Task 6: Extract resource manifests

**Task ID:** `resource-yaml`
**Beads issue:** `bd-150`
**Files:**

- Create: `crates/spur-solver/src/rules/families/resource/family.yaml`
- Create: `crates/spur-solver/src/rules/families/resource/rules/aggregate_capacity.yaml`
- Create: `crates/spur-solver/src/rules/families/resource/rules/quota_capacity.yaml`
- Create: `crates/spur-solver/src/rules/families/resource/rules/request_within_limit.yaml`
- Create: `crates/spur-solver/src/rules/families/resource/rules/minimum_failure_domains.yaml`
- Create: `crates/spur-solver/src/rules/families/resource/rules/topology_max_skew.yaml`
- Create: `crates/spur-solver/tests/resource_rule_manifests.rs`

**Depends on:** `manifest-format`
**Suggested worker:** `codex`

**Scope boundary:** Transcribe resource/placement metadata and static contracts only. Keep quantity maps, topology arithmetic, caps, and constraint generation in Rust. Drift checkpoint: preserve the two existing profiles and exact ownership of all five rules.

**Acceptance criteria:**

- The exact IDs `resource.aggregate_capacity`, `resource.quota_capacity`, `resource.request_within_limit`, `placement.minimum_failure_domains`, and `placement.topology_max_skew` validate.
- Each rule has a unique handler and executable valid/invalid conformance vectors.
- Catalog JSON values remain identical to the Rust constructors.

**Implementation steps:**

1. Add the expected-ID/profile ownership test; run `scripts/spur-cargo test -p spur-solver --test resource_rule_manifests` and confirm RED.
2. Add the family and five rule manifests.
3. Re-run the targeted test and require GREEN.
4. Commit with `feat(spur-solver): bd-150 extract resource rule manifests`.

## Task 7: Build and validate the canonical manifest bundle

**Task ID:** `manifest-build`
**Beads issue:** `bd-36f`
**Files:**

- Create: `crates/spur-solver/build.rs`
- Create: `crates/spur-solver/build_support/manifest_source.rs`
- Create: `crates/spur-solver/tests/rule_manifest_build.rs`
- Modify: `crates/spur-solver/Cargo.toml`
- Modify: `Cargo.lock`

**Depends on:** `manifest-format`, `accessibility-yaml`, `design-yaml`, `policy-yaml`, `resource-yaml`
**Suggested worker:** `claude-code-acp`

**Scope boundary:** Implement source discovery, YAML parsing, cross-document validation, canonical ordering, and JSON emission only. Do not expose runtime loader APIs or alter family registries. Drift checkpoint: `serde_yml` must be a build/dev dependency, never a normal runtime dependency.

**Acceptance criteria:**

- `build.rs` recursively discovers only the approved `family.yaml` and `rules/*.yaml` paths in deterministic lexical order.
- Build diagnostics include source path and rule/family context for syntax and invariant failures.
- Cross-family validation enforces ownership and a bijection between implemented handlers and `NativeHandlerV1::ALL`.
- The output is canonical JSON in `OUT_DIR` and every source path has `cargo:rerun-if-changed`.
- Tests cover deterministic ordering and malformed temporary source sets without modifying repository manifests.

**Implementation steps:**

1. Add tests around a reusable `load_manifest_sources(root)` helper and run `scripts/spur-cargo test -p spur-solver --test rule_manifest_build`; confirm RED because the helper/build script is absent.
2. Implement `build_support/manifest_source.rs`, include the shared DTO by path from `build.rs`, validate the complete bundle, and write `spur_rule_manifests_v1.json` to `OUT_DIR`.
3. Add `serde`, `serde_json`, and `serde_yml` build dependencies with workspace-compatible versions.
4. Run the targeted test, then `scripts/spur-cargo check -p spur-solver`; require GREEN.
5. Commit with `feat(spur-solver): bd-36f build canonical rule manifest bundle`.

## Task 8: Load the embedded bundle into runtime catalog types

**Task ID:** `manifest-loader`
**Beads issue:** `bd-1wp`
**Files:**

- Create: `crates/spur-solver/src/rules/manifest.rs`
- Create: `crates/spur-solver/tests/rule_manifest_loader.rs`
- Modify: `crates/spur-solver/src/rules/mod.rs`

**Depends on:** `catalog-golden`, `manifest-build`
**Suggested worker:** `claude-code-acp`

**Scope boundary:** Deserialize embedded canonical JSON, convert DTOs into existing catalog structs, and expose read-only projections. Do not change family registries, MCP schemas, or compilation dispatch yet. Drift checkpoint: conversion must not reinterpret guidance formulas as constraints.

**Acceptance criteria:**

- A lazily initialized embedded bundle converts to the existing `RuleRegistry` successfully.
- APIs expose the full registry, one family registry, executable rule IDs, a rule contract/handler lookup, and conformance vectors.
- Catalog conversion passes the frozen golden fixture exactly.
- Embedded bundle failures produce deterministic initialization messages.

**Implementation steps:**

1. Add loader tests that call `manifest_registry()` and compare its serialized value with the golden fixture; run `scripts/spur-cargo test -p spur-solver --test rule_manifest_loader` and confirm RED.
2. Implement `include_str!(concat!(env!("OUT_DIR"), "/spur_rule_manifests_v1.json"))`, strict JSON deserialization, catalog conversion, and narrow lookup APIs.
3. Run the loader and equivalence tests and require GREEN.
4. Commit with `feat(spur-solver): bd-1wp load embedded rule manifests`.

## Task 9: Replace Rust catalog constructors with manifest projections

**Task ID:** `registry-switch`
**Beads issue:** `bd-1md`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/accessibility.rs`
- Modify: `crates/spur-solver/src/rules/families/design.rs`
- Modify: `crates/spur-solver/src/rules/families/policy.rs`
- Modify: `crates/spur-solver/src/rules/families/resource.rs`
- Modify: `crates/spur-solver/src/rules/families/mod.rs`

**Depends on:** `manifest-loader`
**Suggested worker:** `claude-code-acp`

**Scope boundary:** Replace metadata/profile/rule/example constructors and built-in registry assembly only. Keep compiler adapters and all native semantic functions. Drift checkpoint: do not change exported catalog JSON or compile diagnostics.

**Acceptance criteria:**

- `builtin_registry()` and family registry functions delegate to the manifest loader.
- Obsolete duplicated Rust catalog/example factories are removed.
- The compiler adapter table remains explicit and complete.
- Golden equivalence and existing catalog tests pass without fixture updates.

**Implementation steps:**

1. Change one registry function to use the manifest projection and run `scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence`; use any mismatch as RED evidence before removing constructors.
2. Switch all four families and merged registry, deleting only obsolete metadata/example construction.
3. Run `scripts/spur-cargo test -p spur-solver --test rule_manifest_equivalence --test platform_rule_catalog` and require GREEN.
4. Commit with `refactor(spur-solver): bd-1md source rule catalogs from manifests`.

## Task 10: Derive the public MCP rule enum directly from the registry

**Task ID:** `top-schema-routing`
**Beads issue:** `bd-rxs`
**Files:**

- Modify: `crates/spur-solver/src/mcp.rs`
- Modify: `crates/spur-solver/tests/solve_rules_mcp.rs`
- Modify: `crates/spur-solver/tests/platform_rule_catalog.rs`

**Depends on:** `registry-switch`
**Suggested worker:** `codex`

**Scope boundary:** Change only top-level schema ID sourcing and its tests. Preserve the simple Bedrock-compatible object schema and every unrelated field. Drift checkpoint: do not introduce `oneOf` or expose catalog-only rules as executable.

**Acceptance criteria:**

- `solve_rules_schema()` gets executable rule IDs directly from the validated registry/loader API.
- Nested family schema scraping is removed.
- All 17 implemented-hard IDs appear exactly once; `rbac.minimum_privilege` is absent.
- Existing Bedrock compatibility assertions continue to pass.

**Implementation steps:**

1. Add a test asserting the public enum equals the registry’s executable ID projection and excludes the catalog-only rule; run `scripts/spur-cargo test -p spur-solver --test solve_rules_mcp --test platform_rule_catalog` and confirm RED against the scraping implementation.
2. Replace schema traversal with the direct registry projection while retaining the existing JSON-schema shape.
3. Re-run the targeted tests and require GREEN.
4. Commit with `refactor(spur-solver): bd-rxs derive MCP rule IDs from registry`.

## Task 11: Derive family compiler rule enums from manifests

**Task ID:** `family-schema-routing`
**Beads issue:** `bd-22z`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/accessibility/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/policy/compile.rs`
- Modify: `crates/spur-solver/src/rules/families/resource/compile.rs`
- Create: `crates/spur-solver/tests/family_rule_schema.rs`

**Depends on:** `registry-switch`
**Suggested worker:** `codex`

**Scope boundary:** Replace hard-coded family rule enum arrays only; do not alter parameter schemas or compilation behavior. Design already derives its enum and should only be covered by the test. Drift checkpoint: family schemas may remain family-specific simple objects.

**Acceptance criteria:**

- Every family compiler schema rule enum equals its manifest-backed executable IDs.
- Policy excludes `rbac.minimum_privilege`.
- No hard-coded rule ID array remains in accessibility, policy, or resource schema construction.

**Implementation steps:**

1. Add per-family enum equality tests and run `scripts/spur-cargo test -p spur-solver --test family_rule_schema`; confirm RED where hard-coded schemas diverge from the registry API contract.
2. Route enum construction through the manifest-backed family registry helpers.
3. Re-run the targeted test and require GREEN.
4. Commit with `refactor(spur-solver): bd-22z derive family rule schemas from manifests`.

## Task 12: Add shared manifest contract validation

**Task ID:** `binding-contracts`
**Beads issue:** `bd-3b6`
**Files:**

- Modify: `crates/spur-solver/src/rules/manifest.rs`
- Create: `crates/spur-solver/tests/rule_manifest_contract.rs`

**Depends on:** `manifest-loader`
**Suggested worker:** `claude-code-acp`

**Scope boundary:** Validate only manifest-representable request shape before dispatch. Do not duplicate family fact-dependent, graph, geometric, or solver-variable semantics. Drift checkpoint: native-object validation delegates to a closed Rust validator and must preserve existing accessibility error wording when surfaced by family adapters.

**Acceptance criteria:**

- Validation covers executable availability, subject cardinality, accepted names, required values, defaults, types, integer bounds, enum membership, array length, and native-object structure.
- The result returns normalized parameters plus the closed `NativeHandlerV1`.
- Unknown/catalog-only rule IDs fail before native dispatch.
- Unit tests cover every parameter kind and boundary.

**Implementation steps:**

1. Add failing table tests for missing required fields, unknown fields, defaults, inclusive bounds, array limits, and accessibility exception object validation; run `scripts/spur-cargo test -p spur-solver --test rule_manifest_contract` and confirm RED.
2. Implement a `validate_binding_contract(rule_id, subjects, parameters)` API returning a validated binding/handler without constructing constraints.
3. Re-run the targeted test and require GREEN.
4. Commit with `feat(spur-solver): bd-3b6 validate manifest binding contracts`.

## Task 13: Route accessibility compilation through manifest handlers

**Task ID:** `accessibility-dispatch`
**Beads issue:** `bd-39q`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/accessibility/compile.rs`
- Create: `crates/spur-solver/tests/accessibility_manifest_dispatch.rs`

**Depends on:** `registry-switch`, `family-schema-routing`, `binding-contracts`
**Suggested worker:** `codex`

**Scope boundary:** Add pre-dispatch contract validation and exhaustive accessibility handler matching. Preserve fact normalization, exception semantics, constraint generation, caps, and diagnostics. Drift checkpoint: no YAML field may select an arbitrary Rust symbol.

**Acceptance criteria:**

- All four accessibility IDs resolve through `NativeHandlerV1` and reach their existing native bodies.
- Manifest defaults/contracts are applied before semantic compilation.
- Valid, invalid, and diagnostic regression cases remain behaviorally identical.

**Implementation steps:**

1. Add dispatch tests for all four handlers plus a contract rejection that must not reach native compilation; run `scripts/spur-cargo test -p spur-solver --test accessibility_manifest_dispatch` and confirm RED.
2. Refactor the family compile entry point to call shared validation then exhaustively match only accessibility handler variants.
3. Run the new test plus `scripts/spur-cargo test -p spur-solver --test audited_rule_regressions` and require GREEN.
4. Commit with `refactor(spur-solver): bd-39q route accessibility manifest handlers`.

## Task 14: Route design compilation through manifest handlers

**Task ID:** `design-dispatch`
**Beads issue:** `bd-319`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/design/compile.rs`
- Create: `crates/spur-solver/tests/design_manifest_dispatch.rs`

**Depends on:** `registry-switch`, `binding-contracts`
**Suggested worker:** `codex`

**Scope boundary:** Route through contracts/handlers only; keep scene normalization, selected nodes, geometric formulas, unknown projection, and diagnostics native. Drift checkpoint: the manifest contract must not replace scene-aware validation.

**Acceptance criteria:**

- All four design handlers dispatch exhaustively to existing semantic bodies.
- Static contract failures occur before scene/constraint generation.
- Existing design execution and audited regression behavior is unchanged.

**Implementation steps:**

1. Add one dispatch assertion per handler plus an early contract-failure case; run `scripts/spur-cargo test -p spur-solver --test design_manifest_dispatch` and confirm RED.
2. Introduce validated handler dispatch around the existing native functions.
3. Run the targeted test plus relevant audited/platform execution tests and require GREEN.
4. Commit with `refactor(spur-solver): bd-319 route design manifest handlers`.

## Task 15: Route policy compilation through manifest handlers

**Task ID:** `policy-dispatch`
**Beads issue:** `bd-i5g`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/policy/compile.rs`
- Create: `crates/spur-solver/tests/policy_manifest_dispatch.rs`

**Depends on:** `registry-switch`, `family-schema-routing`, `binding-contracts`
**Suggested worker:** `codex`

**Scope boundary:** Route the four implemented policy rules through closed handlers; preserve graph preprocessing, reachability/hierarchy logic, caps, and diagnostics. Drift checkpoint: `rbac.minimum_privilege` must remain rejected as catalog-only before native dispatch.

**Acceptance criteria:**

- The four implemented policy rules dispatch to existing native logic.
- The catalog-only rule never has a handler and yields the established unavailable behavior.
- Graph/cycle and separation-of-duty regressions pass unchanged.

**Implementation steps:**

1. Add handler coverage and explicit catalog-only rejection tests; run `scripts/spur-cargo test -p spur-solver --test policy_manifest_dispatch` and confirm RED.
2. Wrap existing native policy functions with shared validation and exhaustive handler dispatch.
3. Run the targeted and audited regression tests and require GREEN.
4. Commit with `refactor(spur-solver): bd-i5g route policy manifest handlers`.

## Task 16: Route resource compilation through manifest handlers

**Task ID:** `resource-dispatch`
**Beads issue:** `bd-3cx`
**Files:**

- Modify: `crates/spur-solver/src/rules/families/resource/compile.rs`
- Create: `crates/spur-solver/tests/resource_manifest_dispatch.rs`

**Depends on:** `registry-switch`, `family-schema-routing`, `binding-contracts`
**Suggested worker:** `codex`

**Scope boundary:** Route five resource/placement rules through contracts/handlers; retain map arithmetic, topology semantics, cap checks, constraint generation, and diagnostics. Drift checkpoint: do not move quantity or domain aggregation semantics into YAML.

**Acceptance criteria:**

- All five handlers dispatch exhaustively to existing native bodies.
- Static manifest contracts reject malformed bindings before semantic computation.
- Resource and topology regression behavior remains unchanged.

**Implementation steps:**

1. Add one test per handler plus early contract failure; run `scripts/spur-cargo test -p spur-solver --test resource_manifest_dispatch` and confirm RED.
2. Refactor the entry point to validate then match `NativeHandlerV1` while reusing existing native functions.
3. Run the targeted and audited regression tests and require GREEN.
4. Commit with `refactor(spur-solver): bd-3cx route resource manifest handlers`.

## Task 17: Drive the family-neutral conformance harness from manifests

**Task ID:** `manifest-conformance`
**Beads issue:** `bd-2sa`
**Files:**

- Modify: `crates/spur-solver/tests/platform_rule_conformance.rs`

**Depends on:** `accessibility-dispatch`, `design-dispatch`, `policy-dispatch`, `resource-dispatch`
**Suggested worker:** `codex`

**Scope boundary:** Replace hand-maintained conformance case enumeration with manifest conformance vectors only. Do not alter public catalog examples or solver result semantics. Drift checkpoint: invalid conformance means deterministic compile/solve rejection, not necessarily one universal diagnostic string.

**Acceptance criteria:**

- The harness discovers every implemented-hard rule from the manifest registry.
- Each rule contributes at least one valid case that compiles/executes and one invalid case that is rejected as declared.
- The harness reports missing, duplicate, or extra handler/rule coverage clearly.
- Public catalog examples are untouched.

**Implementation steps:**

1. Add an assertion that the number and IDs of conformance case pairs equals the manifest executable rule set; run `scripts/spur-cargo test -p spur-solver --test platform_rule_conformance` and confirm RED against the hand-maintained list.
2. Deserialize manifest conformance request vectors into the existing request types and feed them through the existing family-neutral execution helpers.
3. Remove duplicated manual case factories only after the manifest-driven assertions pass.
4. Re-run the targeted test and require GREEN.
5. Commit with `test(spur-solver): bd-2sa drive conformance from rule manifests`.

## Task 18: Verify the complete migration

**Task ID:** `final-verification`
**Beads issue:** `bd-qpa`
**Files:** none (verification only)

**Depends on:** `top-schema-routing`, `manifest-conformance`
**Suggested worker:** `codex`

**Scope boundary:** Verification and evidence collection only. Do not edit source or fixtures; report any failure with the owning predecessor task/file. Drift checkpoint: do not weaken assertions or update golden output to make failures pass.

**Acceptance criteria:**

- All spur-solver tests pass.
- Formatting and clippy pass with warnings denied.
- The golden catalog is unchanged, public schemas remain Bedrock compatible, all 17 executable handlers are covered, and the one catalog-only rule is non-executable.

**Implementation steps:**

1. Run:

   ```bash
   scripts/spur-cargo fmt --all -- --check
   scripts/spur-cargo test -p spur-solver
   SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-solver --all-targets -- -D warnings
   ```

2. Record exact command outcomes and the final executable/catalog-only counts in the beads issue.
3. If any command fails, leave the task open and signal the precise predecessor/file needing repair; do not commit verification-only work.
