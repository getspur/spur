# GCP Build Helpers

## Production Builder Shape

The default remote builder is `c4d-standard-16` in `asia-southeast1-a`, running
as a Spot VM with a persistent 300 GB `hyperdisk-balanced` cache disk. This keeps
the 16 vCPU footprint of the previous `c4d-highcpu-16` builder while doubling RAM
to avoid OOM-killed concurrent `rust-lld` link bursts. Hyperdisk Balanced is
cheaper than the old 300 GB SSD Persistent Disk while still providing enough IOPS
for the cargo target/cache workload.

Remote `sccache` uses a multi-level cache on the builder: a 16 GB tmpfs mounted
at `/mnt/sccache-ram` is the fast L1 disk backend (`SCCACHE_CACHE_SIZE=15G`),
and the regional GCS bucket remains the shared L2 backend. GCS hits are
backfilled into tmpfs by `sccache 0.15.0`, so repeated hot compile artifacts
avoid the network round trip while preserving cross-VM/worktree reuse through
the bucket.

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
- cache disk: `spur-cargo-cache`
- sccache bucket: `wiilearn-spur-sccache-asia`

Use environment overrides only for one-off benchmarks, for example
`VM_NAME=spur-builder-c4d-bench CACHE_DISK=spur-cargo-cache-c4d-bench`.

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
scripts/spur-cargo run -p xtask -- install --remote --notebook-channel green
```

The xtask command is a thin wrapper around the existing shell transport:

1. `scripts/gcp-build/build.sh --auto-spin -- build --release -p spur-cli --locked`
2. `scripts/gcp-build/fetch.sh --to <dest>/spur target/release/spur`

`build.sh` syncs the current worktree and runs the locked release cargo build.
`fetch.sh` copies `target/release/spur` from the VM's per-worktree target
directory to the requested local destination.

`cargo xtask install` without `--remote` stays local and installs `spur` only.
Notebook binaries and `Jute.app` are owned by the standalone
`getspur/spur-notebook` repository after the green cutover.

## Remote Frontend (vitest) Tests

Notebook frontend tests moved to the standalone `getspur/spur-notebook`
repository. Run vitest from that checkout; this repo's `build.sh
--frontend-test` path is intentionally disabled after the green cutover.

## CI Fixture Lockstep Auth

The monorepo `lint-invariants` workflow checks `sdk/fixtures/port-store`
against golden fixtures from the standalone notebook repository. Because
`getspur/spur-notebook` is private, repository settings must define
`SPUR_NOTEBOOK_CHECKOUT_TOKEN` with read access to the private
`getspur/spur-notebook` repository. The workflow preflights that secret before
the cross-repo checkout and passes it explicitly to `actions/checkout`.

## Post-Split pnpm Wrapper

Notebook frontend pnpm commands moved to the standalone
`getspur/spur-notebook` repository. From this repo, `scripts/spur-pnpm` is now a
compatibility wrapper that prints migration guidance unless `SPUR_NOTEBOOK_REPO`
points at a local standalone checkout:

```sh
SPUR_NOTEBOOK_REPO=/path/to/spur-notebook scripts/spur-pnpm test -- src/ui/notebook/NotebookCells.test.tsx
SPUR_NOTEBOOK_REPO=/path/to/spur-notebook scripts/spur-pnpm run typecheck
```

The wrapper forwards to `pnpm --dir "$SPUR_NOTEBOOK_REPO/jute-notebook" ...`.
Run remote pnpm workflows from the standalone repo's own tooling.

`deno` is provisioned system-wide by `startup.sh` (pinned, `/usr/local/bin/deno`)
for standalone notebook kernel tests and auxiliary builder workflows.

## Runtime Constraints

Remote binaries are built on the GCP VM image. Today that means Debian 12, so
the fetched Linux binaries require a compatible glibc ABI on the machine where
they run. They are suitable for matching developer Linux environments, not broad
binary distribution.

The standalone `spur-notebook` Linux binary still depends on the Tauri runtime
stack. Its target Linux host needs WebKit/GTK runtime libraries installed; that
artifact is built and distributed from `getspur/spur-notebook`.
