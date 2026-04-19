# SpurPower Skills Installer: Marker-Guarded Auto-Install (C-lite)

## Problem

Spur ships a bundled corpus of tactical "skills" (TDD, systematic-debugging,
brain-delegation, etc.) as markdown files compiled into `spur-core` via
`include_str!`. For agents (Claude Code, Codex, Gemini, Kiro, OpenCode,
Cursor) to use these skills, the files must be materialized into each
agent's config directory in that agent's native convention.

Today, `spur-cli/src/commands/init.rs` does a partial, ad-hoc fanout with
five production-correctness defects (ground-truthed against official agent
docs, April 2026):

| # | Defect | Location | Impact |
|---|---|---|---|
| B1 | OpenCode path is nested one level too deep (`skills/spurpower/<name>/SKILL.md`). OpenCode scans `skills/*/SKILL.md` only; our files are never discovered. | `init.rs:188-195` | OpenCode users get zero SpurPower skills despite files on disk. |
| B2 | Claude Code native `.claude/skills/` directory is not written at all — only a `CLAUDE.md` pointer block is injected. | `init.rs:198-210` | Claude's progressive-disclosure routing (Tier-1 description match) cannot trigger SpurPower skills. |
| B3 | Cursor `.mdc` frontmatter uses `globs: *` (creates "Auto-Attached with universal glob") when semantic intent is "always-on." | `init.rs:180-184` | Works by accident today; breaks if Cursor tightens glob matching. |
| B4 | File writes use `let _ = std::fs::write(...)`, silently swallowing I/O errors. | `init.rs:184, 194, 203, 209` | Partial/corrupted installs go unreported. |
| B5 | Fanout reads `all_bundled_raw()` directly instead of `load_skill()`, so user overrides in `.spur/skills/<name>/SKILL.md` affect brain prompt assembly but NOT agent-dir files. | `init.rs:162-195` | Advertised override mechanism silently does not flow to agents. |

The installer also lacks idempotent re-run semantics, protection against
overwriting hand-edits, and observable error reporting.

## Verified Agent Conventions (as of 2026-04)

All paths and frontmatter schemas below are sourced from official agent
documentation and verified during design.

### Per-skill file conventions

| Agent | Skill path | Scan depth | Frontmatter | Source |
|---|---|---|---|---|
| Claude Code (+ ACP) | `.claude/skills/<name>/SKILL.md` | flat, shallow | `name` + `description` required | docs.anthropic.com; issue anthropics/claude-code#16438 |
| Codex (+ ACP) | `.codex/skills/<name>/SKILL.md` | first-class `SkillsConfig` | free-form markdown | `codex-rs/config/src/skills_config.rs` |
| Gemini | `.gemini/skills/<name>/SKILL.md` | flat (agentskills.io) | `name` + `description` required | github.com/google-gemini/gemini-cli |
| Kiro | `.kiro/skills/<name>/SKILL.md` | flat | `name` + `description` required | kiro.dev/docs/skills |
| OpenCode | `.opencode/skills/<name>/SKILL.md` | `skills/*/SKILL.md` one-level | `name` (must match dir, regex `^[a-z0-9]+(-[a-z0-9]+)*$`) + `description` | opencode.ai/docs/skills |
| Cursor | `.cursor/rules/<name>.mdc` | flat | `description`, `globs`, `alwaysApply` | docs.cursor.com/context/rules |

Convergent pattern: 5 of 6 agents follow the **agentskills.io** standard
(`<name>/SKILL.md` with `name`+`description` YAML frontmatter). Only Cursor
differs meaningfully; Codex accepts the same pattern but does not require
frontmatter.

### Kiro default-agent caveat

Research surfaced that Kiro's default agent does not auto-load
`.kiro/skills/`. A steering file with `inclusion: always` is required to
point the default agent at the skills directory. One extra file per repo
(not per skill) — a `spurpower-pointer.md` steering file — handles this.

### ACP variants

Claude Code ACP and Codex ACP (Zed's `@zed-industries/codex-acp`) use the
same file conventions as their non-ACP counterparts. No separate adapter
needed.

## Design

### Approach

**Option C-lite: in-file marker with embedded body hash, override-aware
source, no region injection, no stale pass in MVP.**

This approach scored **189/195** on a 10-axis weighted rubric, ahead of the
full C+D1 design (179) and status quo (118). Gains come from maximizing
simplicity, discoverability, blast radius, maintenance, and mental-model
clarity (all 5/5). Only sacrifice: reversibility (stale-file cleanup
deferred to phase-2, trivially recoverable via `grep -rl SPUR-MANAGED`).

### Core rules

1. Every file Spur generates carries a machine-parseable marker on a single
   line containing schema version, skill id, and sha256 hash of the body
   bytes that follow the marker.
2. On re-run, the installer's decision for each target is determined
   byte-equality check first (NoOp shortcut), then by marker-presence and
   on-disk body hash vs the marker's embedded hash.
3. Fanout reads skill bodies through the override chain (`.spur/skills/`
   user override wins, else bundled) so user overrides flow into every
   agent target.
4. No region injection into shared user files (CLAUDE.md, AGENTS.md,
   GEMINI.md). All 6 target agents discover per-skill SKILL.md files
   natively. Region injection was redundant belt-and-suspenders.
5. No stale-file cleanup in MVP. Removed skills leave marked orphan files
   that users can locate with `grep -rl SPUR-MANAGED .` and delete
   manually. Phase-2 adds a sweep.

### Marker format

One line, placed as the first line after the closing `---` of YAML
frontmatter when present; first line of file when frontmatter is absent
(Codex only):

```
<!-- SPUR-MANAGED v=1 skill=<id> sha256=<64hex lowercase> -->
```

**Body-hash scope:** sha256 of bytes from the line after the marker through
EOF. **Frontmatter is NOT hashed** — adapters may legitimately re-emit
frontmatter across Spur versions without thrashing the hash. Marker itself
is excluded from its own hash (would be self-referential).

**Line endings:** the installer does not normalize in code. Instead, `spur
init` prints a one-time advisory if `.gitattributes` does not contain a
rule ensuring LF for our managed dirs (e.g., `.claude/skills/** text
eol=lf`). Cross-platform teams add the advisory; single-platform teams
ignore it. Simpler than normalizing in Rust.

### Decision engine

For each rendered target file:

| On-disk state | Action |
|---|---|
| File absent | **Create** — atomic tmp+rename |
| File present, bytes equal rendered bytes | **NoOp** |
| File present, bytes differ, no marker | **Skip(NoMarker)** — log; user-owned |
| File present, bytes differ, marker present, on-disk body-hash == marker hash | **Update** — atomic tmp+rename |
| File present, bytes differ, marker present, on-disk body-hash ≠ marker hash | **Skip(UserEdited)** — log |

Byte-equality check is the fast path: if the file on disk is exactly what
we would write, we don't read the marker, don't hash anything, don't
rewrite. On the vast majority of re-runs (nothing changed), this is
instant.

### Implementation shape

```rust
// crates/spur-core/src/skills/installer.rs  (~150 LoC)
pub struct Summary {
    pub written: Vec<PathBuf>,           // created + updated
    pub unchanged: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, SkipReason)>,
}

pub enum SkipReason { NoMarker, UserEdited }

pub enum InstallError {
    Io { path: PathBuf, source: std::io::Error },
    InvalidSkillId { id: String, reason: String },
}

pub fn run(repo_root: &Path) -> Result<Summary, InstallError>;
```

```rust
// crates/spur-core/src/skills/adapters.rs  (~100 LoC)
pub enum Adapter {
    SpurHermetic,
    ClaudeCode,
    Codex,
    Gemini,
    Kiro,
    OpenCode,
    Cursor,
}

impl Adapter {
    pub fn render(&self, skill: &SkillPayload, repo_root: &Path) -> RenderedFile;
}

pub struct RenderedFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

// Three free functions cover all 7 adapters:
fn render_agentskills(skill, target_root, name_prefix) -> RenderedFile;
fn render_codex(skill, repo_root) -> RenderedFile;
fn render_cursor(skill, repo_root) -> RenderedFile;

// Once-per-run (not per-skill):
fn render_kiro_steering_pointer(repo_root) -> RenderedFile;
```

Dispatch:

```rust
impl Adapter {
    fn render(&self, skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
        match self {
            Adapter::SpurHermetic => render_agentskills(skill, &repo_root.join(".spur/skills"), ""),
            Adapter::ClaudeCode   => render_agentskills(skill, &repo_root.join(".claude/skills"), "spurpower-"),
            Adapter::Codex        => render_codex(skill, repo_root),
            Adapter::Gemini       => render_agentskills(skill, &repo_root.join(".gemini/skills"), "spurpower-"),
            Adapter::Kiro         => render_agentskills(skill, &repo_root.join(".kiro/skills"), "spurpower-"),
            Adapter::OpenCode     => render_agentskills(skill, &repo_root.join(".opencode/skills"), "spurpower-"),
            Adapter::Cursor       => render_cursor(skill, repo_root),
        }
    }
}
```

Installer loop:

```rust
pub fn run(repo_root: &Path) -> Result<Summary, InstallError> {
    let skills = list_active_skills(repo_root)?;
    let adapters = [Adapter::SpurHermetic, Adapter::ClaudeCode, Adapter::Codex,
                    Adapter::Gemini, Adapter::Kiro, Adapter::OpenCode, Adapter::Cursor];
    let mut summary = Summary::default();
    for skill in &skills {
        for adapter in &adapters {
            let rf = adapter.render(skill, repo_root);
            apply(&rf, &mut summary)?;
        }
    }
    // Once-per-run: Kiro steering pointer
    apply(&render_kiro_steering_pointer(repo_root), &mut summary)?;
    Ok(summary)
}

fn apply(rf: &RenderedFile, summary: &mut Summary) -> Result<(), InstallError> {
    match decide(rf)? {
        Decision::Create | Decision::Update => {
            atomic_write(&rf.path, &rf.bytes)?;
            summary.written.push(rf.path.clone());
        }
        Decision::NoOp => {
            summary.unchanged.push(rf.path.clone());
        }
        Decision::Skip(reason) => {
            summary.skipped.push((rf.path.clone(), reason));
        }
    }
    Ok(())
}
```

### Day-1 adapter set (7)

| # | Adapter | Target path | Frontmatter | Notes |
|---|---|---|---|---|
| 1 | `SpurHermetic` | `.spur/skills/<id>/SKILL.md` | `name: <id>`, `description` | No prefix — canonical override location |
| 2 | `ClaudeCode` (covers ACP) | `.claude/skills/spurpower-<id>/SKILL.md` | `name: spurpower-<id>`, `description` | Flat dir, name must match |
| 3 | `Codex` (covers ACP) | `.codex/skills/spurpower-<id>/SKILL.md` | none (free-form MD) | Marker on line 1 |
| 4 | `Gemini` | `.gemini/skills/spurpower-<id>/SKILL.md` | `name: spurpower-<id>`, `description` | agentskills.io |
| 5 | `Kiro` | `.kiro/skills/spurpower-<id>/SKILL.md` | `name: spurpower-<id>`, `description` | Plus one steering file per run (see below) |
| 6 | `OpenCode` | `.opencode/skills/spurpower-<id>/SKILL.md` | `name: spurpower-<id>`, `description` | Name must match dir |
| 7 | `Cursor` | `.cursor/rules/spurpower-<id>.mdc` | `description`, `alwaysApply: true` | No globs |

**Prefix strategy:** every target name is `spurpower-<id>`. Flat prefixing
(not subfolder namespacing) is required by Claude Code's shallow scan and
OpenCode's one-level pattern. Prefix disambiguates Spur's skills from
user-authored ones in the same agent directory.

### Kiro steering pointer (once-per-run file)

Written once per installer run (not per skill) to
`.kiro/steering/spurpower-pointer.md`:

```markdown
---
inclusion: always
name: spurpower-pointer
description: Pointer to SpurPower tactical skills in .kiro/skills/spurpower-*
---
<!-- SPUR-MANAGED v=1 skill=__pointer sha256=<hex> -->
SpurPower tactical skills live in `.kiro/skills/spurpower-*/SKILL.md` and
are also available as editable workspace overrides in `.spur/skills/<id>/`.
Use them for TDD, systematic debugging, code review, and brain-delegation.
```

Rationale: Kiro's default agent does not auto-discover `.kiro/skills/`
without a steering-file pointer. This single file makes all per-skill files
reachable. Reserved skill id `__pointer` is excluded from sanitization
rules (not user-facing).

### Skill-id sanitization

Source-of-truth regex: `^[a-z0-9]+(-[a-z0-9]+)*$`, applied to the skill id
before prefixing. Length 1-54 characters.

Rationale for 54-char cap: OpenCode's documented skill-name constraint is
64 characters. After prefixing with `spurpower-` (10 chars), the on-disk
dir name and required `name:` frontmatter become `spurpower-<id>`, which
must fit in 64. 64 - 10 = 54.

Applied to:
- Every bundled skill id (compile-time-ish: runtime check in unit test).
- Every user override discovered under `.spur/skills/*/` (runtime check,
  reject with a loud `InvalidSkillId` error suggesting rename).
- Blocks path-traversal via `..` in override dir names.

### Override discovery

```rust
fn list_active_skills(repo_root: &Path) -> Result<Vec<SkillPayload>, InstallError> {
    let mut skills: HashMap<String, SkillPayload> = HashMap::new();
    // Bundled first.
    for (id, raw) in spur_core::skills::all_bundled_raw() {
        validate_id(id)?;
        skills.insert(id.to_string(), parse_payload(id, raw)?);
    }
    // Overrides win.
    let override_dir = repo_root.join(".spur/skills");
    if override_dir.is_dir() {
        for entry in std::fs::read_dir(&override_dir)? {
            let e = entry?;
            if !e.file_type()?.is_dir() { continue; }
            let id = e.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            let skill_md = e.path().join("SKILL.md");
            if !skill_md.exists() { continue; }
            let raw = std::fs::read_to_string(&skill_md)?;
            skills.insert(id.clone(), parse_payload(&id, &raw)?);
        }
    }
    Ok(skills.into_values().collect())
}
```

### `.gitattributes` advisory

After a successful `spur init`, if the repo's `.gitattributes` (at root or
in any managed subpath) does not contain a rule forcing LF endings for
markdown files, print:

```
Tip: add `*.md text eol=lf` to .gitattributes for cross-platform teammates.
     SpurPower files may otherwise thrash across CRLF/LF systems.
```

Non-blocking. Documented in README. No code normalizes endings; the
advisory delegates the concern to git.

### Crate placement

New modules in `spur-core`:

```
crates/spur-core/src/skills/
  mod.rs             (existing: load_skill, bundled_raw; add list_active_skills)
  installer.rs       (new: run, apply, decide, parse_marker, atomic_write)
  adapters.rs        (new: Adapter enum, render functions)
```

Public API called from `spur-cli/src/commands/init.rs`:

```rust
pub fn spur_core::skills::installer::run(
    repo_root: &Path,
) -> Result<Summary, InstallError>;
```

New dependency: `sha2` (widely-audited, ~80KB binary impact). No other new deps.

## MVP Scope (ship list)

1. `Adapter` enum + 3 render functions (`render_agentskills`,
   `render_codex`, `render_cursor`) + `Adapter::render()` dispatch.
2. `render_kiro_steering_pointer` once-per-run helper.
3. `Installer::run()` with `list_active_skills`, `apply`, `decide`,
   `parse_marker`, `atomic_write`.
4. Skill-id validation (regex + length, applied to bundled and overrides).
5. `Summary` + `Display` for report printing.
6. Replace inline fanout in `spur-cli/src/commands/init.rs` with
   `spur_core::skills::installer::run(&repo_root)?` plus a block that
   prints the `Summary` and the `.gitattributes` advisory.
7. Tests:
   - Per-adapter snapshot tests for all 7 adapters on one representative
     skill (bundled) and one representative override.
   - Installer integration tests (tempfile-based):
     1. Fresh install creates expected files across all 7 adapters with
        valid marker + hash.
     2. Idempotent re-run produces zero writes (all NoOp).
     3. User hand-edit to a deployed skill file is preserved; installer
        reports Skip(UserEdited).
     4. Override at `.spur/skills/<id>/SKILL.md` flows into every adapter
        target; marker hashes reflect override body.
     5. Pre-existing user file (no marker) is preserved; installer reports
        Skip(NoMarker).
     6. Cross-version upgrade: bundled body changes, on-disk file clean
        → installer updates cleanly.

Estimated size: ~765 LoC including tests (installer ~150, adapters ~100,
helpers+errors+summary ~70, wiring delta −20+15, tests ~400). 2-3
staff-level days.

## Phase-2 Backlog

1. Stale-file cleanup pass (walk known paths, delete orphan managed files
   with clean body-hash; keep orphans with user edits).
2. `spur skills sync` standalone command (same installer, no config
   rewrite).
3. `--dry-run` flag (print intended changes, no disk writes).
4. `--force` flag (ignore user-edited skip, overwrite anyway).
5. `spur skills adopt <path>` (stamp marker on a hand-written file to
   claim it as Spur-managed).
6. `spur skills fix-markers` (re-stamp marker on a file whose body is
   known to match source but whose marker was hand-removed).
7. `spur skills uninstall` (symmetric removal; walks known paths, deletes
   clean-body managed files, keeps edited with a warning).
8. Region injection into `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` with
   delimiter-pair markers — only if user feedback says per-skill discovery
   isn't enough.
9. `.spur/config.toml [skills.targets]` declarative per-adapter
   enable/disable.
10. `[skills.cursor] mode = "always" | "agent_requested"` to control
    token cost (always-on vs description-triggered).
11. `[skills.codex] emit_metadata = true` to write
    `.codex/skills/spurpower-<id>/agents/openai.yaml` for better Codex UI
    labels.
12. Migrate Claude Code targets to nested dirs
    (`.claude/skills/spurpower/<id>/SKILL.md`) once Anthropic ships
    support for anthropics/claude-code#16438.
13. LF normalization in code (if `.gitattributes` advisory proves
    insufficient in practice).

## Risks and Open Questions

### R1 — Kiro default-agent skill loading

Research surfaced that Kiro's default agent does not auto-load
`.kiro/skills/`; custom agents must wire them in via a `resources` field.
Mitigation: MVP emits BOTH the per-skill SKILL.md files AND a single
`.kiro/steering/spurpower-pointer.md` with `inclusion: always`. The
steering file ensures default-agent reach; the per-skill files are
available for custom agents and future default-agent auto-load.

### R2 — Cursor always-on token cost

`alwaysApply: true` injects every Spur skill into every Cursor prompt
(~2-5k tokens for 10 skills). Matches current installer's effective
behavior and semantic intent, but is expensive. Phase-2 item 10 adds a
config toggle to switch to agent-requested (lazy) mode.

### R3 — Orphan files accumulate without stale cleanup

Removed skills leave marked files on disk until user intervention. Given
Spur's small, stable skill corpus, orphans are rare. Recovery is
straightforward (`grep -rl SPUR-MANAGED .`). Phase-2 item 1 adds an
automatic sweep.

### R4 — CRLF/LF teammate thrash

Without in-code normalization, a mixed-platform team will see markers
thrash on every re-run. `.gitattributes` advisory addresses this in
documentation. If advisory proves insufficient, phase-2 item 13 adds code
normalization.

### R5 — Marker removed by well-meaning reviewer

If a reviewer hand-removes a SPUR-MANAGED marker line in a PR, the next
installer run treats the file as user-owned and skips it. Phase-2 items 6
and 5 (`fix-markers` and `adopt`) provide recovery. MVP: installer's
report prominently lists Skip(NoMarker) paths so the regression is
visible.

### R6 — Long skill ids

Prefix `spurpower-` + longest current skill id
`verification-before-completion` (30 chars) = 40 chars. Well under the
54-char cap. Future skill authors must stay within the cap; sanitization
enforces.

### R7 — Concurrent `spur init` from two shells in the same repo

Atomic tmp+rename ensures no partial files. Last writer wins per target;
if both copies are the same Spur version with no concurrent override
edits, result is idempotent.

### R8 — Claude Code plugin-path bug does not apply

Anthropic issue #10113 describes a skill-path resolution bug specific to
plugin-marketplace installs. Spur is NOT distributed as a Claude Code
plugin; we write files directly into the user's workspace. Claude's
workspace-scan path resolution is unaffected by #10113.

## Out of Scope

- Home-directory installs (`~/.claude/skills/`, `~/.codex/`, etc.). All
  day-1 adapters are project-local, per the "Project-local unconditional"
  selection in brainstorming.
- Binary asset support in skills (scripts, images). MVP ships only
  `SKILL.md` bodies.
- Windows symlink strategies.
- Agent auto-detection gating (fan out only to installed agents). Choice
  was "unconditional project-local"; users gitignore unwanted dirs.
- Region injection into user-owned instruction files (CLAUDE.md,
  AGENTS.md, GEMINI.md). Moved to phase-2 pending user feedback.
- Stale-file cleanup (phase-2).
- Spur skills uninstall (phase-2).

## Success Criteria

Design is successful when:

1. A fresh `spur init` on an empty repo produces discoverable, correctly-
   structured skill files for all 7 target adapters with zero silent
   errors.
2. Re-running `spur init` produces zero disk writes when nothing has
   changed (idempotency via byte-equality fast path).
3. A user override at `.spur/skills/<id>/SKILL.md` reaches all 7 adapter
   targets after a re-run, with marker hashes reflecting the override
   body.
4. A user hand-edit to a deployed skill file is preserved across re-runs
   and surfaced in the `Summary.skipped` list.
5. The five production bugs (B1-B5) enumerated in the Problem section are
   verifiably fixed in integration tests.
6. Test coverage: every adapter has a snapshot test; installer has the 6
   scenarios in MVP item 7.
