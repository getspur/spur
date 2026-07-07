# Standard pattern: macOS cross-compiles on the cloud-build VM via cargo-zigbuild

**Status: standard macOS build path (2026-07-07).** The AWS Graviton build
VM cross-compiles the full `spur` binary (DuckDB, lance, ort/onnxruntime,
ring, arboard/AppKit, native-tls/Security included) into Mach-O executables
— `aarch64-apple-darwin`, `x86_64-apple-darwin`, or a `universal2` fat
binary — that run natively on Macs. Verified on macOS 26.1: `--version`,
`--help`, the TUI, and spur-analyst DuckDB queries with natively-built
extension dylibs (duckpgq, lance, icu) loaded and exceptions flowing across
the dlopen boundary. Full Rust-graph rebuild takes ~9m per arch on
m8gd.4xlarge with C archives warm; a fully cold arch roughly doubles that.

## How to use

```sh
# once per VM lifetime; --publish-bundle also uploads the toolchain bundle
# to S3 so FRESH spot VMs self-provision at boot (startup-aws.sh restores
# it) with no Mac in the loop:
scripts/zigbuild-provision-vm.sh --publish-bundle

# build (remote by default, same dispatch contract as spur-cargo build):
scripts/spur-cargo zigbuild --release -p spur-cli                                  # arm64
scripts/spur-cargo zigbuild --release -p spur-cli --target universal2-apple-darwin # fat

# fetch the artifact (340 MB arm64 / 726 MB fat — use the S3 path, SSM
# rsync is ~5 MB/s):
<cloud-build>/fetch.sh --via-s3 --to /tmp/spur-macos-arm64 \
    target/aarch64-apple-darwin/release/spur
<cloud-build>/fetch.sh --via-s3 --to /tmp/spur-macos-universal2 \
    target/universal2-apple-darwin/release/spur
```

`spur-cargo zigbuild` defaults `--target aarch64-apple-darwin` (pass an
explicit `--target` to override) and, when the caller has no `RUSTFLAGS`,
exports the exact flag set this doc validates (see "CoreML" below).

If the build fails with `couldn't read .../libproc-<hash>/out/
osx_libproc_bindings.rs`, run `bash /mnt/cargo/macsdk/plant-libproc-bindings.sh`
on the VM and rebuild (see "libproc" below for why).

## What the toolchain needed (each item was a real observed failure)

1. **rust-std for every toolchain, not just default** — the repo pins 1.94.1
   via `rust-toolchain.toml`; `rustup target add` against `stable` left the
   pinned toolchain without a darwin std → `E0463: can't find crate for core`.

2. **zig 0.15.2 + cargo-zigbuild 0.23.0** — installed to `/mnt/cargo/zig` and
   `$CARGO_HOME/bin`. The provision script pins "latest stable < 0.16" since
   cargo-zigbuild lags brand-new zig lines. zig's Mach-O linker parses the
   SDK 26.1 tbd stubs fine and ad-hoc signs the output (required on arm64;
   macOS accepts it).

3. **A ~70 MB "linker SDK", not Xcode** — all `.tbd` text stubs
   (Frameworks + PrivateFrameworks + usr/lib), `usr/include`, and
   `SDKSettings.json` from the local Mac's CLT SDK, rsynced to
   `/mnt/cargo/macsdk/MacOSX.sdk` with `SDKROOT` exported in
   `/etc/profile.d/spur-build.sh`. Gotcha: framework-top `X.tbd` files are
   symlinks through `Versions/Current` (also a symlink) — sync with
   `rsync -L` or they dangle and zig reports "unable to find framework
   'CoreFoundation'".

4. **Target-scoped C flags: `CFLAGS_aarch64_apple_darwin="-O2 -mcpu=apple_m1"`**
   (and CXXFLAGS). Two birds: (a) the VM profile's plain
   `CFLAGS=-mcpu=native` would otherwise leak neoverse-v2 (SVE!) codegen into
   darwin C objects — cc-rs prefers target-scoped vars, so Linux builds keep
   their tuning untouched; (b) zig cc's default feature mapping for
   aarch64-macos is internally inconsistent and fails NEON code (blake3,
   zstd) with `always_inline function requires target feature 'altnzcv'` —
   an explicit `-mcpu=apple_m1` (zig spelling, underscores) gives a coherent
   feature set. Every Apple Silicon Mac is ≥ M1, so nothing is lost.

5. **`libclang_rt.osx.a` into `$SDKROOT/usr/lib/`** — pyke's prebuilt
   onnxruntime (`ort-download-binaries`) links `-lclang_rt.osx`, which ships
   with Xcode CLT (compiler-rt), not the SDK. zig searches the SDK lib dir,
   so dropping the ~830 KB archive there resolves it.

6. **libproc (pulled by spur-acp) cannot cross-build as published** — its
   build.rs gates bindgen behind `#[cfg(target_os = "macos")]`, a HOST cfg,
   and hardcodes a `/Library/Developer/...` include path. On a Linux host the
   no-op branch runs and the lib later fails on the missing include.
   Workaround: pre-generate the bindings on the VM with bindgen-cli against
   the synced SDK (`-x c++ -target aarch64-apple-darwin -isysroot $SDKROOT`)
   and plant the file into libproc's `OUT_DIR`. The OUT_DIR hash changes
   whenever RUSTFLAGS change, hence the re-runnable plant helper. Proper fix:
   `[patch.crates-io]` fork (or upstream PR) using `CARGO_CFG_TARGET_OS` +
   `SDKROOT`.

7. **CoreML framework link via RUSTFLAGS** — pyke's static onnxruntime for
   macOS bundles the CoreML execution provider, but ort-sys only emits
   `-framework CoreML` when building ON a macOS host, so the cross link came
   up six `_OBJC_CLASS_$_ML*` symbols short. `spur-cargo zigbuild` bakes in:

   ```
   AWS_RUSTFLAGS_DEFAULT=""   # provider defaults would hijack the link (see below)
   RUSTFLAGS="-Cforce-frame-pointers=yes -Clink-arg=-framework -Clink-arg=CoreML \
              -Clink-arg=$SDKROOT/usr/lib/libc++.tbd -Clinker=/mnt/cargo/macsdk/ld64-link.sh"
   ```

   Why: build.sh appends the AWS provider defaults (`-Clinker=clang
   -Clink-arg=-fuse-ld=…lld -Ctarget-cpu=neoverse-v2`) to any caller
   RUSTFLAGS — fatal for a Mach-O link — so they are suppressed at the
   source; an env RUSTFLAGS replaces the repo `.cargo/config.toml`
   `build.rustflags`, so `force-frame-pointers` is re-added for parity. The
   last two flags are the runtime fix (items 8–10). With no caller RUSTFLAGS
   at all, provider defaults ride `cargo --config`, which external
   subcommands like zigbuild never see — harmless by construction.

## Runtime fix: one C++ runtime, ld64.lld link (same day)

The first artifact ran the TUI but **aborted with `libc++abi: terminating`
on any spur-analyst DuckDB query**. Root cause chain, each step verified:

8. **Two C++ runtimes cannot exchange exceptions.** zig maps `-lc++` to its
   own STATIC libc++/libc++abi, so the binary carried a private C++ runtime
   (no `libc++.1.dylib` in `otool -L`). DuckDB extensions are separate
   dylibs installed from the community/core repos (`INSTALL duckpgq FROM
   community`), built by Apple clang against the SYSTEM libc++. DuckDB
   throws C++ exceptions in normal operation; the first throw crossing the
   dlopen boundary (crash stack: `duckpgq_bind` → `std::terminate`) finds
   no matching handler in the foreign runtime and terminates. Minimal repro:
   a zig-static host dlopening an Apple-clang dylib that throws → same
   abort; relink host against the SDK's `libc++.tbd` → exception caught.
   Fix: link the system libc++ — an explicit `.tbd` path on the link line
   beats zig's `-lc++` substitution (`-Clink-arg=$SDK/usr/lib/libc++.tbd`),
   restoring the exact single-runtime configuration of a native build.

9. **zig's Mach-O linker can't finish that link.** With libc++ symbols now
   dylib imports, zig 0.15.2 (and 0.14.1) fails with `relocation … Overflow`
   on pyke's prebuilt Apple-clang onnxruntime objects. Fix: keep zig as the
   C/C++ *compiler*, hand the final link to **clang + ld64.lld** via a tiny
   driver (`/mnt/cargo/macsdk/ld64-link.sh`, provisioned) selected with
   `-Clinker=…` — rustc honors the LAST `-C linker`, so it cleanly overrides
   the wrapper cargo-zigbuild configures.

10. **Two Debian toolchain quirks in that driver.** (a) Debian clang assumes
    an ancient host ld64 and emits legacy `-macosx_version_min`, which
    ld64.lld rejects ("must specify -platform_version") — claiming
    `-mlinker-version=705` switches clang to the modern flag. (b) The same
    onnxruntime objects reference objc_msgSend selector stubs
    (`_objc_msgSend$sel`, Xcode 14+), which lld-14 cannot synthesize —
    **lld-19** (plain `apt install lld-19` on Debian 12) links them fine.

Verified after the fix: binary links `/usr/lib/libc++.1.dylib`; analyst
queries over `spur mcp` return rows (33,582-row count against
`.spur/analyst.duckdb`); `duckdb_extensions()` shows duckpgq/lance/icu
loaded; DuckDB error paths (Catalog Error) surface as JSON error strings
instead of killing the process.

## universal2 (fat arm64+x86_64) findings

The same recipe extends to `--target universal2-apple-darwin`
(cargo-zigbuild builds both slices and lipo-combines them). Three
x86_64-specific findings:

11. **cc-rs APPENDS `CFLAGS_<target>` after plain `CFLAGS` — it does not
    replace it.** The profile's `-mcpu=native` (neoverse!) therefore reaches
    every darwin C compile and must be overridden by a LATER `-mcpu`: the
    arm flags already did this implicitly (`-mcpu=apple_m1`); x86_64 needs
    an explicit `-mcpu=x86_64` baseline, otherwise zig resolves the arm host
    CPU into an x86 compile and zstd's NEON path explodes.

12. **lance-linalg's AVX-512 `dist_table` kernel cannot cross-compile via
    its own build.rs** — the kernel uses `-march=native` (meaningless on an
    arm host; and the appended `-mcpu=x86_64` breaks clang-20's `evex512`
    ABI checks anyway), while the *feature-skipped* f16/bf16 probes still
    return `Ok` and set the shared `kernel_support="avx512"` cfg (upstream
    bug), leaving the Rust caller referencing a symbol nothing defines.
    Fix: provisioning pre-compiles the runtime-gated kernel
    (`zig cc -target x86_64-macos -mcpu=x86_64_v4`, zig CPU spelling) and
    `ld64-link.sh` appends the object to every x86_64 link.

13. **Per-arch libproc bindings** — the generated bindings differ by arch
    (362 KB x86_64 vs 308 KB arm64), so provisioning generates both and the
    plant helper installs the matching one per target dir.

Verified: `Mach-O universal binary with 2 architectures` (726 MB); the
arm64 slice runs natively (`--version` + analyst MCP query). The x86_64
slice is statically verified (valid Mach-O, system libc++/CoreML linkage,
AVX-512 kernel symbol defined) — live execution still needs an Intel Mac or
a Rosetta-enabled machine (the build host Mac has no Rosetta installed).

## Standardization (cloud-build, spur-notebook repo)

`scripts/zigbuild-provision-vm.sh --publish-bundle` uploads the full
toolchain state (SDK stubs, zig, cargo-zigbuild/bindgen binaries, ld64
driver, libproc bindings, lance kernel, repo rust-pin) to
`s3://$SCCACHE_BUCKET/macsdk/macos-cross-bundle.tar.gz`. cloud-build's
`startup-aws.sh` restores that bundle at boot on every fresh spot VM —
installs lld-19, adds both darwin rust-std targets to the default and
pinned toolchains, and appends the darwin CFLAGS profile block — so macOS
cross survives spot preemption with no Mac in the loop. A missing bundle
changes nothing for the Linux build path.

## Deliberately out of scope / known limits

- **Runtime coverage**: CLI startup, the TUI, and spur-analyst DuckDB
  queries (incl. community extension dylibs) are verified on macOS.
  Embedding (`spur graph embed` / onnxruntime inference) is linked but not
  yet exercised end-to-end.
- **Durability**: everything provisioned lives on instance-store
  `/mnt/cargo` — a spot preemption wipes it. With the S3 bundle published
  (see Standardization above), `startup-aws.sh` restores it at boot; without
  a bundle, re-run `scripts/zigbuild-provision-vm.sh` (idempotent, ~5 min).
- **sccache**: darwin Rust compiles are cached (RUSTC_WRAPPER applies);
  darwin C/C++ objects are NOT (cargo-zigbuild's `CC_<target>` wrappers
  bypass sccache-cc), so DuckDB recompiles on a fresh VM.
- **Licensing**: the tbd stubs, headers, and compiler-rt archive come from
  the user's own Xcode CLT install and serve their own builds; Apple's Xcode
  license nominally ties SDK use to Apple-branded hardware. Flagged for
  awareness before any CI/redistribution use.

## Exact versions (POC run)

| Piece | Version |
|---|---|
| VM | m8gd.4xlarge (Graviton4), Debian 12 arm64 |
| rustc | 1.94.1 (repo pin) |
| zig | 0.15.2 (linux-aarch64) |
| cargo-zigbuild | 0.23.0 |
| bindgen-cli | 0.72.1 (against system libclang-14) |
| link driver | Debian clang + ld64.lld-19 (`/mnt/cargo/macsdk/ld64-link.sh`, arch-sniffing) |
| macOS SDK | 26.1 (CLT, Darwin 25.1 host) |
| Artifact | `spur` 1.7.0, Mach-O arm64 (340 MB) or universal2 fat (726 MB), links system libc++, adhoc linker-signed |
