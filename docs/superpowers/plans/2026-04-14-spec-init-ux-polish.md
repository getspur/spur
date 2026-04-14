# Init UX Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `spur init` output to surface install hints, setup summary, and next-step guidance; add `--force` flag with overwrite guard; do not write config when no agents found.

**Architecture:**
- **Pillar 1** — Extend `DisplayConfig` with `install_hint: Option<String>`. Populate in `seed_agents.toml` for all 5 agents.
- **Pillar 2** — Rewrite `cmd_init` with 3 code paths (agents-found / empty-path / existing-config) and a `--force` flag. Setup summary derived from the resolved `SpurConfig`.

**Tech stack:** Rust, clap, serde TOML.

---

## Pre-flight

- [ ] On `main` with clean tree. Spec 3 commits up through `8d2205a` reachable.
- [ ] `grep -n "async fn cmd_init" crates/spur-cli/src/main.rs` returns one hit (~553).
- [ ] `spur init` currently prints the baseline output (see spec §Problem).

---

## Task 1: `install_hint` field + seed template hints

**Why:** Additive schema change. Tests for each seed having a hint are a forcing function for future contributors.

**Files:**
- Modify: `crates/spur-acp/src/config/entries.rs`
- Modify: `crates/spur-acp/src/seed_agents.toml`
- Modify: `crates/spur-acp/src/config/mod.rs` (append tests)

### Step 1.1 — Write the failing tests

- [ ] In `crates/spur-acp/src/config/mod.rs` inside the existing `#[cfg(test)] mod tests`, append:

```rust
    #[test]
    fn display_config_deserializes_install_hint() {
        let toml = r#"
            handle = "foo"
            install_hint = "brew install foo"
        "#;
        let dc: DisplayConfig = toml::from_str(toml).unwrap();
        assert_eq!(dc.install_hint.as_deref(), Some("brew install foo"));
    }

    #[test]
    fn every_seed_agent_has_install_hint() {
        let seeds = load_seed_template();
        for agent in &seeds.entries {
            assert!(
                agent.display.install_hint.is_some(),
                "seed agent `{}` is missing display.install_hint (required by Spec: init UX polish)",
                agent.name
            );
            let hint = agent.display.install_hint.as_ref().unwrap();
            assert!(
                !hint.trim().is_empty(),
                "seed agent `{}` has empty install_hint",
                agent.name
            );
        }
    }
```

- [ ] Run: `cargo test -p spur-acp install_hint` → FAIL (field undefined; seeds missing hints).

### Step 1.2 — Add the field to `DisplayConfig`

- [ ] In `crates/spur-acp/src/config/entries.rs`, extend `DisplayConfig` with:

```rust
    /// Human-readable command to install this agent (e.g. `brew install
    /// kiro-cli`). Surfaced by `spur init` when the agent is in the
    /// seed template but not on $PATH. Keep terse — one line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
```

No changes to `#[derive]` — the existing `Default` + `Serialize` + `Deserialize` handle the new optional field.

### Step 1.3 — Populate `install_hint` for each seed agent

- [ ] In `crates/spur-acp/src/seed_agents.toml`, add `install_hint` to each `[agents.entries.display]` block:

```toml
# kiro
[agents.entries.display]
handle = "kiro"
install_hint = "brew install kiro-cli"

# claude-code
[agents.entries.display]
handle = "claude-sj"
install_hint = "npm install -g @anthropic-ai/claude-code"

# claude-code-acp
[agents.entries.display]
handle = "claude"
install_hint = "npm install -g npx   # then re-run `spur init`"

# codex
[agents.entries.display]
handle = "codex"
install_hint = "https://docs.openai.com/codex/install"

# gemini
[agents.entries.display]
handle = "gemini"
install_hint = "npm install -g @google/gemini-cli"
```

Preserve the existing handles exactly. Implementer should spot-check each install command against the upstream project's README; if any is wrong (e.g. package renamed), fix it inline and note in the commit message. Best-effort — these will drift and that's OK.

### Step 1.4 — Tests pass

- [ ] Run: `cargo test -p spur-acp install_hint` — both tests PASS.
- [ ] Run: `cargo test -p spur-acp --no-fail-fast` — all green (including the existing `seed_template_*` tests, which still pass because `install_hint` is additive).

### Step 1.5 — Commit

```bash
git add crates/spur-acp/src/config/entries.rs \
        crates/spur-acp/src/config/mod.rs \
        crates/spur-acp/src/seed_agents.toml
git commit -m "feat(spur-acp): DisplayConfig.install_hint + seed-template hints"
```

---

## Task 2: `cmd_init` rewrite with `--force`, summary, next-step

**Why:** Core of this spec. Replace a ~35-line function with a ~130-line version (including helpers) that handles the 3 code paths cleanly.

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

### Step 2.1 — Extend `Commands::Init` with `--force`

- [ ] Find `Commands::Init` around `crates/spur-cli/src/main.rs:44`. Change:

```rust
    /// Initialize SPUR: detect agents, create config
    Init,
```

to:

```rust
    /// Initialize SPUR: detect agents, create config
    Init {
        /// Overwrite existing .spur/config.toml even if it exists.
        #[arg(long)]
        force: bool,
    },
```

- [ ] Update the dispatch at `crates/spur-cli/src/main.rs:163`:

```rust
        Commands::Init { force } => cmd_init(repo_root, force).await,
```

### Step 2.2 — Rewrite `cmd_init`

- [ ] Replace the existing `cmd_init` function body (lines ~553–588) with the block below. The 3 helper functions live right below `cmd_init` in the same file.

```rust
async fn cmd_init(repo_root: PathBuf, force: bool) -> Result<()> {
    let config_path = repo_root.join(".spur").join("config.toml");

    // Path C: existing config + no --force → refuse and guide.
    if config_path.exists() && !force {
        let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
        let entry_count = existing.matches("[[agents.entries]]").count();
        println!(
            "[spur] .spur/config.toml already exists with {entry_count} entr{}.",
            if entry_count == 1 { "y" } else { "ies" }
        );
        println!(
            "[spur] To overwrite (losing any customizations), run `spur init --force`."
        );
        println!(
            "[spur] To re-scan without writing, run `spur agents detect`."
        );
        return Ok(());
    }

    println!("[spur] Scanning agents on $PATH...");
    println!();

    let mut orch = Orchestrator::new(repo_root.clone(), SpurConfig::default())?;
    let found_names = orch.init_agents().await?;

    let seed = spur_acp::config::load_seed_template();
    let total_seed = seed.entries.len();
    let registered: Vec<spur_acp::config::AgentConfig> = orch
        .registry
        .list()
        .into_iter()
        .cloned()
        .collect();

    // Found block.
    println!(
        "Found {} of {} known agents:",
        registered.len(),
        total_seed
    );
    for agent in &registered {
        let args_display = if agent.args.is_empty() {
            String::new()
        } else {
            format!(" {}", agent.args.join(" "))
        };
        let command_col = truncate(&format!("{}{}", agent.command, args_display), 45);
        println!(
            "  ✓ {:<18}{:<45}   ({})",
            agent.name,
            command_col,
            role_label(&agent.role),
        );
    }

    // Skipped block.
    let registered_names: std::collections::HashSet<_> =
        found_names.iter().cloned().collect();
    let skipped: Vec<_> = seed.entries.iter()
        .filter(|a| !registered_names.contains(&a.name))
        .collect();
    if !skipped.is_empty() {
        println!();
        println!("Skipped {} (not installed):", skipped.len());
        for agent in &skipped {
            let hint = agent.display.install_hint
                .as_deref()
                .unwrap_or("see docs");
            println!("  ✗ {:<18}install: {}", agent.name, hint);
        }
    }

    // Path B: zero agents → no write.
    if registered.is_empty() {
        println!();
        println!("No config written. Re-run `spur init` after installing an agent.");
        println!();
        println!("To use a custom agent spur doesn't know about yet:");
        println!("  docs/spur/agent-onboarding-cookbook.md");
        return Ok(());
    }

    // Path A: write config.
    let mut persist_config = SpurConfig::default();
    persist_config.agents.entries = registered.clone();

    let config_dir = repo_root.join(".spur");
    std::fs::create_dir_all(&config_dir)?;
    let toml_str = toml::to_string_pretty(&persist_config)?;
    std::fs::write(&config_path, toml_str)?;

    println!();
    println!("Config written to {}.", config_path.display());

    print_setup_summary(&persist_config);

    println!();
    println!("Next step: try one of");
    println!("  spur run \"describe the repo in 3 bullets\"");
    println!("  spur watch                                   # interactive TUI");
    println!("  spur agents show <name>                      # see full config for one agent");
    println!("  spur config check                            # validate your setup");

    Ok(())
}

/// Truncate to `max` chars, appending `...` if truncated. Returns
/// exactly `max` chars or fewer.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn role_label(role: &spur_acp::types::AgentRole) -> &'static str {
    use spur_acp::types::AgentRole::*;
    match role {
        Brain => "brain",
        Worker => "worker",
        Both => "brain + worker",
    }
}

fn print_setup_summary(cfg: &SpurConfig) {
    println!();
    println!("Setup summary:");
    println!("  Brain agent:      {}", cfg.brain.default);
    if !cfg.brain.fallback.is_empty() {
        println!("  Brain fallbacks:  {}", cfg.brain.fallback.join(", "));
    }
    let bypass_agents: Vec<_> = cfg.agents.entries.iter()
        .filter(|a| a.effective_permissions().skip)
        .map(|a| a.name.as_str())
        .collect();
    let bypass_line = if bypass_agents.is_empty() {
        "safety-default (bypass disabled for all agents)".to_string()
    } else {
        format!("bypass enabled for: {}", bypass_agents.join(", "))
    };
    println!("  Permissions:      {}", bypass_line);
    println!("  Session logs:     .spur/logs/");
    println!("  Cost tracking:    enabled — run `spur cost` after your first task");
}
```

### Step 2.3 — Check imports

- [ ] Ensure `SpurConfig` and `Orchestrator` imports are still in scope at top of `main.rs`. Ensure `spur_acp::config` and `spur_acp::types::AgentRole` are reachable (import if needed).
- [ ] `cargo build -p spur-cli` — expect clean.

### Step 2.4 — Quick smoke test (manual)

- [ ] In a scratch dir:

```bash
cd /tmp && rm -rf spurtest && mkdir spurtest && cd spurtest
PATH="/usr/bin" cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml -p spur-cli -- init
```

Expect Path B (empty-path) output. Verify no `.spur/config.toml` created.

- [ ] Then simulate one agent present:

```bash
echo '#!/bin/sh' > /tmp/spurtest/claude
chmod +x /tmp/spurtest/claude
PATH="/tmp/spurtest:/usr/bin" cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml -p spur-cli -- init
```

Expect Path A output: found 1 of 5, claude-code registered, setup summary, next-step block. Verify `.spur/config.toml` created.

- [ ] Re-run the same command — expect Path C overwrite-guard output. Re-run with `--force` — expect Path A again, file rewritten.

### Step 2.5 — Commit

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): init UX — install hints, setup summary, overwrite guard, --force"
```

---

## Task 3: Integration tests for the three paths

**Why:** Regression guards for the new output paths and `--force` semantics. Uses the same `tempfile + $PATH + stub_binary` pattern as `crates/spur-core/tests/init_agents.rs`.

**Files:**
- Create: `crates/spur-cli/tests/init_ux.rs`

### Step 3.1 — Check Cargo.toml dev-deps

- [ ] In `crates/spur-cli/Cargo.toml`, confirm `[dev-dependencies]` includes `tempfile` and `assert_cmd`. If `assert_cmd` is missing, add:

```toml
assert_cmd = "2"
```

(Rationale: `assert_cmd` runs the binary and captures stdout/stderr — cleanest way to assert on printed output. Alternative: refactor `cmd_init` to take an `&mut dyn io::Write` and inject a buffer — cleaner architecturally but doubles the touch surface. Use `assert_cmd` unless the implementer sees a reason to refactor.)

### Step 3.2 — Write the tests

- [ ] Create `crates/spur-cli/tests/init_ux.rs`:

```rust
//! Integration tests for `spur init` UX: install hints, setup summary,
//! next-step block, and --force overwrite guard.

#![cfg(unix)]

use assert_cmd::Command;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that mutate $PATH via `Command::env`. Not strictly
/// required (assert_cmd spawns a subprocess with its own env), but we
/// also touch filesystem state under the tempdir and want deterministic
/// logs. Keep the Mutex around for future tests.
static LOCK: Mutex<()> = Mutex::new(());

fn stub_binary(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn spur() -> Command {
    Command::cargo_bin("spur").expect("spur binary built")
}

#[test]
fn init_with_zero_agents_writes_no_config() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();

    let output = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .output()
        .expect("spur init ran");

    assert!(output.status.success(), "spur init should succeed even with no agents");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("No agents found") || stdout.contains("No config written"),
        "empty-path output missing; got: {stdout}");
    assert!(stdout.contains("Install at least one") || stdout.contains("install:"),
        "install hints missing; got: {stdout}");
    assert!(!tmp.path().join(".spur/config.toml").exists(),
        "spur init with zero agents must NOT write .spur/config.toml");
}

#[test]
fn init_with_one_agent_writes_config_and_prints_summary() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    stub_binary(tmp.path(), "kiro-cli");

    let output = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .output()
        .expect("spur init ran");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for needle in [
        "Found 1 of",       // found block
        "kiro",             // found agent
        "Skipped",          // skipped block
        "install:",         // install hint for a skipped agent
        "Config written",   // write confirmation
        "Setup summary:",   // summary block header
        "Brain agent:",     // summary content
        "Permissions:",
        "safety-default",   // the P5 enterprise line
        "Next step:",       // next-step block
        "spur run",         // suggested command
        "spur watch",
    ] {
        assert!(stdout.contains(needle),
            "missing `{needle}` in output; got:\n{stdout}");
    }
    assert!(tmp.path().join(".spur/config.toml").exists(),
        "config file must be written on happy path");
}

#[test]
fn init_with_existing_config_requires_force() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    let existing = "# pre-existing\n[[agents.entries]]\nname=\"custom\"\ncommand=\"foo\"\ntransport=\"acp\"\n";
    fs::write(tmp.path().join(".spur/config.toml"), existing).unwrap();
    stub_binary(tmp.path(), "kiro-cli");

    let output = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .output()
        .expect("spur init ran");

    assert!(output.status.success(), "refusing to overwrite should exit 0, not error");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("already exists"),
        "overwrite-guard message missing; got:\n{stdout}");
    assert!(stdout.contains("--force"),
        "overwrite-guard should mention --force; got:\n{stdout}");
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();
    assert_eq!(after, existing, "config must NOT be modified without --force");
}

#[test]
fn init_with_force_overwrites_existing_config() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    fs::write(tmp.path().join(".spur/config.toml"), "# will be overwritten\n").unwrap();
    stub_binary(tmp.path(), "kiro-cli");

    let output = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .args(["init", "--force"])
        .output()
        .expect("spur init --force ran");

    assert!(output.status.success());
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();
    assert!(!after.contains("# will be overwritten"),
        "--force must overwrite the old file");
    assert!(after.contains("name = \"kiro\""),
        "new file must contain the registered agent");
}
```

### Step 3.3 — Run tests

- [ ] `cargo test -p spur-cli --test init_ux`
Expected: 4 tests PASS. The first run may be slow (builds the binary).

### Step 3.4 — Commit

```bash
git add crates/spur-cli/tests/init_ux.rs crates/spur-cli/Cargo.toml
git commit -m "test(spur-cli): integration tests for init UX (3 paths + --force)"
```

---

## Task 4: Docs updates

**Why:** Keep `docs/spur/agent-config.md`, the cookbook, and `.spur/config.toml.example` in sync with the new `install_hint` field.

**Files:**
- Modify: `docs/spur/agent-config.md`
- Modify: `docs/spur/agent-onboarding-cookbook.md`
- Modify: `.spur/config.toml.example`

### Step 4.1 — `agent-config.md`

- [ ] Locate the `DisplayConfig` section. Add a row or subsection:

```markdown
| `install_hint` | `String` | No | Human-readable install command (e.g. `brew install kiro-cli`). Surfaced by `spur init` when the agent is in the seed template but not on $PATH. |
```

### Step 4.2 — `agent-onboarding-cookbook.md`

- [ ] In the "Adding your agent to the seed template" section, add a bullet:

```markdown
4. Add an `install_hint` to `[agents.entries.display]` — a terse one-liner telling new users how to install your agent. Example: `install_hint = "brew install kiro-cli"`.
```

### Step 4.3 — `.spur/config.toml.example`

- [ ] Add `install_hint = "…"` to each `[agents.entries.display]` block, mirroring the seed template values.

### Step 4.4 — Commit

```bash
git add docs/spur/agent-config.md docs/spur/agent-onboarding-cookbook.md .spur/config.toml.example
git commit -m "docs(spur): document install_hint field + sync config example"
```

---

## Task 5: Success-criteria gate

- [ ] `cargo test --workspace --no-fail-fast` — green.
- [ ] `cargo test -p spur-acp install_hint` — 2 tests pass.
- [ ] `cargo test -p spur-cli --test init_ux` — 4 tests pass.
- [ ] Manual smoke test per spec §Success criteria item 7:
  1. `rm -rf /tmp/spurtest && mkdir /tmp/spurtest`
  2. `cd /tmp/spurtest && PATH=/usr/bin spur init` — observe install hints + "No config written".
  3. Stub one agent (`touch /tmp/spurtest/claude && chmod +x /tmp/spurtest/claude`).
  4. `PATH=/tmp/spurtest:/usr/bin spur init` — observe Setup summary + Next step.
  5. `spur init` again — observe overwrite guard.
  6. `spur init --force` — observe overwrite.

No new commit; this is a gate.

---

## Self-review

1. **Placeholder scan:** No TBD/TODO/"add error handling" — every step has concrete code.
2. **Type consistency:** `install_hint: Option<String>` added to `DisplayConfig` in Task 1 is read in Task 2's `print_skipped` block as `agent.display.install_hint.as_deref().unwrap_or("see docs")` — types line up.
3. **Spec coverage:**
   - §Pillar 1 (install_hint field) → Task 1.
   - §Pillar 2 (cmd_init rewrite) → Task 2.
   - §Pillar 3 (output formats) → Task 2 (all three paths implemented).
   - §Pillar 4 (--force flag) → Task 2.1 + Task 2.2 (Path C logic).
   - §Pillar 5 (setup summary) → `print_setup_summary` in Task 2.
   - §Testing unit × 2 → Task 1.1.
   - §Testing integration × 4 → Task 3.2.
   - §Affected files (docs) → Task 4.
   - §Success criteria 1–6 → Task 5.
4. **YAGNI check:** no `spur agents detect` subcommand implemented (Path C references it as future guidance); no interactive prompts; no upgrade-check logic. Out-of-scope items stay out.

---

## Execution

Ready for **superpowers:subagent-driven-development**. Recommended model: sonnet throughout.

Task dependency graph:
- Task 1 → Task 2 (cmd_init reads install_hint)
- Task 2 → Task 3 (tests cover new behavior)
- Task 2 → Task 4 (example config needs install_hint once schema is updated)
- Task 5 gates 1–4.

Suggested order: 1 → 2 → 3 → 4 → 5.
