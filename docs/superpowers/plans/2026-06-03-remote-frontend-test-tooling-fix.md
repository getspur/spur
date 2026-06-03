# Remote Frontend-Test Tooling Fix Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-03-remote-frontend-test-tooling-fix-design.md`
**Design epic:** `bd-3gk2` (closed)

**Goal:** Make `scripts/gcp-build/build.sh --pnpm` run arbitrary pnpm commands (test, typecheck, build) on the GCP VM, not just `install`.

**Architecture:** Symlink-first, flagless commands. Create the in-source `node_modules` symlink to `/mnt/cargo/pnpm-nm/<worktree>` **before** install; keep `--store-dir` on the `install` command only; drop `--modules-dir` everywhere and run every other pnpm command with no store/modules flags. pnpm installs through the symlink (files + `.bin` land on `/mnt/cargo`, hard-links intact) and `node_modules/.bin` resolves for `run`/`test`/`build`.

**Tech Stack:** Bash (`scripts/gcp-build/build.sh`), pnpm v10, GCP remote-build pipeline.

---

### Task 1: Fix the `--pnpm` block in build.sh

**Task ID:** `task-pnpm-fix`

**Files:**
- Modify: `scripts/gcp-build/build.sh` (the `if [[ $PNPM -eq 1 ]]` block, ≈ lines 280–342)

**Depends on:** none

**Acceptance Criteria:**
- [ ] In the reinstall branch, `ensure_node_modules_link` is called immediately after `mkdir -p "$pnpm_node_modules"` and **before** the `pnpm … install` line.
- [ ] The `install` command keeps `--store-dir "$pnpm_store"` but no longer passes `--modules-dir "$pnpm_node_modules"`.
- [ ] The final user-command line no longer passes `--store-dir` or `--modules-dir` (becomes `pnpm --dir "$frontend_dir"$PNPM_ARGS_ESCAPED`), and its preceding `echo` is updated to match.
- [ ] The `ensure_node_modules_link` function definition, the same-filesystem guard, the version-marker skip logic, and the `package-lock.json` branch are all unchanged.
- [ ] No other lines in the file change. `bash -n scripts/gcp-build/build.sh` exits 0.

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: only the `--pnpm` block of `scripts/gcp-build/build.sh`.
- OUT of scope: the cargo path in build.sh, `scripts/spur-pnpm`, `scripts/spur-cargo`, any other gcp-build script, and any frontend/source file. Do NOT attempt to run the remote pipeline (no VM access; verification is the brain's job).
- If you discover you need to touch OUT-OF-SCOPE files, emit `scope_drift` immediately.

**Implementation:**

The block runs inside a `remote_ssh --command="bash -lc '…'"` heredoc, so the lines use escaped quotes (`\"…\"`). Preserve that escaping and the existing indentation exactly. Make exactly two edits.

- [ ] **Step 1: Edit the reinstall branch — link before install, drop `--modules-dir` from install.**

Find (exact, including the escaped quotes and indentation):

```
                mkdir -p \"\$pnpm_node_modules\"
                pnpm --dir \"\$frontend_dir\" --store-dir \"\$pnpm_store\" --modules-dir \"\$pnpm_node_modules\" install \"\${install_flags[@]}\"
```

Replace with:

```
                mkdir -p \"\$pnpm_node_modules\"
                ensure_node_modules_link
                pnpm --dir \"\$frontend_dir\" --store-dir \"\$pnpm_store\" install \"\${install_flags[@]}\"
```

- [ ] **Step 2: Edit the user-command line (and its echo) — drop both flags.**

Find (exact):

```
            echo \"[build] pnpm --dir $NOTEBOOK_FRONTEND_DIR --store-dir \$pnpm_store --modules-dir \$pnpm_node_modules$PNPM_ARGS_ESCAPED\"
            pnpm --dir \"\$frontend_dir\" --store-dir \"\$pnpm_store\" --modules-dir \"\$pnpm_node_modules\"$PNPM_ARGS_ESCAPED
```

Replace with:

```
            echo \"[build] pnpm --dir $NOTEBOOK_FRONTEND_DIR$PNPM_ARGS_ESCAPED\"
            pnpm --dir \"\$frontend_dir\"$PNPM_ARGS_ESCAPED
```

(The `ensure_node_modules_link` call that already sits just before this `echo` — covering the skip-install case — stays as-is. It is idempotent; the new call in Step 1 covers the reinstall case where `$pnpm_node_modules` was just wiped.)

- [ ] **Step 3: Syntax check.**

Run: `bash -n scripts/gcp-build/build.sh`
Expected: exit 0, no output.

- [ ] **Step 4: Confirm the diff is exactly these edits.**

Run: `git diff scripts/gcp-build/build.sh`
Expected: only the lines above changed — one `ensure_node_modules_link` added in the reinstall branch, `--modules-dir …` removed from the install line, and `--store-dir … --modules-dir …` removed from the user-command line + its echo. Nothing else.

- [ ] **Step 5: Commit.**

```bash
git add scripts/gcp-build/build.sh
git commit -m "fix(remote-build): make build.sh --pnpm run test/typecheck/build, not just install"
```

**Scope Drift Checkpoint:**
- If the exact "find" strings above don't match the file verbatim (e.g., the block was refactored since the spec), STOP and emit `scope_drift` rather than guessing — the brain will re-sync the plan to the current file.

---

## Self-Review

**1. Spec coverage:** §3 of the spec lists three changes — (1) symlink before install, (2) install keeps `--store-dir`/drops `--modules-dir`, (3) flagless user command. Step 1 covers (1) and (2); Step 2 covers (3). §4/§6 acceptance (test+typecheck+build remote) is the brain verification gate, explicitly out of worker scope.

**2. Placeholder scan:** No TBD/TODO; exact find/replace strings provided; no vague instructions.

**3. Type consistency:** N/A (shell). Variable names (`$frontend_dir`, `$pnpm_store`, `$pnpm_node_modules`, `$PNPM_ARGS_ESCAPED`, `ensure_node_modules_link`) match the existing build.sh exactly.

**4. DAG validation:** Single task, no dependencies — trivially acyclic.

**5. beads compatibility:** Unique task ID (`task-pnpm-fix`), empty `depends_on`, verifiable acceptance (`bash -n` + diff shape), explicit scope boundary with scope_drift guidance.
