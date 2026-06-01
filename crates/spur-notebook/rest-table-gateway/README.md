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
