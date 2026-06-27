# Skill Assets Split Design

**Decision date:** 2026-06-27
**Target area:** `crates/spur-core/src/skills`, bundled skill corpus

## Goal

Move SPUR's bundled skill markdown out of `spur-core` Rust source so skill text
can change without recompiling the core orchestrator crate.

`spur-core` should own the lightweight behavior around skills: id validation,
frontmatter parsing, override precedence, active-catalog resolution, and
installer rendering. The skill corpus itself should be treated as product
assets, not Rust implementation.

## Problem

The current bundled skill corpus lives under `crates/spur-core/src/skills/`.
`mod.rs` builds a static map with `include_str!` for every `SKILL.md` file.
That makes markdown wording part of the `spur-core` compile graph.

This creates unnecessary friction:

- Editing a skill invalidates Rust compilation even when no Rust behavior
  changed.
- Content review and code review are coupled.
- Tests in `spur-core` assert many content details, so corpus edits feel like
  core behavior changes.
- The source tree makes large prompt assets look like core orchestration logic.

The runtime behavior is useful and should remain: project overrides in
`.spur/skills/<id>/SKILL.md` win, managed generated files are ignored as
overrides, and `spur init` renders active skills into each agent's native
directory.

## Chosen Approach

Use a full asset split.

Move bundled skill directories to a repository-level asset tree:

```text
assets/skills/<skill-id>/SKILL.md
assets/skills/<skill-id>/...
```

`spur-core` keeps a filesystem-backed skill catalog API. Callers continue to
ask for a named skill or the active skill list; they do not care whether the
content came from a project override, configured asset directory, or packaged
asset directory.

No skill markdown should be embedded into `spur-core` with `include_str!`.

## Resolution Order

Skill lookup uses this order:

1. Project override:
   `.spur/skills/<id>/SKILL.md`
2. Configured bundled asset directory:
   environment or config points at a skill asset root
3. Installed/package asset directory:
   a path resolved relative to the installed SPUR binary or distribution
4. Workspace development fallback:
   `<repo-root>/assets/skills`

Project overrides keep the current safety rules:

- If the override file is a SPUR-managed generated file whose marker hash still
  matches its body, ignore it and fall back to bundled content.
- If the override file is a legacy generated SPUR file, ignore it and fall back
  to bundled content.
- If the override file is user-edited, it wins.

Unknown skill ids return `None` for single-skill lookup and are absent from the
active bundled catalog.

## Core API Shape

Keep the existing public functions initially:

```rust
pub fn load_skill(name: &str, repo_root: &Path) -> Option<String>;
pub fn list_active_skills(repo_root: &Path) -> Result<Vec<SkillPayload>, InvalidSkillId>;
```

Internally, introduce a small catalog/resolver layer:

```rust
pub struct SkillCatalog {
    bundled_root: PathBuf,
}

impl SkillCatalog {
    pub fn discover(repo_root: &Path) -> Result<Self, SkillCatalogError>;
    pub fn load_raw(&self, id: &str, repo_root: &Path) -> Result<Option<String>, SkillCatalogError>;
    pub fn list_raw(&self, repo_root: &Path) -> Result<Vec<(String, String)>, SkillCatalogError>;
}
```

The exact type names can change during implementation, but the responsibilities
should stay narrow:

- Discover where bundled assets live.
- Enumerate skill directories with `SKILL.md`.
- Validate ids with the existing `validate_id`.
- Parse frontmatter with the existing parser.
- Preserve deterministic output order for installer rendering and tests.

The prompt path in `Orchestrator::build_brain_prompt_v1` should still call
`load_skill`. The installer should still call `list_active_skills`.

## Asset Layout

The asset tree should preserve each skill's existing directory:

```text
assets/skills/brain-delegation/SKILL.md
assets/skills/brainstorming/SKILL.md
assets/skills/brainstorming/scripts/...
assets/skills/open-design/references/...
```

Only `SKILL.md` files participate in the active catalog. Supporting files remain
available relative to their skill directory for agents that use progressive
disclosure.

The existing deprecated alias should be preserved:

- `brain-delegation-claude-code-acp`
- `brain-delegation-claude-code`

The implementation can preserve this by either a small alias table in Rust or a
manifest entry in the asset tree. Prefer a tiny manifest if it helps avoid
hardcoded corpus knowledge in `spur-core`.

## Configuration And Packaging

Development should work from the repository checkout with no extra setup:

- Running from the SPUR repo uses `assets/skills`.
- Tests can point the resolver at a temp asset directory.

Packaged SPUR must have an explicit bundled asset location:

- The distribution includes the `assets/skills` tree under a stable package
  path, such as `share/spur/skills`.
- The resolver reports which bundled asset root it selected.
- If no bundled asset root is available, errors should be actionable and name
  the checked paths plus the override/config option.

Configuration precedence for the bundled root should be:

1. `SPUR_SKILLS_DIR`, for local development and emergency repair.
2. `[skills].bundled_dir` in the layered SPUR config model
   (`~/.spur/config.toml`, then `<repo>/.spur/config.toml`).
3. Installed package-relative path.
4. Workspace-relative fallback.

The new config section should be explicit and small:

```toml
[skills]
bundled_dir = "/path/to/assets/skills"
```

`SPUR_SKILLS_DIR` wins over config because it is the easiest way to test local
asset edits or repair an installed package without writing config.

## Error Handling

Single prompt lookup should remain resilient:

- If a project override cannot be read, fall back to bundled content.
- If bundled content cannot be found for that id, return `None`.

Catalog listing for installer use should be stricter:

- Invalid bundled skill ids are errors.
- Invalid override ids are errors, matching current behavior.
- Missing or unreadable bundled asset root is an error when running `spur init`,
  because the installer cannot render a complete active catalog.

Errors should include:

- The selected or attempted asset root.
- The skill id when applicable.
- Whether the failure came from override resolution, bundled asset discovery, or
  frontmatter parsing.

## Testing

Focused tests should replace content-heavy core assertions:

- Asset catalog lists every `assets/skills/*/SKILL.md` deterministically.
- Every bundled skill id passes `validate_id`.
- Every bundled `SKILL.md` parses frontmatter and has a description.
- `load_skill` returns bundled content when no override exists.
- Project override still wins.
- Managed generated override files are ignored.
- Legacy generated override files are ignored.
- Invalid override directory names still fail `list_active_skills`.
- The Claude Code deprecated alias resolves to the ACP skill body.
- Installer rendering still receives the same active skills through
  `list_active_skills`.

Content-specific assertions should be reduced to a small number of smoke tests
for essential product invariants. Detailed skill wording belongs in asset review,
not in core orchestrator unit tests.

## Migration Plan Boundaries

Implementation should be split into narrow tasks:

- Add filesystem catalog tests against temp skill directories.
- Add the catalog resolver while keeping public APIs stable.
- Move skill directories from `crates/spur-core/src/skills/` to
  `assets/skills/`.
- Replace `include_str!` map construction with filesystem enumeration.
- Update tests to assert resolver and installer behavior instead of broad
  markdown text details.
- Update packaging or install scripts to include the skill asset tree.
- Run targeted `spur-core` tests, then broader workspace checks through
  `scripts/spur-cargo`.

## Acceptance Criteria

- `crates/spur-core/src/skills/` contains Rust logic only, not the bundled skill
  markdown corpus.
- Editing `assets/skills/<id>/SKILL.md` does not require recompiling
  `spur-core` because of Rust source embedding.
- Brain prompt construction still loads `brain-delegation` and per-agent
  delegation skills.
- `spur init` still renders active skills into the existing adapter targets.
- User overrides in `.spur/skills` keep current precedence and safety behavior.
- Packaged SPUR can find bundled assets without relying on the current working
  directory.
- Missing asset roots fail with clear diagnostics for installer workflows.
