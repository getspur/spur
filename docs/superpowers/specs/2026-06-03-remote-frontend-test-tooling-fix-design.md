# Remote Frontend-Test Tooling Fix (`build.sh --pnpm`) — Design

- **Date:** 2026-06-03
- **Status:** Approved design, pre-plan
- **Design epic:** `bd-3gk2`
- **Scope:** `scripts/gcp-build/build.sh` — the `--pnpm` block only. No change to the
  cargo remote path, `scripts/spur-pnpm`, or `scripts/spur-cargo`.

## 1. Problem

`scripts/spur-pnpm` (→ `build.sh --pnpm`) can `pnpm install` the notebook frontend
on the GCP build VM but cannot run any other pnpm command. Two compounding bugs:

1. **Invalid flags on non-install subcommands.** `build.sh` passes
   `--store-dir`/`--modules-dir` to *every* pnpm invocation (install **and** the
   user command). pnpm v10 accepts those only on `install`:
   - `pnpm run test` → `ERROR  Unknown options: 'store-dir', 'modules-dir'`.
   - bare `pnpm test` → mis-parses and tries to spawn the project directory
     (`spawn …/jute-notebook EACCES`, `ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL`).
2. **Relocated `--modules-dir` yields an unrunnable `node_modules`.** Even invoked
   directly on the VM, `pnpm run test` fails with `vitest: not found`; the expected
   `…/pnpm-nm/main/.bin/vitest` is absent. The relocation-by-flag scheme does not
   produce a `.bin` that `pnpm run` resolves.

Net effect: the remote frontend pipeline is non-functional for `test`, `typecheck`,
and `build` — for **any** change, not just the one that surfaced it.

### 1.1 Hard constraint (must preserve)

Per build.sh's own comment: pnpm hard-links `node_modules` entries from its
content-addressable store, and hard-links only work within one filesystem.
Therefore the real `node_modules` and the pnpm store both live under `/mnt/cargo`
(the cache disk); the in-source `node_modules` is a symlink. Any fix must keep the
real `node_modules` + store co-located on `/mnt/cargo`.

## 2. Decision

| # | Question | Decision | Rationale |
|---|----------|----------|-----------|
| A | Approach | **Symlink-first, flagless commands** (Approach 1) | Root-cause fix; install and run become symmetric; `.bin` resolves naturally |
| B | Acceptance bar | **C: test + typecheck + build** all green remotely | True end-to-end validation of the remote frontend pipeline |
| C | Relocation mechanism | In-source `node_modules` symlink to `/mnt/cargo/pnpm-nm/<worktree>`, created **before** install | Lets pnpm install through the symlink; no per-command `--modules-dir` |
| D | Store location | Keep `--store-dir` on the **install** command only (valid there); drop it everywhere else | Store-dir is irrelevant to `run`/`test`/`build` |
| E | Who verifies remotely | **Brain** (post-merge), not the worker | VM is a spot instance (observed preemption); worker-side remote verify is unreliable |

Rejected alternatives:
- **Approach 2 (relocation via `.npmrc`/env):** keeps the relocated-modules-dir model
  whose `.bin` already failed to resolve on `run`; same wall, more moving parts.
- **Approach 3 (`pnpm exec`/node, bypass `.bin`):** brittle, cannot express
  `build` (`tsc && vite build`).

## 3. The fix

All changes live inside the `if [[ $PNPM -eq 1 ]]` block of
`scripts/gcp-build/build.sh` (≈ lines 280–342). The cargo path is untouched.

1. **Create the symlink before install.** `ensure_node_modules_link` (symlink
   `<frontend>/node_modules` → `$pnpm_node_modules`, i.e.
   `/mnt/cargo/pnpm-nm/<worktree>`) currently runs only *after* install. It must run:
   - inside the reinstall branch, immediately after `mkdir -p "$pnpm_node_modules"`
     (so the symlink exists for the install), and
   - once more before the user command (idempotent — only relinks if wrong).

2. **Install: keep `--store-dir`, drop `--modules-dir`.**
   ```sh
   pnpm --dir "$frontend_dir" --store-dir "$pnpm_store" install "${install_flags[@]}"
   ```
   pnpm resolves the symlinked `node_modules` to its realpath on `/mnt/cargo`,
   installs there (hard-links from the store work — same filesystem), and creates
   `$pnpm_node_modules/.bin`.

3. **User command: fully flagless.**
   ```sh
   pnpm --dir "$frontend_dir"$PNPM_ARGS_ESCAPED
   ```
   No `--store-dir`/`--modules-dir` → no "Unknown options" error; `vitest`/`tsc`/
   `vite` resolve via `node_modules/.bin` through the symlink. `build`
   (`tsc && vite build`) works for the same reason.

Unchanged: the same-filesystem guard (`stat -Lc %d` on `$pnpm_store` vs
`$pnpm_node_modules`), the install-skip version marker, and the
`package-lock.json`-only branch (`NOTEBOOK_FRONTEND_HAS_PNPM_LOCK=0`).

Net diff: ≈5 lines in one block.

## 4. Verification & acceptance

Verified on the VM via the unchanged `scripts/spur-pnpm` wrapper:

- `scripts/spur-pnpm test` → vitest suite passes.
- `scripts/spur-pnpm typecheck` → `tsc` clean.
- `scripts/spur-pnpm build` → `tsc && vite build` succeeds.

All three green on the VM = done. Local execution (`SPUR_REMOTE=0`) is unaffected
(local fallback already runs plain `pnpm --dir <frontend>`).

**Division of labor:** the code edit is a worker task; the remote verification is a
**brain step** run after merge (it doubles as the originally-requested "re-run the
remote frontend test on main"). The VM is a spot instance — `--auto-spin`
re-provisions it if preempted; acceptance is one clean run once it is up.

## 5. Risks & edge cases

- **Spot preemption** (observed this session): operational only; `--auto-spin`
  handles it. Acceptance is a single clean remote run.
- **`--store-dir` on `install`**: confirmed valid in pnpm v10 (install already used
  it successfully).
- **Symlinked `node_modules`**: standard pnpm behavior; realpath resolution keeps
  hard-links on `/mnt/cargo`.
- **No `pnpm-lock.yaml`**: jute-notebook ships `package-lock.json`; the existing
  non-frozen install branch is preserved.
- **Blast radius**: only the `--pnpm` block changes; cargo remote path and both
  wrapper scripts are untouched.

## 6. Task decomposition

- **Task 1 (worker `codex`): edit the `--pnpm` block in
  `scripts/gcp-build/build.sh`** — the three changes in §3. One file, ~5 lines.
  Worker acceptance: `bash -n scripts/gcp-build/build.sh` passes (syntax) and the
  diff matches §3. The worker cannot verify remotely (no VM / spot flakiness).
- **Brain verification gate (post-merge):** run `scripts/spur-pnpm test`,
  `… typecheck`, `… build` against `main`; confirm all three green on the VM.

Single task, no ordering deps, no parallelism.
