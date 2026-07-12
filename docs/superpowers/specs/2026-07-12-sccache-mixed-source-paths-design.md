# Mixed-path C/C++ sccache wrapper fix

Status: approved
Owner surfaces: `getspur/spur-notebook` cloud-build provisioning and SPUR's legacy GCP provisioning mirror

## Problem

`cargo xtask dist` fails in the native Linux `spur-cli` build after
`spur-context-auth` enabled `keyring/vendored`. That feature activates
`libdbus-sys`'s vendored C build. `spur-context-service` is not involved: it is
excluded from the main workspace and is absent from the `spur-cli` dependency
graph.

`libdbus-sys` passes two path classes to `cc-rs`:

- generated headers under `$OUT_DIR/include`; and
- crate-relative sources under `./vendor/dbus/`.

The remote VM's `sccache-cc` and `sccache-cxx` wrappers currently classify a
compile as OUT_DIR-scoped when either its `-c` source or any `-I` include is
under `$OUT_DIR`. They then change directory to `$OUT_DIR` and rewrite
OUT_DIR-prefixed arguments. For a mixed-path invocation, this makes the
crate-relative D-Bus sources resolve beneath `$OUT_DIR`, where they do not
exist. sccache consequently fails to hash the input and `cc-rs` exits 254.

## Design

Classify a compiler invocation as OUT_DIR-scoped only when the source operand
following `-c` is itself beneath `$OUT_DIR`. An OUT_DIR include alone must not
trigger directory switching.

For generated sources such as `libduckdb-sys`, the source is under `$OUT_DIR`,
so the wrappers retain the existing path normalization and cross-worktree cache
keys. For crate-relative sources such as `libdbus-sys`, the wrappers stay in
the crate directory and use it as `SCCACHE_BASEDIR`; the absolute OUT_DIR
include remains valid.

Apply the same logic to both C and C++ wrappers in:

- `spur-notebook/scripts/cloud-build/startup-aws.sh`, the AWS distribution
  builder source of truth; and
- `spur/scripts/gcp-build/startup.sh`, the tracked legacy GCP mirror.

No package feature changes are needed. Vendored D-Bus remains statically linked
for both Linux architectures.

## Testing

Use TDD in both repositories:

1. Add a wrapper regression test that invokes a crate-relative `-c` source with
   an absolute `$OUT_DIR/include` path.
2. Verify the test fails because the wrapper changes into `$OUT_DIR`.
3. Narrow OUT_DIR-scoped detection to the `-c` source operand.
4. Verify the mixed-path regression passes and the existing generated-source
   rewrite test still passes.
5. Run the native Linux `scripts/spur-cargo build --release -p spur-cli`, then
   rerun `cargo xtask dist` behavior through the repository wrapper.

## Rollout and safety

The change affects only provisioning templates; an already-running VM retains
its installed wrapper until reprovisioned or replaced. Verification must ensure
the active builder has received the corrected wrapper before treating a remote
dist result as evidence.

Existing unrelated changes in both worktrees remain unstaged. Each repository
gets commits containing only this fix's test or implementation hunks.
