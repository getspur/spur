# Agent Profile Implementation Plan

> **For agentic workers:** Execute one task per delegation, in DAG order. Every task follows the repo TDD cadence: a failing `test(...)` commit first, then the implementing `feat(...)`/`fix(...)` commit. Build/test ONLY through `scripts/spur-cargo` (never bare cargo). Steps use checkbox (`- [ ]`) syntax.

**Goal:** Per-delegation `profile` — dispatch a worker *as* a named agent persona defined once in Claude agent-markdown under `.spur/agents/<name>.md`, materialized into the worker worktree in the worker kind's native format (git-excluded), and selected on the fresh session via probe-verified ACP RPCs.

**Spec:** `docs/superpowers/specs/2026-07-04-agent-profile-design.md` (decision IDs D1–D10 referenced below). Probe evidence: `docs/superpowers/specs/2026-07-04-agent-profile-acp-probe-results.md`.

**Architecture:** Mirrors m11 (PR #48) plumbing exactly for the request field; adds a new `agent_profiles` module in spur-core (parser + per-kind renderers, modeled on `skills/frontmatter.rs` / `skills/adapters.rs`), a per-worktree git-exclude primitive in spur-worktree, a `ProfileStrategy` per-kind table in spur-acp config, and generalizes `apply_model_effort_override` → `apply_session_overrides` in `worker_attempt.rs`.

**Tech stack:** Rust 2021 workspace; existing deps only (serde, toml, serde_json already in tree). No new crate dependencies.

**Verified seams (2026-07-04, HEAD d33bea69f):**
- `DelegationRequest` at `crates/spur-core/src/delegation_types.rs:114-131` — already carries `model`, `effort`, `config_overrides`.
- `WorkerAttemptCtx` at `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs:118-151`.
- m11 apply helper `apply_model_effort_override` at `worker_attempt.rs:236-303`, invoked at `:571`.
- Overlay slot: `apply_overlays` at `worker_attempt.rs:389-410`; spawn at `:470-489`; `new_session_with_bypass(cwd=worktree)` at `:555-560`.
- Plan threading: `server/types.rs:534-535`, `plan_builder.rs:108-109` (+ label test `:909`), `plan/reconciler/mod.rs:1492-1493`.
- MCP input: `mcp/delegation.rs:151-152`; schemas: `tool_schemas.rs:62-69` (delegate_to_worker), `:97-104` (parallel/plan task entries).
- Capture hazard: `spur-worktree/src/manager.rs` — `worktree_dirty` (`git status --porcelain`), `finalize_worker_branch` `git add -A` (`:916-940`), `scrub_worktree` = `reset --hard` + `clean -fd` (`:503-508`), `collect_diff` (`:842-870`).
- `AgentKind` enum at `crates/spur-acp/src/types.rs:165-183`.

**Commit sub-id scheme:** `AP1`…`AP8` (e.g. `feat(spur-core): AP1 agent profile parser`).

---

### Task AP1: Canonical profile parser (`agent_profiles` module)

**Files:**
- Create: `crates/spur-core/src/agent_profiles/mod.rs`
- Modify: `crates/spur-core/src/lib.rs` (add `pub mod agent_profiles;` alongside the existing `pub mod skills;`)

The parser accepts Claude agent markdown: `---` YAML frontmatter with `name`, `description`, optional `model`, optional `effort` (SPUR extension, claude ignores unknown keys), optional `tools` (comma-separated), then the body (system prompt). Model the line-oriented parsing on `crates/spur-core/src/skills/frontmatter.rs` (`parse_source`) — do NOT reuse it directly: it captures skill-specific fields and drops the raw source, which the claude renderer needs verbatim.

- [ ] **Step 1: Write failing tests** (in `mod tests` inside `mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "---\nname: code-reviewer\ndescription: Reviews diffs for correctness\nmodel: opus\neffort: high\ntools: Read, Grep\n---\nYou are a rigorous code reviewer.\n";

    #[test]
    fn parses_all_frontmatter_fields_and_body() {
        let p = AgentProfile::parse("code-reviewer", RAW).unwrap();
        assert_eq!(p.name, "code-reviewer");
        assert_eq!(p.description, "Reviews diffs for correctness");
        assert_eq!(p.model.as_deref(), Some("opus"));
        assert_eq!(p.effort.as_deref(), Some("high"));
        assert_eq!(p.tools.as_deref(), Some("Read, Grep"));
        assert_eq!(p.body.trim(), "You are a rigorous code reviewer.");
        assert_eq!(p.raw, RAW);
    }

    #[test]
    fn minimal_profile_needs_only_name_description_body() {
        let raw = "---\nname: minimal\ndescription: d\n---\nbody\n";
        let p = AgentProfile::parse("minimal", raw).unwrap();
        assert!(p.model.is_none() && p.effort.is_none() && p.tools.is_none());
    }

    #[test]
    fn frontmatter_name_mismatch_is_error() {
        let raw = "---\nname: other\ndescription: d\n---\nbody\n";
        assert!(AgentProfile::parse("minimal", raw).is_err());
    }

    #[test]
    fn missing_frontmatter_or_description_is_error() {
        assert!(AgentProfile::parse("x", "no frontmatter").is_err());
        assert!(AgentProfile::parse("x", "---\nname: x\n---\nbody\n").is_err());
    }

    #[test]
    fn load_reads_spur_agents_dir_and_none_when_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join(".spur/agents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("code-reviewer.md"), RAW).unwrap();
        assert!(AgentProfile::load(tmp.path(), "code-reviewer").unwrap().is_some());
        assert!(AgentProfile::load(tmp.path(), "absent").unwrap().is_none());
    }

    #[test]
    fn load_rejects_path_traversal_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(AgentProfile::load(tmp.path(), "../evil").is_err());
        assert!(AgentProfile::load(tmp.path(), "a/b").is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `scripts/spur-cargo test -p spur-core agent_profiles`
Expected: compile error — `AgentProfile` not defined.

- [ ] **Step 3: Commit the failing tests**

`test(spur-core): AP1 agent profile parser cases`
(Commit compiles-red is not acceptable in this repo's CI — gate the test module with the implementation stub below in the same commit if needed: an `AgentProfile` with `todo!()` bodies keeps cadence honest while compiling.)

- [ ] **Step 4: Implement**

```rust
//! Canonical agent-profile definitions (spec D1): Claude agent markdown
//! stored under `.spur/agents/<name>.md`. Frontmatter `model`/`effort`
//! act as defaults under D8 precedence (request ▸ profile ▸ agent default).

use anyhow::{bail, Context, Result};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub tools: Option<String>,
    /// System-prompt body (markdown after the frontmatter fence).
    pub body: String,
    /// Byte-exact source, written verbatim for claude targets.
    pub raw: String,
}

impl AgentProfile {
    pub fn parse(expected_name: &str, raw: &str) -> Result<Self> {
        let rest = raw
            .strip_prefix("---\n")
            .context("agent profile missing YAML frontmatter fence")?;
        let idx = rest
            .find("\n---\n")
            .context("agent profile frontmatter not terminated")?;
        let (fm, body) = (&rest[..idx], &rest[idx + 5..]);

        let mut name = None;
        let mut description = None;
        let mut model = None;
        let mut effort = None;
        let mut tools = None;
        for line in fm.lines() {
            if let Some(v) = line.strip_prefix("name:") {
                name = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("description:") {
                description = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("model:") {
                model = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("effort:") {
                effort = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("tools:") {
                tools = Some(v.trim().to_string());
            }
        }
        let name = name.context("agent profile frontmatter missing `name`")?;
        if name != expected_name {
            bail!("agent profile name `{name}` does not match file name `{expected_name}`");
        }
        let description =
            description.context("agent profile frontmatter missing `description`")?;
        Ok(Self {
            name,
            description,
            model,
            effort,
            tools,
            body: body.to_string(),
            raw: raw.to_string(),
        })
    }

    /// Read `.spur/agents/<name>.md` under `repo_root`. `Ok(None)` when the
    /// file does not exist (pass-through selection, spec D4). Parse errors
    /// are hard errors (spec D7).
    pub fn load(repo_root: &Path, name: &str) -> Result<Option<Self>> {
        if name.contains('/') || name.contains("..") || name.contains('\\') {
            bail!("invalid agent profile name: {name}");
        }
        let path = repo_root.join(".spur/agents").join(format!("{name}.md"));
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context(format!("read {}", path.display())),
        };
        Self::parse(name, &raw).map(Some)
    }
}
```

- [ ] **Step 5: Verify green, lint, commit**

Run: `scripts/spur-cargo test -p spur-core agent_profiles` → all pass.
Run: `SPUR_REMOTE=1 scripts/spur-cargo clippy -p spur-core -- -D warnings` (sandboxed agents must force remote).
Commit: `feat(spur-core): AP1 agent profile parser and loader`

---

### Task AP2: Per-kind renderers

**Files:**
- Create: `crates/spur-core/src/agent_profiles/render.rs`
- Modify: `crates/spur-core/src/agent_profiles/mod.rs` (add `pub mod render;`)

One canonical profile → the target kind's native file (spec §3.3). Returns `None` for kinds with no convention. Kiro JSON uses `serde_json`; codex TOML uses the `toml` crate (both already workspace deps).

- [ ] **Step 1: Failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::types::AgentKind;

    fn profile() -> crate::agent_profiles::AgentProfile {
        crate::agent_profiles::AgentProfile::parse(
            "code-reviewer",
            "---\nname: code-reviewer\ndescription: Reviews diffs\nmodel: opus\neffort: high\n---\nYou review code.\n",
        )
        .unwrap()
    }

    #[test]
    fn claude_kinds_get_verbatim_canonical_source() {
        for kind in [AgentKind::ClaudeCodeAcp, AgentKind::ClaudeStreamJson] {
            let r = render_for_kind(&profile(), kind).unwrap();
            assert_eq!(r.rel_path, ".claude/agents/code-reviewer.md");
            assert_eq!(r.contents, profile().raw);
        }
    }

    #[test]
    fn opencode_gets_agent_markdown_with_description_frontmatter() {
        let r = render_for_kind(&profile(), AgentKind::OpenCode).unwrap();
        assert_eq!(r.rel_path, ".opencode/agent/code-reviewer.md");
        assert!(r.contents.starts_with("---\ndescription: Reviews diffs\n---\n"));
        assert!(r.contents.contains("You review code."));
    }

    #[test]
    fn kiro_gets_json_with_name_description_prompt() {
        let r = render_for_kind(&profile(), AgentKind::Kiro).unwrap();
        assert_eq!(r.rel_path, ".kiro/agents/code-reviewer.json");
        let v: serde_json::Value = serde_json::from_str(&r.contents).unwrap();
        assert_eq!(v["name"], "code-reviewer");
        assert_eq!(v["description"], "Reviews diffs");
        assert_eq!(v["prompt"], "You review code.\n");
    }

    #[test]
    fn codex_gets_toml_with_developer_instructions_and_model_defaults() {
        let r = render_for_kind(&profile(), AgentKind::CodexAcp).unwrap();
        assert_eq!(r.rel_path, ".codex/agents/code-reviewer.toml");
        let v: toml::Value = r.contents.parse().unwrap();
        assert_eq!(v["name"].as_str(), Some("code-reviewer"));
        assert_eq!(v["developer_instructions"].as_str(), Some("You review code.\n"));
        assert_eq!(v["model"].as_str(), Some("opus"));
        assert_eq!(v["model_reasoning_effort"].as_str(), Some("high"));
    }

    #[test]
    fn kinds_without_convention_render_nothing() {
        for kind in [AgentKind::Kimi, AgentKind::Gemini, AgentKind::Generic] {
            assert!(render_for_kind(&profile(), kind).is_none());
        }
    }
}
```

- [ ] **Step 2: Red run, commit** `test(spur-core): AP2 per-kind profile renderer cases` (with compiling stub as in AP1).

- [ ] **Step 3: Implement**

```rust
use super::AgentProfile;
use spur_acp::types::AgentKind;

pub struct RenderedProfile {
    /// Worktree-relative target path.
    pub rel_path: String,
    pub contents: String,
}

pub fn render_for_kind(profile: &AgentProfile, kind: AgentKind) -> Option<RenderedProfile> {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => Some(RenderedProfile {
            rel_path: format!(".claude/agents/{}.md", profile.name),
            contents: profile.raw.clone(),
        }),
        AgentKind::OpenCode => Some(RenderedProfile {
            rel_path: format!(".opencode/agent/{}.md", profile.name),
            contents: format!("---\ndescription: {}\n---\n{}", profile.description, profile.body),
        }),
        AgentKind::Kiro => {
            let v = serde_json::json!({
                "name": profile.name,
                "description": profile.description,
                "prompt": profile.body,
            });
            Some(RenderedProfile {
                rel_path: format!(".kiro/agents/{}.json", profile.name),
                contents: serde_json::to_string_pretty(&v).expect("static json"),
            })
        }
        AgentKind::CodexAcp => {
            let mut t = toml::value::Table::new();
            t.insert("name".into(), profile.name.clone().into());
            t.insert("description".into(), profile.description.clone().into());
            t.insert("developer_instructions".into(), profile.body.clone().into());
            if let Some(m) = &profile.model {
                t.insert("model".into(), m.clone().into());
            }
            if let Some(e) = &profile.effort {
                t.insert("model_reasoning_effort".into(), e.clone().into());
            }
            Some(RenderedProfile {
                rel_path: format!(".codex/agents/{}.toml", profile.name),
                contents: toml::to_string_pretty(&toml::Value::Table(t)).expect("static toml"),
            })
        }
        AgentKind::Kimi | AgentKind::Gemini | AgentKind::Generic => None,
    }
}
```

- [ ] **Step 4: Green run + clippy + commit** `feat(spur-core): AP2 render profile per agent kind`

---

### Task AP3: Per-worktree git excludes (spur-worktree)

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs` (new method near `scrub_worktree`, `:503`)

Spec D5. Injected files must be invisible to `git status --porcelain` (drives `worktree_dirty`, `manager.rs:1010`), `git add -A` (finalize, `:916-940`), and worker self-commits. Mechanism: enable `extensions.worktreeConfig` (shared repo config, idempotent), then set per-worktree `core.excludesFile` pointing at a `spur-excludes` file inside the worktree's **private git dir** (`git rev-parse --git-dir` → `<main>/.git/worktrees/<id>/`), which is outside the working tree so the exclude file itself never shows as untracked. Trade-off (documented in code): per-worktree `core.excludesFile` shadows the user's global excludes file inside that ephemeral worker worktree — acceptable.

- [ ] **Step 1: Failing tests** (async tests in `manager.rs` test module; reuse existing `seed_base_repo` / `git` helpers)

```rust
#[tokio::test]
async fn excluded_injected_file_is_invisible_to_status_and_finalize() {
    let tmp = tempfile::TempDir::new().unwrap();
    seed_base_repo(tmp.path()).await;
    let manager = WorktreeManager::new_for_test(tmp.path().to_path_buf());
    let sid = SessionId("s-excl".to_string());
    let wt = tmp.path().join("wt-excl");
    // create real linked worktree via existing helper
    manager.create_worktree_at(&wt, "spur/excl", "HEAD").await.unwrap();
    manager.register_for_test(sid.clone(), wt.clone(), /* base */ head_oid(tmp.path()).await);

    std::fs::create_dir_all(wt.join(".claude/agents")).unwrap();
    std::fs::write(wt.join(".claude/agents/spur-x.md"), "persona").unwrap();
    manager
        .add_worktree_excludes(&wt, &[".claude/agents/spur-x.md".into()])
        .await
        .unwrap();

    // invisible to status → worktree not dirty
    assert!(!manager.worktree_dirty(&wt).await.unwrap());
    // finalize on an idle worker is a NoOp (no junk commit)
    let out = manager.finalize_worker_branch(&sid, "msg", true).await.unwrap();
    assert_eq!(out.case, FinalizeCase::NoOp);
}

#[tokio::test]
async fn excluded_file_never_enters_squashed_commit_or_diff() {
    // same setup; worker makes a real edit AND git add -A commits everything
    // ... write a.txt, run git(["add","-A"]) + commit in the worktree ...
    // assert: `git show --name-only HEAD` does not list .claude/agents/spur-x.md
    // assert: collect_diff(&sid) diff text does not contain "spur-x.md"
}
```

- [ ] **Step 2: Red run** `scripts/spur-cargo test -p spur-worktree excluded_` → fails (`add_worktree_excludes` undefined). Commit `test(spur-worktree): AP3 per-worktree exclude invariants` (with stub).

- [ ] **Step 3: Implement**

```rust
/// Make `patterns` (worktree-relative paths) invisible to git inside this
/// worktree only: status/porcelain, `add -A`, and diffs. Idempotent —
/// re-registering appends only unseen patterns. The exclude manifest also
/// serves as the ownership record for SPUR-injected files (spec D4/D7:
/// a target path NOT listed here is user content and must not be overwritten).
pub async fn add_worktree_excludes(
    &self,
    worktree_path: &Path,
    patterns: &[String],
) -> Result<PathBuf> {
    // 1. Shared, idempotent: allow per-worktree config.
    self.run_git(&["config", "extensions.worktreeConfig", "true"], Some(worktree_path))
        .await?;
    // 2. Private git dir of THIS worktree (outside the working tree).
    let git_dir = self
        .run_git(&["rev-parse", "--absolute-git-dir"], Some(worktree_path))
        .await?;
    let exclude_path = Path::new(git_dir.trim()).join("spur-excludes");
    // 3. Point per-worktree excludes at it.
    self.run_git(
        &["config", "--worktree", "core.excludesFile",
          exclude_path.to_str().ok_or_else(|| anyhow!("non-utf8 git dir"))?],
        Some(worktree_path),
    )
    .await?;
    // 4. Append unseen patterns.
    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut out = existing.clone();
    for p in patterns {
        if !existing.lines().any(|l| l == p) {
            out.push_str(p);
            out.push('\n');
        }
    }
    std::fs::write(&exclude_path, out)?;
    Ok(exclude_path)
}
```

Also add a read-side helper used by AP7's ownership check:

```rust
/// Worktree-relative paths SPUR registered as excluded (empty if none).
pub async fn worktree_excluded_paths(&self, worktree_path: &Path) -> Vec<String>
```

(implementation: re-derive `exclude_path` as above, read + split lines, `Vec::new()` on any error).

- [ ] **Step 4: Green + clippy + commit** `feat(spur-worktree): AP3 per-worktree exclude for injected files`

---

### Task AP4: `profile` on `DelegationRequest`, MCP tools, schemas

**Files:**
- Modify: `crates/spur-core/src/delegation_types.rs:114-131` — add `pub profile: Option<String>,` directly after `agent`.
- Modify: `crates/spur-core/src/mcp/delegation.rs` — parsed input structs gain `profile: Option<String>` (both the single-delegation input and the parallel task entry, same structs that carry `model`/`effort`); thread at the `DelegationRequest` construction (`:151-152` neighborhood).
- Modify: `crates/spur-core/src/tool_schemas.rs:62-69` and `:97-104` — add alongside `model`/`effort`:

```rust
/// Named agent profile from `.spur/agents/<name>.md` (or a pass-through
/// agent/mode name the worker binary already knows). Materialized into the
/// worker worktree and selected on the fresh session; fail-soft on selection.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub profile: Option<String>,
```

- Modify: every `DelegationRequest { .. }` literal — the compiler enumerates them; known sites: `mcp/delegation.rs`, `server/types.rs:534` region, `plan/reconciler/mod.rs:1492` region, `plan/test_util.rs`, plus test fixtures. Populate `profile: None` except the MCP input path.
- D7 validation in the `delegate_to_worker` handler, before dispatch: if `profile` is `Some(name)` and `AgentProfile::load(repo_root, name)` returns `Err`, reply `McpError::invalid_params` naming the parse failure. `Ok(None)` proceeds (pass-through).

- [ ] **Step 1: Failing tests** — extend the existing m11 test group in `mcp/delegation.rs` tests: input JSON with `"profile": "code-reviewer"` deserializes onto the request; absent field → `None`; malformed managed profile file → invalid_params error. Commit `test(spur-core): AP4 profile field parse and D7 validation`.
- [ ] **Step 2: Implement, run** `scripts/spur-cargo test -p spur-core mcp::delegation` **+ full** `scripts/spur-cargo check --workspace` (the struct-literal sweep). Commit `feat(spur-core): AP4 plumb profile through delegation request`.

---

### Task AP5: Plan-path threading (submit_plan → reconciler)

**Files (identical seams to m11's plan threading):**
- Modify: `crates/spur-core/src/server/types.rs` — plan task input struct gains `profile: Option<String>`; thread at `:534-535` alongside `model`/`effort`.
- Modify: `crates/spur-core/src/server/plan_builder.rs` — pass-through around `:108-109`; labels untouched.
- Modify: `crates/spur-core/src/plan/reconciler/mod.rs` — populate `profile: task.spec.profile.clone()` at the `:1492-1493` construction.
- Modify: `crates/spur-core/src/tool_schemas.rs` — `submit_plan` task-entry schema gains the same `profile` field as AP4.
- Verify `submit_plan_mutation` `modify_task_spec` rewrites `profile` like any other spec field (it operates on the task-spec struct; adding the field suffices — add a test).

- [ ] **Step 1: Failing tests**
  - `plan_builder` test cloned from `model_effort_do_not_pollute_agent_label` (`plan_builder.rs:909`): task with `profile: Some("code-reviewer")` → `labels::agent` still receives the bare agent name and no `profile` appears in any label.
  - Reconciler round-trip test: plan task with profile → dispatched `DelegationRequest.profile` matches, including on retry re-dispatch.
  Commit `test(spur-core): AP5 plan task profile threading cases`.
- [ ] **Step 2: Implement + green + commit** `feat(spur-core): AP5 thread profile through plan path`.

---

### Task AP6: `ProfileStrategy` (spur-acp config)

**Files:**
- Create: `crates/spur-acp/src/profile_strategy.rs`; export from `lib.rs`.
- Modify: `crates/spur-acp/src/config/mod.rs` — optional `[agents.entries.profile]` block on `AgentConfig`:

```rust
/// Per-kind profile wiring override (spec D9). When absent, defaults
/// derive from `kind` via `ProfileStrategy::for_kind`.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub profile: Option<ProfileConfig>,
```

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn defaults_encode_the_probe_matrix() {
    use AgentKind::*;
    assert_eq!(ProfileStrategy::for_kind(ClaudeCodeAcp).select,
               SelectStrategy::ConfigOption { id: "agent".into() });
    assert_eq!(ProfileStrategy::for_kind(OpenCode).select,
               SelectStrategy::ConfigOption { id: "mode".into() });
    assert_eq!(ProfileStrategy::for_kind(Kiro).select, SelectStrategy::SessionMode);
    assert_eq!(ProfileStrategy::for_kind(CodexAcp).select, SelectStrategy::None);
    assert_eq!(ProfileStrategy::for_kind(Kimi).select, SelectStrategy::None);
    assert_eq!(ProfileStrategy::for_kind(ClaudeStreamJson).select, SelectStrategy::None); // argv follow-up
}

#[test]
fn config_block_overrides_kind_default() {
    let toml = r#"select = "config_option:agent""#;
    let cfg: ProfileConfig = toml::from_str(toml).unwrap();
    let s = ProfileStrategy::resolve(AgentKind::CodexAcp, Some(&cfg));
    assert_eq!(s.select, SelectStrategy::ConfigOption { id: "agent".into() });
}
```

Commit `test(spur-acp): AP6 profile strategy defaults and override`.

- [ ] **Step 2: Implement**

```rust
use crate::types::AgentKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectStrategy {
    /// `session/set_config_option` with this config id, value = profile name.
    ConfigOption { id: String },
    /// `session/set_mode` with modeId = profile name (kiro).
    SessionMode,
    /// No selection surface — skip with debug log.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileStrategy {
    pub select: SelectStrategy,
    /// Whether AP2 has a renderer for this kind (materialization gate).
    pub materialize: bool,
}

impl ProfileStrategy {
    pub fn for_kind(kind: AgentKind) -> Self { /* match per spec §3.3 */ }
    pub fn resolve(kind: AgentKind, cfg: Option<&ProfileConfig>) -> Self { /* parse
        cfg.select strings "config_option:<id>" | "session_mode" | "none",
        cfg.materialize bool; fall back to for_kind() per field */ }
}

/// `[agents.entries.profile]` — serde struct with `select: Option<String>`,
/// `materialize: Option<bool>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileConfig {
    pub select: Option<String>,
    pub materialize: Option<bool>,
}
```

Materialize defaults: `true` for ClaudeCodeAcp/ClaudeStreamJson/OpenCode/Kiro/CodexAcp, `false` for Kimi/Gemini/Generic (must agree with AP2's `render_for_kind` — add a cross-check test in AP7's integration).

- [ ] **Step 3: Green + clippy + commit** `feat(spur-acp): AP6 profile strategy table`. Run `scripts/spur-cargo test -p spur-acp` (config-validation rule from CLAUDE.md).

---

### Task AP7: Worker-attempt integration (materialize + select)

**Files:**
- Modify: `crates/spur-core/src/orchestrator/delegation/worker_attempt.rs`
- Modify: `crates/spur-core/src/orchestrator/delegation/execute.rs` (thread `profile` from request → ctx; resolve D8 precedence here so retries reuse it)

**Wiring order inside `run_one_worker_attempt` (spec D6):**
1. After `apply_overlays` (`:389-410`) and the base-oid persistence block, before the connection is built (`:470`): materialization.
2. After `new_session_with_bypass` (`:555-569`): selection, then model/effort — replace the `apply_model_effort_override` call at `:571` with `apply_session_overrides`.

- [ ] **Step 1: Failing unit tests** (new `mod profile_override_tests` following the existing recording-connection pattern used by m11 tests in this file)

Test cases — each asserts the exact RPC sequence recorded by the mock connection:
1. `profile=None` → no selection RPC; model/effort behavior unchanged (m11 regression guard).
2. kind=ClaudeCodeAcp, `profile=Some("code-reviewer")` → exactly one `set_session_config_option{configId:"agent", value:"code-reviewer"}` and it precedes any model/effort call.
3. kind=OpenCode → one `set_session_config_option{configId:"mode", ...}`.
4. kind=Kiro → one `set_session_mode{modeId:"code-reviewer"}`, no config-option call.
5. kind=CodexAcp → no selection RPC (debug skip), model/effort still applied.
6. Selection rejected by agent → `warn!`, model/effort still attempted, attempt proceeds (fail-soft, D6-analog).
7. D8 precedence: request `model=None` + profile `model: opus` → model RPC carries `opus`; request `model=Some("sonnet")` wins over profile.
8. Materialization ownership: target rel_path already exists in worktree and is NOT in `worktree_excluded_paths` → file left untouched, selection still attempted (spec §6 collision row).

Commit `test(spur-core): AP7 session override selection cases`.

- [ ] **Step 2: Implement**

`WorkerAttemptCtx` addition (after `effort` at `:124`):

```rust
pub(crate) profile: Option<&'a str>,
/// Loaded+validated in execute_delegation when the profile is SPUR-managed;
/// None for pass-through selection (D4).
pub(crate) profile_def: Option<&'a crate::agent_profiles::AgentProfile>,
```

`execute_delegation`: after `registry.get(&agent)` (`execute.rs:63`), load once:

```rust
let profile_def = match request.profile.as_deref() {
    Some(name) => crate::agent_profiles::AgentProfile::load(&repo_root, name)?, // Err already rejected at MCP layer; defensive re-check
    None => None,
};
// D8 precedence, resolved once so retries are stable:
let effective_model = request.model.clone()
    .or_else(|| profile_def.as_ref().and_then(|p| p.model.clone()));
let effective_effort = request.effort.clone()
    .or_else(|| profile_def.as_ref().and_then(|p| p.effort.clone()));
```

Materialization step (new fn in `worker_attempt.rs`, called between overlay apply and connection build):

```rust
async fn materialize_profile(
    worktrees: &WorktreeManager,
    worktree_path: &Path,
    kind: spur_acp::types::AgentKind,
    strategy: &spur_acp::ProfileStrategy,
    profile: &crate::agent_profiles::AgentProfile,
) {
    if !strategy.materialize {
        return;
    }
    let Some(rendered) = crate::agent_profiles::render::render_for_kind(profile, kind) else {
        return;
    };
    let target = worktree_path.join(&rendered.rel_path);
    let ours = worktrees
        .worktree_excluded_paths(worktree_path)
        .await
        .contains(&rendered.rel_path);
    if target.exists() && !ours {
        tracing::warn!(target: "spur::worker::profile", path = %rendered.rel_path,
            "committed agent file exists; select-only against it");
        return;
    }
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&target, &rendered.contents) {
        tracing::warn!(target: "spur::worker::profile", error = %e, "profile write failed; select-only");
        return;
    }
    if let Err(e) = worktrees
        .add_worktree_excludes(worktree_path, &[rendered.rel_path.clone()])
        .await
    {
        // Never leave a non-excluded injected file (spec §6): remove it.
        let _ = std::fs::remove_file(&target);
        tracing::warn!(target: "spur::worker::profile", error = %e,
            "exclude setup failed; removed injected file, select-only");
    }
}
```

Selection arm — rename `apply_model_effort_override` → `apply_session_overrides`, add before the existing model block:

```rust
if let Some(profile) = profile {
    match &strategy.select {
        spur_acp::SelectStrategy::ConfigOption { id } => {
            let req = spur_acp::SetSessionConfigOptionRequest::new(
                session_id.clone(),
                spur_acp::SessionConfigId::new(id.as_str()),
                spur_acp::SessionConfigValueId::new(profile),
            );
            if let Err(error) = connection.set_session_config_option(req).await {
                tracing::warn!(target: "spur::worker::profile", config_id = %id,
                    value = %profile, %error, "profile selection failed; default persona");
            }
        }
        spur_acp::SelectStrategy::SessionMode => {
            let req = spur_acp::SetSessionModeRequest::new(
                session_id.clone(),
                spur_acp::SessionModeId::new(profile),
            );
            if let Err(error) = connection.set_session_mode(req).await {
                tracing::warn!(target: "spur::worker::profile",
                    value = %profile, %error, "profile set_mode failed; default persona");
            }
        }
        spur_acp::SelectStrategy::None => {
            tracing::debug!(target: "spur::worker::profile", value = %profile,
                "kind has no selection surface; skipped");
        }
    }
}
// ...existing model block, then effort block, unchanged...
```

(Exact constructor names for `SetSessionModeRequest`/`SessionModeId`: match the existing usage in `crate::skip_perm` (`skip_permissions_session_mode` path) — reuse whatever that call site uses.)

- [ ] **Step 3: Green, workspace check, commit**

Run: `scripts/spur-cargo test -p spur-core profile_override` then `scripts/spur-cargo test --workspace`.
Commit: `feat(spur-core): AP7 materialize and select agent profile per delegation`

---

### Task AP8: End-to-end test, docs, verification sweep

**Files:**
- Create: integration test in `crates/spur-core/tests/` (follow the existing delegation e2e recording-connection harness used by the m11 integration test): delegation with `agent kind ClaudeCodeAcp`, `profile="code-reviewer"`, managed file present in a temp `.spur/agents/` → asserts (a) rendered file exists in the worker worktree and is git-excluded (status porcelain empty), (b) `session/set_config_option("agent","code-reviewer")` recorded before the prompt, (c) final collected diff does not mention `.claude/agents/`.
- Modify: `docs/superpowers/specs/2026-07-04-agent-profile-design.md` — flip **Status** to `implemented (AP1–AP8)`.
- Modify: `CLAUDE.md` is NOT touched (no new conventions); add `.spur/agents/` mention to `docs/spur/agent-config.md` if that doc exists in-tree.

- [ ] **Step 1:** Write + red-run the e2e test, commit `test(spur-core): AP8 agent profile end-to-end delegation`.
- [ ] **Step 2:** Fix anything it surfaces; green `scripts/spur-cargo test --workspace`.
- [ ] **Step 3:** `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings` and `scripts/spur-cargo fmt --all`.
- [ ] **Step 4:** Commit `feat(spur-core): AP8 e2e profile delegation + docs status`.

---

## Task DAG

```
AP1 ──► AP2 ─────────────┐
AP3 ─────────────────────┤
AP4 ──► AP5 ──────────┐  ├──► AP7 ──► AP8
AP6 ─────────────────────┘            ▲
                      └───────────────┘   (AP5 needed only by AP8's plan-path assertions)
```

- AP1 → AP2 → AP7; AP3 → AP7; AP4 → AP7; AP6 → AP7; AP4 → AP5 → AP8; AP7 → AP8.
- AP3, AP4, AP6 are mutually independent and parallelizable with AP1/AP2.

## Self-review notes

- Spec coverage: D1/D7 → AP1; §3.3 materialize column → AP2; D5 + §6 leak rows → AP3; D2 → AP4; §5.4 plan path → AP5; D9 → AP6; D3/D4/D6/D8 + §6 fail-soft rows → AP7; §7 testing strategy items 1–5 → AP1/AP2/AP3/AP7/AP5 respectively, item 6 (manual wire probes) stays manual.
- Known deliberate deferrals (spec §8): kiro spawn flags, claude-sj `--agent` argv, kimi prompt plane, audit sentinel, TUI surfacing, `spur agents` CLI, seed bump.
- Type-consistency: `AgentProfile` (AP1) is the type consumed by `render_for_kind` (AP2), `profile_def` (AP7); `RenderedProfile.rel_path` feeds `add_worktree_excludes`/`worktree_excluded_paths` (AP3) and the ownership check (AP7 case 8).
