# Windows (x86_64-pc-windows-msvc) cross-compile POC — cargo-xwin

**Status:** POC complete — Go (2026-07-07). `spur.exe` (PE32+ x86-64,
361 MB, release + thin-LTO) builds green on the aarch64 Linux build VM and
fetches locally via the S3 path. Runtime execution is NOT yet validated
(no Windows host in the loop — see Known gaps). Companion to the macOS
pattern in `2026-07-07-zigbuild-macos-cross-poc.md` — same build VM, same
dispatch pipeline, different toolchain strategy.

## Goal

Prove that `spur-cli` can be cross-compiled to a Windows PE (`spur.exe`,
`x86_64-pc-windows-msvc`) on the aarch64 Linux cloud-build VM, wire it as
`scripts/spur-cargo xwin`, and enumerate the remaining gaps to actual
Windows OS support.

## Approach

[cargo-xwin](https://github.com/rust-cross/cargo-xwin) wraps cargo: it
downloads the MSVC CRT + Windows SDK headers/libs from Microsoft (xwin,
cached in `XWIN_CACHE_DIR=/mnt/cargo/xwin`, `XWIN_ACCEPT_LICENSE=1`), points
`cc`-rs at `clang-cl`, and links with `lld-link`. Contrast with the macOS
cross: no host-donated SDK, no S3 bundle, no custom link driver — the whole
toolchain self-provisions from apt + GitHub + Microsoft, so a fresh spot VM
needs no Mac (or Windows machine) in the loop.

VM pieces (provisioned by `scripts/xwin-provision-vm.sh`, boot-restored by
cloud-build `startup-aws.sh` WINCROSS section in spur-notebook):

- `clang-tools-19` (clang-cl), `llvm-19` (llvm-lib/llvm-rc/llvm-dlltool),
  `lld-19` (lld-link) from the Debian 12 LLVM repo
- `cargo-xwin` 0.23.0 prebuilt (aarch64-unknown-linux-musl) in
  `/mnt/cargo/cargo-home/bin`
- Unversioned `clang-cl`/`lld-link`/`llvm-lib`/`llvm-rc`/`llvm-dlltool`
  symlinks → `-19` in `/mnt/cargo/cargo-home/bin` (precedes `/usr/bin`, where
  the base `lld` package owns an LLD-14 `lld-link`)
- `rustup target add x86_64-pc-windows-msvc` on every installed toolchain
  (default + repo pin — same E0463 trap as darwin)

## Findings

### 1. Source portability was already designed in

The workspace has portability seams nearly everywhere: `spur-acp` gates
`libproc`/`nix` behind `[target.'cfg(unix)'.dependencies]`,
`process_inspector` has an `unsupported` fallback impl for
non-macOS/non-Linux, signal handling (`tokio::signal::unix`) and
`CommandExt::process_group` sites are `#[cfg(unix)]`-gated with fallbacks,
and the TUI uses crossterm (Windows-supported) rather than termion. The one
straggler: `spur-cli`'s `nix` dependency (NOFILE rlimit raise, code already
gated with a `#[cfg(not(unix))]` no-op stub) was declared unconditionally —
moved to `[target.'cfg(unix)'.dependencies]`.

### 2. Plain CFLAGS from the VM profile poisons cl-mode compiles

`/etc/profile.d/spur-build.sh` exports `CFLAGS="-mcpu=native -O2"` for
native Graviton builds. cc-rs APPENDS plain `CFLAGS` to every compile, and
`clang-cl` hard-errors on `-mcpu=` ("unsupported option") — first casualty
was zstd-sys. The darwin fix (target-scoped `CFLAGS_<target>` appended
later with an explicit last `-mcpu`) does not work here: cl-mode rejects the
flag's presence, not its value; and scoping the plain CFLAGS to
`CFLAGS_aarch64_unknown_linux_gnu` on the VM would break build.sh's
documented caller-override flow (portable Lambda builds). Fix: the
`spur-cargo xwin` case exports cl-safe `CFLAGS="-O2"`/`CXXFLAGS="-O2"` when
the caller hasn't set them; build.sh forwards non-empty caller CFLAGS and
re-exports them AFTER sourcing profile.d, so the cl-safe value wins on the
VM. (`-O2` is proven accepted by clang-cl — it rode the failing zstd command
line without complaint.)

### 3. Provider RUSTFLAGS are shielded by the external-subcommand boundary

Same mechanics as zigbuild: with caller RUSTFLAGS unset, build.sh injects
the aarch64-linux provider defaults (`-Ctarget-cpu=neoverse-v2`,
`-Clinker=clang`) via `cargo --config`, which the outer cargo consumes and
cargo-xwin's inner cargo never sees. With caller RUSTFLAGS set, build.sh
would APPEND the provider defaults to it — `spur-cargo xwin` exports
`AWS_RUSTFLAGS_DEFAULT=""` to suppress that at the source.

### 4. xwin's case-variant symlinks miss PascalCase /DEFAULTLIB requests

The first PE link died on `lld-link: error: could not open 'PathCch.lib'`.
The request comes from an embedded `/DEFAULTLIB:PathCch.lib` directive in
MSVC-built prebuilt objects (pyke's onnxruntime — the same static lib that
needed `-framework CoreML` on the macOS cross). xwin generates each SDK
lib under its original name plus lowercase/UPPERCASE variants
(`pathcch.lib`, `PATHCCH.lib`), but on a case-sensitive Linux FS a
PascalCase request matches none of them. Fix: `ln -sf pathcch.lib
PathCch.lib` in `sdk/lib/um/x86_64`, self-healed from a profile.d line on
every dispatch shell (the splat lives on instance-store `/mnt/cargo`, so a
fresh spot VM re-splats and would regress otherwise).

### 5. DirectML.lib is not in the Windows SDK

The next link failure: `could not open 'DirectML.lib'` — pyke's prebuilt
onnxruntime bundles the DirectML execution provider (the Windows twin of
finding 8's CoreML in the macOS POC), and its objects embed
`/DEFAULTLIB:DirectML.lib`. That import lib ships in the
`Microsoft.AI.DirectML` NuGet redistributable, not the SDK xwin splats.
Fix: stage the nupkg on the VM (`/mnt/cargo/directml`, downloaded at boot)
and self-heal `bin/x64-win/DirectML.lib` into the splat from profile.d.
Runtime is safe without shipping a DLL: `DirectML.dll` is an inbox Windows
component since Windows 10 1903 (the nupkg carries a newer copy if we ever
want to ship one beside `spur.exe`).

### 6. Everything else just worked

Worth recording what did NOT need intervention, because it's most of the
risk surface: the bundled DuckDB C++ amalgamation, zstd, ring, tree-sitter,
and libgit2 all compile under clang-cl against the xwin CRT; ort-sys
downloads pyke's windows-msvc onnxruntime binaries on a Linux host without
complaint; lance-linalg's kernel build (the macOS x86_64 AVX-512 landmine,
finding 12 there) behaved on the msvc target; and rustc's PE link through
`lld-link` needed no custom driver — unlike the macOS ld64 story, findings
4–5's missing-lib fixups were the entire link-stage cost.

## Usage

```sh
scripts/spur-cargo xwin build --release -p spur-cli   # → PE on the VM
scripts/cloud-build/fetch.sh --via-s3 target/x86_64-pc-windows-msvc/release/spur.exe
```

For the whole platform matrix in one go (linux native + macOS universal2 +
windows x86_64, fetched into `dist/` with checksums): `cargo xtask dist`.

## Feature degradation on Windows

The unix-domain-socket transports are cfg-gated with graceful fallbacks
rather than ported (Windows 10+ supports AF_UNIX, and tokio named pipes are
an alternative — both deliberately out of POC scope):

- **Embedding sidecar** (spur-analyst): every round trip fails like an
  unreachable sidecar → Auto mode uses in-process embedding, query paths
  degrade to BM25-only exactly as designed for sidecar outages.
- **Notebook daemon** (spur-core bridge, spur-tui client): the datasource
  bridge never runs; `/notebook` commands return a clear platform error.
- **NOFILE rlimit raise** (spur-cli): no-op (Windows has no RLIMIT_NOFILE).
- **Orphan reaping** (spur-acp): `production_inspector` already had an
  `unsupported` fallback (starttime/cmd probes return None, killpg no-ops).

## Known gaps / non-goals of this POC

- **No Windows execution environment**: the VM is aarch64 Linux; the PE is
  not smoke-tested here. Runtime validation needs a Windows host (or wine on
  x86_64, not available on the Graviton VM).
- **DuckDB extensions**: spur-analyst installs community extensions
  (duckpgq etc.) at runtime; DuckDB publishes windows_amd64 builds of these,
  but the two-runtimes lesson from macOS says runtime validation matters.
  One C++ runtime here though: everything is /MD against the msvcrt DLLs.
- **Windows-specific runtime behavior** (paths, process management, ConPTY
  via crossterm) is compile-gated but untested.
- **`cargo test` for windows**: dev-dependency/test code is not gated
  (e.g. a `std::os::unix::fs::symlink` in spur-cli test code); only the
  binary build is in scope.
- **aarch64-pc-windows-msvc**: not attempted; the x86_64 recipe should
  carry over (xwin already splats aarch64 SDK libs) but ort prebuilts and
  DirectML staging would need their arm64 variants.
- **Concurrent-dispatch sync races** (cloud-build-wide, observed during the
  first `cargo xtask dist` validation): all dispatches for a given
  namespace/worktree sync into ONE shared remote tree, last sync wins. The
  build queue serializes cargo invocations but not the content syncs, so a
  concurrent session (or CI) dispatching from an older tree state can
  regress the remote tree between your sync and your build — the dist
  windows stage once compiled pre-gate sources this way. Re-running the
  failed platform (`cargo xtask dist --platforms windows`) re-syncs and
  recovers; a real fix would scope syncs like `run` does (per-invocation
  private copies).
