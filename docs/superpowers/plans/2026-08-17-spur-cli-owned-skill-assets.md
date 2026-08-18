# Spur CLI-Owned Skill Assets Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-08-17-spur-cli-owned-skill-assets-design.ipynb`
**Formal @spec cells:** none
**Design epic:** `bd-2zk` (closed)

**Goal:** Make `crates/spur-cli/assets/skills` the sole checked-in bundled skill corpus while preserving filesystem-backed loading and the installed `share/spur/skills` contract.

**Architecture:** The corpus becomes package-owned data under `spur-cli`, while `spur-core` remains the only catalog and projection implementation. Development discovery points at the sibling CLI crate; `xtask`, cargo-dist, and Cargo packaging consume that source tree and continue staging the same installed layout. Release artifact consolidation and executable embedding remain out of scope.

**Tech Stack:** Rust 2021, Cargo, cargo-dist, `xtask`, filesystem skill assets, npm release installer.

---

## File Structure Map

| Path | Responsibility |
|---|---|
| `crates/spur-cli/assets/skills/**` | Sole checked-in bundled skill corpus and supporting files |
| `crates/spur-core/src/skills/mod.rs` | Bundled-root discovery, diagnostics, catalog behavior, and focused unit tests |
| `crates/spur-core/tests/skills_catalog_mcp.rs` | Catalog MCP integration fixture that imports the real bootstrap skill |
| `xtask/src/main.rs` | Local install and tag-release asset sourcing, staging, and packaging tests |
| `crates/spur-cli/Cargo.toml` | Cargo package inclusion contract for CLI sources, tests, and skill assets |
| `dist-workspace.toml` | cargo-dist inclusion contract for the CLI-owned asset tree |
| `AGENTS.md`, `CLAUDE.md` | Live repository instructions for locating the skill catalog |
| `crates/spur-cli/assets/skills/explainer-video-editor/**` | Live commands that invoke a script by its checked-in source path |

## Dependency DAG

```text
cli-skills-1
├── cli-skills-2
└── (source path available to later documentation edits)

cli-skills-2
└── cli-skills-3
```

`cli-skills-1` establishes the canonical path. `cli-skills-2` consumes it from packaging. `cli-skills-3` runs after both so its repository-wide reference audit and verification observe the final layout.

---

### Task 1: Move the Corpus and Update Core Discovery

**Task ID:** `cli-skills-1`

**Files:**

- Move: `assets/skills/**` → `crates/spur-cli/assets/skills/**`
- Modify: `crates/spur-core/src/skills/mod.rs:30-37,247-332,609-611,1549-1660`
- Modify: `crates/spur-core/tests/skills_catalog_mcp.rs:31`

**Depends on:** none

**Acceptance Criteria:**

- [ ] `crates/spur-cli/assets/skills` contains the complete corpus with no content loss.
- [ ] Repository-root `assets/skills` no longer exists and is not a symlink.
- [ ] The development manifest fallback resolves `crates/spur-cli/assets/skills`.
- [ ] The generic `<repo>/assets/skills` compatibility candidate remains available for configured/test repositories.
- [ ] `SPUR_SKILLS_DIR`, layered config, and package-relative candidates retain their existing precedence.
- [ ] Real-corpus `include_str!` and supporting-file tests point at the CLI-owned source path.
- [ ] Focused `spur-core` skill tests pass.

**Suggested Worker:** `claude-code-acp` — the corpus move and coupled resolver/test references require coordinated multi-file handling.

**Scope Boundary:**

- IN scope: moving the corpus without substantive content edits; `SkillCatalog` development fallback and its tests; direct real-corpus test paths.
- OUT of scope: `xtask`, Cargo/cargo-dist manifests, npm behavior, projection policy, frontmatter, individual skill wording.
- If packaging files must change to make this task pass, emit `scope_drift`; those files belong to `cli-skills-2`.

**Scope Drift Checkpoint:**

- If the move requires changing catalog APIs or runtime precedence, emit `risk` before editing.
- If any file outside the listed scope needs modification, emit `scope_drift` immediately.

**Implementation:**

- [ ] **Step 1: Add the failing development-root test**

Add this unit test beside the existing skill catalog tests in `crates/spur-core/src/skills/mod.rs`:

```rust
#[test]
fn manifest_workspace_asset_root_is_cli_owned() {
    let root = manifest_workspace_asset_root();
    assert!(
        root.ends_with("crates/spur-cli/assets/skills"),
        "expected CLI-owned bundled root, got {}",
        root.display()
    );
    assert!(root.join("spur-way/SKILL.md").is_file());
}
```

- [ ] **Step 2: Run the test and verify the old layout fails**

Run:

```bash
scripts/spur-cargo test -p spur-core manifest_workspace_asset_root_is_cli_owned -- --nocapture
```

Expected: FAIL because `manifest_workspace_asset_root()` still ends in repository-root `assets/skills`.

- [ ] **Step 3: Move the corpus and update the manifest fallback**

Move the directory as one Git-aware operation, preserving all nested support files. Change the fallback implementation to:

```rust
fn manifest_workspace_asset_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spur-cli/assets/skills")
}
```

Keep `repo_root.join("assets/skills")` in `select_bundled_root`; it is a generic compatibility candidate, not the checked-in SPUR corpus. Update the `BundledRootSource::Workspace` label so diagnostics say `workspace crates/spur-cli/assets/skills`.

- [ ] **Step 4: Update real-corpus test paths**

Change the bootstrap import in `crates/spur-core/tests/skills_catalog_mcp.rs` to:

```rust
const BOOTSTRAP_SKILL: &str =
    include_str!("../../spur-cli/assets/skills/skills-catalog/SKILL.md");
```

Within `crates/spur-core/src/skills/mod.rs`, update only tests that use the actual bundled source tree, including the `open-design/references/*` paths. Do not rewrite temporary repositories whose `assets/skills` directory is deliberately exercising generic resolver behavior.

- [ ] **Step 5: Verify the corpus and focused catalog behavior**

Run:

```bash
scripts/spur-cargo test -p spur-core manifest_workspace_asset_root_is_cli_owned -- --nocapture
scripts/spur-cargo test -p spur-core skills::tests -- --nocapture
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
```

Expected: all commands exit 0; the first test reports the CLI-owned root and the catalog integration test imports the moved bootstrap skill.

- [ ] **Step 6: Verify the move is complete and commit**

Run:

```bash
test -d crates/spur-cli/assets/skills
test ! -e assets/skills
git diff --check
```

Then commit only this task's scope:

```bash
git add -A assets/skills crates/spur-cli/assets/skills crates/spur-core/src/skills/mod.rs crates/spur-core/tests/skills_catalog_mcp.rs
git commit -m "refactor(skills): cli-skills-1 move corpus under spur-cli"
```

---

### Task 2: Point Cargo, cargo-dist, and xtask at the CLI-Owned Corpus

**Task ID:** `cli-skills-2`

**Files:**

- Modify: `xtask/src/main.rs:65-71,609-713,1240-1271,1571-1760`
- Modify: `crates/spur-cli/Cargo.toml:1-15`
- Modify: `dist-workspace.toml:31-40`

**Depends on:** `cli-skills-1`

**Acceptance Criteria:**

- [ ] `xtask` has one helper defining the build-time skill source as `crates/spur-cli/assets/skills`.
- [ ] Local install and tag-release packaging both consume that helper.
- [ ] Test workspaces create their package-owned fixture under `crates/spur-cli/assets/skills`.
- [ ] Archive contents and installed output remain `share/spur/skills/<id>/...`.
- [ ] The `spur-cli` Cargo package file list contains every skill and supporting file.
- [ ] cargo-dist includes `crates/spur-cli/assets` rather than repository-root `assets`.
- [ ] Existing separate `spur-skills-<version>.tar.gz` and npm behavior remain unchanged.

**Suggested Worker:** `claude-code-acp` — three packaging contracts must change together without altering their installed-layout interface.

**Scope Boundary:**

- IN scope: `xtask` source selection and fixtures, CLI Cargo package inclusion, cargo-dist inclusion, packaging assertions.
- OUT of scope: release artifact consolidation, npm JavaScript, runtime resolver precedence, substantive skill content.
- If npm code appears to require a change, emit `risk`; the installed layout is specified as unchanged.

**Scope Drift Checkpoint:**

- If `scripts/spur-cargo package -p spur-cli --list` exposes unrelated pre-existing package errors, emit `blocked` with the exact output instead of broadening scope.
- If a fourth implementation file is required, emit `scope_drift` before editing.

**Implementation:**

- [ ] **Step 1: Add failing source-path and cargo-dist assertions**

Add this helper contract test to `xtask/src/main.rs`:

```rust
#[test]
fn bundled_skill_assets_source_is_cli_owned() {
    let root = Path::new("/workspace");
    assert_eq!(
        bundled_skill_assets_source(root),
        root.join("crates/spur-cli/assets/skills")
    );
}
```

Tighten `dist_workspace_includes_skill_assets_for_archives_and_installers` to require the exact moved path:

```rust
assert!(
    raw.contains("include = [\"crates/spur-cli/assets\"]"),
    "dist-workspace.toml must include the spur-cli asset tree"
);
```

- [ ] **Step 2: Run the tests and verify they fail**

Run:

```bash
scripts/spur-cargo test -p xtask bundled_skill_assets_source_is_cli_owned -- --nocapture
scripts/spur-cargo test -p xtask dist_workspace_includes_skill_assets_for_archives_and_installers -- --nocapture
```

Expected: the helper test fails to compile because the helper does not exist; after adding only the test seam if needed, the cargo-dist assertion fails against `include = ["assets"]`.

- [ ] **Step 3: Introduce one canonical xtask source helper**

Add:

```rust
const CLI_SKILL_ASSETS: &str = "crates/spur-cli/assets/skills";

fn bundled_skill_assets_source(workspace_root: &Path) -> PathBuf {
    workspace_root.join(CLI_SKILL_ASSETS)
}
```

Use `bundled_skill_assets_source(workspace_root)` in both `package_dist_skill_assets` and `install_bundled_skill_assets`. Update their comments and error text to name the CLI-owned source. Update the affected temporary test fixtures to create the same package-owned source layout.

- [ ] **Step 4: Make packaging inclusion explicit**

Add an explicit package inclusion contract under `[package]` in `crates/spur-cli/Cargo.toml`:

```toml
include = ["src/**", "tests/**", "assets/skills/**"]
```

Change cargo-dist configuration to:

```toml
include = ["crates/spur-cli/assets"]
```

Keep the archive staging path, archive name, checksum behavior, npm platform mapping, and installed `share/spur/skills` path unchanged.

- [ ] **Step 5: Run focused and packaging verification**

Run:

```bash
scripts/spur-cargo test -p xtask
scripts/spur-cargo package -p spur-cli --list
```

Expected: xtask tests pass. The package list contains `assets/skills/spur-way/SKILL.md`, `assets/skills/skills-catalog/SKILL.md`, and nested supporting files such as `assets/skills/open-design/references/deck-mode.md`.

Also run:

```bash
scripts/spur-cargo test -p xtask package_dist_skill_assets_ships_share_spur_skills_layout -- --nocapture
scripts/spur-cargo test -p xtask install_built_binary_copies_skill_assets_to_cargo_home_share -- --nocapture
git diff --check
```

Expected: both behavior tests exit 0 and continue asserting the installed `share/spur/skills` layout.

- [ ] **Step 6: Commit the packaging change**

```bash
git add xtask/src/main.rs crates/spur-cli/Cargo.toml dist-workspace.toml
git commit -m "refactor(release): cli-skills-2 source skills from spur-cli"
```

---

### Task 3: Update Live Instructions and Complete the Reference Audit

**Task ID:** `cli-skills-3`

**Files:**

- Modify: `AGENTS.md:18-29`
- Modify: `CLAUDE.md:1-20`
- Modify: `crates/spur-cli/assets/skills/explainer-video-editor/SKILL.md:66`
- Modify: `crates/spur-cli/assets/skills/explainer-video-editor/references/handoff-contract.md:153`

**Depends on:** `cli-skills-2`

**Acceptance Criteria:**

- [ ] Live repository instructions point to `crates/spur-cli/assets/skills`.
- [ ] Live explainer-video validation commands use the moved script path.
- [ ] Every remaining `assets/skills` occurrence is classified as generic runtime/config behavior, a temporary fixture, installed-layout compatibility, or immutable historical documentation.
- [ ] No live instruction points to the removed repository-root corpus.
- [ ] Formatting, focused core tests, xtask tests, and Cargo package listing pass.

**Suggested Worker:** `codex` — this is a bounded documentation and verification audit after the implementation tasks land.

**Scope Boundary:**

- IN scope: four live documentation files, repository-wide path audit, final prescribed verification.
- OUT of scope: rewriting historical specs/plans, renaming generic config examples, changing code or release behavior already owned by tasks 1 and 2.
- If an incorrect live code reference remains, emit `scope_drift` and identify which prior task owns it instead of editing outside scope.

**Scope Drift Checkpoint:**

- If a remaining path cannot be confidently classified, emit `risk` with file, line, and intended interpretation.
- Do not mechanically replace all matches; generic resolver fixtures must remain generic.

**Implementation:**

- [ ] **Step 1: Capture the failing live-reference audit**

Run:

```bash
rg -n 'assets/skills' AGENTS.md CLAUDE.md crates/spur-cli/assets/skills/explainer-video-editor
```

Expected: output includes live instructions that still name repository-root `assets/skills`.

- [ ] **Step 2: Update only live source-path instructions**

Use `crates/spur-cli/assets/skills/skills-catalog/SKILL.md` in `AGENTS.md` and `CLAUDE.md`. Change both explainer-video command examples to:

```bash
crates/spur-cli/assets/skills/explainer-video-editor/scripts/validate-delivery.sh MANIFEST.json VIDEO.mp4
```

Do not rewrite historical creation logs or old approved plans merely because they describe the former layout.

- [ ] **Step 3: Audit every remaining occurrence by meaning**

Run:

```bash
rg -n 'assets/skills' --glob '!target/**' --glob '!.git/**'
```

Review every result. Allowed remaining categories are:

- `crates/spur-cli/assets/skills` — the new source path;
- `share/spur/skills` — the stable installed path;
- `<repo>/assets/skills`, temp paths, or config examples — generic resolver behavior;
- dated documents under `docs/superpowers/specs`, `docs/superpowers/plans`, or historical creation logs — historical record.

Any other repository-root source instruction is a failure and must be reported to the owning task.

- [ ] **Step 4: Run final verification**

Run:

```bash
scripts/spur-cargo fmt --all -- --check
scripts/spur-cargo test -p spur-core skills::tests -- --nocapture
scripts/spur-cargo test -p spur-core --test skills_catalog_mcp
scripts/spur-cargo test -p xtask
scripts/spur-cargo package -p spur-cli --list
git diff --check
```

Expected: every command exits 0. Confirm the package list contains the complete `assets/skills/**` subtree relative to the `spur-cli` crate.

- [ ] **Step 5: Commit the live instruction updates**

```bash
git add AGENTS.md CLAUDE.md crates/spur-cli/assets/skills/explainer-video-editor/SKILL.md crates/spur-cli/assets/skills/explainer-video-editor/references/handoff-contract.md
git commit -m "docs(skills): cli-skills-3 point live guidance at spur-cli"
```

---

## Plan Acceptance Checklist

- [ ] The approved notebook's source ownership, unchanged precedence, installed-layout, packaging, migration, verification, and non-goal requirements each map to at least one task.
- [ ] Task IDs are unique and dependency references form an acyclic graph.
- [ ] No two concurrently eligible tasks modify the same file.
- [ ] Every task has a worker route, scope boundary, acceptance criteria, failing check, passing check, and commit command.
- [ ] The plan never invokes bare `cargo`; all Rust commands use `scripts/spur-cargo`.
- [ ] No task embeds skills, changes npm's two-download model, or consolidates release artifacts.
