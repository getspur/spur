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

There are three hand-written implementations:

- Rust `PortStore`: `crates/spur-notebook/src/dag/ports.rs:101-242`
- `python_bootstrap`: `crates/spur-notebook/src/dag/inject.rs:20-271`
- `javascript_bootstrap`: `crates/spur-notebook/src/dag/inject.rs:273-733`

Finding 1: this replication is intentional because kernels run in separate
runtimes. There is no static edge that binds the three implementations together.
The contract is enforced only by tests:

- Rust serde oracle and manifest-shape tests:
  `crates/spur-notebook/src/dag/ports.rs:419-508`
- Deno cross-language round-trip test:
  `crates/spur-notebook/tests/notebook_read_tools.rs:601-766`

## Change Checklist

When changing this contract:

1. Update Rust `PortStore`.
2. Update `python_bootstrap`.
3. Update `javascript_bootstrap`.
4. Update the Rust serde oracle / manifest-shape tests.
5. Update the Deno cross-language round-trip tests.
6. Update this document.
