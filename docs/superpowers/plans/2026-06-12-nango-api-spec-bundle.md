# Nango API Spec Bundle Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-nango-api-spec-bundle-design.md`  
**Design epic:** Direct doc-first design, committed as `237d453cd` and review-updated as `801c0b948`  
**Goal:** Build a deterministic Nango-to-OpenAPI catalog path that can generate reviewed REST table gateway manifests without redistributing unreviewed upstream catalog or spec content.

**Architecture:** Add a crate-local `adapter::catalog` module inside `spur-rest-table-gateway` for provider normalization, APIs.guru snapshot parsing, crosswalk matching, diagnostics, provenance, and generated-manifest assembly. Add a `nango-catalog` binary that reads pinned local inputs and writes deterministic JSON/CSV artifacts; keep runtime scan behavior and the existing `nango-import`/`openapi-import` binaries unchanged.

**Tech Stack:** Rust 2021, `serde`, `serde_json`, `serde_yaml`, `sha2`, existing `adapter::nango`, existing `adapter::openapi`, existing manifest parser, `scripts/spur-cargo`.

---

## File Structure Mapping

- `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`: public module surface, shared structs, diagnostics, provenance enums.
- `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/provider.rs`: Nango provider normalization and seed classification.
- `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/apis_guru.rs`: APIs.guru `list.json` parser and snapshot metadata hashing.
- `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/crosswalk.rs`: provider-to-spec matching rules, aliases, confidence, CSV/JSON row model.
- `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/generate.rs`: reviewed-source manifest generation by combining Nango auth/base metadata with OpenAPI table blocks.
- `crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs`: deterministic CLI for catalog/crosswalk/manifests.
- `crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs`: end-to-end fixture tests for catalog generation and manifest output.
- `crates/spur-notebook/rest-table-gateway/README.md`: document the new command and license/provenance guardrails.
- `crates/spur-notebook/rest-table-gateway/THIRD_PARTY_NOTICES`: record Nango ELv2 and APIs.guru provenance rules for generated artifacts.

## Dependency DAG

```text
task-1-provider-catalog
task-2-apis-guru-snapshot
  \                         /
   \                       /
    -> task-3-crosswalk-engine -> task-4-catalog-cli -> task-5-reviewed-manifest-generation -> task-6-docs-and-verification
```

Tasks 1 and 2 can run independently. Every later task depends on stable interfaces from earlier tasks.

---

### Task 1: Normalize Nango Providers

**Task ID:** `task-1-provider-catalog`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/provider.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `adapter::catalog` is exported from `adapter/mod.rs`.
- [ ] `ProviderCatalogEntry` captures provider key, display name, categories, auth mode, base URL, connection config keys, credential keys, proxy headers/query/body keys, pagination, verification endpoints, authorization URL, token URL, license, and source commit.
- [ ] Provider seed classification returns `BaseUrlOnly`, `RestCollectionLikeDocsEndpoint`, `RestSingletonOrUnknownDocsEndpoint`, `VerificationEndpointOnly`, `GraphqlCandidate`, or `MetadataOnly`.
- [ ] Unit tests cover API key, OAuth2, base-url-only, verification-only, docs endpoint, and GraphQL candidate fixtures.
- [ ] `scripts/spur-cargo test -p spur-rest-table-gateway catalog::provider -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `adapter::catalog` module creation and provider normalization only.
- OUT of scope: APIs.guru parsing, crosswalk matching, CLI, manifest generation, UI changes.
- If provider normalization needs runtime scan changes, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing tests** in `provider.rs` under `#[cfg(test)]`:

```rust
#[test]
fn normalize_provider_keeps_auth_proxy_and_license_metadata() {
    let yaml = r#"
github:
  display_name: GitHub
  categories: [dev-tools]
  auth_mode: OAUTH2
  authorization_url: https://github.com/login/oauth/authorize
  token_url: https://github.com/login/oauth/access_token
  proxy:
    base_url: https://api.github.com
    headers:
      X-GitHub-Api-Version: "2022-11-28"
    verification:
      method: GET
      endpoint: /user
"#;
    let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");
    let github = entries.iter().find(|entry| entry.provider == "github").unwrap();
    assert_eq!(github.base_url.as_deref(), Some("https://api.github.com"));
    assert_eq!(github.auth_mode.as_deref(), Some("OAUTH2"));
    assert_eq!(github.nango_license, "Elastic License 2.0");
    assert_eq!(github.nango_commit, "988efd014");
}
```

- [ ] **Step 2: Verify failure**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::provider -- --nocapture`  
Expected: FAIL because `adapter::catalog::provider` does not exist.

- [ ] **Step 3: Implement provider normalization**

Use existing `adapter::nango::parse_providers` as the parser source. Add catalog structs with `Serialize`/`Deserialize` where output is persisted.

- [ ] **Step 4: Verify pass**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::provider -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/mod.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/provider.rs
git commit -m "feat(rest-gateway): task-1 add nango provider catalog"
```

---

### Task 2: Parse APIs.guru Snapshot Metadata

**Task ID:** `task-2-apis-guru-snapshot`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/apis_guru.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `ApisGuruSnapshot` records retrieval timestamp supplied by the caller, SHA-256 content hash, total API entries, and parsed `ApiSpecSource` rows.
- [ ] Parser handles APIs.guru nested shape: top-level API key, `preferred`, and `versions`.
- [ ] Each source row includes provider-like key, title, version, `swaggerUrl`, source kind `ApisGuru`, format, provenance URL, default license status `NeedsReview`, and initial confidence `Candidate`.
- [ ] Unit tests cover multiple versions, missing preferred version, OpenAPI 2.0, OpenAPI 3.x, and deterministic hash.
- [ ] `scripts/spur-cargo test -p spur-rest-table-gateway catalog::apis_guru -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: parsing caller-provided APIs.guru JSON text and computing deterministic metadata.
- OUT of scope: network fetching, crosswalk matching, manifest generation.
- If a new dependency appears necessary, first verify the crate already has a suitable workspace dependency such as `sha2`.

**Implementation:**
- [ ] **Step 1: Add failing tests** in `apis_guru.rs`:

```rust
#[test]
fn parse_apis_guru_snapshot_flattens_versions_and_hashes_input() {
    let json = r#"{
      "github.com": {
        "preferred": "1.1.4",
        "versions": {
          "1.1.4": {
            "info": {"title": "GitHub v3 REST API"},
            "swaggerUrl": "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json",
            "openapiVer": "3.0.0"
          },
          "1.1.3": {
            "info": {"title": "GitHub v3 REST API"},
            "swaggerUrl": "https://api.apis.guru/v2/specs/github.com/1.1.3/swagger.json",
            "swaggerVersion": "2.0"
          }
        }
      }
    }"#;
    let snapshot = ApisGuruSnapshot::parse(json, "2026-06-12T00:00:00Z").expect("snapshot parses");
    assert_eq!(snapshot.total_entries, 2);
    assert_eq!(snapshot.sources.len(), 2);
    assert!(snapshot.sha256.len() == 64);
    assert!(snapshot.sources.iter().any(|source| source.version.as_deref() == Some("1.1.4")));
}
```

- [ ] **Step 2: Verify failure**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::apis_guru -- --nocapture`  
Expected: FAIL because APIs.guru snapshot support does not exist.

- [ ] **Step 3: Implement snapshot parsing**

Use `serde_json::Value` or typed structs. Do not fetch `https://api.apis.guru/v2/list.json` in tests.

- [ ] **Step 4: Verify pass**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::apis_guru -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/apis_guru.rs
git commit -m "feat(rest-gateway): task-2 parse apis guru snapshots"
```

---

### Task 3: Build Crosswalk and Diagnostics

**Task ID:** `task-3-crosswalk-engine`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/crosswalk.rs`

**Depends on:** `task-1-provider-catalog`, `task-2-apis-guru-snapshot`

**Acceptance Criteria:**
- [ ] `ProviderSpecCrosswalk` includes provider, spec source key, source kind, URL, match confidence, match reasons, license status, Nango commit, APIs.guru hash, and generation eligibility.
- [ ] Matching supports exact key, normalized display name, base URL host overlap, docs URL overlap when available, and manual aliases for at least `github-pat -> github`, `stripe-api-key -> stripe`, `sendgrid-api-key -> sendgrid`, and `twilio -> twilio.com`.
- [ ] Candidate matches are never marked generation-eligible unless license status is `Redistributable` and confidence is `Exact` or `Strong`.
- [ ] Diagnostics summarize providers by seed class, total spec candidates, distinct matched providers, and rejected ambiguous candidates.
- [ ] Unit tests cover exact, alias, host-overlap, candidate-only, and ambiguous-match cases.
- [ ] `scripts/spur-cargo test -p spur-rest-table-gateway catalog::crosswalk -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: pure in-memory crosswalk logic and diagnostics.
- OUT of scope: file I/O, CLI, OpenAPI parsing, generated manifests.
- If matching rules need live APIs.guru data, emit `scope_drift`; tests must stay fixture-based.

**Implementation:**
- [ ] **Step 1: Add failing tests** in `crosswalk.rs`:

```rust
#[test]
fn alias_match_can_be_strong_but_not_generated_without_redistributable_license() {
    let provider = provider_fixture("stripe-api-key", "Stripe", "https://api.stripe.com");
    let source = apis_guru_fixture("stripe.com", "Stripe", "https://api.apis.guru/v2/specs/stripe.com/openapi.json");
    let rows = build_crosswalk(&[provider], &[source], CrosswalkOptions::default());
    assert_eq!(rows[0].confidence, MatchConfidence::Strong);
    assert!(rows[0].match_reasons.iter().any(|reason| reason == "manual_alias"));
    assert!(!rows[0].generation_eligible);
}
```

- [ ] **Step 2: Verify failure**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::crosswalk -- --nocapture`  
Expected: FAIL because crosswalk logic does not exist.

- [ ] **Step 3: Implement matching**

Use deterministic sorting by provider key, source key, confidence rank, then URL. Record every reason used to reach a confidence level.

- [ ] **Step 4: Verify pass**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway catalog::crosswalk -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/crosswalk.rs
git commit -m "feat(rest-gateway): task-3 add provider spec crosswalk"
```

---

### Task 4: Add Deterministic `nango-catalog` CLI

**Task ID:** `task-4-catalog-cli`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs`

**Depends on:** `task-3-crosswalk-engine`

**Acceptance Criteria:**
- [ ] CLI usage supports `nango-catalog <providers.yaml> <apis-guru-list.json> <out_dir> --nango-commit <sha> --apis-guru-fetched-at <timestamp>`.
- [ ] CLI writes deterministic `provider_harvest_candidates.csv`, `table_seed_classes.csv`, `apis_guru_crosswalk.csv`, `provider_spec_crosswalk.json`, and `coverage_summary.json`.
- [ ] CLI refuses to run without `--nango-commit` and `--apis-guru-fetched-at`.
- [ ] Output JSON contains Nango ELv2 license metadata and APIs.guru SHA-256 metadata.
- [ ] E2E test uses temp fixtures and verifies output row counts and stable ordering.
- [ ] `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: local file input and deterministic artifact output.
- OUT of scope: network downloads, checking in generated artifacts, UI changes, GraphQL generation.
- If output format grows beyond the listed files, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Add failing E2E test**:

```rust
#[test]
fn nango_catalog_cli_writes_deterministic_crosswalk_outputs() {
    let bin = env!("CARGO_BIN_EXE_nango-catalog");
    let temp = tempfile::tempdir().expect("tempdir");
    let providers = temp.path().join("providers.yaml");
    let apis = temp.path().join("list.json");
    let out = temp.path().join("out");
    std::fs::write(&providers, PROVIDERS_FIXTURE).unwrap();
    std::fs::write(&apis, APIS_GURU_FIXTURE).unwrap();

    let status = std::process::Command::new(bin)
        .arg(&providers)
        .arg(&apis)
        .arg(&out)
        .arg("--nango-commit")
        .arg("988efd014")
        .arg("--apis-guru-fetched-at")
        .arg("2026-06-12T00:00:00Z")
        .status()
        .expect("run nango-catalog");

    assert!(status.success());
    assert!(out.join("provider_spec_crosswalk.json").exists());
    assert!(out.join("coverage_summary.json").exists());
}
```

- [ ] **Step 2: Verify failure**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture`  
Expected: FAIL because `nango-catalog` does not exist.

- [ ] **Step 3: Implement CLI**

Use explicit argument parsing consistent with `src/bin/nango-import.rs`; avoid adding a new CLI dependency.

- [ ] **Step 4: Verify pass**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs \
  crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs
git commit -m "feat(rest-gateway): task-4 add nango catalog cli"
```

---

### Task 5: Generate Reviewed Spec-Backed Manifests

**Task ID:** `task-5-reviewed-manifest-generation`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/generate.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs`

**Depends on:** `task-4-catalog-cli`

**Acceptance Criteria:**
- [ ] CLI supports `--reviewed-source <provider>=<spec-path>` for local reviewed OpenAPI files.
- [ ] Reviewed-source generation combines `provider_to_manifest_stub` output with `openapi::spec_to_tables` and `openapi::tables_to_toml`.
- [ ] Generated manifests are written under `<out_dir>/connections/<provider>.connection.toml`.
- [ ] Generated TOML reparses with `Manifest::from_toml`.
- [ ] Generation is blocked for `NeedsReview`, `UrlOnly`, `Blocked`, or `Candidate` crosswalk rows unless an explicit reviewed source is supplied.
- [ ] E2E test verifies a GitHub-like fixture produces one parseable manifest with one table.
- [ ] `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture` passes.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: reviewed local OpenAPI file generation only.
- OUT of scope: fetching specs by URL, writing tier-a curated manifests, UI integration, GraphQL generation.
- If generated manifest behavior requires changes in `adapter::openapi` or `adapter::manifest`, emit `scope_drift`.

**Implementation:**
- [ ] **Step 1: Extend E2E test with reviewed OpenAPI fixture**

```rust
#[test]
fn nango_catalog_cli_generates_parseable_reviewed_manifest() {
    let manifest = run_catalog_with_reviewed_source_fixture("github", OPENAPI_COLLECTION_FIXTURE);
    let parsed = spur_rest_table_gateway::adapter::manifest::Manifest::from_toml(&manifest)
        .expect("generated manifest parses");
    assert_eq!(parsed.source.name, "github");
    assert_eq!(parsed.tables.len(), 1);
}
```

- [ ] **Step 2: Verify failure**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture`  
Expected: FAIL because reviewed-source generation does not exist.

- [ ] **Step 3: Implement generation**

Make the reviewed source path the license gate. Do not infer redistributability from APIs.guru alone.

- [ ] **Step 4: Verify pass**

Run: `scripts/spur-cargo test -p spur-rest-table-gateway --test nango_catalog_e2e -- --nocapture`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/src/adapter/catalog/generate.rs \
  crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs \
  crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs \
  crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs
git commit -m "feat(rest-gateway): task-5 generate reviewed api manifests"
```

---

### Task 6: Document and Verify the Catalog Workflow

**Task ID:** `task-6-docs-and-verification`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/README.md`
- Modify: `crates/spur-notebook/rest-table-gateway/THIRD_PARTY_NOTICES`
- Modify: `docs/superpowers/specs/2026-06-12-nango-api-spec-bundle-design.md`

**Depends on:** `task-5-reviewed-manifest-generation`

**Acceptance Criteria:**
- [ ] README documents `nango-catalog` command usage, required pinned inputs, generated outputs, and reviewed-source behavior.
- [ ] THIRD_PARTY_NOTICES mentions Nango ELv2 provider metadata and APIs.guru mixed-provenance OpenAPI metadata.
- [ ] Source spec status is updated from `Draft design` to `Implemented plan submitted` or equivalent.
- [ ] Full crate tests pass with `scripts/spur-cargo test -p spur-rest-table-gateway`.
- [ ] Formatting passes with `scripts/spur-cargo fmt --all`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: docs and final verification only.
- OUT of scope: adding new features, changing UI, checking in generated provider catalogs.
- If verification fails due to implementation defects from prior tasks, report exact failing task area instead of making broad refactors.

**Implementation:**
- [ ] **Step 1: Update docs after code exists**

Document this command shape:

```bash
scripts/spur-cargo run -p spur-rest-table-gateway --bin nango-catalog -- \
  resources/nango/packages/providers/providers.yaml \
  .spur/vendor/apis-guru/list.json \
  .spur/nango-catalog \
  --nango-commit 988efd014 \
  --apis-guru-fetched-at 2026-06-12T00:00:00Z \
  --reviewed-source github=crates/spur-notebook/rest-table-gateway/specs/tier-a/github.json
```

- [ ] **Step 2: Verify docs mention license gates**

Run: `rg -n "Elastic License 2.0|APIs.guru|reviewed-source|nango-catalog" crates/spur-notebook/rest-table-gateway/README.md crates/spur-notebook/rest-table-gateway/THIRD_PARTY_NOTICES`

- [ ] **Step 3: Run final verification**

Run: `scripts/spur-cargo fmt --all`  
Run: `scripts/spur-cargo test -p spur-rest-table-gateway`

- [ ] **Step 4: Commit**

```bash
git add crates/spur-notebook/rest-table-gateway/README.md \
  crates/spur-notebook/rest-table-gateway/THIRD_PARTY_NOTICES \
  docs/superpowers/specs/2026-06-12-nango-api-spec-bundle-design.md
git commit -m "docs(rest-gateway): task-6 document nango catalog workflow"
```

---

## Self-Review

- Spec coverage: Phase 1 catalog/crosswalk is covered by Tasks 1 through 4. Phase 2 reviewed manifest generation is covered by Task 5. Provenance/license handling is covered by Tasks 2, 4, 5, and 6. UI preview and GraphQL remain outside this first implementation, matching the source spec's recommended boundary.
- Placeholder scan: the plan uses no unresolved placeholder markers.
- Type consistency: `ProviderCatalogEntry`, `ApiSpecSource`, `ProviderSpecCrosswalk`, `LicenseStatus`, and `MatchConfidence` are introduced before downstream tasks use them.
- DAG validation: Tasks 1 and 2 are independent roots; Task 3 depends on both; Tasks 4, 5, and 6 are sequential because each consumes the previous task's interface or command.
- beads compatibility: each task has a unique ID, dependencies, acceptance criteria, suggested worker, scope boundary, and verifiable command.
