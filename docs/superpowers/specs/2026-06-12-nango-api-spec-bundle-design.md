# Nango API Spec Bundle for REST Table Gateway

| Field | Value |
|---|---|
| Status | Implemented plan submitted |
| Date | 2026-06-12 |
| Crates | `crates/spur-notebook/rest-table-gateway`, `crates/spur-notebook` |
| Inputs | Nango `providers.yaml`, public OpenAPI/Swagger catalogs, provider docs, GraphQL metadata |
| Output | Bundled provider/spec catalog and generated SPUR gateway manifests |

## Problem

SPUR's REST table gateway already has a Nango adapter and an OpenAPI-to-table importer, but the current bundled provider coverage is small. Nango's upstream catalog has 851 providers, while SPUR currently carries only a small snapshot and a handful of curated manifests/specs.

The missing piece is not only provider count. A Nango provider entry gives us authentication and proxy metadata, but it usually does not contain table schemas. A public OpenAPI or GraphQL schema can provide endpoint and response-shape information, but it usually does not know how SPUR should authenticate through Nango-compatible provider metadata.

This design combines both sides:

- Nango provider metadata supplies auth, base URL, headers, connection config, token URLs, and pagination hints.
- OpenAPI/Swagger/Discovery/GraphQL sources supply endpoint and schema information.
- Nango docs and verification endpoints supply fallback table seeds when no spec is available.

## Grounding

Discovery was run against a local Nango checkout at `resources/nango`:

- Nango source: `https://github.com/NangoHQ/nango.git`
- Nango checkout HEAD: `988efd014`
- Nango repository license at this checkout: Elastic License 2.0 (`resources/nango/LICENSE`)
- APIs.guru index source: `https://api.apis.guru/v2/list.json`; Phase 1 must pin the fetched snapshot date and content hash before committing derived counts.
- SPUR graph hash: `4bf57ce26d580b4e709199114d1af15b6b09ea6058468d8909e2dbc62a774f2f`
- Indexed by SPUR graph: 2,117 files, 11,763 analyst nodes, 37,532 resolved edges

Generated discovery artifacts:

- `resources/nango/.spur/provider_harvest_candidates.csv`
- `resources/nango/.spur/table_seed_classes.csv`
- `resources/nango/.spur/docs_endpoint_candidates.csv`
- `resources/nango/.spur/apis_guru_crosswalk.csv`

These artifacts are analysis outputs, not yet committed source-of-truth inputs. The implementation must either commit the generator and deterministic inputs, or replace the ad hoc discovery step with an `xtask` command that can regenerate the same files from pinned upstream inputs.

Observed source coverage:

| Source | Observed coverage |
|---|---:|
| Nango providers in `packages/providers/providers.yaml` | 851 |
| Providers with `proxy.base_url` | 815 |
| Providers with docs sample endpoints | 442 |
| Providers with verification endpoints | 256 |
| Providers with official docs links | 315 |
| APIs.guru API entries | 2,529 |
| Exact-ish Nango providers matched to APIs.guru | 87 |
| APIs.guru API entries matched to Nango providers | 208 |
| Crosswalk rows | 295 |

Table seed classes from local discovery:

| Seed class | Providers |
|---|---:|
| Base URL only | 277 |
| REST singleton or unknown docs endpoint | 262 |
| REST collection-like docs endpoint | 172 |
| Verification endpoint only | 122 |
| GraphQL candidate | 11 |
| Metadata only | 7 |

### Current Implementation Snapshot

After the June 12 plan merge, the implementation is stronger than the original
catalog-only baseline but still not provider-complete. Treat these as distinct
support levels:

| Level | Meaning | Current evidence |
|---|---|---|
| Cataloged | Nango provider metadata is parsed and can be crosswalked to candidate spec sources. | `nango-catalog` writes deterministic harvest, seed-class, crosswalk, and coverage files from pinned local Nango/APIs.guru inputs. |
| Generated | A reviewed spec source can be converted into a parseable SPUR `*.connection.toml` with auth metadata and one or more `[[table]]` blocks. | `--reviewed-source github=<local-spec>` generates a parseable GitHub manifest in tests. |
| Scannable generic path | The runtime can scan a generated table shape against a local API fixture. | `nango-import` + hand-added table scans an API-key, cursor-paginated REST envelope; `openapi-import` scans typed rows from a generated OpenAPI table. |
| Provider-specific E2E | A named provider manifest/adapter proves auth plus request plus typed table/action rows. | Google Ads proves OAuth refresh -> bearer -> provider headers -> typed POST action rows. Polymarket proves no-auth REST table and table-function scans. Linear proves GraphQL table scan shape but not REST/OpenAPI. |
| Live provider E2E | The committed provider path is exercised against the real upstream API. | Polymarket live tests exist but are ignored by default; other provider paths are mock-only today. |

Current committed provider-specific coverage is therefore:

| Provider | Transport | Runtime shape | Auth evidence | Table/action evidence | Status |
|---|---|---|---|---|---|
| Polymarket | REST | `markets` table, `orderbook` table function | No auth | Mock tests plus ignored live tests | Supported no-auth REST adapter, not Nango-derived. |
| Google Ads | REST | `google_ads_search` POST action | OAuth refresh exchange plus bearer and provider headers in mock E2E | Typed action rows from preset manifest | Supported action flow; not a normal GET table. |
| Linear | GraphQL | `issues` table | Manifest declares `LINEAR_API_KEY`; mock scan does not require env auth | Typed GraphQL table rows | Useful precedent for GraphQL follow-on, outside REST/OpenAPI phase. |
| Facebook Ads | REST | `facebook_ads_insights` POST action | Manifest declares OAuth refresh | Parse/shape coverage only | Not yet provider-specific request/auth E2E. |
| GitHub | REST/OpenAPI | Generated `[[table]]` from reviewed local spec | Nango API-key/header metadata in generated manifest | Manifest reparses; no scan test yet | Generated candidate, not yet provider-specific E2E. |

Untracked generated manifests currently exist in the workspace for providers
such as GitHub, Stripe, Zendesk, SendGrid, Twilio, OpenAI, Mailchimp, Datadog,
Algolia, Square, and others. They must not be counted as supported until they
are reviewed, committed, covered by provider-specific tests, and cleared by the
license/provenance gate.

## Goals

1. Build a reproducible bundled catalog that maps Nango providers to API spec sources.
2. Generate SPUR gateway `*.connection.toml` manifests that combine Nango auth metadata with spec-derived `[[table]]` blocks.
3. Preserve provenance and license status for every spec-backed table.
4. Let the UI distinguish high-confidence spec-backed tables from lower-confidence docs/verification seeds.
5. Keep the existing manual Nango/OpenAPI import flows working.

## Non-Goals

- Do not claim all 851 Nango providers have complete table schemas.
- Do not bundle third-party specs whose redistribution status is unclear.
- Do not depend on the hosted Nango runtime.
- Do not make live provider API calls during normal catalog build.
- Do not generate GraphQL tables in the same implementation step as REST/OpenAPI tables; GraphQL needs a smaller follow-on design.

## Primary Sources

### Nango Provider Catalog

Path:

`resources/nango/packages/providers/providers.yaml`

Useful fields:

- `display_name`
- `categories`
- `auth_mode`
- `authorization_url`
- `authorization_params`
- `scope_separator`
- `token_url`
- `connection_config`
- `credentials`
- `proxy.base_url`
- `proxy.headers`
- `proxy.query`
- `proxy.body`
- `proxy.paginate`
- `proxy.verification`

### APIs.guru OpenAPI Directory

Repository:

`https://github.com/APIs-guru/openapi-directory`

Machine index:

`https://api.apis.guru/v2/list.json`

APIs.guru is the best broad public catalog. Its README describes a directory of OpenAPI 2.0 and 3.x definitions, REST API access, weekly updates, and public/community-driven curation. Licensing is mixed: contributed definitions are CC0, while some acquired public-source definitions are described under fair-use principles. SPUR must track provenance and license status per spec.

Because the public index is updated regularly, SPUR should not treat the live entry count as a stable invariant. The catalog build must cache or record the exact `list.json` digest and retrieval timestamp used for each crosswalk run.

### Official Provider Spec Repositories

High-value official sources should override or supplement broad catalog matches:

| Provider family | Source |
|---|---|
| GitHub | `https://github.com/github/rest-api-description` |
| Stripe | `https://github.com/stripe/openapi` |
| Microsoft Graph | `https://github.com/microsoftgraph/msgraph-metadata` |
| Google APIs | Google Discovery Service JSON via `googleapis` artifacts |
| Shopify | Official/admin GraphQL and REST metadata where available |

### Nango Docs Endpoint Seeds

Nango docs contain sample proxy endpoints for many providers. These are useful as fallback table seeds but not complete schemas.

Generated artifact:

`resources/nango/.spur/docs_endpoint_candidates.csv`

Observed:

- 921 endpoint rows
- 464 docs with at least one endpoint candidate

## Architecture

The feature has four layers.

### 1. Provider Metadata Ingest

Parse Nango `providers.yaml` into normalized provider records.

Output:

```rust
struct ProviderCatalogEntry {
    provider: String,
    display_name: String,
    categories: Vec<String>,
    auth_mode: String,
    base_url: Option<String>,
    connection_config_keys: Vec<String>,
    credential_keys: Vec<String>,
    proxy_headers: IndexMap<String, String>,
    proxy_query: IndexMap<String, String>,
    proxy_body: IndexMap<String, String>,
    pagination: Option<NangoPagination>,
    verification: Vec<VerificationEndpoint>,
    authorization_url: Option<String>,
    token_url: Option<String>,
}
```

This layer owns Nango semantics only. It does not decide which endpoints become tables.

### 2. Spec Registry Ingest

Load machine-readable spec catalogs and official source overrides.

Output:

```rust
struct ApiSpecSource {
    provider: String,
    source_kind: SpecSourceKind,
    spec_format: SpecFormat,
    url: String,
    version: Option<String>,
    title: Option<String>,
    provenance: String,
    license_status: LicenseStatus,
    confidence: MatchConfidence,
}

enum SpecSourceKind {
    ApisGuru,
    OfficialRepo,
    OfficialUrl,
    GoogleDiscovery,
    Manual,
}

enum SpecFormat {
    OpenApi2,
    OpenApi3,
    GoogleDiscovery,
    GraphqlSdl,
    GraphqlIntrospection,
}

enum LicenseStatus {
    Redistributable,
    UrlOnly,
    NeedsReview,
    Blocked,
}
```

The initial implementation should ingest APIs.guru first because it has a public JSON index and direct `swaggerUrl` entries.

### 3. Crosswalk Engine

Map Nango provider keys to spec sources.

Signals:

- exact provider key match
- normalized display name match
- base URL host/domain overlap
- official docs URL overlap
- known aliases such as `github-pat -> github`, `stripe-api-key -> stripe`
- manual overrides for important providers

Confidence:

```rust
enum MatchConfidence {
    Exact,
    Strong,
    Candidate,
    Rejected,
}
```

Rules:

- Manual overrides beat fuzzy matches.
- Official provider repos beat APIs.guru if both exist and the official source is usable.
- Candidate matches must not auto-generate bundled tables without review.
- Many-to-one mappings are allowed, e.g. Twilio has many APIs.guru entries for one Nango provider.

Output:

```rust
struct ProviderSpecCrosswalk {
    provider: String,
    spec_source_id: String,
    confidence: MatchConfidence,
    match_reason: String,
}
```

### 4. Manifest and Table Generator

Combine Nango metadata with spec-derived endpoints.

For OpenAPI:

- Read spec.
- Select `GET` endpoints.
- Prefer endpoints returning arrays or conventional envelopes such as `data`, `items`, `results`, `records`, `values`.
- Map query parameters to SPUR filters.
- Map response schema fields to SPUR columns.
- Skip endpoints requiring path params unless defaults/examples are known.
- Preserve required path params as table-function args in a later phase.

For fallback docs/verification endpoints:

- Generate table seeds, not final high-confidence tables.
- Mark source as `docs_endpoint` or `verification_endpoint`.
- Require UI preview/approval before saving.

For GraphQL:

- Mark as `graphql_candidate` in this design.
- Do not generate GraphQL tables in the REST/OpenAPI phase.

## Generated Bundle Layout

Proposed path:

`crates/spur-notebook/rest-table-gateway/catalog/`

Generated files:

```text
catalog/
  providers.nango.json
  spec_sources.json
  provider_spec_crosswalk.json
  table_seed_index.json
  notices/
    NOTICE.md
    sources.json
```

Generated manifests:

```text
crates/spur-notebook/rest-table-gateway/connections/generated/
  github.connection.toml
  stripe.connection.toml
  twilio.connection.toml
  ...
```

The generated directory should be reproducible. If checked in, diffs must be stable and sorted by provider key.

## Manifest Contract

Generated TOML must keep current gateway compatibility.

Example shape:

```toml
# Generated from Nango provider metadata + OpenAPI source.
# provider = "github"
# spec_source = "apis_guru:github.com"
# provenance = "https://api.apis.guru/v2/specs/github.com/1.1.4/openapi.json"
# confidence = "strong"

[source]
name = "github"
base_url = "https://api.github.com"
auth = { scheme = "bearer", env = "GITHUB_TOKEN" }

[[table]]
name = "repos"
path = "/user/repos"
response_path = "$"

[table.columns]
id = { json = "$.id", type = "Int64" }
name = { json = "$.name", type = "Utf8" }
full_name = { json = "$.full_name", type = "Utf8" }
```

Each generated table should carry enough provenance in comments or sidecar metadata to explain why it exists.

## UI Behavior

The provider picker should surface:

- provider name
- auth mode
- catalog tier
- spec availability
- generated table count
- match confidence
- provenance source

Suggested badges:

- `Spec-backed`
- `Docs-seeded`
- `Verification-only`
- `GraphQL`
- `Needs review`

Before attach, the user should preview generated tables and filters. Low-confidence docs/verification-only seeds should be visually distinct from spec-backed tables.

## License and Provenance Policy

Every spec source must have provenance.

Bundling policy:

| License status | Behavior |
|---|---|
| `Redistributable` | Spec content can be bundled. |
| `UrlOnly` | Store URL, hash, and metadata; fetch on demand. |
| `NeedsReview` | Do not ship generated tables by default. |
| `Blocked` | Exclude from bundle. |

APIs.guru requires special handling because its README says contributed definitions are CC0, while some public-source definitions are acquired under fair-use principles. The first implementation should store APIs.guru spec URLs and provenance, but only check in generated manifests for sources reviewed as redistributable.

Nango's provider catalog requires separate handling from OpenAPI specs. The local checkout used for this design is under Elastic License 2.0, so SPUR should not blindly redistribute the upstream `providers.yaml` or generated snapshots derived wholesale from it. Phase 1 should default to one of these safer modes until legal review approves broader bundling:

1. URL/hash/index-only catalog metadata that points to the pinned Nango commit.
2. A small manually reviewed provider subset, with attribution and license notice.
3. User-local generation from a user-provided Nango checkout or downloaded archive.

Generated manifests that only encode user-facing connection templates still need provenance back to the Nango source fields used to derive auth/base URL behavior.

## Error Handling

- If a spec URL fails to fetch, keep the provider metadata and mark the spec source unavailable.
- If OpenAPI parsing fails, record a diagnostic with provider, source URL, parser error, and source hash.
- If a spec match is ambiguous, keep all candidates but generate no tables until a manual override selects one.
- If a generated table has zero columns, drop it and record a diagnostic.
- If a path requires unresolved path params, skip in v1 unless examples/defaults are present.

## Testing

Unit tests:

- Nango provider parser handles representative auth modes and proxy fields.
- APIs.guru index parser handles multiple versions and `swaggerUrl`.
- Crosswalk normalization maps known aliases correctly.
- OpenAPI selector skips unsafe/unsupported endpoints.
- License gate blocks `NeedsReview` and `Blocked` sources from bundled output.

Golden tests:

- GitHub: Nango auth + OpenAPI tables.
- Stripe: official OpenAPI source + Nango auth variants.
- Twilio: one Nango provider mapped to multiple spec sources.
- Asana or Box: APIs.guru single-source provider.

Integration tests:

- Generated manifest reparses via `Manifest::from_toml`.
- Generated table can scan against a local wiremock fixture.
- Each promoted provider must have one provider-specific mock E2E that proves
  the declared auth mode is applied to the outgoing request and that at least
  one generated table/action returns typed rows.
- A provider may be labeled `cataloged` or `generated` without live credentials,
  but it must not be labeled `supported` until the provider-specific mock E2E
  passes.
- Existing `nango-import` and `openapi-import` behavior remains unchanged.

## Phasing

### Phase 1: Catalog and Crosswalk

Deliver:

- `nango-catalog` import path or an `xtask` command.
- Deterministic harvest generator for `provider_harvest_candidates.csv`, `table_seed_classes.csv`, `docs_endpoint_candidates.csv`, and `apis_guru_crosswalk.csv`.
- Pinned upstream input metadata: Nango commit, Nango license identifier, APIs.guru `list.json` retrieval timestamp, and APIs.guru `list.json` content hash.
- Parse full Nango provider metadata.
- Parse APIs.guru list.
- Produce `provider_spec_crosswalk.json`.
- Produce diagnostics and coverage summary.

No table generation in this phase.

### Phase 2: Spec-Backed Table Generation

Deliver:

- Fetch/read selected OpenAPI specs.
- Generate `[[table]]` blocks from safe `GET` collection endpoints.
- Combine with Nango auth/base metadata.
- Write generated manifests under a generated output path.
- Promote only reviewed providers whose generated manifest has provider-specific
  mock E2E coverage for auth, request construction, pagination where relevant,
  and typed rows.

### Phase 3: Provenance and License Gate

Deliver:

- Source notices.
- Per-spec license status.
- Reproducible source hashes.
- CI check that blocks unreviewed bundled specs.

### Phase 4: UI Preview

Deliver:

- Provider picker shows spec/table availability.
- Table preview before attach.
- Low-confidence seeds require explicit user acceptance.

### Phase 5: GraphQL Follow-On

Deliver a separate design and implementation plan for GraphQL introspection/schema import.

Initial candidates:

- `altrata`
- `fireflies`
- `greenhouse-onboarding`
- `jobber`
- `linear`
- `plain`
- `qualia`
- `shopify-api-key`
- `shopify-partner`
- `skio`
- `slab`

## Success Criteria

- At least 80 Nango providers have spec-source candidates from APIs.guru or official sources.
- At least 25 high-value providers produce parseable generated manifests with one or more `[[table]]` blocks.
- At least 5 high-value REST providers are promoted to `supported`, meaning
  committed manifest plus provider-specific mock E2E for auth, request, and
  typed rows. Initial promotion candidates should come from API-key/no-auth
  providers before OAuth-heavy providers.
- Generated manifests for GitHub, Stripe, Twilio, Asana, Box, GitLab, SendGrid, and Slack are either produced or explicitly diagnosed.
- Every generated table has provenance.
- No unreviewed third-party spec content is bundled.
- Existing REST table gateway tests still pass.

## Risks

| Risk | Mitigation |
|---|---|
| License ambiguity | Store URL-only sources by default; require review before bundling Nango-derived catalog data or third-party spec content. |
| False-positive crosswalk | Confidence levels, manual overrides, and diagnostics. |
| Drifting upstream indexes | Pin Nango commits and APIs.guru snapshot hashes; make coverage summaries reproducible from checked-in tooling. |
| OpenAPI path params block scans | Skip unresolved path-param endpoints in v1; later map them to table-function args. |
| Base URL mismatch between Nango and spec | Nango base URL wins for auth/runtime; spec server URL is advisory. |
| Huge generated manifests | Cap tables per provider initially; let UI reveal more. |
| GraphQL complexity | Split into a separate GraphQL design. |

## Open Questions

1. Should generated manifests be checked into the repo, or generated into `.spur/` cache and only curated ones committed?
2. What is the minimum license review process for APIs.guru fair-use entries?
3. What Nango-derived provider metadata, if any, can SPUR redistribute under Elastic License 2.0 without legal review?
4. Should docs/verification endpoint seeds be visible in the UI by default, or hidden behind an "experimental seeds" toggle?
5. Should required path params become table-function args in Phase 2, or wait for a later phase?
6. Should the first crosswalk source be stored as CSV for inspection, JSON for runtime, or both?
7. Which status labels should the UI expose: `cataloged`, `generated`,
   `supported`, and `live-verified`, or a smaller user-facing vocabulary?

## Recommended Implementation Boundary

This design should become one implementation plan with these file-scope boundaries:

1. Catalog ingest and crosswalk: new module under `rest-table-gateway/src/adapter/catalog/` or `src/catalog/`.
2. OpenAPI generation reuse: extend existing `adapter/openapi.rs` without changing runtime scan behavior.
3. Provenance/license gate: new metadata structs and generated notices.
4. UI preview: notebook daemon command and React wizard changes.

The first implementation should stop after Phase 1 and Phase 2 for a small reviewed provider set. That creates a safe foundation without over-claiming full 851-provider coverage.
