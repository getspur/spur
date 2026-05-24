# Sharing the sccache Rust cache across git worktrees (and CI workers)

*Published 2026-05-24*

If you run Rust builds across multiple checkouts of the same repo — git worktrees on a dev machine, per-PR worker dirs in CI, ephemeral build VMs — you almost certainly have a shared compilation cache configured (sccache, ccache, or cargo's own incremental). And there's a good chance it isn't actually sharing anything between those checkouts for Rust.

This post is the write-up of an investigation that started with "our GCS-backed sccache reports 0% Rust cache hit rate" and ended with a five-line shell wrapper that delivered **79.5% cross-worktree Rust hits** on a workload that previously recompiled from scratch every time.

The intermediate steps are worth showing because almost every fix I tried first was wrong, and the reasons each one failed are useful in their own right.

---

## TL;DR

Three things must be true for sccache to share Rust compilation results across two checkouts of the same repo:

1. **`CARGO_INCREMENTAL=0`** — sccache marks every incremental rustc call as `CannotCache`. With Cargo's default `CARGO_INCREMENTAL=1` you silently get 0% Rust cache hits even when everything else is perfect.
2. **`SCCACHE_BASEDIRS` covers the exact workspace root, set per-invocation** — not the parent directory. A RUSTC_WRAPPER that does `git rev-parse --show-toplevel` (or walks `$PWD` up for the topmost `Cargo.toml`) and exports `SCCACHE_BASEDIRS=$workspace_root` is the cleanest way.
3. **Do not export `CARGO_TARGET_DIR`** per checkout. sccache hashes every env var starting with `CARGO_`; a per-worker target dir poisons every cache key. If you need artifacts on a separate disk, symlink `target/` instead of using the env var.

The complete wrapper script — copy-pasteable:

```bash
#!/bin/bash
# /usr/local/bin/sccache-worktree
ROOT=""
DIR="$PWD"
while [[ "$DIR" != "/" ]]; do
    if [[ -f "$DIR/Cargo.toml" ]]; then
        ROOT="$DIR"
    fi
    DIR="$(dirname "$DIR")"
done
[[ -n "$ROOT" ]] && export SCCACHE_BASEDIRS="$ROOT"
exec /usr/local/bin/sccache "$@"
```

Set `RUSTC_WRAPPER=/usr/local/bin/sccache-worktree`. Set `CARGO_INCREMENTAL=0`. Don't set `CARGO_TARGET_DIR`. Done.

---

## The setup

We run Rust builds for the SPUR workspace on a shared GCP build VM. Each of our agents (workers, in our parlance) gets its own checkout of the source at `~/spur/worktrees/<UUID>/`, with target artifacts on a separate 400 GB persistent disk at `/mnt/cargo/targets/worktrees/<UUID>/`. sccache server, configured via `SCCACHE_GCS_BUCKET`, talks to a bucket in the same region for shared object storage.

We had it all wired up "correctly":
- `RUSTC_WRAPPER=/usr/local/bin/sccache` in `/etc/profile.d/spur-build.sh`
- `SCCACHE_GCS_BUCKET=wiilearn-spur-sccache-asia`
- Per-worker `CARGO_TARGET_DIR=/mnt/cargo/targets/worktrees/<UUID>`
- Per-worker `SCCACHE_BASEDIRS=$WORKTREE_ROOT`

And `sccache --show-stats` was very polite about it:

```
Compile requests              3880
Cache hits                     683     Cache hits rate           26.66 %
Cache misses                  1879
  Rust misses                 1863     Cache hits rate (Rust)     0.00 %
  C/C++ misses                  16     Cache hits rate (C/C++)   97.71 %

Non-cacheable calls           1275
Non-cacheable reasons:
  incremental                  587
  crate-type                   406
  unknown source language      193
```

C/C++ caching was perfect. Rust caching was *zero*. The bucket was 20 GiB and growing, mostly write-only. The disk it backed was filling up with redundant per-worker `target/` dirs because nothing ever recompiled from cache. Tests were getting `ld: signal 7 [Bus error]` at link time because the disk hit 99%.

So: why 0% Rust?

---

## Wrong turn #1: `CARGO_INCREMENTAL`

The 587 `Non-cacheable: incremental` line is the obvious starting point. sccache and Cargo's incremental compilation are mutually incompatible — sccache refuses to cache any rustc invocation that emits incremental artifacts, because the incremental output depends on previous artifacts in the same target dir. sccache marks them `CannotCache` and runs rustc directly.

This is a real fix. Setting `CARGO_INCREMENTAL=0` in `/etc/profile.d/spur-build.sh` turned 587 silent skips into 587 *actual* cache attempts. Verified end-to-end:

```diff
- Compile requests executed     0
- Cache hits / misses           0 / 0
- Cache hits rate (Rust)        0.00 %  (because nothing was attempted)

+ Compile requests executed   249
+ Cache hits / misses         0 / 249
+ Cache hits rate (Rust)      0.00 %  (still 0, but now writing to GCS)
```

The hit rate is still 0% on a cold run — we just populated the cache. But on a *second* run of the same crate in the same target dir, we got 1/1 Rust hit. So sccache was now actually exchanging artifacts with GCS for Rust.

But cross-target, cross-worker, cross-worktree? Still zero.

**Lesson**: `CARGO_INCREMENTAL=0` is a hard prerequisite. Without it, every other fix is invisible because there's nothing to fix yet — sccache isn't being asked. If you're debugging sccache "doing nothing" on a Cargo project, check the `Non-cacheable reasons:` section of `--show-stats` first.

---

## Wrong turn #2: `--remap-path-prefix`

The next intuition was about paths. Each worker compiles spur-core from a different absolute path:

```
~/spur/main/crates/spur-core
~/spur/worktrees/<UUID>/crates/spur-core
~/spur/worktrees/<OTHER-UUID>/crates/spur-core
```

If those absolute paths leak into rustc's hash inputs, no two workers ever share a hit. Rust has a built-in flag for exactly this case: `--remap-path-prefix=<original>=<replacement>`, which makes rustc rewrite paths in its output (and theoretically in its hash inputs).

So I exported `RUSTFLAGS="--remap-path-prefix=$WORKTREE_ROOT=/spur-src"`. And the hit rate went… *down*. Way down — every Rust invocation became uncached.

Tracing sccache with `SCCACHE_LOG=trace` gave the answer:

```
CannotCache(--remap-path-prefix)
```

sccache 0.8.2 (which we were running) had an explicit deny-list of arguments it refused to cache, and `--remap-path-prefix` was on it. The flag we were adding to *enable* caching was instead *disabling* it.

This was fixed in sccache PR [#2270](https://github.com/mozilla/sccache/pull/2270) (closed January 2025, landed in 0.9+). So I upgraded the binary from 0.8.2 → 0.15.0 and re-tested. Now the flag was at least accepted as cacheable. But the cross-worktree hit rate was still 0%, because — as we'll see below — `--remap-path-prefix` doesn't normalize the inputs that matter.

**Lesson**: `--remap-path-prefix` is a rustc flag about *debug info embedded in output binaries* (so two builds at different paths produce byte-identical `.rlib`s). It is *not* a hash normalization mechanism for sccache. Adding it to RUSTFLAGS doesn't tell sccache "treat these as the same compile" — that's a category error. Some sccache versions tolerate it; older ones reject it outright. Either way it doesn't deliver cross-checkout cache hits.

---

## Wrong turn #3: `SCCACHE_BASEDIRS` set server-side

If `--remap-path-prefix` is the wrong tool, what *is* the right one? sccache has its own mechanism: `SCCACHE_BASEDIRS`, a colon-separated list of directories whose prefixes get stripped from absolute paths *before sccache hashes them*. From the sccache 0.14 README:

> When multiple directories are provided, the longest matching prefix is used.

So the natural fix is: set `SCCACHE_BASEDIRS` to a list that covers every worktree root, plus the shared target parent, plus `$CARGO_HOME`. Then all paths get stripped before hashing and identical source content produces identical hashes regardless of the absolute path it lives at.

I tried it. Set it server-side (because the sccache server inherits env at startup, not per-request — that's the architectural assumption I was operating on):

```bash
SCCACHE_BASEDIRS="$HOME/spur/main:$HOME/spur/worktrees:/mnt/cargo/targets/main:/mnt/cargo/targets/worktrees" \
    sccache --start-server
```

Cross-target hit rate: **still 0%**.

This is where I almost gave up. I went and read the sccache source. The Rust hash key construction in [`src/compiler/rust.rs`](https://github.com/mozilla/sccache/blob/v0.15.0/src/compiler/rust.rs#L1440-L1545) hashes:

1. A static `CACHE_VERSION`
2. The compiler's shared library digests
3. The full commandline (with `-L`, `--extern`, `--out-dir`, `--check-cfg`, `--diagnostic-width` *excluded*)
4. The content digest of all source files
5. The content digest of all externs (rlibs) listed on the commandline
6. The content digest of all static libs
7. The content digest of the target JSON file (if any)
8. **All env vars starting with `CARGO_`** (only excluding `CARGO_MAKEFLAGS`, `CARGO_REGISTRIES_*`, `CARGO_BUILD_JOBS`, `CARGO_ENCODED_RUSTFLAGS`)
9. **The compile's cwd, hashed verbatim**
10. The compiler version

Item 9 — `cwd.hash()` with no apparent normalization — looked like a brick wall. The cwd of the compile *is* the workspace root, which differs per checkout. If sccache hashes it verbatim, there's nothing `SCCACHE_BASEDIRS` could do.

I wrote a long, sad summary explaining that cross-worktree caching was structurally impossible without upstream changes. There's a real open issue, [mozilla/sccache#2652](https://github.com/mozilla/sccache/issues/2652), titled exactly "Wire SCCACHE_BASEDIRS into Rust hash key." I cited it as the blocker.

It is, in fact, *partially* the blocker. But not in the way I read.

**Lesson** (preview of the right answer): reading the code is no substitute for running the experiment. The hash function calls `cwd.hash()` but the `cwd` it receives has already been prefix-stripped against `SCCACHE_BASEDIRS` upstream of that line — when basedirs is set in the **client environment** (i.e. per-request), not the **server's startup environment** (which is read once and forgotten).

---

## The right answer: per-invocation `SCCACHE_BASEDIRS`

Just before throwing in the towel, I checked an old shell script in our local dev tooling — `scripts/sccache-worktree.sh` — that nobody had touched in months. It was a five-line RUSTC_WRAPPER that did exactly one thing: ran `git rev-parse --show-toplevel`, set `SCCACHE_BASEDIRS` to that, and `exec`d sccache.

The associated RCA from April claimed this delivered cross-worktree cache hits on local dev. I had assumed those hits were "incidental" — registry deps share `$CARGO_HOME` so they always hit, dwarfing the workspace-crate misses.

I tested it. Built `spur-core` in `/Volumes/Projects/spur` (main repo), then in `/Volumes/Projects/spur/.spur/worktrees/<UUID>` (a real git worktree on the same machine). The wrapper was active in both invocations.

```
=== RUN A: spur-core in MAIN ===
Cache hits (Rust)     26
Cache misses (Rust)   12
Cache hits rate       68.42%

=== RUN B: spur-core in WORKTREE (different absolute path) ===
Cache hits (Rust)    419   ← +393 cross-worktree hits
Cache misses (Rust)  175
Cache hits rate     70.54%   ← cross-worktree
```

**70% Rust cross-worktree hit rate.** Workspace crates, not just registry deps.

The per-invocation `SCCACHE_BASEDIRS` was the trick. Setting it server-side hadn't worked because… well, the source comments suggest the server reads it at startup, but the actual measured behavior is that the **request env wins** for hash normalization. Whatever the precise mechanism, the empirical answer is: set it per rustc invocation.

Wiring the same trick on our GCP VM took two follow-up debugs:

### The worktrees-on-the-VM-aren't-real-git-worktrees thing

The wrapper used `git rev-parse --show-toplevel` to find the workspace root. Worker checkouts on the VM are rsync'd copies — no `.git` directory — so `git rev-parse` failed and the wrapper fell back to a hardcoded parent path. Same static-parent problem as before, just hidden in a fallback.

Fix: change the wrapper to walk `$PWD` up for the topmost `Cargo.toml`. Works for both git worktrees and plain copies. That's the version shown in the TL;DR.

### The `CARGO_TARGET_DIR` env-hash thing

I deployed the wrapper, re-tested cross-worktree on the VM, and got… **0% Rust hits again**.

This sent me back to the source. The clue was in item 8 of the hash inputs: *all env vars starting with `CARGO_`*. We had this in the worker dispatch:

```bash
export CARGO_TARGET_DIR=/mnt/cargo/targets/worktrees/$UUID
```

Different per worker. Hashed into every cache key. No amount of path normalization could rescue us — the env var defeated all of it.

Comparing the two runs:
- With wrapper + `unset CARGO_TARGET_DIR` (cargo's default `target/` inside source): **79.5%** Rust hits cross-worktree
- With wrapper + per-worker `CARGO_TARGET_DIR` exported: **0%** Rust hits

But we needed target artifacts to live on `/mnt/cargo` (separate disk), not on the small boot disk inside each worktree. The fix was as simple as the bug:

```bash
# Don't:
export CARGO_TARGET_DIR=/mnt/cargo/targets/worktrees/$UUID

# Do:
mkdir -p /mnt/cargo/targets/worktrees/$UUID
ln -sf /mnt/cargo/targets/worktrees/$UUID ~/spur/worktrees/$UUID/target
```

Cargo follows the symlink and writes its artifacts to `/mnt/cargo`. `CARGO_TARGET_DIR` is unset, so it's not in the env hash. The `-L dependency=` and `--extern` args that point at the symlink target are absolute paths into `/mnt/cargo` — but those args are *excluded* from the rustc hash (sccache hashes the rlib *content*, not the path). So nothing about the symlink leaks into any hash input.

Final result, validated on the live VM with a real worker-shaped invocation:

```
=== spur-core in worktree A (cold, with wrapper, no CARGO_TARGET_DIR) ===
Cache misses (Rust)   575     ← populates GCS

=== spur-core in worktree B (different absolute path, identical source) ===
Cache hits (Rust)     434
Cache misses (Rust)   112
Cache hits rate (Rust) 79.5%   ← cross-worktree

Wall time: 46s (vs ~70-90s for cold without wrapper)
Target physical location: /mnt/cargo/targets/worktrees/<UUID>/  (via symlink)
```

---

## The complete recipe

For anyone running sccache + Cargo across multiple checkouts of the same repo, on the same machine or via a shared remote cache:

### 1. Disable Cargo's incremental compilation

In your global build env (`/etc/profile.d/...`, CI env, `~/.cargo/config.toml`'s `[env]`):

```bash
export CARGO_INCREMENTAL=0
```

This is non-negotiable. Without it, sccache silently drops every rustc call as `Non-cacheable: incremental` and you'll spend hours wondering why your cache hit rate is 0%.

### 2. Install a per-invocation `SCCACHE_BASEDIRS` wrapper

Save as `/usr/local/bin/sccache-worktree`:

```bash
#!/bin/bash
# Walks $PWD up to the topmost Cargo.toml (workspace root), exports
# SCCACHE_BASEDIRS=that, then execs sccache. Works for git worktrees,
# rsync'd copies, container mounts — anything where rustc cd's into
# a directory under a workspace.
ROOT=""
DIR="$PWD"
while [[ "$DIR" != "/" ]]; do
    if [[ -f "$DIR/Cargo.toml" ]]; then
        ROOT="$DIR"
    fi
    DIR="$(dirname "$DIR")"
done
[[ -n "$ROOT" ]] && export SCCACHE_BASEDIRS="$ROOT"
exec /usr/local/bin/sccache "$@"
```

```bash
chmod 0755 /usr/local/bin/sccache-worktree
```

Point Cargo at it:

```bash
export RUSTC_WRAPPER=/usr/local/bin/sccache-worktree
```

### 3. Don't export `CARGO_TARGET_DIR`

Let Cargo use its default `target/` inside the workspace.

If you need artifacts on a different disk (e.g. you have a small boot disk and a big data disk), **symlink** instead:

```bash
# at worktree provision time:
mkdir -p /big/disk/targets/$WORKTREE_KEY
ln -sf /big/disk/targets/$WORKTREE_KEY $WORKTREE_ROOT/target
```

Cargo follows the symlink, writes to `/big/disk`, and `CARGO_TARGET_DIR` stays unset → not in the env hash → cross-worktree cache hits still work.

### 4. Use sccache ≥ 0.9 (preferably 0.15+)

0.8.2 has known issues:
- Refuses to cache rustc calls carrying `--remap-path-prefix` (PR [#2270](https://github.com/mozilla/sccache/pull/2270) fix, closed Jan 2025)
- Hashes `CARGO_ENCODED_RUSTFLAGS` causing spurious misses (PR [#2651](https://github.com/mozilla/sccache/pull/2651), in 0.15.0)

The first one bites you if you ever add `--remap-path-prefix` to RUSTFLAGS. The second is more subtle — if your RUSTFLAGS varies even slightly between invocations (different lint configs, profile-specific flags), you'll miss.

### 5. Verify

After a cold build that populates the cache:

```
$ sccache --show-stats | head -15
Compile requests            829
Cache hits                  76
Cache hits (Rust)            0     ← will be 0 on cold run (correct)
Cache misses              594
Cache misses (Rust)       546
```

Then build the same workspace in a *different absolute path* (git worktree, fresh checkout, whatever):

```
$ cd /different/path/to/same/source && cargo check -p mycrate
$ sccache --show-stats | head -15
Compile requests           1660
Cache hits                  152
Cache hits (Rust)            ?     ← THIS is the number that matters
```

If `Cache hits (Rust)` is well under 50% after a warm cross-path build, something is wrong. Common culprits, in order of likelihood:

1. `CARGO_INCREMENTAL` is set to `1` somewhere (check shell env, `~/.cargo/config.toml`, `cargo`'s own defaults if you're on an old version)
2. `CARGO_TARGET_DIR` is set to different values across paths
3. The wrapper isn't actually being invoked — verify `RUSTC_WRAPPER` is your wrapper, not bare `sccache`
4. Some other `CARGO_*` env var differs across runs (rare; check `env | grep ^CARGO_`)

---

## Why the wrong turns happened

The investigation had three pieces of misleading evidence that almost convinced me to give up:

**1. The sccache source code reads like `cwd` is hashed verbatim.** It is, but not from the value you'd guess. The `cwd` parameter at `rust.rs:1540` has already been prefix-stripped against the per-request `SCCACHE_BASEDIRS` before that line runs. The code says `cwd.hash(...)`, the docs say "Base directories: strips prefixes from absolute paths before hashing," and reconciling those required actually running the experiment, not staring at the function.

**2. The local wrapper had been "working" the whole time but nobody had measured.** The April RCA at `docs/rca/2026-04-27-sccache-worktree-cache-miss.md` reported 271 cross-worktree hits with the wrapper. I assumed those were almost entirely C/C++ deps and registry hits — the kind of accidental sharing that doesn't depend on path normalization. They were actually workspace-crate Rust hits. The RCA's stats didn't break down hits by language so the real victory was hidden.

**3. My controlled minimal-repro test produced a misleading negative.** I built a tiny `Cargo.toml` + `lib.rs` in `/tmp/A`, ran with the wrapper, then again in `/tmp/B`. Zero cross-path hits. I took this as confirmation that the wrapper didn't work for Rust. What actually happened: the wrapper resolved `$PWD = /tmp/A` walking up for `Cargo.toml`, set `SCCACHE_BASEDIRS=/tmp/A` for that invocation. Then for `/tmp/B` it set `SCCACHE_BASEDIRS=/tmp/B`. Each invocation correctly *stripped its own path* — but the stripped *content* (just `Cargo.toml` + `src/lib.rs` with relative paths) still produced different hashes because the *cwd* (after stripping) was `""` vs `""`, but the **rustc command line** that cargo emits for a fresh build of a one-file crate genuinely differs between invocations in ways I didn't account for (output filenames, fingerprint metadata, etc.). The test wasn't measuring what I thought it was measuring.

When I switched to a real spur-core build in two real worktrees of a real workspace — with a full dep graph, stable Cargo.lock, and the wrapper picking up the actual workspace root in both cases — it worked.

**Meta-lesson**: when an experiment "proves" that a published, in-use tool doesn't work, the experiment is wrong more often than the tool is. Reproduce the *actual* scenario, not the simplified one. The minimum reproducible example should be your *second* test, not your first.

---

## What you don't need

A few things I tried that didn't help, and that you don't need to try:

- **`--remap-path-prefix`** — wrong tool. It affects what's written *into* rlibs (debug info), not what's hashed *about* compilations. Some sccache versions explicitly refuse to cache invocations carrying this flag.
- **Server-side `SCCACHE_BASEDIRS`** — the server reads its env at startup, but for hash normalization the *request env* is what counts. Wrapping per-invocation is correct.
- **Mount namespaces (`unshare -m`) with shared sccache server** — structurally incompatible. The sccache server forks rustc in the *host* namespace where your bind mount doesn't exist.
- **Patching sccache** to add a hypothetical `SCCACHE_CWD_REMAP` env var — the existing `SCCACHE_BASEDIRS` already does what you need, just not from the env you'd first try.
- **Upgrading to sccache 0.15.0** — strictly speaking, the per-invocation wrapper trick works on 0.8.2 too. We did upgrade for unrelated reasons (better `--remap-path-prefix` handling, `CARGO_ENCODED_RUSTFLAGS` exclusion) but it's not load-bearing for cross-worktree caching itself.

---

## Numbers

For our SPUR workspace (~200 crates, mostly Rust), measured on a GCP `c2d-highcpu-16` build VM with a same-region GCS-backed sccache bucket:

| Scenario | Wall time | Rust hit rate |
|---|---|---|
| Cold `cargo check -p spur-core` in a fresh worktree, before fix | ~75 s | 0.00 % |
| Same, after fix, populates GCS | ~70 s | 0.00 % (correct — cold) |
| Same workspace, *different* worktree, after fix | **46 s** | **79.5 %** |
| Same target dir, second run (warm local cache) | 1.85 s | 100 % (control) |

The 79.5% hit rate isn't 100% because some rustc invocations are non-cacheable by design (`crate-type=bin`, `crate-type=proc-macro`, build scripts). That's a sccache + Rust ecosystem limitation we live with. The remaining wall time is dominated by linking the binaries that *aren't* cacheable, plus the irreducible bookkeeping cargo does even when nothing recompiles.

For a 16-worker concurrent build, the practical impact on our setup was:
- Per-worker `target/` size dropped from ~40-110 GB to ~5-15 GB (since incremental artifacts are no longer written and most of the deps stayed in GCS)
- Disk-pressure incidents (the `ld: signal 7 [Bus error]` link failures from full disk) went from daily to zero
- First-build wall time for a fresh worker dropped from ~5 min to ~2 min

---

## Acknowledgements + links

- sccache: [mozilla/sccache](https://github.com/mozilla/sccache)
- The `SCCACHE_BASEDIRS` mechanism: introduced in [sccache#1880](https://github.com/mozilla/sccache/pull/1880)
- Open issue tracking the limits of `SCCACHE_BASEDIRS` for git worktrees: [#2595](https://github.com/mozilla/sccache/issues/2595), [#2652](https://github.com/mozilla/sccache/issues/2652)
- Cargo's incremental compilation docs: [doc.rust-lang.org/cargo/reference/profiles.html#incremental](https://doc.rust-lang.org/cargo/reference/profiles.html#incremental)
- The earlier SPUR RCA that documented the wrapper but didn't measure its full effect: `docs/rca/2026-04-27-sccache-worktree-cache-miss.md`
- The implementation in this repo: `scripts/gcp-build/startup.sh` (wrapper install + profile.d wiring), `scripts/gcp-build/build.sh` (target symlink)
