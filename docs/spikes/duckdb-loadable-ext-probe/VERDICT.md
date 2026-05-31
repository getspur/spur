# DuckDB Loadable Extension Probe Verdict

Overall verdict: **FEASIBLE-WITH-CAVEATS**.

A local macOS arm64 Python `duckdb` process can `LOAD` an unsigned Rust-built
`.duckdb_extension` from an absolute local path when
`allow_unsigned_extensions` is enabled. The loaded extension can register a
DuckDB table function through duckdb-rs' C extension entrypoint, and named table
function parameters work with the requested `:=` SQL syntax.

The caveat is important: for `abi_type = C_STRUCT`, the metadata footer and the
duckdb-rs `#[duckdb_entrypoint_c_api(..., min_duckdb_version = ...)]` argument
must use the DuckDB **C API** version, not the DuckDB engine/package version.
For DuckDB 1.5.2, that C API version is `v1.2.0`.

## Exact Environment

- Host platform: `Darwin arm64`
- Local cargo: `cargo 1.94.1 (29ea6fb6a 2026-03-24)`
- Local Python used for the pinned test venv: `Python 3.12.6`
- Rust dependency: `duckdb = "=1.10502.0"` with `loadable-extension` and `vtab`
- Python dependency in test venv: `duckdb==1.5.2`
- Built artifact type: `Mach-O 64-bit dynamically linked shared library arm64`
- Artifact size: `492K`

## Files

- `Cargo.toml`: standalone non-workspace spike crate with its own `[workspace]`
  section, matching the existing `duckdb-vtab-probe` isolation pattern.
- `src/lib.rs`: loadable extension entrypoint and `spur_probe` table function.
- `scripts/build-local.sh`: local host `cargo build --release` plus metadata
  footer append.
- `scripts/append_extension_metadata.py`: local copy of the DuckDB footer layout
  used by `extension-ci-tools`.
- `scripts/test_load.py`: Python `LOAD` smoke test.
- `scripts/check_spur_kernel.py`: bonus check for the SPUR Jupyter kernel venv.

Generated artifacts are intentionally ignored by `.gitignore`:

- `.venv/`
- `target/`
- `build/`

## Build Command

Run from the spike directory on the local macOS host:

```sh
cd docs/spikes/duckdb-loadable-ext-probe
scripts/build-local.sh
```

The script uses local `cargo`, not `scripts/spur-cargo`. It does not enable
duckdb-rs `bundled`, so it does not compile bundled DuckDB C++ sources. It
builds the Rust loadable extension and then stamps the raw dylib:

```text
Finished `release` profile [optimized] target(s) in 1.63s
Creating extension binary:
 - Input file: target/release/libspur_probe.dylib
 - Output file: build/release/spur_probe.duckdb_extension
 - Metadata:
 - FIELD8 (unused) = EMPTY
 - FIELD7 (unused) = EMPTY
 - FIELD6 (unused) = EMPTY
 - FIELD5 (abi_type) = C_STRUCT
 - FIELD4 (extension_version) = 0.1.0
 - FIELD3 (duckdb_version) = v1.2.0
 - FIELD2 (duckdb_platform) = osx_arm64
 - FIELD1 (header signature) = 4
/Volumes/Projects/spur/.spur/worktrees/a8cf0833-a11b-4bd1-98f1-a829bfd1b93e/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
```

## Stamping Mechanism

DuckDB's Rust extension template says the build produces a shared library, then
runs a script that appends a binary footer and writes the resulting
`.duckdb_extension` file. The local script mirrors DuckDB's
`extension-ci-tools/scripts/append_extension_metadata.py` footer layout.

Sources:

- DuckDB extension-template-rs build flow:
  https://github.com/duckdb/extension-template-rs
- DuckDB extension-ci-tools metadata script:
  https://github.com/duckdb/extension-ci-tools/blob/main/scripts/append_extension_metadata.py
- DuckDB unsigned extension and platform compatibility docs:
  https://duckdb.org/docs/lts/extensions/extension_distribution

Two failed attempts established the C API version requirement:

1. Footer stamped with `v1.5.2` failed before initialization:

```text
Invalid Input Error: Failed to load '<path>/spur_probe.duckdb_extension',
The file was built for DuckDB C API version 'v1.5.2', but we can only load
extensions built for DuckDB C API 'v1.2.0' and lower.
```

2. Footer stamped with `v1.2.0`, but macro `min_duckdb_version = "v1.5.2"`,
   failed during initialization:

```text
An error was thrown during initialization of the extension '<path>/spur_probe.duckdb_extension':
Unsupported C CAPI version detected during extension initialization: v1.5.2
```

Working configuration:

- Footer `--abi-type C_STRUCT`
- Footer `--duckdb-version v1.2.0`
- Footer `--duckdb-platform osx_arm64`
- Macro `#[duckdb_entrypoint_c_api(ext_name = "spur_probe", min_duckdb_version = "v1.2.0")]`

## Python LOAD Test

Venv setup:

```sh
cd docs/spikes/duckdb-loadable-ext-probe
python3 -m venv .venv
.venv/bin/python -m pip install --no-cache-dir duckdb==1.5.2
```

Working Python incantation:

```python
import duckdb

ext = "/Volumes/Projects/spur/.spur/worktrees/a8cf0833-a11b-4bd1-98f1-a829bfd1b93e/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension"
con = duckdb.connect(config={"allow_unsigned_extensions": "true"})
con.execute(f"LOAD '{ext}'")
print(con.sql("SELECT * FROM spur_probe()").fetchall())
print(con.sql("SELECT * FROM spur_probe(n := 5)").fetchall())
print("duckdb", duckdb.__version__)
```

Exact output from `scripts/test_load.py` in the pinned `duckdb==1.5.2` venv:

```text
extension /Volumes/Projects/spur/.spur/worktrees/a8cf0833-a11b-4bd1-98f1-a829bfd1b93e/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
default [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')]
named_colon [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3'), (4, 'probe-4'), (5, 'probe-5')]
named_equals [(1, 'probe-1'), (2, 'probe-2')]
duckdb 1.5.2
```

Named-parameter verdict: **PASS**. `SELECT * FROM spur_probe(n := 5)` works
through duckdb-rs `VTab::named_parameters()` and
`BindInfo::get_named_parameter()`. `n = 2` also works.

Relevant API sources:

- duckdb-rs loadable extension feature and version mapping:
  https://github.com/duckdb/duckdb-rs
- duckdb-rs loadable example:
  https://github.com/duckdb/duckdb-rs/blob/main/crates/duckdb/examples/hello-ext/main.rs
- DuckDB C table-function API named parameters:
  https://duckdb.org/docs/current/clients/c/table_functions
- PyPI `duckdb==1.5.2` macOS arm64 wheel availability:
  https://pypi.org/project/duckdb/1.5.2/

## SPUR Kernel Venv Bonus

The current SPUR-provisioned Jupyter venv exists and already has DuckDB:

```text
python /Users/kevintruong/.spur/jupyter/venv/bin/python
duckdb 1.5.3
```

The same built extension also loaded in that venv:

```text
default [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')]
named_colon [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3'), (4, 'probe-4'), (5, 'probe-5')]
named_equals [(1, 'probe-1'), (2, 'probe-2')]
duckdb 1.5.3
```

Plan 3 should still explicitly own this dependency. The managed kernel venv's
`duckdb` package version is the loader version. If SPUR wants deterministic
support, kernel provisioning should install a pinned `duckdb` version and the
extension build/release process should stamp/package per platform.

## Plan 3 Constraints

1. The extension must be built on or for the same native platform as the Python
   kernel process. For the local SPUR notebook this is `osx_arm64`; a Linux
   artifact from `scripts/spur-cargo` will not load into the macOS kernel.
2. The Python kernel must connect with
   `config={"allow_unsigned_extensions": "true"}` before `LOAD`.
3. The `.duckdb_extension` footer must match the platform and the supported C
   API version for `C_STRUCT` extensions. For DuckDB 1.5.2/1.5.3 in this test,
   that was `osx_arm64` and `v1.2.0`.
4. The duckdb-rs entrypoint macro's `min_duckdb_version` name is misleading for
   this path; it is the minimum C API version passed to
   `duckdb_rs_extension_api_init`.
5. SPUR should pin the Python `duckdb` package in its managed kernel venv and
   build/distribute the extension per target platform.

