# POC: macOS (aarch64-apple-darwin) cross-compiles on the cloud-build VM via cargo-zigbuild

**Status: proven end-to-end (2026-07-07).** The Linux Graviton build VM now
cross-compiles the full `spur` binary (DuckDB, lance, ort/onnxruntime, ring,
arboard/AppKit, native-tls/Security included) into a Mach-O arm64 executable
that runs natively on an Apple Silicon Mac. Verified: `spur 1.7.0`
(356,735,088 bytes, ad-hoc linker-signed by zig) executes `--version`/`--help`
on macOS 26.1. Full Rust-graph rebuild takes ~9m on m8gd.4xlarge with C
archives warm; the first-ever build (C cold, DuckDB amalgamation dominates)
roughly doubles that.

## How to use

```sh
# once per VM lifetime (and again after a spot preemption — see Durability):
scripts/zigbuild-provision-vm.sh

# build (remote by default, same dispatch contract as spur-cargo build):
scripts/spur-cargo zigbuild --release -p spur-cli

# fetch the artifact (340 MB — use the S3 path, SSM rsync is ~5 MB/s):
<cloud-build>/fetch.sh --via-s3 --to /tmp/spur-macos-arm64 \
    target/aarch64-apple-darwin/release/spur
```

`spur-cargo zigbuild` defaults `--target aarch64-apple-darwin` (pass an
explicit `--target` to override) and, when the caller has no `RUSTFLAGS`,
exports the exact flag set the POC validated (see "CoreML" below).

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
   RUSTFLAGS="-Cforce-frame-pointers=yes -Clink-arg=-framework -Clink-arg=CoreML"
   ```

   Why both: build.sh appends the AWS provider defaults (`-Clinker=clang
   -Clink-arg=-fuse-ld=…lld -Ctarget-cpu=neoverse-v2`) to any caller
   RUSTFLAGS — fatal for a Mach-O link that must stay in zig's hands — so
   they are suppressed at the source; and an env RUSTFLAGS replaces the repo
   `.cargo/config.toml` `build.rustflags`, so `force-frame-pointers` is
   re-added for parity. With no caller RUSTFLAGS at all, provider defaults
   ride `cargo --config`, which external subcommands like zigbuild never see
   — harmless by construction.

## Deliberately out of scope / known limits

- **Runtime coverage**: CLI startup (`--version`, `--help`) is verified, plus
  a CoreFoundation-calling smoke binary. TUI, DuckDB queries, lance, and
  embedding runtime paths are untested on macOS. Suggested next check:
  `spur graph embed` + an analyst query against a scratch repo using the
  fetched binary.
- **Durability**: everything provisioned lives on instance-store
  `/mnt/cargo` — a spot preemption wipes it. Re-run
  `scripts/zigbuild-provision-vm.sh` (idempotent, ~5 min). Durable options:
  fold into `startup-aws.sh`/the golden AMI and host the SDK subset in the
  sccache S3 bucket so a fresh VM self-provisions without a Mac present.
- **sccache**: darwin Rust compiles are cached (RUSTC_WRAPPER applies);
  darwin C/C++ objects are NOT (cargo-zigbuild's `CC_<target>` wrappers
  bypass sccache-cc), so DuckDB recompiles on a fresh VM.
- **x86_64-apple-darwin / universal2**: untested. The same recipe should
  apply (`--target x86_64-apple-darwin`, `-mcpu` swap, x86 clang_rt slice is
  in the same CLT archive; `cargo zigbuild --target universal2-apple-darwin`
  exists) — separate POC.
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
| macOS SDK | 26.1 (CLT, Darwin 25.1 host) |
| Artifact | `spur` 1.7.0, Mach-O arm64, 356,735,088 B, adhoc linker-signed |
