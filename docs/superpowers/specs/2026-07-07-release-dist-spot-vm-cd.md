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
   ├─ scripts/cloud-build/spin.sh           VM_NAME=spur-release-<run_id>
   ├─ cargo xtask dist --platforms …        (spur-cargo → build.sh → VM; fetch --via-s3)
   ├─ upload-artifact dist/                 (spur-<ver>-<triple>[.exe] + SHA256SUMS)
   ├─ [publish=true] gh release create      (getspur/spur-releases, GH_RELEASES_TOKEN)
   └─ always(): re-auth OIDC → scripts/cloud-build/teardown.sh
```

Key decisions:

- **Per-run `VM_NAME=spur-release-<run_id>`.** The provider resolves instances
  by Name tag, so a unique name isolates the release box from the shared dev
  builder (`spur-builder`) and from concurrent runs, and makes the teardown
  step surgical — it can only ever terminate its own instance.
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

## Relationship to `release.yml` (cargo-dist) and rollout

The two flows coexist deliberately:

- `release-dist.yml` is **workflow_dispatch-only**. Tag pushes still drive the
  cargo-dist flow, so there is no race on `getspur/spur-releases` while the VM
  flow is validated.
- Artifact-set differences to accept before cutover: `xtask dist` emits **raw
  binaries + SHA256SUMS** (linux is **aarch64-only** — the Graviton builder's
  native target), no shell/msi installers, no npm package. cargo-dist's linux
  x86_64, installers, and `publish-npm` job have no equivalent here yet.
- Cutover plan once trusted: add a `push: tags:` trigger here (deriving the
  tag from `github.ref_name` instead of the input), drop the same trigger
  from `release.yml`, and decide whether npm publishing moves here or stays
  on a slim cargo-dist flow. An x86_64-linux platform would need either an
  x86 builder shape or zigbuild's `x86_64-unknown-linux-gnu` target on the VM.

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
