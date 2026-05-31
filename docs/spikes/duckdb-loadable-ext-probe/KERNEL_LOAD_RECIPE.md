# DuckDB Kernel Load Recipe

Status: **VERIFIED** on this worker with Python `duckdb==1.5.3` and the spike's
local `spur_probe.duckdb_extension`.

## Recommendation

Emit this at the top of SPUR's managed datasource setup cell, before any
`duckdb.sql("ATTACH ...")`, `duckdb.sql("CREATE VIEW ...")`, or extension-backed
view creation:

```python
import duckdb

_SPUR_DUCKDB_EXTENSION_PATH = "/absolute/path/to/spur_polymarket.duckdb_extension"
_SPUR_DUCKDB_EXTENSION_SQL = _SPUR_DUCKDB_EXTENSION_PATH.replace("'", "''")

if "_SPUR_DUCKDB_CONNECTION" not in globals():
    _SPUR_DUCKDB_CONNECTION = duckdb.connect(
        database=":memory:",
        config={"allow_unsigned_extensions": "true"},
    )

duckdb.set_default_connection(_SPUR_DUCKDB_CONNECTION)
duckdb.sql(f"LOAD '{_SPUR_DUCKDB_EXTENSION_SQL}'")

# Existing generated setup statements continue to use the module-level default:
# duckdb.sql("ATTACH ...")
# duckdb.sql("CREATE OR REPLACE VIEW ...")
```

Then user SQL cells in the same kernel process can keep using normal module-level
DuckDB calls:

```python
import duckdb

duckdb.sql("SELECT * FROM polymarket_markets()")
duckdb.sql(
    "CREATE OR REPLACE VIEW polymarket_markets_view AS "
    "SELECT * FROM polymarket_markets()"
)
```

## Connection Contract

- The setup cell owns a shared SPUR DuckDB connection named
  `_SPUR_DUCKDB_CONNECTION`.
- That connection must be created with
  `config={"allow_unsigned_extensions": "true"}`. This cannot be applied later
  with `SET`.
- `duckdb.set_default_connection(_SPUR_DUCKDB_CONNECTION)` registers that
  configured connection as the module-level default used by bare `duckdb.sql`.
- The setup cell should reuse the global connection on rerun. This preserves
  existing session objects and still allows repeated `LOAD '<local path>'`.
- User cells see the loaded extension functions and generated views when they
  use module-level `duckdb.sql(...)` in the same Python kernel process.
- A separate `duckdb.connect()` made by user code is a different connection; it
  will not see the loaded functions or views unless it also loads/attaches them
  or is registered with `duckdb.set_default_connection`.
- If setup runs after a prior unconfigured module-level default was used, SPUR
  can still replace the default with the configured connection. Objects on the
  old default connection are not copied.

## Verified Findings

| Claim | Status | Evidence |
|---|---:|---|
| Bare `duckdb.sql("LOAD '<unsigned local extension>'")` on the stock default fails. | VERIFIED | `default-load-no-config` |
| `duckdb.default_connection(config=...)` is not supported. | VERIFIED | `default-connection-config-arg` |
| `SET allow_unsigned_extensions = true` after connection creation is rejected. | VERIFIED | `set-after-default-connect` |
| `DUCKDB_ALLOW_UNSIGNED_EXTENSIONS=true` did not affect Python's default connection in this test. | VERIFIED | `duckdb-env-prefix` |
| `duckdb.connect(config=...)` alone does not rebind module-level `duckdb.sql`. | VERIFIED | `connect-config-no-rebind` |
| `duckdb.set_default_connection(con)` makes bare `duckdb.sql(...)` use the configured connection. | VERIFIED | `set-default-before-load` |
| The setup-cell snippet works for a later user cell using bare `duckdb.sql(...)`. | VERIFIED | `recommended-setup-then-user-cell` |
| Rerunning setup with a reused global connection preserves session state. | VERIFIED | `recommended-rerun-reuses-connection` |
| Repeated `LOAD '<same local extension>'` on the same connection succeeds. | VERIFIED | `repeat-load-same-connection` |
| Replacing the default after prior default use works, but swaps to a fresh catalog. | VERIFIED | `replace-after-prior-default-use` |
| The actual future `polymarket_markets()` extension function works. | UNVERIFIED | The Plan 3 extension does not exist in this spike; `spur_probe()` verifies the same local unsigned load/default-connection mechanics. |

## Commands And Output

Working directory:

```text
/Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe
```

Venv setup:

```sh
python3 -m venv .venv
.venv/bin/pip install duckdb
```

`python3 -m venv .venv` produced no output. Pip output:

```text
WARNING: The directory '/Users/kevintruong/Library/Caches/pip' or its parent directory is not owned or is not writable by the current user. The cache has been disabled. Check the permissions and owner of that directory. If executing pip with sudo, you should use sudo's -H flag.
Collecting duckdb
  Downloading duckdb-1.5.3-cp312-cp312-macosx_11_0_arm64.whl.metadata (4.2 kB)
Downloading duckdb-1.5.3-cp312-cp312-macosx_11_0_arm64.whl (15.4 MB)
   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ 15.4/15.4 MB 2.9 MB/s eta 0:00:00
Installing collected packages: duckdb
Successfully installed duckdb-1.5.3

[notice] A new release of pip is available: 24.2 -> 26.1.1
[notice] To update, run: python3.12 -m pip install --upgrade pip
```

Build note: `scripts/build-local.sh` failed in this worker when Cargo wrote
dependency files under the worktree-local `target/` directory:

```text
error: error writing dependencies to `/Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/target/release/deps/stable_deref_trait-8419818761eeaca1.d`: Operation not permitted (os error 1)
```

The extension was built with a temporary Cargo target under `/private/tmp`,
then stamped into the spike's ignored `build/release/` directory:

```sh
env CARGO_TARGET_DIR=/private/tmp/spur-probe-duckdb-target cargo build --release
python3 scripts/append_extension_metadata.py --library-file /private/tmp/spur-probe-duckdb-target/release/libspur_probe.dylib --extension-name spur_probe --out-file build/release/spur_probe.duckdb_extension --duckdb-platform osx_arm64 --duckdb-version v1.2.0 --extension-version 0.1.0
```

Relevant output:

```text
Finished `release` profile [optimized] target(s) in 1m 53s
Creating extension binary:
 - Input file: /private/tmp/spur-probe-duckdb-target/release/libspur_probe.dylib
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
```

Fresh configured-connection smoke test:

```sh
.venv/bin/python scripts/test_load.py
```

Output:

```text
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
default [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')]
named_colon [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3'), (4, 'probe-4'), (5, 'probe-5')]
named_equals [(1, 'probe-1'), (2, 'probe-2')]
duckdb 1.5.3
```

Kernel default-connection probe:

```sh
.venv/bin/python scripts/kernel_load_recipe_probe.py
```

Output:

```text
=== default-load-no-config ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
duckdb.sql LOAD ERROR IOException: IO Error: Extension "/Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension" could not be loaded because its signature is either missing or invalid and unsigned extensions are disabled by configuration (allow_unsigned_extensions)
=== default-connection-config-arg ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
duckdb.default_connection(config=...) ERROR TypeError: default_connection(): incompatible function arguments. The following argument types are supported:
    1. () -> duckdb.DuckDBPyConnection

Invoked with: kwargs: config={'allow_unsigned_extensions': 'true'}
=== set-after-default-connect ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
SET allow_unsigned_extensions ERROR InvalidInputException: Invalid Input Error: Cannot change allow_unsigned_extensions setting while database is running
duckdb.sql LOAD after SET ERROR IOException: IO Error: Extension "/Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension" could not be loaded because its signature is either missing or invalid and unsigned extensions are disabled by configuration (allow_unsigned_extensions)
=== duckdb-env-prefix ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
DUCKDB_ALLOW_UNSIGNED_EXTENSIONS true
duckdb.sql LOAD ERROR IOException: IO Error: Extension "/Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension" could not be loaded because its signature is either missing or invalid and unsigned extensions are disabled by configuration (allow_unsigned_extensions)
=== connect-config-no-rebind ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
con.execute LOAD OK loaded
con.sql SELECT OK [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')]
module duckdb.sql SELECT ERROR CatalogException: Catalog Error: Table Function with name spur_probe does not exist!
Did you mean "query_table"?
=== set-default-before-load ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
default_is_con True
duckdb.sql LOAD OK None
duckdb.sql CREATE VIEW OK None
duckdb.sql direct SELECT OK [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')]
duckdb.sql view SELECT OK [(1, 'probe-1'), (2, 'probe-2')]
=== recommended-setup-then-user-cell ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
setup LOAD OK None
setup CREATE VIEW OK None
later user cell OK {'direct': [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3')], 'view': [(1, 'probe-1'), (2, 'probe-2')]}
=== recommended-rerun-reuses-connection ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
same_connection_object True
user temp table survived setup rerun OK [(42,)]
managed view after rerun OK [(1, 'probe-1'), (2, 'probe-2')]
=== repeat-load-same-connection ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
first LOAD OK None
second LOAD OK None
SELECT after repeat LOAD OK [(1, 'probe-1')]
=== set-default-after-load-on-con ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
con.execute LOAD OK loaded
default_is_con True
module duckdb.sql SELECT OK [(1, 'probe-1'), (2, 'probe-2'), (3, 'probe-3'), (4, 'probe-4')]
=== replace-after-prior-default-use ===
returncode 0
duckdb 1.5.3
extension /Volumes/Projects/spur/.spur/worktrees/a68f36e6-d82e-4c9d-be00-e48b55154eea/docs/spikes/duckdb-loadable-ext-probe/build/release/spur_probe.duckdb_extension
prior_default_query [(1,)]
old_default_is_current False
new_default_is_con True
duckdb.sql LOAD OK None
duckdb.sql SELECT OK [(1, 'probe-1')]
```
