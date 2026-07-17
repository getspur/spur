# Runtime Skill Projection Design

**Decision date:** 2026-07-17
**Status:** Approved in brainstorming
**Target area:** `crates/spur-core/src/skills`, runtime brain/worker launch,
`spur skills init`

## Summary

SPUR will resolve and materialize the effective skill set immediately before a
brain or worker agent starts. The effective v1 set contains every bundled skill
and every active ecosystem-pool skill. SPUR renders that set in the running
agent's native format into an immutable runtime generation, then projects the
generation into the agent's discovery directory with persistent, SPUR-owned
symlinks. When symlinks are unavailable, SPUR uses tracked copies.

The same resolver and projection service will power `spur skills init`, so
explicit initialization and automatic TUI startup have identical source,
precedence, rendering, ownership, and safety behavior.

This design makes all skills under `assets/skills/` built-in and installable for
every supported adapter. A bundled skill's `role` metadata no longer prevents
adapter installation. Repository-local overrides retain their current role
restrictions.

## Problem

The bundled catalog contains brain-only and worker/both skills. The current
installer deliberately skips brain-only skills when it writes worker-agent
directories. As a result, `spur skills init` does not expose the complete
`assets/skills/` catalog through Codex, Claude, Gemini, Kiro, OpenCode, Kimi, or
Cursor discovery paths.

Pool skills have a separate dispatch-time materializer. It writes rendered
files directly into worker worktrees, only on the worker path. Brain startup
has no equivalent hook, and the installer and runtime materializer can drift in
selection, ownership, and failure behavior.

Directly linking agent directories to canonical `.spur/skills/<id>` sources is
not correct because adapters require different names and file formats. For
example, agentskills-compatible adapters use `spurpower-<id>` frontmatter,
Codex has its own rendering, and Cursor consumes a `.mdc` file.

## Goals

- Make every bundled asset a first-class built-in skill that can be installed
  by `spur skills init` for every supported adapter.
- Automatically project skills before both brain and worker agent startup.
- Include all active pool skills in v1, without per-agent assignment.
- Reuse the existing adapter renderers rather than introduce a second format
  implementation.
- Keep projections persistent and reconcile them on the next launch.
- Prefer symlinks while supporting filesystems where symlink creation fails.
- Never overwrite or delete user-managed files.
- Keep generated files out of commits and avoid modifying repository
  `.gitignore` files.

## Non-goals

- Per-agent or per-profile skill allowlists. This is the next-stage selection
  policy and is deliberately deferred.
- Mid-session skill updates. Agent harnesses discover skills at session start.
- Installing every available agent persona into every harness. Existing
  agent-profile selection and materialization remain responsible for the one
  profile that is actually running.
- Replacing the ecosystem pool's pin, license, scan, and replacement gates.
- Changing adapter-native skill formats.
- Projecting skills for agent kinds that do not have a supported SPUR skill
  adapter. Such kinds retain their current behavior and are not reported as
  successfully materialized.

## Relationship to Existing Designs

This design extends the marker-guarded installer in
`2026-04-19-skills-installer-design.md`, the asset catalog in
`2026-06-27-skill-assets-split-design.md`, and the pool in
`2026-07-07-explore-command-design.md`.

For skill delivery only, it supersedes three decisions in the `/explore`
design:

- materialization is persistent and reconciled, not session-ephemeral;
- v1 selects all active pool skills, not a task-specific subset;
- projection failure stops the affected agent startup instead of silently
  degrading to bundled skills.

Pool persona storage, agent-profile selection, and harness-native persona
materialization are unchanged. The selected running profile may itself come
from built-in configuration or the pool; skill projection runs after that
profile is resolved and before the agent session starts.

## Chosen Approach

Use an immutable rendered projection store plus reconciled links.

Two alternatives were rejected:

1. Writing rendered files directly into every agent directory duplicates
   content and makes ownership, stale cleanup, and recovery harder.
2. Linking agent directories directly to canonical skill sources cannot
   preserve adapter-specific naming and frontmatter, and cannot produce
   Cursor's `.mdc` representation.

Generated generations live outside `.spur/skills`. That directory remains a
canonical source/override namespace whose child directories are scanned as
skill IDs; nesting an adapter cache there would make cache directories look
like malformed skills.

## Architecture

Introduce one runtime projection service in `spur-core`, shared by the CLI and
orchestrator. Its public boundary accepts a launch root, adapter, running role,
and selection policy, and returns a structured reconciliation summary.

Conceptually:

```text
bundled catalog -----+
active pool ---------+--> resolve --> render --> publish generation
repo overrides ------+                              |
                                                    v
agent target dir <-- reconcile links/copies <-- manifest
```

The implementation should keep four responsibilities separate:

1. **Resolver** — produces deterministic `ResolvedSkill` values with canonical
   ID, source identity, source tree, role eligibility, and content digest.
2. **Projection builder** — copies supporting assets, renders the adapter entry
   point, and publishes an immutable generation.
3. **Reconciler** — safely updates only SPUR-owned target paths and records the
   result.
4. **Launch integration** — invokes the service at the required point for CLI,
   brain, and worker flows.

The current pool-only worker materializer should delegate to or be replaced by
this service. There must be one skill-rendering and reconciliation path.

## Effective Skill Resolution

### Source identity

"Built-in" is a source property, represented by the existing bundled source
identity, not a new frontmatter flag that asset authors must repeat. Every
valid `assets/skills/<id>/SKILL.md` entry is built-in.

### Selection policy

V1 has one runtime policy: `AllActive`.

- Every bundled skill is selected for every supported adapter, even when its
  frontmatter declares `role: brain`.
- Every active, gate-approved pool skill is selected.
- No per-profile or per-delegation narrowing is introduced by this change.

The policy should be an explicit type or enum rather than an implicit loop so a
future per-agent allowlist can replace `AllActive` without changing rendering,
ownership, or launch hooks.

### Precedence

For each canonical ID, eligible candidates resolve in this order:

1. repository-local override;
2. active pool skill;
3. bundled skill.

Eligibility is evaluated before precedence. Therefore, a brain-only
repository override does not hide a lower-precedence bundled skill from a
worker projection. Repository override role behavior stays unchanged while
bundled assets bypass adapter-install role filtering.

An active pool item may replace a bundled ID only after the existing pool gate
has recorded the required replacement decision. The pool layer continues to
resolve global/local pool precedence and verify pinned content before the
projection resolver receives the item.

Managed `SpurHermetic` projections under `.spur/skills/` remain delivery
artifacts and are ignored as repository override candidates. An unowned or
user-edited `.spur/skills/<id>` entry remains a repository override under the
existing rules.

Output order is deterministic by canonical ID.

## Adapter Rendering and Generation Layout

The projection builder uses the existing `skills::adapters` renderers. It does
not link raw source `SKILL.md` files into adapter directories.

For each selected skill, the builder stages the complete source directory so
scripts, references, and other progressive-disclosure assets retain their
relative paths. It then overlays the adapter-rendered entry point:

- agentskills-style adapters receive their native directory and frontmatter;
- Codex receives its existing Codex rendering;
- Cursor receives a rendered `.mdc` file;
- adapter-level companion files such as the Kiro steering pointer are part of
  the same generation and manifest.

Runtime data is scoped to the launch root:

```text
.spur/runtime/skill-projections/<adapter>/
  reconcile.lock
  manifest.json
  pending.json              # present only during an interrupted transaction
  generations/
    <sha256>/
      <rendered target tree>
```

The generation digest covers the selected canonical IDs, resolved source
digests, adapter identity, renderer schema version, and rendered bytes. Equal
inputs therefore reuse an existing generation without rewriting it. A new
generation is built under a temporary sibling and atomically renamed into
`generations/<sha256>` only after every output validates.

Targets use relative symlinks where the platform permits them. Directory-based
adapters link a skill directory; file-based adapters such as Cursor link the
rendered file. If any symlink creation attempt fails, the reconciler falls back
to copying that target and records the mode and digest.

## Manifest and Ownership

`manifest.json` is the ownership authority for the current adapter projection.
It is written atomically and records at least:

- schema version;
- adapter and renderer schema version;
- selection policy and generation digest;
- canonical skill ID and resolved source kind/digest;
- agent-relative target path;
- projection mode (`symlink` or `copy`);
- expected symlink destination or copied-content digest;
- adapter-level companion targets.

A target is SPUR-owned only when one of these conditions holds:

- it matches an entry in the current manifest;
- it matches a validated old/new state in the current pending transaction;
- it carries a valid legacy `SPUR-MANAGED` marker;
- it is recorded by the existing pool-materialization metadata during the
  one-time migration.

An arbitrary path, an unrecorded link, or a file with an invalid/edited marker
is user-owned. Merely having a `spurpower-` name is never proof of ownership.

## Reconciliation Algorithm

Reconciliation is serialized by a per-launch-root, per-adapter lock.

1. Resolve the effective set and publish or reuse its immutable generation.
2. Load and validate the prior manifest. Reject paths that escape either the
   launch root or projection root.
3. Preflight every desired target and classify it as absent, unchanged,
   SPUR-owned, legacy-adoptable, or user-owned.
4. Preserve user-owned targets, emit a warning, and omit only the colliding
   projection. A collision does not block other skills or agent startup.
5. Atomically write `pending.json` with every target's validated old state and
   intended new state.
6. Create missing targets and atomically switch SPUR-owned targets to the new
   generation. Use a tracked copy when symlink creation fails.
7. Remove stale targets only when their current state still matches the prior
   manifest. A changed link or edited copied fallback is preserved and
   ownership is relinquished with a warning.
8. Atomically commit the new manifest, remove `pending.json`, then
   garbage-collect unreferenced generations.

The pending transaction lets the reconciler roll back ordinary I/O failures
that occur while switching targets. The old manifest and generation remain
until the new manifest commits. If a process crash interrupts a multi-target
switch, the next reconciliation validates `pending.json` against the old
manifest and actual target states, then completes or rolls back the transaction
before resolving a new generation. No agent starts while recovery is pending.

Garbage collection never removes a generation still referenced by a preserved
user-owned link or copied target.

## Git Hygiene

Every SPUR-owned target and `.spur/runtime/skill-projections/` path is added to
the launch root's worktree-local excludes through the existing worktree exclude
mechanism. SPUR does not edit a repository `.gitignore`.

If a target was already tracked by Git, projection treats it as user-owned
unless the ownership rules above prove it was generated by SPUR. A warning
explains why it was skipped.

## Launch Integration

### Brain

Brain projection runs against the main repository root after the brain profile
and adapter are resolved, but before the ACP connection/session is created.
This guarantees the harness sees the reconciled discovery directory during its
startup scan.

### Worker

Worker projection runs inside the provisioned worker worktree after the
selected agent profile is materialized and before the worker process or ACP
session starts. It replaces the pool-only materialization call and resolves the
union of built-in, active pool, and eligible repository skills.

### Manual initialization

`spur skills init` invokes the same resolver, builder, and reconciler for the
adapters selected by the command. It remains the explicit prewarm/refresh
surface and installs every bundled skill regardless of bundled role metadata.
Automatic brain or worker startup does not require the command to have run
first.

Projections persist after a session ends. Session teardown performs no skill
cleanup; the next initialization or launch reconciles stale state.

## Error Handling

These conditions are warnings and do not block startup:

- an unowned target collision;
- a user-edited copied fallback;
- a stale target whose ownership can no longer be proven.

The colliding skill is omitted, and the summary names its canonical ID and
path.

These conditions fail projection and stop the affected agent before its
connection/session starts:

- bundled catalog or active pool resolution failure;
- invalid skill ID, source path traversal, or pool digest mismatch;
- adapter rendering or generation publication failure;
- both symlink creation and copy fallback failure;
- manifest corruption that cannot be safely reconstructed;
- lock, rollback, or required exclude-update failure.

No failure is silent. Because projection precedes agent startup, SPUR never
launches an agent believing it received a complete generation after a fatal
projection error.

## Migration

The first reconciliation adopts legacy output only when the existing generated
marker validates or old pool-materialization metadata proves ownership. It
replaces adopted direct files with links or tracked copies and records them in
the new manifest.

Unmarked files remain untouched. Legacy output that was hand-edited fails its
marker check, is preserved as user-owned, and produces a warning.

Old pool metadata may be retained until the new manifest commits. A failed
migration is therefore retryable and cannot erase the last known ownership
record.

## Observability

The service returns a structured summary used by both CLI and TUI launch logs:

- skills linked;
- skills copied through fallback;
- unchanged projections;
- stale owned targets removed;
- legacy targets migrated;
- user-owned collisions or edits skipped;
- selected source for IDs whose higher-precedence candidate won;
- generation digest and adapter.

Warnings identify the path and safe reason for skipping it. Fatal errors carry
the launch root, adapter, phase, skill ID when applicable, and underlying I/O or
validation cause.

## Testing

### Resolver tests

- Enumerate every valid `assets/skills/*/SKILL.md` as built-in.
- Select every bundled skill for brain and worker adapters regardless of
  bundled role metadata.
- Preserve role filtering for repository-local overrides.
- Verify repository override > active pool > bundled precedence.
- Verify an ineligible higher-precedence override falls back to an eligible
  lower-precedence candidate.
- Verify deterministic order and rejected/invalid pool replacements.

### Projection tests

- Render each supported adapter with its existing naming/frontmatter format.
- Preserve supporting files relative to the rendered entry point.
- Produce stable generation hashes and reuse equal generations.
- Create relative directory and file symlinks.
- Inject symlink failure and verify tracked-copy fallback.
- Verify copied-content digests and adapter companion targets.

### Reconciliation tests

- Re-run unchanged projection idempotently.
- Update only manifest-owned links/copies.
- Preserve unowned collisions and user-edited copied fallbacks.
- Remove stale, unchanged owned targets and retain changed targets.
- Migrate valid legacy markers and old pool metadata.
- Roll back injected switch/write failures.
- Recover an interrupted transaction using the prior manifest and pending
  journal.
- Serialize concurrent reconciliation for the same root/adapter.
- Garbage-collect only unreferenced generations.
- Leave Git status clean through worktree-local excludes.

### Launch integration tests

- `spur skills init` projects all bundled skills for selected adapters.
- Brain projection completes before connection/session creation.
- Worker projection completes after worktree/profile materialization and before
  process/session spawn.
- Brain and worker launches work without prior `spur skills init`.
- Brain and worker launches include active pool plus bundled skills.
- Fatal projection errors prevent startup; collisions warn and allow startup.

All Rust compilation and tests must run through `scripts/spur-cargo`.

## Acceptance Criteria

1. A clean `spur skills init` makes every valid skill in `assets/skills/`
   discoverable through every selected supported adapter, including skills
   marked `role: brain`.
2. Starting a supported brain or worker through the TUI automatically
   reconciles its effective skills before the agent scans its discovery path.
3. V1 includes all active pool skills; an authorized pool duplicate overrides
   the bundled skill with the same canonical ID.
4. Repeated launches with unchanged inputs perform no projection rewrites.
5. SPUR never overwrites or removes an unowned or user-modified target.
6. Symlink-unavailable environments receive equivalent tracked copies.
7. Removing or changing a selected source is reflected on the next launch
   without session-end cleanup.
8. Projection artifacts do not appear in worker or main-worktree Git status.
9. Manual initialization and automatic launch produce the same resolved and
   rendered skill generation for the same root, adapter, role, and source set.
