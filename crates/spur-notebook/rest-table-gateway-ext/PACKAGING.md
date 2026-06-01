# spur_rest Packaging

`spur_rest` is a native DuckDB C-API extension. A single host cannot
cross-build every supported target, so release packaging needs one native build
per platform.

## Artifact names

Packaged artifacts are named:

`spur_rest-<platform>.duckdb_extension`

`scripts/build.sh` derives `<platform>` from `(OS, ARCH)`:

| OS | ARCH | Platform |
| --- | --- | --- |
| Darwin | arm64 or aarch64 | `osx_arm64` |
| Darwin | x86_64 or amd64 | `osx_amd64` |
| Linux | x86_64 or amd64 | `linux_amd64` |
| Linux | arm64 or aarch64 | `linux_arm64` |

The local build output is `build/release/spur_rest.duckdb_extension`. The local
install copy is `~/.spur/extensions/spur_rest.duckdb_extension`.

## Filename constraint

DuckDB derives the C-API init symbol from the loaded file's stem, so the
installed/LOADed file MUST be `spur_rest.duckdb_extension` to match entrypoint
`spur_rest_init_c_api`. Bundled and staged artifacts keep the disambiguating
`spur_rest-<platform>.duckdb_extension` name, then startup install and
`scripts/build.sh` rename that source artifact to the bare load name.

## Runtime resolution

On startup, the app resolves the current platform and copies:

`<resource_dir>/extensions/spur_rest-<platform>.duckdb_extension`

into:

`~/.spur/extensions/spur_rest.duckdb_extension`

The copy is copy-if-absent. If `scripts/build.sh` already installed a local
`~/.spur/extensions/spur_rest.duckdb_extension`, startup leaves it in place. That
makes `scripts/build.sh` the local-development override for testing a freshly
built extension without clobbering it from app resources.

The notebook kernel setup cell `LOAD`s from `~/.spur/extensions/`, not directly
from the app bundle.

## Multi-platform gap

`xtask` currently stages only the host artifact into the Jute app bundle. To
ship all supported installers, CI should use this shape:

1. One native job per platform runs `scripts/build.sh`.
2. The platform jobs upload `spur_rest-osx_arm64.duckdb_extension`,
   `spur_rest-osx_amd64.duckdb_extension`,
   `spur_rest-linux_amd64.duckdb_extension`, and
   `spur_rest-linux_arm64.duckdb_extension`.
3. A bundling job downloads all four artifacts into
   `crates/spur-notebook/jute-notebook/src-tauri/extensions/` before
   `tauri build`.

With all artifacts present before `tauri build`, each per-platform installer
ships the correct `spur_rest-<platform>.duckdb_extension` in its app resources.
