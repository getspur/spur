# Codex Profile Probe Artifact Hardening Design

**Date:** 2026-07-10
**Status:** Approved recovery design
**Recovery base:** `spur/worker/v2/codex/c5a8f46529983e46/06370a6f-5d92-43f9-b7b4-2328c9ceca38`

## Context

The Codex `0.144.1` reprobe established the functional result: Codex native
subagents work through `@agentclientprotocol/codex-acp@1.1.2`. A restricted
positive control bound the exact requested `agent_type` to the exact child
`agent_role`, and the raw child result contained the role-only canary. The
no-profile control returned exactly `NO_PROFILE_ACTIVE` without spawning a
child. The probe also verified the executed CLI version, `CODEX_PATH`, stable
multi-agent V1 selection, and app-server warning capture.

The third implementation attempt was rejected because two artifact-boundary
claims were not true:

1. Newly created Codex rollout JSONL files lived under the existing
   `~/.codex/sessions` tree with mode `0644`. The parent and child rollouts
   contained the generated canaries, but the probe audited only its local
   output directory.
2. The output and app-server-log paths were not consistently no-follow.
   A symlinked app-server log directory was followed, and an external
   `app-server.log` was truncated and recreated.

This recovery keeps the validated probe and seed changes and fixes only these
artifact boundaries plus one normalization mismatch found during review.

## Goals

- Keep every canary-bearing local artifact and newly created Codex rollout
  inaccessible to group and other users for the entire restricted run.
- Reject a symlinked output root, nested artifact directory, app-server log
  directory, or final artifact file before modifying an external target.
- Audit the exact new rollout files and their directory chain before reporting
  `PROFILE_PROBE_PASS`.
- Use one normalization rule when recognizing ACP `spawn_agent` titles so every
  accepted spawn input participates in the canary-leak check.
- Preserve all previously validated behavior and the general-purpose probes.
- Finish with the full cross-crate regression gate that was skipped when the
  original plan exhausted its review attempts.

## Non-goals

- Changing Codex's global rollout policy outside the opt-in restricted probe.
- Rewriting the general ACP probe or removing its filesystem and terminal
  capabilities.
- Copying, persisting, or logging Codex authentication material into a probe
  artifact directory.
- Changing profile selection semantics, model/effort precedence, native-role
  rendering, or the shipped adapter version.
- Retrofitting permissions on unrelated historical Codex rollout files.

## Approaches considered

### A. Restricted-process umask plus exact rollout hardening (selected)

The inner restricted probe sets a private process umask before it launches
`npx`; the adapter and Codex descendants inherit it. The outer probe then
validates, secures, and audits the exact rollout paths created after each
before/after snapshot. This preserves the user's existing Codex login and
configuration while eliminating the `0644` creation window on POSIX systems.

This is the smallest approach that addresses both confidentiality and live
probe reliability. It also gives the verdict explicit evidence about every
new rollout rather than treating the global session tree as implicitly safe.

### B. Dedicated per-run `CODEX_HOME`

This gives the cleanest filesystem isolation, but Codex authentication and
configuration also live beneath `CODEX_HOME`. Making the live run work would
require copying credentials, linking to the user's auth state, or inventing a
new authentication handoff. Each option creates a larger and riskier security
surface than the artifact problem being fixed. This approach is rejected.

### C. Post-run `chmod` only

This is simple, but canary-bearing rollout files remain group/world-readable
while the model turn is active. It also cannot substantiate the claim that the
artifacts were private for the entire run. Post-run repair remains useful as
defense in depth, but it is insufficient by itself.

## Design

### 1. Fresh, no-follow artifact root

The profile probe must use a fresh output leaf. The default already contains a
timestamp and random suffix; an explicit `--profile-probe-out-dir` must not
exist. Before canonicalizing the location, the probe checks the supplied leaf
with `lstat` and rejects symlinks or any pre-existing entry. It then creates
the leaf with mode `0700` and retains a directory handle for child creation.

All probe-owned subdirectories are created relative to a verified directory
handle. Existing entries are rejected rather than reused. Artifact files are
opened with create/truncate plus no-follow semantics and immediately forced to
`0600`. The design must not unlink an existing path as a way to prepare it.

This fresh-root rule removes the need to support rerunning into a partially
populated directory. A blocked or failed run remains available for inspection;
the next run uses a new path.

### 2. App-server log preparation

`prepare_app_server_log` uses the same private-directory and private-file
primitives as the raw ACP log and captured stdout/stderr. A symlinked log
directory or log file is an error. Preparation creates a new empty
`app-server.log` at `0600`; it never follows a directory symlink and never
unlinks an external target.

The adapter still receives the absolute verified log directory through
`APP_SERVER_LOGS`. Missing, empty, or malformed-warning-bearing logs remain
verdict failures.

### 3. Private Codex rollout boundary

Restricted mode sets umask `0077` inside the dedicated inner Python process
before spawning the adapter. General probe mode does not change the process
umask or authentication environment.

The outer probe snapshots the canonical Codex session root before each
control and computes the exact new rollout set afterward. For each new path it
must:

- prove the path is beneath the expected canonical session root;
- reject symlinks and non-regular files using `lstat` plus a no-follow file
  descriptor where the platform supports it;
- force the rollout file to `0600` and verify the resulting mode;
- validate every directory from the session root through the rollout parent,
  rejecting symlinks and tightening current-user-owned directories to `0700`;
- record a sanitized audit result without logging canary values.

On POSIX, the inherited umask prevents the exposure window and the descriptor
audit proves the final state. On a platform that cannot express numeric POSIX
modes, the probe must not print a POSIX-mode pass; it must either use an
equivalent access-control check or return a blocked result with the exact
unsupported invariant.

The activity and role-binding readers consume only the rollout paths that pass
this audit. Missing, malformed, unreadable, out-of-root, symlinked, or
permission-insecure rollout evidence fails closed.

### 4. Final artifact audit

`protect_artifact_tree` becomes an assertion-and-hardening pass, not a best
effort walk. It fails on any symlink or unsupported entry type. Every
probe-owned directory must be `0700`; every regular file must be `0600`.

The final pass condition requires both audits:

- the complete local output tree is private and symlink-free; and
- every exact new Codex rollout used as evidence passed the external rollout
  audit.

The July 10 addendum must state these two scopes separately. It must not call
the local output tree the complete token-bearing tree while external rollouts
exist.

### 5. Consistent spawn-title normalization

`load_codex_probe_evidence` and `codex_profile_activity_failures` use the same
normalizer for `spawnAgent`, `spawn_agent`, and space-separated renderings.
Any title accepted by the activity verdict must have its raw input captured
and scanned for both generated canaries.

## Error semantics

- A symlink, out-of-root path, unsafe mode, or incomplete artifact audit is a
  deterministic probe failure (`exit 1`).
- Authentication, network, package-resolution, or adapter-startup failures
  remain blocked outcomes (`exit 2`) with sanitized exact commands/errors.
- No failure path may weaken permissions, skip an unsafe entry, or report
  `PROFILE_PROBE_PASS`.
- Cleanup must not delete user-owned Codex sessions or historical rollouts.

## Test strategy

Implementation follows two commits for the bugfix: a failing regression commit
followed by the minimal fix commit.

Unit regressions cover:

- explicit output root is a symlink;
- a nested artifact directory is a symlink;
- app-server log directory and final log file are symlinks;
- external victim content is unchanged after every rejected case;
- restricted descendants create files with private modes;
- exact new rollout paths are regular, beneath the expected root, `0600`, and
  have a verified private directory chain;
- unsafe, missing, malformed, unreadable, or out-of-root rollouts fail closed;
- final local-tree audit rejects rather than skips symlinks;
- space-separated spawn titles cannot bypass canary scanning;
- general Node/Python probes preserve their existing auth and capability
  behavior.

The focused live probe must then prove, without printing canary values:

- adapter `1.1.2` and executed `codex-cli 0.144.1`;
- `{}` client capabilities and zero agent-to-client requests;
- exact `spawnAgent + wait` ACP activity;
- exact `spawn_agent + wait_agent` rollout activity;
- exact requested and loaded child role;
- strict no-profile control;
- clean adapter and app-server warning surfaces;
- private local artifacts and private exact rollout evidence.

## Final integration gate

After the hardening task is approved, a separate regression task runs the
original combined contract over all predecessor overlays:

1. `scripts/spur-cargo fmt --all -- --check`
2. `scripts/spur-cargo test -p spur-acp`
3. `scripts/spur-cargo test -p spur-core`
4. `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-acp -p spur-core --all-targets -- -D warnings`
5. `scripts/spur-cargo test --workspace`
6. `git diff --check`

Compile-heavy commands run remotely through `scripts/spur-cargo` from the task
worktree. The live probe is rerun when authentication/network permit. Exact
commands, counts, blocked outcomes, and remaining risks are appended to the
July 10 addendum, with automated, live, and source-only evidence kept separate.

## Acceptance criteria

- The two reproduced Important findings have RED tests that fail on recovery
  base `803ed3ff2` and pass after the fix.
- A symlinked output/log path cannot modify an external victim.
- Canary-bearing rollouts used by the verdict are never left group/world
  readable on the verified POSIX run.
- The functional Codex `0.144.1` native-agent result remains unchanged.
- The July 5 RCA remains untouched.
- The hardening and final integration tasks are independently reviewable and
  committed.
