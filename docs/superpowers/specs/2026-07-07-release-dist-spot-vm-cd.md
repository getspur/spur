# Release CD on an Ephemeral AWS Spot Builder (`release-dist.yml` + `cargo xtask dist`)

Status: implemented (workflow_dispatch-only rollout)
Owner surface: `.github/workflows/release-dist.yml`, `scripts/spur-cargo`
(`SPUR_NO_LOCAL_FALLBACK`), `scripts/cloud-build/`, `xtask dist`

## Problem

The existing `release.yml` is a cargo-dist–generated flow that compiles every
platform natively on GitHub-hosted matrix runners. That duplicates the build
infrastructure this repo already standardizes on: the `scripts/cloud-build`
Spot builder compiles the full platform matrix (linux native aarch64, macOS
universal2 via zigbuild, windows x86_64 via xwin) behind one entry point,
`cargo xtask dist`, with a warm region-local S3 sccache L2. GitHub macOS/Windows
runners are slow, their caches are cold relative to the shared sccache bucket,
and the zigbuild/xwin toolchain contracts (system-libc++ Mach-O link, clang-cl
PE link) live on the VM, not on runners.

## Design

One ubuntu runner orchestrates; every compile happens on a **per-run AWS Spot
VM** in `ap-southeast-5` using the exact same `cloud-build` scripts developers
use locally:

```
GitHub OIDC ──► aws-actions/configure-aws-credentials (vars.AWS_RELEASE_ROLE_ARN)
   │
   ├─ install session-manager-plugin        (SSH-over-SSM transport)
   ├─ ssh-keygen ephemeral key              (public half → VM authorized_keys)
   ├─ scripts/cloud-build/spin.sh           SPUR_BUILD_POOL_NAME=spur-release-<run_id>
   ├─ cargo xtask dist --platforms …        (spur-cargo → build.sh → VM; fetch --via-s3)
   ├─ upload-artifact dist/                 (spur-<ver>-<triple>[.exe] + SHA256SUMS)
   ├─ [publish=true] gh release create      (getspur/spur-releases, GH_RELEASES_TOKEN)
   └─ always(): re-auth OIDC → scripts/cloud-build/teardown.sh --all
```

Key decisions:

- **Per-run `SPUR_BUILD_POOL_NAME=spur-release-<run_id>`.** The provider still
  resolves concrete instances by Name tag, but `build.sh` can derive overflow
  names (`spur-release-<run_id>`, `spur-release-<run_id>-2`) from the pool. A
  unique pool isolates release boxes from the shared dev builders and concurrent
  runs, and `teardown.sh --all` stays surgical because it only walks that pool's
  configured names.
- **`SPUR_CLOUD=aws-my`, `SPUR_CLOUD_FALLBACK=""`.** Deterministic region:
  the m8gd→c8gd same-region Spot fallback inside `provider-aws.sh` still
  applies, but the cross-region Tokyo hop is disabled (cold L2, role is
  provisioned per-region).
- **`SPUR_NO_LOCAL_FALLBACK=1`** (new `spur-cargo` guard): when every remote
  cloud is unreachable, `spur-cargo` exits 200 instead of falling back to
  local cargo. On a CI runner the local fallback is strictly harmful — it
  would compile an x86_64-linux binary the fetch step can't use, or fail on
  the missing zigbuild/xwin toolchains after wasting the compile.
- **`spin.sh` before `xtask dist`.** `build.sh --auto-spin` would create the
  VM too, but spinning first fails fast on Spot-capacity/IAM errors before
  the runner invests anything in the build.
- **Ephemeral SSH key per run.** Generated on the runner, public half injected
  through EC2 user-data, discarded with the runner. No long-lived key secret.
- **Credential lifetime.** The job holds AWS credentials for up to 3 h
  (`role-duration-seconds: 10800` — the role's `MaxSessionDuration` must
  allow it), and the teardown step re-exchanges OIDC first so cleanup never
  runs on expired credentials.

### VM lifecycle guarantees (termination after the release)

1. `always()` teardown step → `terminate-instances` on the per-run Name tag.
2. `spur-autoshutdown` on the VM self-terminates after 30 idle minutes — the
   backstop when the runner is force-killed and no step runs.
3. The instance is Spot with `InstanceInterruptionBehavior=terminate` and
   `instance-initiated-shutdown-behavior terminate`; the EBS root has
   `DeleteOnTermination=true`. Nothing outlives the run except the durable
   sccache bucket (by design — it is the shared warm cache).

## Required configuration

| Item | Kind | Purpose |
|---|---|---|
| `AWS_RELEASE_ROLE_ARN` | repo **variable** | OIDC-assumable role the workflow uses for the whole VM lifecycle |
| `SPUR_BUILDER_AMI_ID` | repo **variable** (optional) | Golden AMI from `bake-ami.sh` for ~1-2 min boots; empty → base Debian 12 arm64 + full provisioning |
| `GH_RELEASES_TOKEN` | repo **secret** (existing) | `gh release create` on `getspur/spur-releases` (publish=true only) |
| cloud-build S3 bundle | `s3://<sccache-bucket>/ci/cloud-build/bundle.tar.gz` | See below — the runner restores the cloud-build scripts from it |

### The cloud-build S3 bundle

`scripts/cloud-build` in this repo is a **git-tracked symlink** into the
sibling private `spur-notebook` checkout, so a bare runner checkout has
nothing behind it. Cloning `getspur/spur-notebook` in CI was rejected: the
scripts evolve in that repo's *working tree* ahead of its origin (and the
repo is private, needing an extra token). Instead — following the same
pattern as the zigbuild macOS SDK bundle and the e2e toolchain provisioner —
`scripts/cloud-build-publish-bundle.sh` snapshots the resolved working-tree
scripts into the sccache bucket, and the workflow restores them to the
sibling path on the runner so the tracked symlink resolves unchanged.

- Refresh with `scripts/cloud-build-publish-bundle.sh` whenever cloud-build
  scripts change (it validates and excludes `*.local.env` so CI's AMI comes
  only from `SPUR_BUILDER_AMI_ID`).
- The OIDC role's existing S3 read on the sccache bucket covers the download;
  no extra credentials.
- Skew risk: the bundle is a manual snapshot. If a release run fails with a
  provider/VM mismatch, republish the bundle first.

### IAM shape for `AWS_RELEASE_ROLE_ARN`

Trust policy: `token.actions.githubusercontent.com` federated principal,
`sub` restricted to this repo (recommended: this workflow ref). Set
`MaxSessionDuration ≥ 10800`.

Permissions (least-privilege sketch; region `ap-southeast-5`):

- `ec2:Describe{Instances,Images,Subnets,SecurityGroups,InstanceTypeOfferings}` — resolve AMI/subnet/SG, poll state (`*`)
- `ec2:RunInstances` + `ec2:CreateTags` — launch the Spot box (constrainable to the region/AMI/subnet)
- `ec2:TerminateInstances`, `ec2:StopInstances`, `ec2:StartInstances` — lifecycle; recommend a `aws:ResourceTag/Name` condition on `spur-release-*`
- `iam:PassRole` on `role/spur-builder` with `iam:PassedToService=ec2.amazonaws.com` — the instance profile grants the VM its S3 sccache access
- `ssm:StartSession` on the instances + document `AWS-StartSSHSession`, `ssm:TerminateSession`, `ssm:DescribeInstanceInformation` — the SSH-over-SSM transport
- `s3:ListBucket` on `wiilearn-spur-sccache-apse5`; `s3:{Get,Put,Delete}Object` on its `/*` — `fetch.sh --via-s3` artifact hop (same surface the existing `AWS_SCCACHE_ROLE_ARN` grants, so that policy can be reused as a base)

## Relationship to `release.yml` (cargo-dist) — CUTOVER DONE 2026-07-07

After the v1.8.0 end-to-end validation (run 28859026141: all three platforms
built on the spot VM in 15 min and published to `getspur/spur-releases`),
tag pushes were **cut over to `release-dist.yml`**:

- `release-dist.yml` triggers on `v*.*.*` tag pushes (publish implied, tag =
  `github.ref_name`) and stays manually dispatchable for dry runs
  (artifact-only unless `publish=true`).
- `release.yml` (cargo-dist) keeps only its `pull_request` plan check; its
  shell/powershell installers stopped shipping (accepted).
- **npm publishing restored (v1.12.0, cd-25..cd-31).** Investigation showed
  npm had already been stale at `@getspur/spur-cli@1.6.0` — every v1.7.0
  cargo-dist run failed before the cutover. The new in-repo wrapper
  (`npm/spur-cli/`) keeps the package lineage: postinstall downloads the
  version's platform binary from `getspur/spur-releases`, verifies it
  against SHA256SUMS, and a node bin shim execs it (lazy-installs under
  `--ignore-scripts`). Published by the workflow after the GitHub release
  exists; needs `secrets.NPM_TOKEN`.
- **The platform matrix is now four legs** — npm parity forced a linux
  x86_64 artifact back into existence. That leg compiles with Debian's real
  cross GCC (`g++-x86-64-linux-gnu`), NOT zig: pyke's prebuilt GCC
  onnxruntime needs libstdc++ symbols zig's bundled libc++ cannot provide
  (the linux twin of the darwin system-libc++ story). Supporting cast, all
  in `startup-aws.sh`: multiarch `libssl-dev:amd64` + a merged OpenSSL
  include tree (Debian splits headers into shared + arch-only dirs), and an
  `x64-link.sh` driver that lazily compiles lance-linalg's AVX-512
  dist_table kernel from the registry source and appends it to every link —
  the ELF twin of the darwin `ld64-link.sh`. The workflow smoke-tests the
  x64 binary on the runner (the one dist platform the runner can execute)
  before anything publishes.

## Instance sizing

The workflow pins `AWS_INSTANCE_TYPE=m8gd.8xlarge` (32 vCPU Graviton4) with
`SPUR_BUILD_JOBS=32` for fast release builds, falling back to `m8gd.4xlarge`
(the dev-builder shape) when Spot capacity or the regional Spot vCPU quota
blocks the big instance. `MaxSpotInstanceCountExceeded` is treated as a
retryable launch error in `provider-aws.sh` specifically so this fallback
fires — an AZ cycle cannot fix a quota squeeze, but a smaller type can fit
under the remaining headroom. Note the `ap-southeast-5` Spot quota was 32
vCPUs as of 2026-07 (exactly one 8xlarge OR two 4xlarges): while the dev
builder is running, the release primary only launches after a quota bump
(request: Service Quotas → EC2 → L-34B43A08 → 64+).

## Fresh-VM boot hardening (learned from the first live runs)

Four one-shot-CI failure modes were found and fixed in the cloud-build
scripts (see spur-notebook history around 2026-07-07):

1. bash 5.2 + `set -u` rejects `${#arr[@]}` on declared-but-unset arrays —
   `_aws_candidate_azs` died on ubuntu runners (fine on macOS bash).
2. The e2e-toolchain provisioner's exit-code bug (`[[ $RUN_SMOKE -eq 1 ]] &&
   run_vhs_smoke` as last statement) aborted the whole startup under
   `set -e`; provisioning is now non-fatal and the S3 provisioner is fixed.
3. The PathCch/DirectML import-lib self-heal ran at shell init — before the
   xwin splat exists on a fresh VM — so the first-ever PE link failed;
   startup now warms the splat with a hello-world `cargo xwin build` (~30 s)
   and plants the libs at boot.
4. Quota errors (`MaxSpotInstanceCountExceeded`) were terminal instead of
   falling back to the smaller instance type.
5. Parallel dist legs (`cargo xtask dist --parallel`, v1.10.0) raced the
   rustup first-use ensure in the shared `RUSTUP_HOME` — two legs downloading
   the pinned toolchain's components collided on the `.partial` rename.
   `build.sh` now runs a `flock`-serialized `rustc --version` before cargo.
6. `build.sh`'s client-side FIFO admission queue now separates per-VM slots from
   fleet width. On `aws-my`, the default pool is **2 builders × 3 slots**:
   four-leg release matrices place the fourth leg on `spur-release-<run_id>-2`
   instead of overloading one VM or waiting behind the first three. An explicit
   `SPUR_BUILD_MAX_CONCURRENT` override still wins as the fleet-wide cap.

## Measured runs (2026-07-07, version-bump-cold caches)

| Release | Mode | Total | Build step | Legs |
|---|---|---|---|---|
| v1.8.0 | sequential, old AMI | 15m10s | — | — |
| v1.9.0 | sequential, baked AMI | 21m17s | ~13.5m | sum of legs |
| v1.10.0 | parallel ×3, `-j16`, baked AMI | 17m35s | 12m05s | linux 2m31s, macos 5m10s, **windows 10m19s** |
| v1.11.0 | + msvc sccache (cd-19) | **14m11s** | 8m44s | **windows 2m03s**, linux 3m32s, **macos 5m55s** |

v1.10.0's Windows leg was the critical path because cargo-xwin's cl-mode
C/C++ compiles (DuckDB) bypassed sccache: cargo-xwin resolves plain
`clang-cl` through its own PATH-prepended cache-dir symlink (which points at
`which clang` — clang-16 in cl mode, not the provisioned clang-cl-19) and
stomps `CC_x86_64_pc_windows_msvc` on the inner cargo. Fix (cd-19): build.sh
injects the **dash-variant** `CC_x86_64-pc-windows-msvc` (checked first by
cc-rs, never set by cargo-xwin) pointing at `/usr/local/bin/sccache-clang-cl`
(pinned to clang-cl-19). Isolated fresh-VM measurement: the full windows
build dropped 10m19s → **3m45s** with 430/435 C/C++ cache hits; in the
v1.11.0 release the leg ran 2m03s. The macOS universal2 leg (double
arch compile + lipo) is now the critical path; splitting its two arches into
parallel legs is the next lever if sub-12-min releases matter.

## Known limits

- **Cold start on the base AMI** (no `SPUR_BUILDER_AMI_ID`): 8-12 min of
  provisioning inside a 15-min `startup done` wait ceiling
  (`provider-aws.sh::_aws_wait_startup_done`) — the xwin toolchain bootstrap
  can brush that ceiling on slow mirror days. Bake a golden AMI
  (`SPUR_CLOUD=aws-my scripts/cloud-build/bake-ami.sh`) and set the repo
  variable; startup then only mounts NVMe + seeds the toolchain.
- **Spot capacity**: if m8gd and c8gd are both exhausted across all Malaysia
  AZs, `spin.sh` fails and the run fails fast (by design). Re-dispatch, or
  temporarily set `AWS_INSTANCE_TYPE` via a workflow env edit.
- **Mid-build Spot reclaim** is handled by `build.sh`'s single recovery
  re-spin (`ensure_vm_up` after a failed remote step); a second reclaim in
  one run fails the job.
- The first release on a fresh sccache bucket/prefix is a full cold compile
  of all three targets (~the sum of the three local cold builds); subsequent
  releases ride the shared warm L2 that dev builds keep hot.
