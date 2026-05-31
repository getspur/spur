# Deno Notebook Kernel Spike

## Summary

The Deno runtime path is feasible for SPUR's JavaScript notebook port helper. A
hand-written kernelspec with `deno jupyter --kernel --conn {connection_file}`
launched successfully through `jupyter_client`, executed a headless cell, imported
`npm:apache-arrow@21.1.0`, wrote an Arrow IPC File under
`~/.spur/notebooks/deno-kernel-spike/ports`, renamed it into place, read it back,
and emitted a raw Jupyter display bundle.

## Runtime

Installed Deno with the official shell installer because `deno` was absent from
PATH:

```text
Deno was installed successfully to /private/tmp/deno/bin/deno
```

Validated version:

```text
deno 2.8.1 (stable, release, aarch64-apple-darwin)
v8 14.9.207.2-rusty
typescript 6.0.3
```

`deno jupyter --help` confirms the current connection flag is `--conn <conn>`.
There is no `--conn-file` flag in Deno 2.8.1.

## Kernelspec

Use this `kernel.json` at `<data_dir>/kernels/deno/kernel.json`:

```json
{
  "argv": [
    "deno",
    "jupyter",
    "--kernel",
    "--conn",
    "{connection_file}"
  ],
  "display_name": "Deno",
  "language": "typescript"
}
```

For this spike, the temporary test kernelspec used the same `argv` and prepended
`/private/tmp/deno/bin` to `PATH` so `deno` resolved to the official-script
installation.

## Permission Decision

Do not add Deno permission flags to the kernelspec. `deno jupyter` currently runs
notebook code with `--allow-all`; the Deno documentation calls this a temporary
limitation. The local help for `deno jupyter` also exposes no granular
`--allow-read`, `--allow-write`, `--allow-env`, `--allow-net`, or
`--allow-import` options for the subcommand.

The smoke cell proved these permissions are available non-interactively:

- `Deno.env.get("HOME")`
- `Deno.mkdirSync`
- `Deno.makeTempFileSync`
- `Deno.writeFileSync`
- `Deno.renameSync`
- `Deno.readFileSync`
- `await import("npm:apache-arrow@21.1.0")`
- `await Deno.jupyter.display(bundle, { raw: true })`

The first sandboxed attempt failed before runtime validation because Codex was
not allowed to create `~/.spur/notebooks/...`. Re-running the same kernel launch
outside the workspace sandbox succeeded. Separately, Deno's default npm cache
under `~/Library/Caches/deno` was not writable inside the sandbox, so the smoke
harness set `DENO_DIR=/private/tmp/deno-cache`. This is a harness workaround, not
part of the kernelspec. Current SPUR `LocalKernel::start` has a `TODO: Handle
spec.env`, so relying on a kernelspec `env` stanza would not work until that Rust
launcher behavior changes.

## npm:apache-arrow Resolution

Pin imports to:

```ts
await import("npm:apache-arrow@21.1.0");
```

`deno info npm:apache-arrow` resolved the unpinned package to
`apache-arrow@21.1.0` during the spike. Pinning the exact npm specifier avoids
future unpinned major-version drift while keeping Deno's native npm resolver.

The validated Arrow IPC File path used:

```ts
const arrow = await import("npm:apache-arrow@21.1.0");
const table = arrow.tableFromArrays({
  id: [1, 2, 3],
  label: ["one", "two", "three"],
});
const ipc = arrow.tableToIPC(table, "file");
Deno.writeFileSync(tmpPath, ipc);
Deno.renameSync(tmpPath, finalPath);
const back = arrow.tableFromIPC(Deno.readFileSync(finalPath));
```

## Headless Smoke Result

The real Jupyter launch used:

```json
{
  "argv": ["deno", "jupyter", "--kernel", "--conn", "{connection_file}"],
  "display_name": "Deno",
  "language": "typescript"
}
```

The executed cell wrote and read:

```text
/Users/kevintruong/.spur/notebooks/deno-kernel-spike/ports/roundtrip.arrow
```

The Jupyter shell reply was:

```json
{
  "reply_status": "ok",
  "rows": 3,
  "id0": 1,
  "label2": "three",
  "bytes": 762
}
```

The kernel process printed this warning on startup:

```text
Warning "deno jupyter" is unstable and might change in the future.
```

That warning is not a blocker for this spike, but provisioning should treat the
`deno jupyter` CLI shape as version-sensitive and keep the exact `--conn` argv
covered by a smoke test.

## References

- Deno Jupyter docs: https://docs.deno.com/runtime/reference/cli/jupyter/
- Deno installer docs: https://docs.deno.com/runtime/manual/getting_started/installation
