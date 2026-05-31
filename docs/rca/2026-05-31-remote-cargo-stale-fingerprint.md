# RCA: remote cargo builds compile against a stale path-dependency artifact

- **Date:** 2026-05-31
- **Component:** `scripts/gcp-build/build.sh`, `scripts/spur-cargo` (remote-build pipeline)
- **Severity:** High — silent wrong builds. The remote toolchain reports success
  while compiling source the worker did not write.

## Symptom

A codex worker added `NotebookStore::path()` to the `jute` crate
(`crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs`), synced,
and ran a focused remote check
(`spur-cargo test -p spur-notebook notebook_dag_status_snapshot_uses_tauri_payload_shape`).
The remote build kept compiling a **stale `jute`** that contained an earlier
edit (a command helper) but **not** the new `path()` method — despite the source
syncing cleanly. `spur-cargo clean -p jute` followed by a rerun did not help.

## Root cause

Two independent defects compounded.

### 1. No mtime normalization after rsync (the real bug)

`spur-notebook` depends on `jute` as a **path dependency**
(`crates/spur-notebook/Cargo.toml:24`). Cargo decides whether to rebuild a path
dependency by comparing **source-file mtimes** against the cached artifact's
mtime — not by content hash.

The pipeline syncs with `rsync -az` (`build.sh`). `-a` implies `-t`, so rsync
**preserves the dev machine's mtimes** on the VM copy. The remote target dir is
persistent (`/mnt/cargo/targets/<worktree-key>`, symlinked in at sync time) and
survives across builds and VM restarts. There was **no step anywhere** that
re-stamped synced files to the VM clock.

Consequently, when the dev Mac's clock/file-mtimes lag the VM clock even by a
few seconds — the regime a worker hits when iterating edit→build→edit→build —
a freshly edited file lands on the VM with an mtime **older** than a `jute`
artifact the VM built moments earlier. Cargo declares `jute` fresh and skips the
recompile, so `path()` never gets compiled in. The command helper the worker
*did* see was baked into that earlier artifact; `path()`, added afterward, was
shadowed by it. It is all one crate and one mtime comparison — no cross-crate
behavior involved.

A clock probe confirmed the necessary direction: the VM is **not behind** the
dev Mac (it is at least even, likely a few seconds ahead), which is exactly the
condition under which a preserved dev-mtime can fall below a VM artifact mtime.

### 2. `spur-cargo clean` was local-only (the mask)

`scripts/spur-cargo` dispatched only `build|check|test|clippy|doc` to the remote
VM. `clean` was **not** remote-capable, so `spur-cargo clean -p jute` fell
through to the **local** cargo and cleaned the unused local target dir — a no-op
against the remote `/mnt/cargo` artifact. The worker's cache-bust therefore did
nothing, making the staleness look intractable.

## Fix

`scripts/gcp-build/build.sh`
- Capture the exact set of files rsync transferred via `--out-format='%n'`
  (verified supported by macOS `openrsync`; emits only created/updated files,
  nothing for unchanged files, no directory entries).
- After sync, `touch -c` exactly those files on the VM to the VM's "now". This
  guarantees changed sources are newer than any prior artifact (correct rebuild)
  while leaving unchanged files' mtimes alone (incremental cache preserved).

`scripts/spur-cargo`
- Add `clean` to the remote-capable subcommand set so cache-busting cleans the
  remote target where the artifacts actually live. Still falls back to local
  clean when the VM is unavailable.

## Why not other approaches

- **`rsync --no-times`**: makes rsync stamp files at receipt time, but then the
  quick-check retransfers everything every run (over-transfer) and still relies
  on the VM clock — strictly worse than touching only the delta.
- **Touch *all* synced files to now**: forces a full rebuild every sync,
  defeating the entire remote-cache purpose.
- **Content-hash freshness (`-Z checksum-freshness`)**: cargo-unstable
  (nightly-only); not viable on the stable toolchain.

## Verification

- `bash -n` clean on both scripts.
- `openrsync --out-format='%n'` confirmed locally: first sync lists all new
  files; a second sync with one changed file lists only that file; exit 0.

## Defense-in-depth / follow-ups

- The VM has no `chrony` running. Confirm `systemd-timesyncd` is active so the VM
  clock stays NTP-disciplined; a large VM-ahead skew would widen the window the
  touch step closes (the touch fix is correct regardless, but tight clocks keep
  the delta small).
