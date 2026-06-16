# GCP Build Helpers

## Production Builder Shape

The default remote builder is `c4d-standard-16-lssd` in `asia-southeast1-a`,
running as a Spot VM with one attached 375 GiB Local SSD. The Local SSD is
mounted at `/mnt/cargo` and used for cargo target dirs, cargo-home, rustup,
pnpm stores, temp files, and the local sccache L1. This trades persistent
target/ reuse for much lower-latency random I/O, while the regional GCS sccache
bucket remains the durable L2 across Spot deletion and VM recreation.

Remote `sccache` uses a multi-level cache on the builder: a 50 GB disk backend
under `/mnt/cargo/sccache` is the fast Local SSD L1
(`SCCACHE_CACHE_SIZE=50G`), and the regional GCS bucket remains the shared L2
backend. GCS hits are backfilled into Local SSD by `sccache 0.15.0`, so repeated
hot compile artifacts avoid the network round trip while preserving
cross-VM/worktree reuse through the bucket.

Source sync prefers direct SSH to the VM's external IP on
`SPUR_DIRECT_SSH_PORT` (default `22`) and falls back to IAP. Fresh VMs keep sshd
on tcp:22 for GCE/IAP compatibility. Set `SPUR_DIRECT_SSH=0` to force IAP-only.
Direct SSH probing defaults to a short 3s connection timeout
(`SPUR_DIRECT_SSH_CONNECT_TIMEOUT`) so blocked public SSH fails quickly; IAP
probing defaults to 10s (`SPUR_IAP_SSH_CONNECT_TIMEOUT`) because tunnel setup can
need more time for SSH banner exchange.

`build.sh` locally backpressures remote dispatches before syncing to the VM.
By default, at most three callers from the same workstation are admitted at a
time; later callers wait in FIFO ticket order. Override the limit with
`SPUR_BUILD_MAX_CONCURRENT` or set it to `0` to disable the queue. The queue root
defaults to `/tmp/spur-gcp-build-queue` and can be changed with
`SPUR_BUILD_QUEUE_DIR`.

The production resource names intentionally stay stable:

- VM: `spur-builder`
- sccache bucket: `wiilearn-spur-sccache-asia`

Use environment overrides only for one-off benchmarks, for example
`VM_NAME=spur-builder-c4d-bench VM_MACHINE_TYPE=c4d-standard-32-lssd`.

## Local macOS GCS sccache

Local macOS builds can opt into the same GCS-backed sccache bucket without using
the remote builder:

```sh
SPUR_REMOTE=0 SPUR_SCCACHE_GCS=1 scripts/spur-cargo check -p spur-core
SPUR_SCCACHE_GCS=1 scripts/spur-cargo clippy -p spur-core
```

`scripts/spur-cargo` injects `scripts/sccache-worktree.sh`, disables Cargo
incremental compilation when `CARGO_INCREMENTAL` is unset, and restarts an idle
local sccache server into the GCS config when needed. The wrapper exports
`SCCACHE_GCS_BUCKET=${GCP_PROJECT:-wiilearn}-spur-sccache-asia` by default and
`SCCACHE_MULTILEVEL_CHAIN=disk,gcs` for sccache builds that support multi-level
caching. Older sccache builds ignore the multi-level variable and use GCS as the
single configured backend.

Run `scripts/gcloud_auth auth` once per local login session to authenticate
gcloud and start a localhost token endpoint plus local `sccache` in
`disk,gcs` mode. The token endpoint uses gcloud service-account impersonation,
so no long-lived service-account key is stored on disk. If GCS startup fails,
`spur-cargo` exits before invoking Cargo and points at
`${SCCACHE_ERROR_LOG:-/tmp/spur-sccache-gcs.log}`.

Override `SCCACHE_GCS_RW_MODE=READ_ONLY` to consume the shared bucket without
writing local macOS artifacts back to it.

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

## Remote pnpm Commands

Run notebook frontend pnpm commands through the same remote sync path with:

```sh
scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx
scripts/spur-pnpm run typecheck
```

`scripts/spur-pnpm` dispatches to `build.sh --pnpm` by default and falls back to
local pnpm only when the VM is unavailable. On the VM, pnpm uses the shared
store at `/mnt/cargo/pnpm-store`, while each worktree's frontend
`node_modules` is a symlink to `/mnt/cargo/pnpm-nm/<worktree-key>`. Keeping both
paths on `/mnt/cargo` lets pnpm hard-link packages from the content-addressable
store instead of copying them across filesystems. Override the activated pnpm
version with `SPUR_PNPM_VERSION`.

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
