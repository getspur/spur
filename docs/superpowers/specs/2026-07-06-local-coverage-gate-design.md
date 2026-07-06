# Local test-coverage gate for `spur-cargo` — design

Status: approved by user (design phase), 2026-07-06

## Problem

The workspace has no test-coverage tooling at all today (no `tarpaulin`,
`llvm-cov`, or `codecov` config anywhere). The user wants a **local, manual
gate** they run via `spur-cargo` before merging a branch into local `main`:
overall workspace line coverage must stay above **75%**, and coverage of the
lines actually changed on the branch (vs. `main`) must be above **85%**. No
GitHub Actions job, no git hook — this is an explicit, developer-invoked
check (`scripts/spur-cargo coverage`), matching the existing `graph-embed`
convenience-shortcut pattern in `scripts/spur-cargo`.

## Tool choice: `cargo-llvm-cov`

Source-based coverage via LLVM instrumentation, the same mechanism
`rustc`/`rustup` already ship. Cross-platform (macOS dev boxes included,
unlike ptrace-based `cargo-tarpaulin` which is Linux-only), and it wraps
plain `cargo test` directly — no need to introduce `nextest` into the remote
build path (the existing `spur-cargo test` remote path already runs plain
`cargo test`, not `nextest`; `nextest` is only used in the separate GitHub
Actions `ci.yml`). Requires the `llvm-tools` rustup component.

## Where it runs

Reuses the *existing* `run`-class remote dispatch in `scripts/spur-cargo`
(the same mechanism `graph-embed`/`embed` already use) — **no changes to the
wrapper's routing/CLASS logic**. A new `coverage` case in the wrapper's
"shortcut expansion" block rewrites `scripts/spur-cargo coverage <flags>`
into `run -p xtask -- coverage <flags>`, so it is remote-by-default (heavy
instrumented rebuild belongs on the VM) with `SPUR_REMOTE=0` forcing local.

## `cargo xtask coverage` subcommand

New subcommand in `xtask/` (currently a zero-dependency crate with an
`install` subcommand following a `match subcommand.as_str()` dispatch
pattern; `coverage` follows the same shape):

1. **Self-bootstrap.** Probe for `cargo-llvm-cov` on `PATH`/`CARGO_HOME/bin`;
   if absent, run `cargo install cargo-llvm-cov --locked`. This is the
   correctness fallback for any box (a fresh dev laptop, an unbaked AMI, the
   GCP fallback path) — see "VM provisioning" below for why this alone isn't
   sufficiently fast on the default AWS path.
2. **Measure.** Run
   `cargo llvm-cov --workspace --lib --lcov --output-path coverage/lcov.info`.
   `--lib` matches `ci.yml`'s existing conservative scope (some integration
   tests need external infra, e.g. the DuckDB CLI for
   `spur-analyst/tests/lance_session.rs`). `coverage/` is a worktree-relative
   path *outside* `target/` — deliberate, because `target/` is excluded from
   the VM→local rsync used by `spur-cargo run`'s sync-back, so anything
   written under `target/` would never reach the local checkout.
3. **Parse.** Read the lcov file (`SF:<path>`, `DA:<line>,<hits>`,
   `end_of_record`) into a per-file line→hit-count map.
4. **Total coverage.** `covered_lines / total_lines * 100`, compared against
   `--floor` (default 75).
5. **Diff coverage.** `git diff --unified=0 <base>...HEAD -- '*.rs'` (default
   `<base>` = `main`; three-dot diff = merge-base diff, so it isn't polluted
   by unrelated commits landing on `main` after the branch point) to find
   added/changed line numbers per file, cross-referenced against the lcov
   line-hit map. Lines with no lcov entry (comments, blank lines, non-`.rs`
   files) are excluded from both numerator and denominator, matching standard
   diff-coverage tool behavior. Result compared against `--diff-floor`
   (default 85). If there are zero coverable changed lines (e.g. a docs-only
   branch), this check passes trivially with a note.
6. **Report + exit code.** Print both percentages and pass/fail against their
   thresholds; exit non-zero if either fails, listing the failing metric.

Known unknown: since no coverage tooling has run on this workspace before,
the 75% floor is unmeasured — the first real run may fail it. That's
expected and actionable, not a defect in the tool.

## VM provisioning (`scripts/cloud-build`)

Investigated because the default remote path (`SPUR_CLOUD=aws-my` →
`aws`) is an **ephemeral AWS Spot box that self-terminates after 30 min
idle**; `/mnt/cargo` (`CARGO_HOME`) lives on the *instance-store* NVMe, wiped
on every respin. The only durable layer across respins is the golden AMI
(`bake-ami.sh`, which snapshots `/opt/spur-rust/{cargo-home,rustup}`) plus the
S3 sccache bucket. `cargo-llvm-cov` is not a rustup component — it's a
separate installed binary — and it is not baked into the golden AMI today.
Without a change, the `xtask` self-bootstrap (step 1 above) would silently
re-pay a ~1-2 min `cargo install` on **every fresh Spot respin**, forever.

Fix: add a step to `bake-ami.sh`'s provisioning block that runs
`cargo install cargo-llvm-cov --locked` (using the bake box's own freshly
installed cargo) immediately before the existing `cp -a .../cargo-home`
snapshot step, so it's captured into the golden AMI going forward. The
`llvm-tools` rustup *component* needs no equivalent change — like
`rustfmt`/`clippy` today, it is fetched on-demand by rustup's
`rust-toolchain.toml` override on first invocation per box, baked or not.

This means after merging, a human must re-run
`SPUR_CLOUD=aws-my scripts/cloud-build/bake-ami.sh` (and the `aws`/Tokyo
variant) to produce the new golden AMI — that spins real billed on-demand EC2
instances, so it is called out as a required manual follow-up, not something
automated here.

## Other changes

- `rust-toolchain.toml`: add `"llvm-tools"` to `components` (alongside
  `rustfmt`, `clippy`). Single source of truth already read by local dev, CI,
  and both cloud VMs.
- `.gitignore`: add `coverage/` (generated artifact).
- `CLAUDE.md`: document `scripts/spur-cargo coverage` in the Build/Test
  Commands section, matching the existing bullet-list style.

## Testing plan (TDD)

Pure-function unit tests in `xtask`, following its existing style of testing
`Command` argument construction and pure logic directly (see
`cargo_build_command_includes_requested_features` et al.):

- lcov parser: given sample lcov text, produces the correct per-file
  line→hit-count map (multiple `SF:`/`end_of_record` sections, lines with
  0 and >0 hits).
- diff-hunk parser: given sample `git diff --unified=0` output, produces the
  correct set of added/changed line numbers per file (including multiple
  hunks, added-only vs. modified vs. deleted-only hunks).
- threshold evaluator: given precomputed total/diff percentages and
  floor/diff-floor, returns the correct pass/fail + message, including the
  "zero coverable changed lines" trivial-pass case.
- `coverage` subcommand's own `Command` construction (the `cargo llvm-cov
  ...` invocation args), tested the same way `remote_install_build_command`
  is tested today.

Not unit-tested (consistent with existing xtask coverage of `install`):
actually invoking `cargo-llvm-cov`/`git` end-to-end — verified manually once
implemented.

## Out of scope (explicit)

- No GitHub Actions coverage job.
- No git hook enforcement.
- No Codecov/external SaaS upload.
- No changes to the legacy GCP remote-build path (`SPUR_CLOUD=gcp`) — it is
  not in the default `aws-my → aws → local` fallback chain.
