# SpurPower Skills Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a marker-guarded, override-aware skills installer in `spur-core` that materializes Spur's bundled skill corpus into 7 agent config directories (`.spur`, `.claude`, `.codex`, `.gemini`, `.kiro`, `.opencode`, `.cursor`) with idempotent re-runs and user-edit preservation.

**Architecture:** A single public entry point `spur_core::skills::installer::run(repo_root)` resolves the active skill set (bundled ∪ `.spur/skills/` overrides), then iterates (skill × adapter) to render target files. Each rendered file carries a single-line `<!-- SPUR-MANAGED v=1 skill=<id> sha256=<hex> -->` marker whose embedded hash is compared against the on-disk body to decide Create/Update/NoOp/Skip. An `enum Adapter` with 3 render helper functions covers all 7 targets; the `spur-cli init` command replaces its current ad-hoc fanout with a single call.

**Tech Stack:** Rust 1.88 (edition 2021), `sha2 = "0.10"` (new dep in spur-core), `regex = "1"` (new dep in spur-core), `tempfile = "3"` (already in dev-deps), `thiserror` (already in workspace), `tokio` (existing; not required by installer itself — synchronous design).

**Spec:** [`docs/superpowers/specs/2026-04-19-skills-installer-design.md`](../specs/2026-04-19-skills-installer-design.md)

---

## File Structure

Files to create:

- `crates/spur-core/src/skills/installer.rs` — public `run()` entry, `Summary`, `SkipReason`, `InstallError`, `Decision` enum, `parse_marker`, `atomic_write`, `decide`, `apply` (~180 LoC + tests)
- `crates/spur-core/src/skills/adapters.rs` — `Adapter` enum, `SkillPayload`, `RenderedFile`, `render_agentskills`, `render_codex`, `render_cursor`, `render_kiro_steering_pointer` (~140 LoC + snapshot tests)
- `crates/spur-core/src/skills/frontmatter.rs` — tiny helper to extract `description:` from a source SKILL.md file (~40 LoC + unit tests)
- `crates/spur-core/tests/skills_installer.rs` — 6 integration tests using `tempfile` (~350 LoC)

Files to modify:

- `crates/spur-core/Cargo.toml` — add `sha2` and `regex` to `[dependencies]`
- `crates/spur-core/src/skills/mod.rs` — add `pub mod installer;`, `pub mod adapters;`, `mod frontmatter;`, plus a new public `list_active_skills()` function
- `crates/spur-cli/src/commands/init.rs` — replace inline fanout (lines 158-210) with a single call to `spur_core::skills::installer::run` + `.gitattributes` advisory

---

## Task 1: Add sha2 and regex dependencies to spur-core

**Files:**
- Modify: `crates/spur-core/Cargo.toml`

- [ ] **Step 1: Add sha2 to workspace deps if not present**

Check `Cargo.toml` at workspace root for `sha2`. If absent, add to `[workspace.dependencies]`:

```toml
sha2 = "0.10"
regex = "1"
```

If already present in workspace, skip this step.

- [ ] **Step 2: Add sha2 and regex to spur-core's `[dependencies]`**

Edit `crates/spur-core/Cargo.toml`. In `[dependencies]`, add:

```toml
sha2 = "0.10"
regex = "1"
```

Place alphabetically between existing entries (after `rand = "0.8"` and before `spur-acp`).

- [ ] **Step 3: Verify build**

Run: `cargo build -p spur-core`
Expected: PASS (no errors; new crates compile).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/Cargo.toml Cargo.toml Cargo.lock
git commit -m "chore(spur-core): add sha2 and regex deps for skills installer"
```

---

## Task 2: Frontmatter parser helper

**Files:**
- Create: `crates/spur-core/src/skills/frontmatter.rs`
- Modify: `crates/spur-core/src/skills/mod.rs`

This extracts `description` from a source SKILL.md's YAML frontmatter, and returns the body after stripping frontmatter + any leading SPUR-MANAGED marker. Reused by both bundled-skill parsing and override-file parsing.

- [ ] **Step 1: Write the failing tests**

Create `crates/spur-core/src/skills/frontmatter.rs` with this content:

```rust
//! Parse `name` + `description` YAML frontmatter from a source SKILL.md,
//! and return the body with any leading SPUR-MANAGED marker stripped.
//!
//! Inputs come from two sources:
//! 1. Bundled SKILL.md files (have frontmatter, no marker).
//! 2. User-edited override files under `.spur/skills/<id>/SKILL.md`
//!    (have frontmatter AND a SPUR-MANAGED marker we wrote previously).
//!
//! Both cases flow through this parser.

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ParsedSource<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub body: &'a str,
}

pub(crate) fn parse_source(raw: &str) -> ParsedSource<'_> {
    // Strip `---\n<yaml>\n---\n` frontmatter if present.
    let (frontmatter, after_fm) = match raw.strip_prefix("---\n") {
        Some(rest) => match rest.find("\n---\n") {
            Some(idx) => (Some(&rest[..idx]), &rest[idx + 5..]),
            None => (None, raw),
        },
        None => (None, raw),
    };

    // Strip a leading SPUR-MANAGED marker line if present.
    let body = match after_fm.strip_prefix("<!-- SPUR-MANAGED ") {
        Some(rest) => match rest.find(" -->\n") {
            Some(idx) => &rest[idx + 5..],
            None => after_fm,
        },
        None => after_fm,
    };

    let mut name = None;
    let mut description = None;
    if let Some(fm) = frontmatter {
        for line in fm.lines() {
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(v.trim());
            } else if let Some(v) = line.strip_prefix("description:") {
                description = Some(v.trim());
            }
        }
    }

    ParsedSource { name, description, body }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_skill_with_frontmatter() {
        let raw = "---\nname: tdd\ndescription: Use for TDD\n---\nBody here\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description, Some("Use for TDD"));
        assert_eq!(p.body, "Body here\n");
    }

    #[test]
    fn parses_override_with_marker() {
        let raw = "---\nname: tdd\ndescription: Override desc\n---\n\
                   <!-- SPUR-MANAGED v=1 skill=tdd sha256=abc -->\n\
                   Body after marker\n";
        let p = parse_source(raw);
        assert_eq!(p.name, Some("tdd"));
        assert_eq!(p.description, Some("Override desc"));
        assert_eq!(p.body, "Body after marker\n");
    }

    #[test]
    fn no_frontmatter_returns_raw_body() {
        let raw = "just body\n";
        let p = parse_source(raw);
        assert_eq!(p.name, None);
        assert_eq!(p.description, None);
        assert_eq!(p.body, "just body\n");
    }

    #[test]
    fn empty_description_is_empty_string() {
        let raw = "---\ndescription:\n---\nbody";
        let p = parse_source(raw);
        assert_eq!(p.description, Some(""));
    }
}
```

- [ ] **Step 2: Register the module**

Edit `crates/spur-core/src/skills/mod.rs`. Add after the existing `use` lines (near line 8):

```rust
mod frontmatter;
```

- [ ] **Step 3: Run tests, verify they pass**

Run: `cargo test -p spur-core skills::frontmatter`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/frontmatter.rs crates/spur-core/src/skills/mod.rs
git commit -m "feat(spur-core): add SKILL.md frontmatter parser for installer"
```

---

## Task 3: Skill-id validation

**Files:**
- Modify: `crates/spur-core/src/skills/mod.rs`

Add `validate_id` and `InvalidSkillId` for reuse by installer + tests.

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-core/src/skills/mod.rs` inside the existing `#[cfg(test)] mod tests { ... }` block:

```rust
    #[test]
    fn validate_id_accepts_standard_names() {
        for ok in [
            "tdd",
            "test-driven-development",
            "a",
            "verification-before-completion",
        ] {
            assert!(validate_id(ok).is_ok(), "should accept {ok}");
        }
    }

    #[test]
    fn validate_id_rejects_bad_names() {
        for bad in [
            "",
            "Uppercase",
            "has space",
            "has_underscore",
            "trailing-",
            "-leading",
            "double--hyphen",
            "with/slash",
            "..",
            "../evil",
        ] {
            assert!(validate_id(bad).is_err(), "should reject {bad}");
        }
    }

    #[test]
    fn validate_id_enforces_length_cap() {
        let ok_54 = "a".to_string() + &"b".repeat(53);
        let too_long_55 = "a".to_string() + &"b".repeat(54);
        assert!(validate_id(&ok_54).is_ok());
        assert!(validate_id(&too_long_55).is_err());
    }
```

- [ ] **Step 2: Add the implementation above the test module**

In `crates/spur-core/src/skills/mod.rs`, add above the `#[cfg(test)]` block:

```rust
use std::sync::OnceLock;
// ... (keep existing imports)

static SKILL_ID_RE: OnceLock<regex::Regex> = OnceLock::new();

fn skill_id_regex() -> &'static regex::Regex {
    SKILL_ID_RE.get_or_init(|| {
        regex::Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)*$").expect("static regex")
    })
}

/// Error returned when a skill directory name violates the naming rules.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid skill id `{id}`: {reason}")]
pub struct InvalidSkillId {
    pub id: String,
    pub reason: &'static str,
}

/// Validate a skill id: regex `^[a-z0-9]+(-[a-z0-9]+)*$`, length 1..=54.
///
/// The 54-char cap is OpenCode's 64-char skill-name limit minus the
/// `spurpower-` 10-char prefix we add in adapter output.
pub fn validate_id(id: &str) -> Result<(), InvalidSkillId> {
    if id.is_empty() {
        return Err(InvalidSkillId { id: id.to_string(), reason: "empty" });
    }
    if id.len() > 54 {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "longer than 54 characters",
        });
    }
    if !skill_id_regex().is_match(id) {
        return Err(InvalidSkillId {
            id: id.to_string(),
            reason: "must match ^[a-z0-9]+(-[a-z0-9]+)*$",
        });
    }
    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::tests::validate_id`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/mod.rs
git commit -m "feat(spur-core): add validate_id for skill-name sanitization"
```

---

## Task 4: `list_active_skills` + `SkillPayload`

**Files:**
- Modify: `crates/spur-core/src/skills/mod.rs`

Unions bundled corpus with `.spur/skills/*/SKILL.md` overrides. Override wins when the id matches.

- [ ] **Step 1: Write the failing tests**

Append to the existing `#[cfg(test)] mod tests { ... }` in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn list_active_skills_returns_bundled_when_no_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let skills = list_active_skills(dir.path()).unwrap();
        let ids: Vec<&str> = skills.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"test-driven-development"));
        assert!(ids.contains(&"brain-delegation"));
        // All bundled entries should have non-empty body.
        for s in &skills {
            assert!(!s.body.is_empty(), "{}: empty body", s.id);
        }
    }

    #[test]
    fn list_active_skills_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/test-driven-development");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("SKILL.md"),
            "---\nname: test-driven-development\ndescription: MY OVERRIDE\n---\nMy body here\n",
        )
        .unwrap();

        let skills = list_active_skills(dir.path()).unwrap();
        let tdd = skills.iter().find(|s| s.id == "test-driven-development").unwrap();
        assert_eq!(tdd.description, "MY OVERRIDE");
        assert_eq!(tdd.body, "My body here\n");
        assert!(matches!(tdd.source, SkillSource::Override));
    }

    #[test]
    fn list_active_skills_rejects_invalid_override_id() {
        let dir = tempfile::tempdir().unwrap();
        let bad_override = dir.path().join(".spur/skills/Bad_Name");
        std::fs::create_dir_all(&bad_override).unwrap();
        std::fs::write(
            bad_override.join("SKILL.md"),
            "---\nname: bad\ndescription: x\n---\nbody",
        )
        .unwrap();

        let err = list_active_skills(dir.path()).unwrap_err();
        assert!(err.to_string().contains("Bad_Name"));
    }
```

- [ ] **Step 2: Write the implementation**

Above the `#[cfg(test)]` block in `crates/spur-core/src/skills/mod.rs`, add:

```rust
/// Where a skill's body came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    Bundled,
    Override,
}

/// A resolved skill ready for rendering across adapters.
#[derive(Debug, Clone)]
pub struct SkillPayload {
    pub id: String,
    pub description: String,
    pub body: String,
    pub source: SkillSource,
}

/// Resolve the active skill set: bundled corpus merged with
/// `.spur/skills/<id>/SKILL.md` overrides (override wins per id).
///
/// Validates every skill id (bundled and override) through `validate_id`.
pub fn list_active_skills(repo_root: &Path) -> Result<Vec<SkillPayload>, InvalidSkillId> {
    let mut by_id: std::collections::BTreeMap<String, SkillPayload> =
        std::collections::BTreeMap::new();

    // Bundled first.
    for (id, raw) in bundled_raw() {
        validate_id(id)?;
        let parsed = frontmatter::parse_source(raw);
        by_id.insert(
            id.to_string(),
            SkillPayload {
                id: id.to_string(),
                description: parsed.description.unwrap_or("").to_string(),
                body: parsed.body.to_string(),
                source: SkillSource::Bundled,
            },
        );
    }

    // Overrides.
    let override_dir = repo_root.join(".spur/skills");
    if override_dir.is_dir() {
        let entries = match std::fs::read_dir(&override_dir) {
            Ok(e) => e,
            Err(_) => return Ok(by_id.into_values().collect()),
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            validate_id(&id)?;
            let skill_md = entry.path().join("SKILL.md");
            let raw = match std::fs::read_to_string(&skill_md) {
                Ok(r) => r,
                Err(_) => continue, // no SKILL.md in that dir
            };
            let parsed = frontmatter::parse_source(&raw);
            by_id.insert(
                id.clone(),
                SkillPayload {
                    id,
                    description: parsed.description.unwrap_or("").to_string(),
                    body: parsed.body.to_string(),
                    source: SkillSource::Override,
                },
            );
        }
    }

    Ok(by_id.into_values().collect())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::tests::list_active_skills`
Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/mod.rs
git commit -m "feat(spur-core): add list_active_skills with override chain"
```

---

## Task 5: Marker type + render/parse

**Files:**
- Create: `crates/spur-core/src/skills/installer.rs`
- Modify: `crates/spur-core/src/skills/mod.rs`

Introduce the installer module with only the Marker types to start. More will be added in later tasks.

- [ ] **Step 1: Create installer.rs with Marker + tests**

Create `crates/spur-core/src/skills/installer.rs`:

```rust
//! Skills installer: renders bundled+override skills into per-adapter
//! agent dirs, protects user hand-edits via an in-file marker + sha256.

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// SPUR-MANAGED marker embedded in every file the installer writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    pub version: u8,
    pub skill_id: String,
    pub sha256: String, // lowercase hex, 64 chars
}

impl Marker {
    /// Render the marker as a single line including the trailing newline.
    pub fn render(&self) -> String {
        format!(
            "<!-- SPUR-MANAGED v={} skill={} sha256={} -->\n",
            self.version, self.skill_id, self.sha256
        )
    }
}

static MARKER_RE: OnceLock<regex::Regex> = OnceLock::new();

fn marker_regex() -> &'static regex::Regex {
    MARKER_RE.get_or_init(|| {
        regex::Regex::new(
            r"^<!-- SPUR-MANAGED v=(\d+) skill=(\S+) sha256=([0-9a-f]{64}) -->$",
        )
        .expect("static regex")
    })
}

/// Parse a marker line (without trailing newline) into its components.
pub(crate) fn parse_marker(line: &str) -> Option<Marker> {
    let caps = marker_regex().captures(line)?;
    Some(Marker {
        version: caps.get(1)?.as_str().parse().ok()?,
        skill_id: caps.get(2)?.as_str().to_string(),
        sha256: caps.get(3)?.as_str().to_string(),
    })
}

/// Lowercase hex sha256 of the given bytes.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrip() {
        let m = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: "a".repeat(64),
        };
        let rendered = m.render();
        let line = rendered.trim_end_matches('\n');
        let parsed = parse_marker(line).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn parse_marker_rejects_garbage() {
        assert!(parse_marker("not a marker").is_none());
        assert!(parse_marker("<!-- SPUR-MANAGED v=1 skill=x -->").is_none()); // no sha
        assert!(parse_marker(
            "<!-- SPUR-MANAGED v=1 skill=x sha256=ZZZ -->"
        ).is_none()); // bad hex
    }

    #[test]
    fn parse_marker_accepts_reserved_pointer_id() {
        // `__pointer` uses underscores: reserved for Kiro steering.
        let m = parse_marker(&format!(
            "<!-- SPUR-MANAGED v=1 skill=__pointer sha256={} -->",
            "b".repeat(64)
        ))
        .unwrap();
        assert_eq!(m.skill_id, "__pointer");
    }

    #[test]
    fn sha256_hex_format() {
        let h = sha256_hex(b"hello");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}
```

- [ ] **Step 2: Register the module**

In `crates/spur-core/src/skills/mod.rs`, add near the other `mod` lines:

```rust
pub mod installer;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::installer`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/installer.rs crates/spur-core/src/skills/mod.rs
git commit -m "feat(spur-core): add SPUR-MANAGED marker render + parse"
```

---

## Task 6: Adapter enum + SkillPayload + RenderedFile skeleton

**Files:**
- Create: `crates/spur-core/src/skills/adapters.rs`
- Modify: `crates/spur-core/src/skills/mod.rs`

Set up the Adapter enum and RenderedFile without render functions yet. Render functions land in tasks 7-10 to keep diffs small.

- [ ] **Step 1: Create adapters.rs**

Create `crates/spur-core/src/skills/adapters.rs`:

```rust
//! Per-agent skill file renderers. Each adapter knows its target path
//! and frontmatter schema; rendering is a pure function of SkillPayload.

use std::path::{Path, PathBuf};

use super::SkillPayload;

/// A file the installer intends to write.
#[derive(Debug, Clone)]
pub struct RenderedFile {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// Every per-skill file destination this installer knows about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// All 7 adapters in deterministic iteration order.
    pub fn all() -> &'static [Adapter] {
        &[
            Adapter::SpurHermetic,
            Adapter::ClaudeCode,
            Adapter::Codex,
            Adapter::Gemini,
            Adapter::Kiro,
            Adapter::OpenCode,
            Adapter::Cursor,
        ]
    }

    /// Render a single skill into this adapter's target path.
    pub fn render(&self, skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
        match self {
            Adapter::SpurHermetic => {
                render_agentskills(skill, &repo_root.join(".spur/skills"), "")
            }
            Adapter::ClaudeCode => {
                render_agentskills(skill, &repo_root.join(".claude/skills"), "spurpower-")
            }
            Adapter::Codex => render_codex(skill, repo_root),
            Adapter::Gemini => {
                render_agentskills(skill, &repo_root.join(".gemini/skills"), "spurpower-")
            }
            Adapter::Kiro => {
                render_agentskills(skill, &repo_root.join(".kiro/skills"), "spurpower-")
            }
            Adapter::OpenCode => {
                render_agentskills(skill, &repo_root.join(".opencode/skills"), "spurpower-")
            }
            Adapter::Cursor => render_cursor(skill, repo_root),
        }
    }
}

// --- render helpers land in tasks 7-10 ---

fn render_agentskills(
    _skill: &SkillPayload,
    _target_root: &Path,
    _name_prefix: &str,
) -> RenderedFile {
    unimplemented!("Task 7")
}

fn render_codex(_skill: &SkillPayload, _repo_root: &Path) -> RenderedFile {
    unimplemented!("Task 8")
}

fn render_cursor(_skill: &SkillPayload, _repo_root: &Path) -> RenderedFile {
    unimplemented!("Task 9")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_all_has_seven_unique() {
        let all = Adapter::all();
        assert_eq!(all.len(), 7);
        let mut seen = std::collections::HashSet::new();
        for a in all {
            assert!(seen.insert(*a), "{a:?} appears twice");
        }
    }
}
```

- [ ] **Step 2: Register the module and export**

In `crates/spur-core/src/skills/mod.rs`, add:

```rust
pub mod adapters;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::adapters::tests::adapter_all_has_seven_unique`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs crates/spur-core/src/skills/mod.rs
git commit -m "feat(spur-core): add Adapter enum + RenderedFile skeleton"
```

---

## Task 7: `render_agentskills` (shared renderer for 5 adapters)

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`

This one function powers SpurHermetic, ClaudeCode, Gemini, Kiro, and OpenCode — all of which follow the agentskills.io pattern.

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-core/src/skills/adapters.rs`'s `#[cfg(test)] mod tests { ... }` block:

```rust
    fn sample_skill() -> SkillPayload {
        SkillPayload {
            id: "tdd".to_string(),
            description: "Use for TDD".to_string(),
            body: "Write the test first.\n".to_string(),
            source: crate::skills::SkillSource::Bundled,
        }
    }

    #[test]
    fn agentskills_render_no_prefix() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_agentskills(&skill, &root.join(".spur/skills"), "");
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.spur/skills/tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\nname: tdd\ndescription: Use for TDD\n---\n"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=tdd sha256="));
        assert!(s.trim_end().ends_with("Write the test first."));
    }

    #[test]
    fn agentskills_render_with_spurpower_prefix() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.claude/skills/spurpower-tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.contains("name: spurpower-tdd"));
        assert!(s.contains("skill=spurpower-tdd"));
    }

    #[test]
    fn agentskills_render_is_deterministic() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let a = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        let b = render_agentskills(&skill, &root.join(".claude/skills"), "spurpower-");
        assert_eq!(a.bytes, b.bytes);
    }
```

- [ ] **Step 2: Implement render_agentskills**

Replace the `unimplemented!` body of `render_agentskills` in `crates/spur-core/src/skills/adapters.rs`:

```rust
fn render_agentskills(
    skill: &SkillPayload,
    target_root: &Path,
    name_prefix: &str,
) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("{name_prefix}{}", skill.id);
    let path = target_root.join(&id).join("SKILL.md");
    let body = &skill.body;
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\nname: {id}\ndescription: {desc}\n---\n{marker}{body}",
        id = id,
        desc = skill.description,
        marker = marker.render(),
        body = body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}
```

- [ ] **Step 3: Make internal helpers accessible**

In `crates/spur-core/src/skills/installer.rs`, change `pub(crate)` on `Marker`, `parse_marker`, `sha256_hex` if they were narrower (they already are `pub(crate)` — confirm no change needed). If any is currently private, upgrade to `pub(crate)`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core skills::adapters`
Expected: 4 passed (3 new + 1 existing `adapter_all_has_seven_unique`).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs
git commit -m "feat(spur-core): implement render_agentskills for 5 adapters"
```

---

## Task 8: `render_codex` (no frontmatter)

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`

- [ ] **Step 1: Write the failing test**

Append to `adapters.rs`'s test module:

```rust
    #[test]
    fn codex_render_has_no_frontmatter_and_marker_on_line_one() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_codex(&skill, &root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.codex/skills/spurpower-tdd/SKILL.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(
            s.starts_with("<!-- SPUR-MANAGED v=1 skill=spurpower-tdd sha256="),
            "marker must be on line 1, got: {s:?}",
        );
        assert!(s.contains("Write the test first."));
    }
```

- [ ] **Step 2: Implement render_codex**

Replace the `unimplemented!` body of `render_codex`:

```rust
fn render_codex(skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("spurpower-{}", skill.id);
    let path = repo_root.join(".codex/skills").join(&id).join("SKILL.md");
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(skill.body.as_bytes()),
    };
    let bytes = format!("{}{}", marker.render(), skill.body).into_bytes();
    RenderedFile { path, bytes }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::adapters::tests::codex_render`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs
git commit -m "feat(spur-core): implement render_codex (free-form MD)"
```

---

## Task 9: `render_cursor` (`.mdc` with `alwaysApply`)

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`

- [ ] **Step 1: Write the failing test**

Append to `adapters.rs` test module:

```rust
    #[test]
    fn cursor_render_mdc_with_alwaysapply() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_cursor(&skill, &root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.cursor/rules/spurpower-tdd.mdc"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\ndescription: Use for TDD\nalwaysApply: true\n---\n"));
        assert!(!s.contains("globs:"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=spurpower-tdd sha256="));
        assert!(s.contains("Write the test first."));
    }
```

- [ ] **Step 2: Implement render_cursor**

Replace the `unimplemented!` body of `render_cursor`:

```rust
fn render_cursor(skill: &SkillPayload, repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let id = format!("spurpower-{}", skill.id);
    let path = repo_root.join(".cursor/rules").join(format!("{id}.mdc"));
    let marker = Marker {
        version: 1,
        skill_id: id.clone(),
        sha256: sha256_hex(skill.body.as_bytes()),
    };
    let bytes = format!(
        "---\ndescription: {desc}\nalwaysApply: true\n---\n{marker}{body}",
        desc = skill.description,
        marker = marker.render(),
        body = skill.body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::adapters::tests::cursor_render`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs
git commit -m "feat(spur-core): implement render_cursor (.mdc + alwaysApply)"
```

---

## Task 10: `render_kiro_steering_pointer` (once-per-run)

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`

This is a single file emitted once per `run()`, not per skill. Reserved skill id `__pointer` identifies it in the marker.

- [ ] **Step 1: Write the failing test**

Append to `adapters.rs` test module:

```rust
    #[test]
    fn kiro_steering_pointer_renders_once() {
        let root = std::path::PathBuf::from("/tmp/repo");
        let rf = render_kiro_steering_pointer(&root);
        assert_eq!(
            rf.path,
            std::path::PathBuf::from("/tmp/repo/.kiro/steering/spurpower-pointer.md"),
        );
        let s = std::str::from_utf8(&rf.bytes).unwrap();
        assert!(s.starts_with("---\ninclusion: always\n"));
        assert!(s.contains("name: spurpower-pointer"));
        assert!(s.contains("<!-- SPUR-MANAGED v=1 skill=__pointer sha256="));
        assert!(s.contains(".kiro/skills/spurpower-"));
    }
```

- [ ] **Step 2: Implement the function**

Append to `crates/spur-core/src/skills/adapters.rs`, above the `#[cfg(test)]` block:

```rust
/// Single per-run steering file that points Kiro's default agent at
/// `.kiro/skills/spurpower-*`. Emitted once per installer run, not per
/// skill. Uses the reserved skill id `__pointer` in its marker.
pub fn render_kiro_steering_pointer(repo_root: &Path) -> RenderedFile {
    use crate::skills::installer::{sha256_hex, Marker};
    let path = repo_root.join(".kiro/steering/spurpower-pointer.md");
    let body = "SpurPower tactical skills live in \
                `.kiro/skills/spurpower-*/SKILL.md` and are also available \
                as editable workspace overrides in `.spur/skills/<id>/`. \
                Use them for TDD, systematic debugging, code review, and \
                brain-delegation workflows.\n";
    let marker = Marker {
        version: 1,
        skill_id: "__pointer".to_string(),
        sha256: sha256_hex(body.as_bytes()),
    };
    let bytes = format!(
        "---\ninclusion: always\nname: spurpower-pointer\n\
         description: Pointer to SpurPower tactical skills in .kiro/skills/spurpower-*\n\
         ---\n{marker}{body}",
        marker = marker.render(),
        body = body,
    )
    .into_bytes();
    RenderedFile { path, bytes }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::adapters::tests::kiro_steering_pointer`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs
git commit -m "feat(spur-core): render Kiro spurpower-pointer.md steering file"
```

---

## Task 11: `Adapter::render` smoke test (all 7 variants)

**Files:**
- Modify: `crates/spur-core/src/skills/adapters.rs`

Confirm every enum variant routes to a non-panicking renderer with a correct path prefix.

- [ ] **Step 1: Write the test**

Append to `adapters.rs` test module:

```rust
    #[test]
    fn adapter_render_all_variants() {
        let skill = sample_skill();
        let root = std::path::PathBuf::from("/tmp/repo");
        let expected_prefixes = [
            (Adapter::SpurHermetic, "/tmp/repo/.spur/skills/tdd/"),
            (Adapter::ClaudeCode, "/tmp/repo/.claude/skills/spurpower-tdd/"),
            (Adapter::Codex, "/tmp/repo/.codex/skills/spurpower-tdd/"),
            (Adapter::Gemini, "/tmp/repo/.gemini/skills/spurpower-tdd/"),
            (Adapter::Kiro, "/tmp/repo/.kiro/skills/spurpower-tdd/"),
            (Adapter::OpenCode, "/tmp/repo/.opencode/skills/spurpower-tdd/"),
            (Adapter::Cursor, "/tmp/repo/.cursor/rules/"),
        ];
        for (a, prefix) in expected_prefixes {
            let rf = a.render(&skill, &root);
            assert!(
                rf.path.to_string_lossy().starts_with(prefix),
                "{a:?}: got {}, expected prefix {}",
                rf.path.display(),
                prefix,
            );
            assert!(!rf.bytes.is_empty());
        }
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-core skills::adapters::tests::adapter_render_all_variants`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/skills/adapters.rs
git commit -m "test(spur-core): verify all 7 Adapter variants dispatch correctly"
```

---

## Task 12: `InstallError` + `Summary` + `SkipReason`

**Files:**
- Modify: `crates/spur-core/src/skills/installer.rs`

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-core/src/skills/installer.rs`'s `#[cfg(test)] mod tests { ... }`:

```rust
    #[test]
    fn summary_display_empty_run() {
        let s = Summary::default();
        let rendered = format!("{s}");
        assert!(rendered.contains("wrote 0"));
    }

    #[test]
    fn summary_display_reports_skips() {
        let mut s = Summary::default();
        s.written.push(std::path::PathBuf::from("/x/a"));
        s.skipped.push((std::path::PathBuf::from("/x/b"), SkipReason::UserEdited));
        s.skipped.push((std::path::PathBuf::from("/x/c"), SkipReason::NoMarker));
        let rendered = format!("{s}");
        assert!(rendered.contains("wrote 1"));
        assert!(rendered.contains("skipped 2"));
        assert!(rendered.contains("/x/b"));
    }
```

- [ ] **Step 2: Implement the types**

Append to `crates/spur-core/src/skills/installer.rs` (above the `#[cfg(test)]` block):

```rust
use std::path::PathBuf;

/// Why a target path was not written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// File exists, has no SPUR-MANAGED marker — treat as user-owned.
    NoMarker,
    /// File exists, has a marker, but body hash does not match marker's
    /// embedded hash — user edited since last install.
    UserEdited,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkipReason::NoMarker => write!(f, "user-owned (no marker)"),
            SkipReason::UserEdited => write!(f, "user-edited"),
        }
    }
}

/// Report of what the installer did in a single run.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub written: Vec<PathBuf>,
    pub unchanged: Vec<PathBuf>,
    pub skipped: Vec<(PathBuf, SkipReason)>,
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "SpurPower skills: wrote {w}, unchanged {u}, skipped {s}",
            w = self.written.len(),
            u = self.unchanged.len(),
            s = self.skipped.len(),
        )?;
        for (p, reason) in &self.skipped {
            writeln!(f, "  skipped {} ({reason})", p.display())?;
        }
        Ok(())
    }
}

/// Error variant for any failure during install.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },

    #[error("invalid skill id `{id}`: {reason}")]
    InvalidSkillId { id: String, reason: String },
}

impl From<crate::skills::InvalidSkillId> for InstallError {
    fn from(e: crate::skills::InvalidSkillId) -> Self {
        InstallError::InvalidSkillId {
            id: e.id,
            reason: e.reason.to_string(),
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::installer::tests::summary`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/installer.rs
git commit -m "feat(spur-core): add Summary, SkipReason, InstallError types"
```

---

## Task 13: `atomic_write` helper

**Files:**
- Modify: `crates/spur-core/src/skills/installer.rs`

- [ ] **Step 1: Write the failing test**

Append to installer.rs test module:

```rust
    #[test]
    fn atomic_write_creates_missing_dirs_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/c/file.md");
        atomic_write(&target, b"hello world").unwrap();
        let read = std::fs::read_to_string(&target).unwrap();
        assert_eq!(read, "hello world");
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.md");
        std::fs::write(&target, "old").unwrap();
        atomic_write(&target, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
    }
```

- [ ] **Step 2: Implement atomic_write**

Append to `crates/spur-core/src/skills/installer.rs` above the `#[cfg(test)]` block:

```rust
use std::path::Path;

/// Atomic write: write to a sibling tempfile, then rename(2) over the
/// target. Creates parent directories as needed.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), InstallError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| InstallError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|source| {
        InstallError::Io { path: parent.to_path_buf(), source }
    })?;
    use std::io::Write as _;
    tmp.write_all(bytes).map_err(|source| InstallError::Io {
        path: tmp.path().to_path_buf(),
        source,
    })?;
    tmp.persist(path).map_err(|e| InstallError::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::installer::tests::atomic_write`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/installer.rs
git commit -m "feat(spur-core): atomic_write helper via tempfile + rename"
```

---

## Task 14: `decide` function (the core decision engine)

**Files:**
- Modify: `crates/spur-core/src/skills/installer.rs`

- [ ] **Step 1: Write the failing tests**

Append to installer.rs test module:

```rust
    use crate::skills::adapters::RenderedFile;

    fn rf_with(path: std::path::PathBuf, bytes: Vec<u8>) -> RenderedFile {
        RenderedFile { path, bytes }
    }

    fn wrap_with_marker(body: &str, skill_id: &str) -> Vec<u8> {
        let marker = Marker {
            version: 1,
            skill_id: skill_id.to_string(),
            sha256: sha256_hex(body.as_bytes()),
        };
        format!("---\nfoo: bar\n---\n{m}{body}", m = marker.render())
            .into_bytes()
    }

    #[test]
    fn decide_create_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let rf = rf_with(dir.path().join("x.md"), b"body".to_vec());
        assert_eq!(decide(&rf).unwrap(), Decision::Create);
    }

    #[test]
    fn decide_noop_when_bytes_identical() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        let bytes = wrap_with_marker("hello", "tdd");
        std::fs::write(&target, &bytes).unwrap();
        let rf = rf_with(target, bytes);
        assert_eq!(decide(&rf).unwrap(), Decision::NoOp);
    }

    #[test]
    fn decide_update_when_marker_body_hash_matches_but_bytes_differ() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        // Disk: old-version frontmatter + same body.
        let marker = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: sha256_hex(b"hello"),
        };
        let on_disk = format!("---\nold: fm\n---\n{m}hello", m = marker.render());
        std::fs::write(&target, &on_disk).unwrap();
        // Rendered: new frontmatter + same body + same hash.
        let rendered = format!("---\nnew: fm\n---\n{m}hello", m = marker.render());
        let rf = rf_with(target, rendered.into_bytes());
        assert_eq!(decide(&rf).unwrap(), Decision::Update);
    }

    #[test]
    fn decide_skip_usermarker_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        std::fs::write(&target, "totally user's file").unwrap();
        let rf = rf_with(target, b"spur version".to_vec());
        assert_eq!(
            decide(&rf).unwrap(),
            Decision::Skip(SkipReason::NoMarker),
        );
    }

    #[test]
    fn decide_skip_user_edited_when_body_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.md");
        // Disk: marker claims body hash of "hello", but body is "edited".
        let marker = Marker {
            version: 1,
            skill_id: "tdd".to_string(),
            sha256: sha256_hex(b"hello"),
        };
        let on_disk = format!("---\nfm: x\n---\n{m}edited body", m = marker.render());
        std::fs::write(&target, &on_disk).unwrap();
        let rf = rf_with(target, b"anything".to_vec());
        assert_eq!(
            decide(&rf).unwrap(),
            Decision::Skip(SkipReason::UserEdited),
        );
    }
```

- [ ] **Step 2: Implement `decide` + `body_after_marker` helper**

Append to `crates/spur-core/src/skills/installer.rs`:

```rust
use crate::skills::adapters::RenderedFile;

/// Outcome of `decide()` for a single target file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    Create,
    Update,
    NoOp,
    Skip(SkipReason),
}

/// Return the bytes after the SPUR-MANAGED marker line, if present.
/// Searches the first few lines (tolerates optional YAML frontmatter).
fn body_after_marker(bytes: &[u8]) -> Option<(Marker, &[u8])> {
    let text = std::str::from_utf8(bytes).ok()?;
    for (line, rest_start) in iter_lines_with_positions(text) {
        if let Some(m) = parse_marker(line) {
            return Some((m, &bytes[rest_start..]));
        }
        // Optimization: bail after ~20 lines; marker should be near the top.
        if rest_start > 2048 {
            break;
        }
    }
    None
}

fn iter_lines_with_positions(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.split_inclusive('\n').scan(0usize, |pos, line| {
        let start = *pos;
        *pos += line.len();
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        Some((trimmed, *pos))
    })
}

pub(crate) fn decide(rf: &RenderedFile) -> Result<Decision, InstallError> {
    if !rf.path.exists() {
        return Ok(Decision::Create);
    }
    let disk = std::fs::read(&rf.path).map_err(|source| InstallError::Io {
        path: rf.path.clone(),
        source,
    })?;
    if disk == rf.bytes {
        return Ok(Decision::NoOp);
    }
    let Some((marker, body)) = body_after_marker(&disk) else {
        return Ok(Decision::Skip(SkipReason::NoMarker));
    };
    let disk_hash = sha256_hex(body);
    if disk_hash == marker.sha256 {
        Ok(Decision::Update)
    } else {
        Ok(Decision::Skip(SkipReason::UserEdited))
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core skills::installer::tests::decide`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/skills/installer.rs
git commit -m "feat(spur-core): installer decide() covers all 5 outcomes"
```

---

## Task 15: `run` + `apply` (installer orchestration)

**Files:**
- Modify: `crates/spur-core/src/skills/installer.rs`

Wires everything together. No integration tests yet — those come in tasks 16-20. A single unit test exercises the happy path.

- [ ] **Step 1: Write the failing test**

Append to installer.rs test module:

```rust
    #[test]
    fn run_creates_all_seven_adapter_files_for_one_skill() {
        let dir = tempfile::tempdir().unwrap();
        let override_dir = dir.path().join(".spur/skills/my-skill");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: test\n---\nMy body\n",
        )
        .unwrap();
        // Clear bundled set by not relying on it — bundled skills will also
        // be written, but we only assert our override landed.
        let summary = run(dir.path()).unwrap();
        // Expected paths for `my-skill` (override) across 7 adapters + Kiro pointer.
        for expected in [
            ".spur/skills/my-skill/SKILL.md",
            ".claude/skills/spurpower-my-skill/SKILL.md",
            ".codex/skills/spurpower-my-skill/SKILL.md",
            ".gemini/skills/spurpower-my-skill/SKILL.md",
            ".kiro/skills/spurpower-my-skill/SKILL.md",
            ".opencode/skills/spurpower-my-skill/SKILL.md",
            ".cursor/rules/spurpower-my-skill.mdc",
            ".kiro/steering/spurpower-pointer.md",
        ] {
            let p = dir.path().join(expected);
            assert!(p.exists(), "missing {expected}");
            assert!(summary.written.contains(&p), "not in summary.written: {expected}");
        }
    }
```

- [ ] **Step 2: Implement `run` + `apply`**

Append to `crates/spur-core/src/skills/installer.rs`:

```rust
use crate::skills::adapters::{render_kiro_steering_pointer, Adapter};
use crate::skills::list_active_skills;

/// Render the active skill set into every known agent directory under
/// `repo_root`. Returns a structured Summary of what was written, what
/// was unchanged, and what was skipped.
pub fn run(repo_root: &Path) -> Result<Summary, InstallError> {
    let skills = list_active_skills(repo_root)?;
    let mut summary = Summary::default();

    // Per-skill × per-adapter fanout.
    for skill in &skills {
        for adapter in Adapter::all() {
            let rf = adapter.render(skill, repo_root);
            apply(&rf, &mut summary)?;
        }
    }

    // Once-per-run files (currently only Kiro steering pointer).
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

- [ ] **Step 3: Expose `render_kiro_steering_pointer`**

In `crates/spur-core/src/skills/adapters.rs`, confirm `render_kiro_steering_pointer` is declared `pub` (it is — per Task 10). If not, make it `pub`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core skills::installer::tests::run_creates_all_seven_adapter_files`
Expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/installer.rs
git commit -m "feat(spur-core): installer run() orchestrates skill × adapter fanout"
```

---

## Task 16: Integration test — fresh install

**Files:**
- Create: `crates/spur-core/tests/skills_installer.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/spur-core/tests/skills_installer.rs`:

```rust
//! End-to-end installer tests using tempdir roots.

use spur_core::skills::installer::{run, SkipReason};
use spur_core::skills::SkillSource;
use std::path::Path;

fn count_files_under(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    walkdir_shim(dir).filter(|p| p.is_file()).count()
}

// Minimal recursive walker to avoid a new dev-dep.
fn walkdir_shim(dir: &Path) -> Box<dyn Iterator<Item = std::path::PathBuf>> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir_shim(&p));
            } else {
                out.push(p);
            }
        }
    }
    Box::new(out.into_iter())
}

#[test]
fn fresh_install_creates_all_expected_files() {
    let tmp = tempfile::tempdir().unwrap();

    let summary = run(tmp.path()).unwrap();

    // Every bundled skill × 7 adapters should be written, + Kiro pointer.
    let bundled_count = spur_core::skills::list_active_skills(tmp.path())
        .unwrap()
        .iter()
        .filter(|s| matches!(s.source, SkillSource::Bundled))
        .count();
    let expected = bundled_count * 7 + 1; // +1 for Kiro pointer
    assert_eq!(
        summary.written.len(),
        expected,
        "expected {expected} writes, got {}",
        summary.written.len(),
    );

    // Every bundled skill should have a file under `.spur/skills/<id>/`.
    for skill in spur_core::skills::list_active_skills(tmp.path()).unwrap() {
        let p = tmp.path().join(".spur/skills").join(&skill.id).join("SKILL.md");
        assert!(p.exists(), "missing {}", p.display());
    }

    // Every adapter root should exist.
    for d in [
        ".spur/skills",
        ".claude/skills",
        ".codex/skills",
        ".gemini/skills",
        ".kiro/skills",
        ".kiro/steering",
        ".opencode/skills",
        ".cursor/rules",
    ] {
        let root = tmp.path().join(d);
        assert!(root.is_dir(), "expected dir {}", root.display());
        assert!(count_files_under(&root) > 0, "expected files under {}", root.display());
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --test skills_installer fresh_install`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/skills_installer.rs
git commit -m "test(spur-core): fresh install creates all expected adapter files"
```

---

## Task 17: Integration test — idempotent re-run

**Files:**
- Modify: `crates/spur-core/tests/skills_installer.rs`

- [ ] **Step 1: Write the test**

Append to `crates/spur-core/tests/skills_installer.rs`:

```rust
#[test]
fn rerun_is_idempotent_no_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let _first = run(tmp.path()).unwrap();
    let second = run(tmp.path()).unwrap();
    assert!(
        second.written.is_empty(),
        "expected no writes on re-run, got {}: {:?}",
        second.written.len(),
        second.written,
    );
    assert!(
        second.skipped.is_empty(),
        "expected no skips on re-run, got: {:?}",
        second.skipped,
    );
    assert!(!second.unchanged.is_empty(), "expected some NoOps");
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --test skills_installer rerun_is_idempotent`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/skills_installer.rs
git commit -m "test(spur-core): re-running installer writes nothing (idempotent)"
```

---

## Task 18: Integration test — user hand-edit preserved

**Files:**
- Modify: `crates/spur-core/tests/skills_installer.rs`

- [ ] **Step 1: Write the test**

Append to `crates/spur-core/tests/skills_installer.rs`:

```rust
#[test]
fn user_hand_edit_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    run(tmp.path()).unwrap();

    // Pick one adapter file and edit its body.
    let target = tmp
        .path()
        .join(".cursor/rules/spurpower-test-driven-development.mdc");
    let original = std::fs::read_to_string(&target).unwrap();
    let edited = format!("{original}\n\nUSER ADDITION\n");
    std::fs::write(&target, &edited).unwrap();

    // Re-run: should not clobber.
    let summary = run(tmp.path()).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), edited);
    let skipped_paths: Vec<_> = summary.skipped.iter().map(|(p, _)| p.clone()).collect();
    assert!(
        skipped_paths.contains(&target),
        "expected target in skipped list: {skipped_paths:?}",
    );
    assert!(summary
        .skipped
        .iter()
        .any(|(p, r)| p == &target && *r == SkipReason::UserEdited));
}

#[test]
fn preexisting_user_file_without_marker_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp
        .path()
        .join(".cursor/rules/spurpower-test-driven-development.mdc");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "totally user-authored, no SPUR marker").unwrap();

    let summary = run(tmp.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "totally user-authored, no SPUR marker",
    );
    assert!(summary
        .skipped
        .iter()
        .any(|(p, r)| p == &target && *r == SkipReason::NoMarker));
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p spur-core --test skills_installer -- user_hand_edit preexisting_user_file`
Expected: 2 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/skills_installer.rs
git commit -m "test(spur-core): installer preserves user edits and user-owned files"
```

---

## Task 19: Integration test — override flows through all adapters

**Files:**
- Modify: `crates/spur-core/tests/skills_installer.rs`

- [ ] **Step 1: Write the test**

Append to `crates/spur-core/tests/skills_installer.rs`:

```rust
#[test]
fn override_body_flows_into_every_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let override_dir = tmp.path().join(".spur/skills/test-driven-development");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("SKILL.md"),
        "---\nname: test-driven-development\ndescription: My TDD override\n---\nMY OVERRIDE BODY\n",
    )
    .unwrap();

    run(tmp.path()).unwrap();

    for p in [
        ".spur/skills/test-driven-development/SKILL.md",
        ".claude/skills/spurpower-test-driven-development/SKILL.md",
        ".codex/skills/spurpower-test-driven-development/SKILL.md",
        ".gemini/skills/spurpower-test-driven-development/SKILL.md",
        ".kiro/skills/spurpower-test-driven-development/SKILL.md",
        ".opencode/skills/spurpower-test-driven-development/SKILL.md",
        ".cursor/rules/spurpower-test-driven-development.mdc",
    ] {
        let contents = std::fs::read_to_string(tmp.path().join(p)).unwrap();
        assert!(
            contents.contains("MY OVERRIDE BODY"),
            "{p}: override body missing\n{contents}",
        );
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --test skills_installer override_body_flows`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/skills_installer.rs
git commit -m "test(spur-core): .spur/skills override flows to every adapter target"
```

---

## Task 20: Integration test — cross-version upgrade

**Files:**
- Modify: `crates/spur-core/tests/skills_installer.rs`

Simulates: user has disk files from Spur vN (clean, marker matches body). Now Spur vN+1 changes the bundled body. Next `run()` should update cleanly, not skip.

- [ ] **Step 1: Write the test**

Append to `crates/spur-core/tests/skills_installer.rs`:

```rust
#[test]
fn cross_version_upgrade_updates_unedited_files() {
    let tmp = tempfile::tempdir().unwrap();
    run(tmp.path()).unwrap();

    // Simulate "next Spur version" by using a workspace override with a
    // different body. From the installer's point of view, this is
    // indistinguishable from a bundled-body change.
    let id = "test-driven-development";
    let override_dir = tmp.path().join(".spur/skills").join(id);
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("SKILL.md"),
        format!(
            "---\nname: {id}\ndescription: new\n---\nNEW VERSION BODY\n"
        ),
    )
    .unwrap();

    let summary = run(tmp.path()).unwrap();

    // All adapter outputs for this skill should have been rewritten —
    // NOT skipped as "user-edited" (their on-disk bodies still matched
    // their old markers, even though the new render is different).
    for p in [
        ".claude/skills/spurpower-test-driven-development/SKILL.md",
        ".codex/skills/spurpower-test-driven-development/SKILL.md",
        ".cursor/rules/spurpower-test-driven-development.mdc",
    ] {
        let path = tmp.path().join(p);
        assert!(
            summary.written.contains(&path),
            "{p}: expected in written, got skipped: {:?}",
            summary.skipped,
        );
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("NEW VERSION BODY"), "{p}: body not updated");
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-core --test skills_installer cross_version_upgrade`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/skills_installer.rs
git commit -m "test(spur-core): cross-version upgrade updates unedited files"
```

---

## Task 21: Replace inline fanout in `spur-cli init.rs`

**Files:**
- Modify: `crates/spur-cli/src/commands/init.rs`

Remove the manual fanout block (lines 158-210) and replace with a single call to the installer.

- [ ] **Step 1: Read the current init.rs block**

Run: `cargo run --bin spur -- init --help` to confirm the command still builds before changes.

- [ ] **Step 2: Replace the fanout block**

In `crates/spur-cli/src/commands/init.rs`, locate the section from line 158 that starts with:

```rust
    // ── Setup Hermetic Workspace Skills ──
    let skills_dir = repo_root.join(".spur").join("skills");
    std::fs::create_dir_all(&skills_dir)?;

    let bundled_skills = spur_core::skills::all_bundled_raw();
```

…and continues through the Cursor, OpenCode, and CLAUDE.md writes up to approximately line 210 (end of the `claude_instructions` if/else block).

Replace that entire block with:

```rust
    // ── Install SpurPower skills across agent dirs ──
    match spur_core::skills::installer::run(&repo_root) {
        Ok(summary) => {
            println!();
            print!("{summary}");
        }
        Err(e) => {
            eprintln!("[spur] skills install failed: {e}");
            return Err(anyhow::anyhow!(e));
        }
    }
```

- [ ] **Step 3: Build and run existing CLI tests**

Run: `cargo build -p spur-cli && cargo test -p spur-cli`
Expected: PASS (init_ux.rs continues to pass — it does not assert on specific fanout file names).

- [ ] **Step 4: Manually verify against a scratch repo**

Run:
```bash
cd $(mktemp -d) && git init -q && cargo run --quiet --bin spur -- init 2>&1 | head -40
```

Expected: prints `SpurPower skills: wrote <N>, unchanged 0, skipped 0` plus the existing spur init output. Confirm `.claude/skills/spurpower-test-driven-development/SKILL.md` exists.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/init.rs
git commit -m "feat(spur-cli): delegate skills install to spur-core installer (fixes B1-B5)"
```

---

## Task 22: `.gitattributes` advisory in `spur init`

**Files:**
- Modify: `crates/spur-cli/src/commands/init.rs`

Print a one-time advisory if `.gitattributes` doesn't cover managed markdown files.

- [ ] **Step 1: Write the advisory helper**

At the bottom of `crates/spur-cli/src/commands/init.rs`, add:

```rust
fn print_gitattributes_advisory_if_needed(repo_root: &std::path::Path) {
    let path = repo_root.join(".gitattributes");
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    // Advisory if the file doesn't mention LF normalization for .md files.
    if !(contents.contains("*.md") && contents.contains("eol=lf")) {
        println!();
        println!(
            "Tip: add `*.md text eol=lf` to .gitattributes for cross-platform"
        );
        println!(
            "     teammates. SpurPower marker files may thrash across CRLF/LF"
        );
        println!("     systems otherwise.");
    }
}
```

- [ ] **Step 2: Call it after the installer runs**

In the `Ok(summary) => { ... }` arm of the match block from Task 21, after `print!("{summary}");`, add:

```rust
            print_gitattributes_advisory_if_needed(&repo_root);
```

- [ ] **Step 3: Build**

Run: `cargo build -p spur-cli`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/src/commands/init.rs
git commit -m "feat(spur-cli): advise .gitattributes eol=lf after skills install"
```

---

## Task 23: Verification — full test + clippy

**Files:** none

- [ ] **Step 1: Full test suite**

Run: `cargo test --workspace`
Expected: PASS across all crates.

- [ ] **Step 2: Clippy clean**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero warnings.

- [ ] **Step 3: Format check**

Run: `cargo fmt --all -- --check`
Expected: PASS (no formatting drift).

- [ ] **Step 4: Manual smoke test**

Run in a fresh temp dir:
```bash
cd $(mktemp -d) && git init -q
cargo run --quiet --bin spur -- init 2>&1 | tail -20
ls -la .claude/skills/spurpower-test-driven-development/
ls -la .cursor/rules/ | grep spurpower
ls -la .kiro/steering/spurpower-pointer.md
```

Expected: all three artifacts exist; `spur init` prints a Summary line and the `.gitattributes` advisory.

- [ ] **Step 5: Final commit (if any fixups)**

If clippy or fmt revealed issues, fix them and commit as:
```bash
git add -u
git commit -m "chore(spur-core): clippy + fmt after skills installer"
```

Otherwise, no commit needed.

---

## Self-Review Notes

**Spec coverage check:**
- MVP items 1–6 from the spec map to Tasks 7–15 (adapter rendering + installer core).
- MVP item 7 (tests) maps to Tasks 5, 7–15 (unit tests inline) + 16–20 (integration tests, 5 of 6 scenarios). **The "pre-existing user file" scenario (6th) was folded into Task 18** — `preexisting_user_file_without_marker_is_preserved`. All 6 spec scenarios covered.
- Production bugs B1–B5: B1 (OpenCode path) and B3 (Cursor frontmatter) fixed by Tasks 7 and 9 renderers. B2 (missing `.claude/skills/`) fixed by Task 7 (ClaudeCode variant). B4 (swallowed errors) fixed by Task 15 (`Result`-returning `run`) and Task 21 (CLI propagation). B5 (override ignored) fixed by Task 4 (`list_active_skills` with override chain).
- `.gitattributes` advisory (spec): Task 22.
- Skill-id validation (spec): Task 3.
- Marker format (spec): Task 5.
- Atomic writes (spec): Task 13.
- Decision engine (spec): Task 14.

**Placeholder scan:** none — every step contains full code or exact commands.

**Type consistency:**
- `Adapter::all()` returns `&'static [Adapter]` — used in Task 15 by iteration.
- `RenderedFile { path, bytes }` — consistent across Tasks 6, 14, 15.
- `Summary { written, unchanged, skipped }` — consistent across Tasks 12, 14, 15, 16–20.
- `SkipReason { NoMarker, UserEdited }` — consistent.
- `InstallError::Io` variant field is `source` (thiserror `#[source]`) — consistent.
- `validate_id` returns `Result<(), InvalidSkillId>` — consistent between Task 3 and Task 12's `From` impl.
- `render_agentskills` / `render_codex` / `render_cursor` signatures match between the skeleton (Task 6) and implementation (Tasks 7–9).
- `render_kiro_steering_pointer` is `pub fn` and called from `installer::run` in Task 15 — consistent.

No drift detected.
