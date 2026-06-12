# APIs.guru-Backed Provider Fulfillment Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-12-nango-api-spec-bundle-design.md`  
**Design epic:** current Nango/API.guru fulfillment thread  

**Goal:** Make all APIs.guru-backed Nango providers traceable and visible, then promote the next 10 providers to Ready API-as-table/action packages with E2E coverage.

**Architecture:** Keep the full 851-provider Nango catalog as the provider source of truth, keep APIs.guru rows as a separate provenance/status index, and expose provider-level status to the notebook Wizard. Candidate manifests are generated and traceable, but only reviewed manifests under `connections/supported/` are Ready.

**Tech Stack:** Rust (`spur-notebook`, `spur-rest-table-gateway`), TOML manifests, serde JSON/CSV, Jute notebook React Wizard, `wiremock` E2E tests.

---

## Current Grounding

- Full Nango providers: `851` from `resources/nango/packages/providers/providers.yaml`
- APIs.guru crosswalk: `295` spec rows across `87` Nango providers
- Current Ready table providers: `10`
- Current Ready action providers: `2`
- First promotion batch:
  - Simple/alias group: `github-pat`, `1password-events`, `atlassian-admin`, `azure-devops`, `clicksend`
  - Wizard-visible OAuth group: `asana`, `slack`, `jira`, `notion`, `trello`

## Dependency DAG

```text
task-1-status-model
  -> task-2-candidate-generation
  -> task-3-backend-provider-status
  -> task-4-wizard-status-ui
  -> task-5-e2e-harness
      -> task-6-promote-simple-five
      -> task-7-promote-visible-oauth-five
          -> task-8-coverage-docs
```

### Task 1: Define Provider Fulfillment Status Model

**Task ID:** `task-1-status-model`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/mod.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs`

**Depends on:** none

**Acceptance Criteria:**
- `nango-catalog` emits a deterministic fulfillment matrix for all `87` APIs.guru-backed providers and `295` spec rows.
- Every row has one status: `Ready`, `Candidate`, or `Blocked`.
- Ready status is derived only from committed supported manifests.
- Candidate status is assigned when a parseable candidate manifest can be generated.
- Blocked status includes a machine-readable reason.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: catalog data model, CLI output, catalog E2E.
- OUT of scope: Wizard UI, provider promotion manifests.

**Implementation:**
- Add `ProviderFulfillmentStatus` with variants `Ready`, `Candidate`, `Blocked`.
- Add matrix output, e.g. `.spur/nango-catalog/api_guru_fulfillment_matrix.json`.
- Include fields: `provider_key`, `spec_source_key`, `spec_url`, `status`, `blocked_reason`, `supported_manifest`, `candidate_manifest`, `table_count`, `action_count`.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway nango_catalog`.

### Task 2: Generate Candidate Manifests For All APIs.guru Rows

**Task ID:** `task-2-candidate-generation`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/src/bin/nango-catalog.rs`
- Modify: `crates/spur-notebook/rest-table-gateway/src/adapter/catalog/generate.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/nango_catalog_e2e.rs`

**Depends on:** `task-1-status-model`

**Acceptance Criteria:**
- All `295` APIs.guru rows produce either a candidate manifest or a blocked matrix row.
- Candidate manifests are written under generated output only, not committed as Ready.
- Provenance comments include Nango provider key, APIs.guru source key, spec URL, and status.
- Candidate manifests parse with `Manifest::from_toml`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: generator logic and E2E fixtures.
- OUT of scope: committing generated candidate manifests under `connections/supported/`.

**Implementation:**
- Extend existing experimental manifest generation to classify `Candidate` vs `Blocked`.
- Generate only safe collection `GET` tables by default.
- Add explicit blocked reasons for zero-table specs, unsupported auth, missing base URL, parse failure, or unsafe endpoint-only specs.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway nango_catalog_cli_can_write_experimental_crosswalk_manifests`.

### Task 3: Expose Provider-Level Status In Backend

**Task ID:** `task-3-backend-provider-status`

**Files:**
- Modify: `crates/spur-notebook/src/mcp/mod.rs`
- Modify: `crates/spur-notebook/jute-notebook/src-tauri/src/commands.rs`
- Modify: `crates/spur-notebook/jute-notebook/src/bindings/ProviderSummary.ts`

**Depends on:** `task-1-status-model`

**Acceptance Criteria:**
- `list_nango_providers` returns all `851` Nango providers.
- All `87` APIs.guru-backed providers have `experimentalSpecCount > 0`.
- Each APIs.guru-backed provider has `fulfillmentStatus`: `Ready`, `Candidate`, or `Blocked`.
- Existing supported providers remain `supportLevel = "supported"`.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: daemon command payloads and generated TS bindings.
- OUT of scope: visual Wizard changes.

**Implementation:**
- Extend `ProviderSummary` with `fulfillmentStatus` and optional `blockedReason`.
- Keep `supportLevel` backward-compatible.
- Add tests around `asana`, `github-pat`, one blocked fixture if available, and the `87/295` totals.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-notebook list_nango_providers_tiers`.

### Task 4: Show Ready/Candidate/Blocked In Wizard

**Task ID:** `task-4-wizard-status-ui`

**Files:**
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/ui/notebook/AddRestApiWizard.test.tsx`
- Modify: `crates/spur-notebook/jute-notebook/src/daemon/control.ts`

**Depends on:** `task-3-backend-provider-status`

**Acceptance Criteria:**
- Wizard shows provider-level status, not table/spec-row status.
- Ready providers remain selectable for direct import.
- Candidate providers are selectable as experimental/spec-backed imports.
- Blocked providers are visible but explain why they are blocked.
- The provider list can show all `87` APIs.guru-backed providers without duplicating per spec row.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: provider cards, filtering, status badges, frontend validation.
- OUT of scope: backend status computation.

**Implementation:**
- Add badge mapping: `Ready`, `Candidate`, `Blocked`, `Catalog`.
- Preserve current `Experimental` copy where useful, but make status precise.
- Add tests for `asana` Candidate, `github-pat` Ready, and a Blocked provider fixture.
- Run remote verification only:
  `scripts/spur-pnpm test -- src/ui/notebook/AddRestApiWizard.test.tsx`.

### Task 5: Generalize Provider E2E Harness

**Task ID:** `task-5-e2e-harness`

**Files:**
- Modify: `crates/spur-notebook/rest-table-gateway/tests/tier_a_connection_e2e.rs`
- Create: `crates/spur-notebook/rest-table-gateway/tests/provider_manifest_harness.rs`

**Depends on:** `task-2-candidate-generation`

**Acceptance Criteria:**
- Shared harness can scan one table or invoke one action per provider.
- Harness installs auth env vars deterministically.
- Harness supports bearer, header, basic, API key query, and OAuth-refresh/BYO-token cases used by the first 10 promotions.
- Existing Tier A providers keep passing under the harness.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: test helpers and focused migration of existing E2E usage.
- OUT of scope: adding new provider manifests.

**Implementation:**
- Extract env guard, mock response builder, and typed-row assertions.
- Keep provider-specific path/header assertions configurable.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway tier_a_connection_e2e`.

### Task 6: Promote Simple/Auth-Alias Five Providers

**Task ID:** `task-6-promote-simple-five`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/1password_events.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/atlassian_admin.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/azure_devops.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/clicksend.connection.toml`
- Modify: `crates/spur-notebook/rest-table-gateway/connections/supported/github.connection.toml`
- Modify: `crates/spur-notebook/src/mcp/mod.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/provider_manifest_harness.rs`

**Depends on:** `task-5-e2e-harness`

**Acceptance Criteria:**
- `github-pat`, `1password-events`, `atlassian-admin`, `azure-devops`, and `clicksend` are Ready.
- Each provider has at least one reviewed table-function.
- Each provider has provider-specific mock E2E proving auth, request construction, and typed rows.
- Wizard lists all five as Ready.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: five provider manifests, curated preset mapping, E2E.
- OUT of scope: OAuth providers and multi-spec broad table expansion.

**Implementation:**
- For `github-pat`, reuse/alias existing GitHub manifest where compatible and assert `provider_key = github-pat` imports the Ready preset.
- For the other four, select one safe collection GET endpoint from candidate generation.
- Commit only reviewed supported manifests, not generated candidate output.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway provider_manifest_harness`.

### Task 7: Promote Wizard-Visible OAuth Five Providers

**Task ID:** `task-7-promote-visible-oauth-five`

**Files:**
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/asana.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/jira.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/notion.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/slack.connection.toml`
- Create: `crates/spur-notebook/rest-table-gateway/connections/supported/trello.connection.toml`
- Modify: `crates/spur-notebook/src/mcp/mod.rs`
- Test: `crates/spur-notebook/rest-table-gateway/tests/provider_manifest_harness.rs`

**Depends on:** `task-5-e2e-harness`

**Acceptance Criteria:**
- `asana`, `slack`, `jira`, `notion`, and `trello` are Ready.
- OAuth/OAuth1 providers use explicit BYO-token/env behavior unless hosted OAuth is already supported.
- Each provider has at least one reviewed table-function or action-function with typed rows.
- Wizard lists all five as Ready and no longer Candidate.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: five provider manifests, auth/env mapping, curated preset mapping, E2E.
- OUT of scope: hosted OAuth browser flow and broad multi-table expansion.

**Implementation:**
- Prefer bearer-token table manifests where the gateway supports it today.
- If OAuth1 cannot be represented safely for `trello`, mark it Blocked with a reason and promote the next highest simple-auth provider from the matrix in the same task.
- Run remote verification only:
  `SPUR_REMOTE=1 scripts/spur-cargo test -p spur-rest-table-gateway provider_manifest_harness`.

### Task 8: Publish Coverage Report And Docs

**Task ID:** `task-8-coverage-docs`

**Files:**
- Modify: `docs/superpowers/specs/2026-06-12-nango-api-spec-bundle-design.md`
- Create: `docs/superpowers/specs/2026-06-13-api-guru-provider-fulfillment-status.md`
- Modify: `crates/spur-notebook/rest-table-gateway/README.md`

**Depends on:** `task-6-promote-simple-five`, `task-7-promote-visible-oauth-five`

**Acceptance Criteria:**
- Docs report `87/87` APIs.guru-backed providers visible.
- Docs report `295/295` spec rows traceable.
- Docs list Ready, Candidate, and Blocked counts.
- Docs list the 10 newly promoted providers and their table/action counts.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: status docs and README workflow notes.
- OUT of scope: additional provider promotion.

**Implementation:**
- Generate docs from the fulfillment matrix where possible.
- Include commands for refreshing the matrix.
- Run remote/non-compile verification:
  `git diff --check`

## Self-Review

- Spec coverage: tasks cover provider visibility, row traceability, provider status, and first 10 Ready promotions with E2E.
- Placeholder scan: all tasks have concrete files, providers, acceptance criteria, and commands.
- Type consistency: `ProviderSummary`, Wizard provider cards, and fulfillment matrix all use provider-level status.
- DAG validation: linear setup through status/UI/harness, then two parallel promotion batches, then docs.
