# spur-rest-table-gateway

`spur-rest-table-gateway` maps REST APIs into table-shaped scans backed by Arrow record batches and DuckDB virtual table integration.

## Nango connection import

The `nango-import` helper converts entries from Nango's `providers.yaml` into starter `*.connection.toml` manifests:

```sh
cargo run -p spur-rest-table-gateway --bin nango-import -- <providers.yaml> <out_dir> [--tier A] [--category dev-tools,analytics] [names...]
```

The generated manifests include `[source]` auth, base URL, pagination, and connection-template metadata. They intentionally leave table definitions as TODOs; add `[[table]]` blocks with paths, response envelopes, columns, and filters before using them for scans.

Runtime values come from environment variables:

- `SPUR_CONN_<name>` supplies `${connectionConfig.<name>}` template values from imported base URLs.
- `<NAME>_API_KEY` supplies imported `API_KEY` providers, where `<NAME>` is the uppercased provider name with non-alphanumeric characters replaced by `_`.
- `<NAME>_USER` and `<NAME>_PASS` supply imported `BASIC` providers.
- `<NAME>_TOKEN` supplies BYO-token OAuth-family providers and other bearer-token stubs.

Auth support is grouped into import tiers:

- Tier A: drop-in `API_KEY`, `BASIC`, and `NONE` providers.
- Tier B: OAuth-family providers imported as BYO-token manifests; SPUR does not run the hosted OAuth flow.
- Tier C: unsupported or unknown auth modes that require manual review before use.

Nango provider metadata is licensed under Elastic License 2.0. Preserve the generated notice comments and this crate's `THIRD_PARTY_NOTICES` when distributing imported `*.connection.toml` files. Bundling imported manifests is intended for local distribution only, not for offering a hosted or managed Nango-derived service.

## Nango catalog workflow

The `nango-catalog` helper builds a deterministic inspection bundle from pinned local inputs: a Nango `providers.yaml`, an APIs.guru `list.json` snapshot, the exact Nango commit, and the APIs.guru snapshot retrieval timestamp.

```sh
scripts/spur-cargo run -p spur-rest-table-gateway --bin nango-catalog -- \
  resources/nango/packages/providers/providers.yaml \
  .spur/vendor/apis-guru/list.json \
  .spur/nango-catalog \
  --nango-commit 988efd014 \
  --apis-guru-fetched-at 2026-06-12T00:00:00Z \
  --reviewed-source github=crates/spur-notebook/rest-table-gateway/specs/tier-a/github.json
```

The command reads only local files. It does not fetch Nango, APIs.guru, or provider specs from the network, and generated provider catalogs should not be checked in without review. The Nango commit and APIs.guru timestamp are required so every generated crosswalk can be traced back to stable source inputs.

The output directory contains:

- `provider_harvest_candidates.csv`: normalized Nango provider metadata, auth/base URL fields, seed class, Nango license, and source commit.
- `table_seed_classes.csv`: provider counts by seed class.
- `apis_guru_crosswalk.csv`: provider-to-spec candidate rows for review.
- `provider_spec_crosswalk.json`: deterministic crosswalk rows with Nango ELv2 metadata and APIs.guru SHA-256 provenance.
- `coverage_summary.json`: provider/spec counts, match diagnostics, and snapshot metadata.

Reviewed OpenAPI sources are opt-in through `--reviewed-source <provider>=<local-spec-path>`. Only those local reviewed specs are converted into `connections/<provider>.connection.toml`; APIs.guru URL matches with `NeedsReview`, `UrlOnly`, `Blocked`, or candidate confidence stay as catalog metadata and do not generate bundled manifests by themselves.

## OpenAPI table import

The `openapi-import` helper converts OpenAPI collection `GET` endpoints into `[[table]]` manifest blocks:

```sh
cargo run -p spur-rest-table-gateway --bin openapi-import -- <spec> <out_dir> [--into <name>.connection.toml]
```

Generated tables flatten nested response fields into denormalized columns using dotted JSON paths. Nested arrays, free-form objects, and object arrays are represented as `Utf8` JSON text columns so scans stay table-shaped without requiring child table generation.

Auth, pagination, base URL templating, and runtime connection values come from the connection layer, usually from a Nango-imported `*.connection.toml` stub. A typical onboarding flow is:

```sh
cargo run -p spur-rest-table-gateway --bin nango-import -- <providers.yaml> <out_dir> <provider-name>
cargo run -p spur-rest-table-gateway --bin openapi-import -- <spec> <out_dir> --into <out_dir>/<provider-name>.connection.toml
```
