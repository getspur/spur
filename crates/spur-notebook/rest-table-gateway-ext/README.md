# spur-rest DuckDB Extension

This standalone crate packages the REST table gateway as a loadable DuckDB
extension. It is intentionally not a member of the root SPUR workspace; its
`Cargo.toml` has its own `[workspace]` section so the loadable-extension build
does not perturb the main workspace dependency graph.

## Build

```sh
cd crates/spur-notebook/rest-table-gateway-ext
scripts/build.sh
```

The script runs host `cargo build --release`, stamps the shared library with the
DuckDB extension footer, and writes:

- `build/release/spur_rest.duckdb_extension`
- `~/.spur/extensions/spur_rest-<platform>.duckdb_extension`

`<platform>` is detected from the build host:

- Linux x86_64: `linux_amd64`
- Linux aarch64: `linux_arm64`
- macOS arm64: `osx_arm64`
- macOS x86_64: `osx_amd64`

The footer uses the proven C-API metadata:

- `abi_type=C_STRUCT`
- `duckdb-version=v1.2.0`
- `duckdb-platform=<host>`

The `min_duckdb_version = "v1.2.0"` entrypoint argument is the DuckDB C API
version for this extension ABI path, not the engine/package version.

## Runtime

The extension registers:

- `polymarket_markets()`
- `polymarket_orderbook(token_id := ..., depth := ...)`

By default it uses live Polymarket endpoints:

- `SPUR_POLYMARKET_GAMMA_BASE`, default `https://gamma-api.polymarket.com`
- `SPUR_POLYMARKET_CLOB_BASE`, default `https://clob.polymarket.com`

Tests set those environment variables to a local mock server before `LOAD`.

Unsigned local loading requires a DuckDB connection configured with
`allow_unsigned_extensions=true`.

## E2E

```sh
CARGO_TARGET_DIR=/private/tmp/spur-rest-table-gateway-ext-test-target \
  cargo test --test load_extension_e2e -- --nocapture
```

The test builds and stamps the extension, starts a mocked REST API, loads the
artifact into a separate DuckDB client harness, and queries through the loaded
extension.
