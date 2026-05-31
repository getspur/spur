# GCP Build Helpers

## Remote xtask Install

Run this from any SPUR worktree to build the Linux release binaries on the GCP
builder VM and install them into `${CARGO_HOME:-$HOME/.cargo}/bin` locally:

```sh
cargo xtask install --remote
```

The xtask command is a thin wrapper around the existing shell transport:

1. `scripts/gcp-build/build.sh --auto-spin -- build --release -p spur-cli -p spur-notebook --features spur-notebook/custom-protocol --locked`
2. `scripts/gcp-build/fetch.sh --bins`

`build.sh` syncs the current worktree, builds `crates/spur-notebook/jute-notebook`
frontend assets on the VM when the notebook production feature is present, then
runs the locked release cargo build. `fetch.sh --bins` copies
`target/release/spur` and `target/release/spur-notebook` from the VM's
per-worktree target directory to the local cargo bin directory.

`cargo xtask install` without `--remote` stays local. On macOS, local install
still builds `spur` and installs `Jute.app`; `cargo xtask install --remote`
instead fetches the Linux `spur` and `spur-notebook` binaries only and does not
build or install a `.app` bundle.

## Remote Frontend (vitest) Tests

`vitest` is a per-project devDependency under
`crates/spur-notebook/jute-notebook/node_modules` — it is gitignored and never
synced, so a bare `vitest` is not on the VM's PATH. Run the suite on the VM
(reusing the same worktree sync as the build) with:

```sh
scripts/gcp-build/build.sh --auto-spin --frontend-test
```

This syncs the current worktree, runs `npm ci` on the VM only when
`node_modules` is missing or older than `package-lock.json`, then runs the
`test` npm script (`vitest run`). Override the command via
`SPUR_FRONTEND_TEST_CMD` (e.g. `SPUR_FRONTEND_TEST_CMD='npx vitest run src/foo'`).

`deno` is provisioned system-wide by `startup.sh` (pinned, `/usr/local/bin/deno`)
so the spur-notebook Deno Jupyter kernel tests run on the VM instead of skipping.

## Runtime Constraints

Remote binaries are built on the GCP VM image. Today that means Debian 12, so
the fetched Linux binaries require a compatible glibc ABI on the machine where
they run. They are suitable for matching developer Linux environments, not broad
binary distribution.

`spur-notebook` also depends on the Tauri Linux runtime stack. The target Linux
host still needs WebKit/GTK runtime libraries installed; fetching the binary
does not vendor those shared system dependencies.
