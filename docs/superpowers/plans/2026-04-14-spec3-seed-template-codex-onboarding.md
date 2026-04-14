# Spec 3 — Seed Template + Codex Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 114-line hardcoded `SeedAgent` struct + `known_agents` array in `Orchestrator::init_agents()` with an embedded `seed_agents.toml` that uses the real `AgentConfig` schema. Ship a complete codex config with static commands proving Spec 2's framework. Write the onboarding cookbook.

**Architecture:**
- **Pillar 1** — Move seed parsing into `spur-acp` where config-parsing already lives. Add `pub fn load_seed_template() -> AgentsConfig` backed by `include_str!("seed_agents.toml")`. Keeps `spur-core` free of a new `toml` dep.
- **Pillar 2** — Rewrite `init_agents()` to iterate `load_seed_template().entries`, `which`-check each command, and `registry.register(config)`. Target body ≤20 lines. Delete `SeedAgent` struct + `known_agents` array.
- **Pillar 3** — Write `docs/spur/agent-onboarding-cookbook.md` with decision tree, worked examples, and a "test the agent at runtime" step.

**Tech stack:** Rust, serde TOML, tokio process spawning.

---

## Pre-flight

- [ ] On `main` with clean tree. Spec 2 commits up through `075462f` reachable.
- [ ] `grep -n "fn init_agents" crates/spur-core/src/orchestrator.rs` returns exactly one line (~912).
- [ ] Current `init_agents()` body spans ~114 lines (912–1025). Measurable shrink target.

---

## Task 1: Seed template + `load_seed_template()` in spur-acp

**Why:** Keep config-parsing in `spur-acp` (where `toml` already lives). Placing the seed TOML here avoids adding `toml` as a dep to `spur-core` — cleaner layering. Compile-time parse test is the primary guard against malformed seed edits.

**Files:**
- Create: `crates/spur-acp/src/seed_agents.toml`
- Modify: `crates/spur-acp/src/config/mod.rs` (add loader)
- Modify: `crates/spur-acp/src/lib.rs` (re-export loader)

### Step 1.1 — Write the failing parse test

- [ ] Append to `crates/spur-acp/src/config/mod.rs` inside an existing `#[cfg(test)] mod tests` (or create it):

```rust
    #[test]
    fn seed_template_parses_and_has_five_agents() {
        let seeds = load_seed_template();
        assert!(seeds.entries.len() >= 5,
            "seed template must have ≥5 agents, got {}", seeds.entries.len());
        let names: Vec<_> = seeds.entries.iter().map(|a| a.name.as_str()).collect();
        for expected in ["kiro", "claude-code", "claude-code-acp", "codex", "gemini"] {
            assert!(names.contains(&expected),
                "missing seed agent: {expected} (got {names:?})");
        }
    }

    #[test]
    fn seed_template_codex_has_static_commands() {
        let seeds = load_seed_template();
        let codex = seeds.entries.iter().find(|a| a.name == "codex")
            .expect("codex should be in seed template");
        assert!(!codex.commands.static_commands.is_empty(),
            "codex must have at least one static command (proves Spec 2)");
    }

    #[test]
    fn seed_template_passes_validator() {
        // Every seed entry must pass validate_agent_config — seed template is
        // a contract that all framework users inherit.
        let seeds = load_seed_template();
        for agent in &seeds.entries {
            let errs = crate::config::validate_agent_config(agent);
            let fatal: Vec<_> = errs.iter().filter(|e| e.is_fatal()).collect();
            assert!(fatal.is_empty(),
                "seed agent `{}` has fatal validator errors: {fatal:?}",
                agent.name);
        }
    }
```

- [ ] Run: `cargo test -p spur-acp config::tests::seed_template` → FAIL (undefined symbol).

### Step 1.2 — Create the seed template file

- [ ] Create `crates/spur-acp/src/seed_agents.toml`:

```toml
# Seed agents for `spur init`. Same schema as .spur/config.toml.
#
# `spur init` parses this, scans $PATH for each `command`, and registers
# matching entries into the in-memory AgentRegistry. The CLI then writes
# that registry to .spur/config.toml as the user's starting config.
#
# IMPORTANT: `spur init` OVERWRITES .spur/config.toml. If you've
# customized your config, edit it by hand rather than re-running init.
#
# To add a new seed agent: append an [[agents.entries]] block below. No
# Rust changes are required. See docs/spur/agent-onboarding-cookbook.md.

# ── kiro ──────────────────────────────────────────────────────────
[[agents.entries]]
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "kiro"

[agents.entries.commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

[[agents.entries.commands.ingest]]
method = "_kiro.dev/commands/available"
parser = "json_path_list"
path = "availableCommands"
item_schema = "acp_available_command"

[[agents.entries.commands.response]]
method = "_kiro.dev/commands/execute/response"
render = "system_note"

[agents.entries.permissions]
args = ["--trust-all-tools"]

# ── claude-code (stream-json fallback) ────────────────────────────
[[agents.entries]]
name = "claude-code"
command = "claude"
args = ["-p", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--permission-mode", "acceptEdits"]
transport = "stream-json"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "claude-sj"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
args = ["--dangerously-skip-permissions"]

# ── claude-code-acp ───────────────────────────────────────────────
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
cost_tier = "medium"

[agents.entries.display]
handle = "claude"

[agents.entries.commands]
dispatch = "prompt_text"

[agents.entries.permissions]
session_mode = "bypassPermissions"

# ── codex ─────────────────────────────────────────────────────────
# NOTE: `args = ["--acp"]` reflects the current assumption. Validate
# against your installed codex binary — flag may differ across versions.
# If codex needs a different ACP mode, edit this entry.
[[agents.entries]]
name = "codex"
command = "codex"
args = ["--acp"]
transport = "acp"
role = "both"
cost_tier = "low"

[agents.entries.display]
handle = "codex"

[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

# ── gemini ────────────────────────────────────────────────────────
[[agents.entries]]
name = "gemini"
command = "gemini"
args = []
transport = "cli-wrap"
role = "worker"
cost_tier = "low"

[agents.entries.display]
handle = "gemini"

[agents.entries.commands]
dispatch = "prompt_text"
```

### Step 1.3 — Add `load_seed_template()` loader

- [ ] In `crates/spur-acp/src/config/mod.rs`, append (outside test module):

```rust
/// Embedded seed template. Parsed by `load_seed_template()`. Source of
/// truth is `crates/spur-acp/src/seed_agents.toml`.
const SEED_TOML: &str = include_str!("../seed_agents.toml");

/// Parse the embedded seed template. Returns the pre-known agent set
/// that `spur init` discovers on $PATH.
///
/// Errors are unreachable in production thanks to the compile-time
/// parse test (`seed_template_parses_and_has_five_agents`). If a
/// maintainer skips tests and commits a bad edit, users see a clear
/// diagnostic instead of a panic.
pub fn load_seed_template() -> AgentsConfig {
    #[derive(serde::Deserialize)]
    struct SeedFile { agents: AgentsConfig }
    let parsed: SeedFile = toml::from_str(SEED_TOML).unwrap_or_else(|e| {
        panic!(
            "embedded seed_agents.toml failed to parse (this is a spur bug, \
             please report): {e}"
        )
    });
    parsed.agents
}
```

Note: the TOML uses `[[agents.entries]]` (nested under `agents`), so we deserialize through a throwaway `SeedFile` struct. Alternative: flatten by having the TOML be a bare `[[entries]]` list — but matching the real `.spur/config.toml` shape means the seed can be literally pasted/diff'd against user configs.

- [ ] In `crates/spur-acp/src/lib.rs`, extend the `pub use config::{ ... }` list with `load_seed_template`.

### Step 1.4 — Tests pass

- [ ] Run: `cargo test -p spur-acp config::tests::seed_template`
Expected: all 3 tests PASS.

- [ ] Run: `cargo build -p spur-acp`
Expected: clean.

### Step 1.5 — Commit

```bash
git add crates/spur-acp/src/seed_agents.toml \
        crates/spur-acp/src/config/mod.rs \
        crates/spur-acp/src/lib.rs
git commit -m "feat(spur-acp): embedded seed template + load_seed_template()"
```

---

## Task 2: Refactor `init_agents()` — delete `SeedAgent`

**Why:** This is the removal-heavy core of Spec 3. The 114-line body collapses to ~15 once `AgentConfig` comes from TOML instead of being hand-constructed from a parallel struct.

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

### Step 2.1 — Read current body to understand call sites

- [ ] Confirm `init_agents()` body at `crates/spur-core/src/orchestrator.rs:912–1025` — no external code consumes the `SeedAgent` struct (it's scoped inside the function).
- [ ] Confirm `grep -rn "SeedAgent" crates/` returns only hits inside that one function.

### Step 2.2 — Rewrite `init_agents()`

- [ ] Replace the function body (lines 911–1025) with:

```rust
    /// Initialize: scan $PATH for agents declared in the embedded seed
    /// template (`spur-acp::load_seed_template`), register those whose
    /// `command` is on $PATH.
    pub async fn init_agents(&mut self) -> Result<Vec<String>> {
        let seeds = spur_acp::config::load_seed_template();
        let mut found = Vec::new();
        for seed in seeds.entries {
            let ok = tokio::process::Command::new("which")
                .arg(&seed.command)
                .output()
                .await
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                info!(agent = %seed.name, command = %seed.command, "Found agent");
                found.push(seed.name.clone());
                self.registry.register(seed);
            }
        }
        Ok(found)
    }
```

This is 14 lines — under the 20-line success criterion.

### Step 2.3 — Unused imports

- [ ] After saving, `cargo build -p spur-core` will flag any unused imports (e.g. `TransportKind`, `AgentRole`, `CostTier` if this was the only site). Remove them. Run clippy to double-check.

### Step 2.4 — Grep-zero success criteria

- [ ] `grep -rn "SeedAgent" crates/` → 0 hits
- [ ] `grep -rn "known_agents" crates/` → 0 hits
- [ ] `grep -rn "skip_permissions_args" crates/spur-core/src/orchestrator.rs` — still 0 in the init path (the field migration happened in Spec 1 via `effective_permissions`)

### Step 2.5 — Workspace tests

- [ ] `cargo test --workspace --no-fail-fast`
Expected: green. The existing `cmd_init` round-trip (writing to `.spur/config.toml`) should still work end-to-end because `SpurConfig::agents.entries` is now populated with richer data.

### Step 2.6 — Commit

```bash
git add -u
git commit -m "refactor(spur-core): init_agents reads from embedded seed template, drop SeedAgent"
```

---

## Task 3: `init_agents()` behavior tests

**Why:** The refactor is nontrivial — swapping a hand-built struct for TOML parsing. Add tests that pin down the happy path and a "nothing on PATH" empty case. Use a temp-dir + `$PATH` override to avoid depending on the test machine.

**Files:**
- Modify or create: `crates/spur-core/tests/init_agents.rs` (prefer new integration test file)

### Step 3.1 — Write the tests

- [ ] Create `crates/spur-core/tests/init_agents.rs`:

```rust
//! Integration tests for `Orchestrator::init_agents()`.
//!
//! Uses a temp directory as an isolated $PATH so these tests don't
//! depend on what's installed on the developer's machine.

use spur_acp::config::SpurConfig;
use spur_core::Orchestrator;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use tempfile::TempDir;

/// Create an executable stub at `<dir>/<name>` that exits 0.
fn stub_binary(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

#[tokio::test]
async fn init_agents_finds_only_stubs_on_path() {
    let tmp = TempDir::new().unwrap();
    stub_binary(tmp.path(), "kiro-cli");
    // Deliberately NOT stubbing claude/codex/npx/gemini.

    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", tmp.path());
    let result = {
        let mut orch = Orchestrator::new(tmp.path().into(), SpurConfig::default()).unwrap();
        orch.init_agents().await.unwrap()
    };
    if let Some(p) = prev_path { std::env::set_var("PATH", p); }

    assert_eq!(result, vec!["kiro".to_string()],
        "only kiro-cli is stubbed, only kiro should be found");
}

#[tokio::test]
async fn init_agents_with_empty_path_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", tmp.path()); // empty dir
    let result = {
        let mut orch = Orchestrator::new(tmp.path().into(), SpurConfig::default()).unwrap();
        orch.init_agents().await.unwrap()
    };
    if let Some(p) = prev_path { std::env::set_var("PATH", p); }

    assert!(result.is_empty(), "nothing on PATH should find no agents, got {result:?}");
}

#[tokio::test]
async fn init_agents_registers_full_spec12_config() {
    // Proves seed agents carry commands/permissions/display blocks,
    // not just the 6 fields the old SeedAgent struct had.
    let tmp = TempDir::new().unwrap();
    stub_binary(tmp.path(), "kiro-cli");

    let prev_path = std::env::var_os("PATH");
    std::env::set_var("PATH", tmp.path());
    let registered = {
        let mut orch = Orchestrator::new(tmp.path().into(), SpurConfig::default()).unwrap();
        orch.init_agents().await.unwrap();
        orch.registry.list().into_iter().cloned().collect::<Vec<_>>()
    };
    if let Some(p) = prev_path { std::env::set_var("PATH", p); }

    let kiro = registered.iter().find(|a| a.name == "kiro")
        .expect("kiro should be registered");
    assert_eq!(kiro.commands.dispatch, spur_acp::DispatchKind::VendorExec);
    assert_eq!(kiro.commands.exec_method.as_deref(),
        Some("_kiro.dev/commands/execute"));
    assert!(!kiro.commands.ingest.is_empty(), "kiro should have ingest binding");
    assert!(!kiro.commands.response.is_empty(), "kiro should have response binding");
    assert_eq!(kiro.effective_permissions().args, vec!["--trust-all-tools"]);
}
```

Ensure `tempfile` is available for dev-dependencies in `spur-core/Cargo.toml`:

- [ ] Check `crates/spur-core/Cargo.toml` has `tempfile = { workspace = true }` under `[dev-dependencies]`. If missing, add it (it's already in workspace deps — other test files use it).

### Step 3.2 — Tests pass

- [ ] `cargo test -p spur-core --test init_agents`
Expected: 3 tests PASS. Platform note: these use `set_mode(0o755)` — Unix-only. If the workspace needs Windows support, gate with `#[cfg(unix)]`. Check existing test patterns in the crate; if other tests are already unix-only the precedent holds.

### Step 3.3 — Commit

```bash
git add crates/spur-core/tests/init_agents.rs crates/spur-core/Cargo.toml
git commit -m "test(spur-core): integration tests for init_agents PATH scan"
```

---

## Task 4: `.spur/config.toml.example` refresh + overwrite warning

**Why:** The example config should mirror the seed template so users hand-editing have a reliable reference. The `spur init` destructive-overwrite footgun gets worse with richer configs — add a warning comment at the top.

**Files:**
- Modify: `.spur/config.toml.example`

### Step 4.1 — Update the example

- [ ] Read current `.spur/config.toml.example`. It covers claude + kiro from the Spec 1+2 work. Extend it to cover the full 5 seed agents (kiro, claude-code, claude-code-acp, codex, gemini) by copying the relevant blocks from `seed_agents.toml`, then prepend a warning comment block:

```toml
# .spur/config.toml — per-repo spur configuration.
#
# IMPORTANT: `spur init` OVERWRITES this file with the seed template
# populated from agents found on $PATH. If you've hand-customized this
# config, edit it directly rather than re-running init.
#
# Reference: docs/spur/agent-config.md (schema)
# Cookbook: docs/spur/agent-onboarding-cookbook.md (how to add an agent)
```

Everything below the warning mirrors `seed_agents.toml`. Keep the codex block *uncommented* here (this is the user-facing example — they can comment out agents they don't want).

### Step 4.2 — Commit

```bash
git add .spur/config.toml.example
git commit -m "docs(spur): refresh config.toml.example to mirror seed template"
```

---

## Task 5: Onboarding cookbook

**Why:** Success criterion #6. The cookbook is what a user actually reads when they want to add a new agent — it's the framework's public surface.

**Files:**
- Create: `docs/spur/agent-onboarding-cookbook.md`

### Step 5.1 — Write the cookbook

- [ ] Create `docs/spur/agent-onboarding-cookbook.md`:

````markdown
# Agent Onboarding Cookbook

This guide walks through adding a new agent to spur in under 10 minutes.
No Rust required when the agent uses an existing transport + dispatch combination.

## Quick start

Open `.spur/config.toml`, add a `[[agents.entries]]` block, then run `spur config check` to validate.

If your agent is one spur knows about (kiro, claude-code, claude-code-acp, codex, gemini), `spur init` writes a matching block automatically when the binary is on `$PATH`. **`spur init` overwrites `.spur/config.toml`** — if you have customizations, edit by hand instead.

## Decision tree

### Step 1 — Choose a transport

| Your agent… | `transport =` | Example |
|---|---|---|
| Speaks ACP natively (has `--acp` or an ACP wrapper) | `"acp"` | kiro (`kiro-cli acp`), codex |
| Emits Claude-style stream-json on stdout | `"stream-json"` | `claude -p --output-format stream-json …` |
| Accepts a prompt on stdin, emits text on stdout | `"stdio"` | (rare — custom integrations) |
| Is a plain CLI that takes a prompt as trailing arg | `"cli-wrap"` | `gemini "your prompt here"` |

**How do I know which my agent speaks?** Run it with `--help`. If you see an ACP flag, pick `"acp"`. If the docs mention stream-json, pick `"stream-json"`. If it accepts a prompt as a positional argument, `"cli-wrap"`. When in doubt start with `"cli-wrap"` — it's the simplest and works for most CLIs.

### Step 2 — Choose a dispatch

| Your agent… | `[commands]` |
|---|---|
| Accepts `/slash` commands as ordinary prompt text | `dispatch = "prompt_text"` |
| Has a vendor-ext RPC that receives structured commands | `dispatch = "vendor_exec"` + `exec_method = "…"` |

Most agents use `prompt_text`. Use `vendor_exec` only if your agent exposes a custom JSON-RPC method for commands (kiro is the canonical example).

### Step 3 — Add static commands (optional)

Static commands are slash commands that appear in the `/` popup before your agent connects. Useful when your agent doesn't expose a discovery endpoint.

```toml
[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"

[[agents.entries.commands.static]]
name = "model"
description = "Switch model"
hint = "[model-name]"
```

If your agent later advertises commands at runtime via an `ingest` binding, the dynamic entries override statics on `(handle, name)` match.

### Step 4 — Add permissions (optional)

If your agent has a bypass-permissions flag or session mode:

```toml
[agents.entries.permissions]
args = ["--trust-all-tools"]         # CLI flag appended at spawn
session_mode = "bypassPermissions"   # ACP session mode set after new_session
```

Both fields are applied when `skip = true` (or via the legacy flat `skip_permissions = true`).

### Step 5 — Validate config shape

```
$ spur config check
✓ my-agent
```

This checks your agent block parses cleanly and the `vendor_exec` / `prompt_text` choices are consistent (e.g. `vendor_exec` requires `exec_method`). It does **not** boot your agent.

### Step 6 — Test at runtime

```
$ spur chat my-agent "say hello"
```

This launches the agent, opens a session, and sends one prompt. If it hangs or errors, the most likely causes are (a) wrong `command` / `args` — the binary can't be found or doesn't accept those flags; (b) wrong `transport` — the binary speaks a different dialect. Check the terminal for error output; spur logs to `.spur/logs/` for post-mortem.

## Worked example: codex

```toml
[[agents.entries]]
name = "codex"
command = "codex"
args = ["--acp"]
transport = "acp"
role = "both"
cost_tier = "low"

[agents.entries.display]
handle = "codex"

[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.static]]
name = "compact"
description = "Compact conversation history"
```

Zero Rust. `/compact` appears in the popup immediately — submitting it sends `"/compact"` as a plain text prompt to codex.

## Worked example: a hypothetical `my-agent`

Suppose `my-agent` is a Python CLI that accepts a prompt on stdin and emits text on stdout. It has no slash-command machinery, no ACP support.

```toml
[[agents.entries]]
name = "my-agent"
command = "my-agent"
args = ["--stream"]
transport = "stdio"
role = "worker"
cost_tier = "medium"

[agents.entries.display]
handle = "my"

[agents.entries.commands]
dispatch = "prompt_text"

[[agents.entries.commands.static]]
name = "help"
description = "Print agent help"
```

`spur chat my-agent "refactor foo.py"` now works. Users can type `/help` to send the literal text `/help` to the agent.

## Adding your agent to the seed template

Once your config works, contribute it back so other spur users get it via `spur init`:

1. Add an `[[agents.entries]]` block to `crates/spur-acp/src/seed_agents.toml`.
2. Run `cargo test -p spur-acp config::tests::seed_template` to confirm the compile-time parse + validator checks still pass.
3. Open a PR. No Rust changes are required.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `spur init` didn't register my agent | Binary not on `$PATH` (seed template only) — add entry manually. |
| Config check fails "vendor_exec requires exec_method" | Add `exec_method = "..."` under `[commands]`. |
| `/command` does nothing | `dispatch` mismatch — check your agent expects prompt_text or vendor_exec. |
| Agent errors "unknown flag: `--acp`" | Wrong `args` for your installed version — check `--help` and update. |

## Further reading

- `docs/spur/agent-config.md` — full schema reference.
- `docs/superpowers/specs/2026-04-14-agent-onboarding-roadmap.md` — design context.
````

### Step 5.2 — Commit

```bash
git add docs/spur/agent-onboarding-cookbook.md
git commit -m "docs(spur): agent-onboarding cookbook with decision tree and worked examples"
```

---

## Task 6: Spec-success-criteria gate

**Why:** The spec lists 8 objective criteria. Verify them all before declaring done.

- [ ] `grep -rn "SeedAgent" crates/` → 0 hits
- [ ] `grep -rn "known_agents" crates/` → 0 hits
- [ ] `wc -l` on `init_agents()` body: ≤20 lines (count from opening `{` to closing `}` exclusive)
- [ ] Codex block in `seed_agents.toml` has ≥1 `[[agents.entries.commands.static]]` — already asserted by a Task 1 test
- [ ] `docs/spur/agent-onboarding-cookbook.md` exists (Task 5)
- [ ] At least 5 `[[agents.entries]]` blocks in `seed_agents.toml` — already asserted by a Task 1 test
- [ ] `cargo test --workspace --no-fail-fast` green
- [ ] Manual: run `cargo run -p spur-cli -- init` in a scratch repo; confirm `.spur/config.toml` writes a valid config that passes `spur config check`

If all pass: no new commit needed — this task is a gate, not an edit.

---

## Self-review

1. **Placeholder scan:** No TBD / TODO / "add error handling" — specific code in every step.
2. **Type consistency:** `load_seed_template` signature `() -> AgentsConfig` matches between Task 1's test fixture and Task 2's call site. `seed.command` field referenced in Task 2 matches `AgentConfig.command: String` from Spec 1.
3. **Spec coverage check** (against `docs/superpowers/specs/2026-04-14-spec3-seed-template-codex-onboarding.md`):
   - §Pillar 1 (seed template) → Task 1.
   - §Pillar 2 (init_agents refactor) → Task 2.
   - §Pillar 3 (cookbook) → Task 5.
   - §Config example update → Task 4.
   - §Testing: compile-time parse → Task 1.1; 3 unit tests → Task 1.1 + Task 3; 2 validation tests → Task 1.1 (validator sweep) + Task 6 (manual round-trip).
   - §Success criteria 1–8 → Task 6.
   - **Gaps added by this plan (from MCTS review):** `toml` dep resolved by keeping parsing in spur-acp (Task 1 design choice); friendly panic message instead of `.expect()` (Task 1.3); overwrite warning in cookbook + example (Tasks 4, 5); `which`-mocking strategy via tempdir+`$PATH` override (Task 3); runtime-test Step 6 in cookbook (Task 5).
4. **DRY:** seed_agents.toml and config.toml.example have overlapping content. That's intentional — the example is user-facing docs, the seed is compile-time-embedded code. Drift risk is limited to the 5 seed blocks and is caught by a human reviewing the PR.

---

## Execution

This plan is ready for **superpowers:subagent-driven-development**. Recommended model: sonnet for all tasks (all are mechanical once the TOML content is given).

Task dependency graph:

- Task 1 → Task 2 → Task 3 (Task 3 validates Task 2's refactor)
- Task 1 → Task 4 (config example mirrors seed template)
- Task 5 is independent (cookbook writing)
- Task 6 runs last (gate)

Suggested order: 1 → 2 → 3 → 4 → 5 → 6.
