# Notebook Port On-Disk Contract

This document is the normative contract for SPUR notebook ports. The contract is
cross-language and on-disk: Rust, Python bootstrap code, and JavaScript bootstrap
code must continue to write and read the same files.

## On-Disk Layout

Notebook ports live under:

```text
~/.spur/notebooks/<nb-id>/ports/
```

Each written port version is an Arrow IPC file:

```text
~/.spur/notebooks/<nb-id>/ports/<port>@v<N>.arrow
```

The manifest is:

```text
~/.spur/notebooks/<nb-id>/ports/manifest.json
```

`<nb-id>` is:

```text
"nb-" + blake3_hex(normalized_path)[..24]
```

`<nb-id>` is computed only in Rust by `notebook_id_for_path`
(`crates/spur-notebook/src/dag/inject.rs:7-11`) and injected into the bootstraps
as a string literal. Finding 2: this is intentionally single-source,
verified-safe behavior; no code path re-derives the notebook id outside the Rust
helper.

## Manifest Shape

`manifest.json` has this shape:

```json
{
  "ports": {
    "<port>": {
      "path": "<path>",
      "version": 1,
      "schema": {}
    }
  }
}
```

The Rust manifest structs are `PortManifest` and `PortEntry`
(`crates/spur-notebook/src/dag/ports.rs:53-62`). `version` is a `u64`; `schema`
is `arrow_schema::Schema` and is deserialized directly into
`PortEntry.schema` (`crates/spur-notebook/src/dag/ports.rs:56`).

Therefore `schema` MUST be byte-faithful to `arrow-schema` v58 serde. Field JSON
uses the Arrow serde field shape:

```json
{
  "name": "<field-name>",
  "data_type": "<arrow-schema serde data type>",
  "nullable": true,
  "dict_id": 0,
  "dict_is_ordered": false,
  "metadata": {}
}
```

Do not invent or normalize type spellings in a bootstrap. If a spelling differs
from this document, defer to the Rust serde oracle test
(`crates/spur-notebook/src/dag/ports.rs:458-508`) and `arrow-schema` v58 serde.

## Schema Authority

The Arrow IPC FILE footer is the authoritative schema. The manifest `schema` is
a faithful convenience copy for quick inspection and Rust deserialization.

After t1, emitters either match Arrow serde exactly or raise at `put()` for
unrepresentable types. They never write a wrong label.

## Write Protocol

For each `put(port, value)`:

1. Compute `version = previous_version + 1`, starting at `1`.
2. Write `<port>@v<N>.arrow` using Arrow IPC file format.
3. Update `manifest.json` with the new `path`, `version`, and faithful `schema`.
4. Atomically replace `manifest.json` via temp-file plus rename.

Rust follows this flow in `PortStore::put` and `persist_manifest`
(`crates/spur-notebook/src/dag/ports.rs:156-190`,
`crates/spur-notebook/src/dag/ports.rs:229-241`). The Python bootstrap writes an
IPC file with `pyarrow.ipc.new_file` and replaces the manifest with
`os.replace` (`crates/spur-notebook/src/dag/inject.rs:37-58`,
`crates/spur-notebook/src/dag/inject.rs:119-131`). The JavaScript bootstrap writes
IPC file bytes with `tableToIPC(table, "file")` and renames the temp manifest
(`crates/spur-notebook/src/dag/inject.rs:374-390`,
`crates/spur-notebook/src/dag/inject.rs:439-448`).

## Implementations

This on-disk contract is implemented by three hand-written bodies:

- Rust `PortStore`, including `impl PortStore::put` and
  `impl PortStore::persist_manifest`.
- Python helper body `crates/spur-notebook/jute-notebook/src-tauri/src/assets/ports_bootstrap.py`,
  exposed by `python_bootstrap`.
- JavaScript/Deno helper body `crates/spur-notebook/jute-notebook/src-tauri/src/assets/ports_bootstrap.js`,
  exposed by `javascript_bootstrap`.

The helper bodies do not receive a wrapped per-cell root literal. Rust computes
the root with `notebook_port_root` and supplies it to kernels as
`SPUR_NOTEBOOK_PORT_ROOT` via `apply_notebook_port_root_env`; the Python and
JavaScript helpers read that environment variable when they install `spur`.

The helper is injected once for each fresh kernel session by
`inject_port_bootstrap`, using a silent Jupyter `execute_request`, before user
cells run. `drain_port_bootstrap_events` treats injection failures as kernel
startup/restart failures. User cell source is not per-cell wrapped for ports.

Finding 1: this replication is intentional because kernels run in separate
runtimes. There is no static edge that binds the implementations together. The
contract is enforced by tests named for the behaviors they protect, including
`generated_schema_shape_matches_arrow_schema_serde`,
`generated_helpers_fail_loud_instead_of_utf8_fallback`,
`python_helper_round_trips_arrow_and_emits_display_mirror`, and
`start_local_kernel_spawned_env_includes_notebook_port_root_and_preserves_parent_env`.

The design reference for the session-injected helper contract is
`docs/superpowers/specs/2026-06-02-notebook-port-integration-design.ipynb`.

## Change Checklist

When changing this contract:

1. Update Rust `PortStore`.
2. Update `ports_bootstrap.py` and keep `python_bootstrap` as the loader.
3. Update `ports_bootstrap.js` and keep `javascript_bootstrap` as the loader.
4. Verify `SPUR_NOTEBOOK_PORT_ROOT` is still supplied by
   `apply_notebook_port_root_env` and consumed by both helpers.
5. Verify `inject_port_bootstrap` still installs the helper once per kernel
   session via a silent `execute_request`.
6. Update the Rust serde oracle / manifest-shape tests.
7. Update the Python/JavaScript helper round-trip tests.
8. Update the spec notebook and this document.
