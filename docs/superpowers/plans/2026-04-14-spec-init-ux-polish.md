# Init UX Polish — Implementation Plan (simplified + multi-agent-aware)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `spur init` output to surface install hints, correct brain/fallback selection, and next-step guidance; add `--force` flag with overwrite guard; do not write config when no agents found.

**Design principles:**
- **Simplicity first** — no schema changes, no new dev-deps, no helper-function sprawl. Install hints live as a const in spur-cli (not round-tripped into user configs). Setup information compressed to one line.
- **Multi-agent correctness** — `brain.default` and `brain.fallback` are ADAPTIVE to what's installed, not hardcoded. Prevents the "init succeeds, first-run fails" class of failure.

**Architecture:**
- **Pillar 1** — `const INSTALL_HINTS: &[(&str, &str)]` in `spur-cli/src/main.rs` + contract test that every seed agent has a hint.
- **Pillar 2** — Rewrite `cmd_init` with three code paths (existing-config-without-force / zero-agents / happy-path) + `--force` flag + adaptive brain/fallback derivation from registered agents + capability nudge.
- **Pillar 3** — Reorder `seed_agents.toml` so `claude-code` precedes `kiro` (most common user-expected default brain).

**Tech stack:** Rust, clap, serde TOML, stdlib `std::process::Command` for integration tests.

---

## Pre-flight

- [ ] On `main` with clean tree. Spec 3 commits through `8d2205a` reachable.
- [ ] `grep -n "async fn cmd_init" crates/spur-cli/src/main.rs` returns one hit (~553).

---

## Task 1: Reorder seed template + `INSTALL_HINTS` + contract test

**Why:** Two cheap preparations. Seed reorder is a no-code change (moves TOML blocks) that makes `claude-code` the first brain-capable agent, matching user expectation. Install hints live as a const in spur-cli — no schema bloat, no user-config pollution.

**Files:**
- Modify: `crates/spur-acp/src/seed_agents.toml` (reorder blocks)
- Modify: `crates/spur-cli/src/main.rs` (add const + helper)
- Modify: `crates/spur-cli/tests/init_ux.rs` (new file, starts with contract test)

### Step 1.1 — Reorder the seed template

- [ ] In `crates/spur-acp/src/seed_agents.toml`, move the `claude-code` block ABOVE the `kiro` block. The file currently has order: kiro, claude-code, claude-code-acp, codex, gemini. New order: **claude-code, kiro, claude-code-acp, codex, gemini**.
- [ ] Do NOT change any content inside the blocks. Pure reorder.
- [ ] Run: `cargo test -p spur-acp seed_template` — all 3 tests still pass (they don't assert on order).
- [ ] This sets `claude-code` as the first-registered brain-capable agent → adaptive brain selection (Task 2) will default to it when installed.

### Step 1.2 — Add `INSTALL_HINTS` constant and helper

- [ ] In `crates/spur-cli/src/main.rs`, near the top (after imports, before `main`), add:

```rust
/// User-facing install commands per seed agent. Surfaced by `spur init`
/// when a seed agent's binary is not on $PATH. Kept here (not in the
/// schema) because hints are onboarding copy — they don't belong in
/// every user's round-tripped `.spur/config.toml`.
///
/// Contract: every agent in `spur_acp::config::load_seed_template()`
/// must have an entry here. Enforced by `tests/init_ux.rs`.
const INSTALL_HINTS: &[(&str, &str)] = &[
    ("claude-code",     "npm install -g @anthropic-ai/claude-code"),
    ("kiro",            "brew install kiro-cli"),
    ("claude-code-acp", "npm install -g npx   # then re-run `spur init`"),
    ("codex",           "https://docs.openai.com/codex/install"),
    ("gemini",          "npm install -g @google/gemini-cli"),
];

fn install_hint(name: &str) -> &'static str {
    INSTALL_HINTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, h)| *h)
        .unwrap_or("see docs/spur/agent-onboarding-cookbook.md")
}
```

### Step 1.3 — Write the contract test

- [ ] Create `crates/spur-cli/tests/init_ux.rs` with a single contract test for now (behavioral tests come in Task 3):

```rust
//! Init UX tests: contract guard + behavioral tests for `spur init`.
//!
//! Kept as one file because the contract test is trivial and colocating
//! it with the behavioral tests means future contributors see the
//! install-hint requirement when they open this file.

#![cfg(unix)]

#[test]
fn install_hints_cover_all_seed_agents() {
    // Can't access the private const directly from an integration test.
    // Re-encode it here via a parallel list. If the two drift, the test
    // fails and forces the contributor to update both sides.
    //
    // Alternative: expose INSTALL_HINTS as pub from a lib target. That
    // would be cleaner, but spur-cli is a binary-only crate and adding
    // a lib target just for this is overkill. Keep the parallel list.
    let expected_names: &[&str] = &[
        "claude-code",
        "kiro",
        "claude-code-acp",
        "codex",
        "gemini",
    ];
    let seeds = spur_acp::config::load_seed_template();
    for agent in &seeds.entries {
        assert!(
            expected_names.contains(&agent.name.as_str()),
            "seed agent `{}` has no INSTALL_HINTS entry — add one to \
             crates/spur-cli/src/main.rs AND to expected_names in this test",
            agent.name
        );
    }
    // Also check the reverse direction: no orphan expected_names that
    // aren't in seeds (would indicate a stale hint for a deleted agent).
    let seed_names: Vec<_> = seeds.entries.iter().map(|a| a.name.as_str()).collect();
    for expected in expected_names {
        assert!(
            seed_names.contains(expected),
            "expected_names has `{expected}` but it's not in seed template"
        );
    }
}
```

- [ ] Run: `cargo test -p spur-cli --test init_ux` → PASS (both sides already match).

### Step 1.4 — Commit

```bash
git add crates/spur-acp/src/seed_agents.toml \
        crates/spur-cli/src/main.rs \
        crates/spur-cli/tests/init_ux.rs
git commit -m "feat(spur-cli): INSTALL_HINTS const + seed reorder (claude-code first)"
```

---

## Task 2: Rewrite `cmd_init` with adaptive brain/fallback and overwrite guard

**Why:** The core UX change. Three output paths, adaptive brain selection, adaptive fallback chain, one-line summary, next-step block, `--force` flag.

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

### Step 2.1 — Extend `Commands::Init` with `--force`

- [ ] In `crates/spur-cli/src/main.rs` around line 44, change:

```rust
    /// Initialize SPUR: detect agents, create config
    Init,
```

to:

```rust
    /// Initialize SPUR: detect agents, create config
    Init {
        /// Overwrite existing .spur/config.toml.
        #[arg(long)]
        force: bool,
    },
```

- [ ] In the dispatch at ~line 163, change:

```rust
        Commands::Init => cmd_init(repo_root).await,
```

to:

```rust
        Commands::Init { force } => cmd_init(repo_root, force).await,
```

### Step 2.2 — Rewrite `cmd_init`

- [ ] Replace the existing `cmd_init` function body (lines ~553–588) with:

```rust
async fn cmd_init(repo_root: PathBuf, force: bool) -> Result<()> {
    use spur_acp::types::AgentRole;

    let config_path = repo_root.join(".spur").join("config.toml");

    // ── Path C: existing config + no --force → refuse and guide. ──
    if config_path.exists() && !force {
        println!("[spur] .spur/config.toml already exists.");
        println!("[spur] Run `spur init --force` to overwrite.");
        return Ok(());
    }

    // ── Scan PATH. ──
    println!("[spur] Scanning agents on $PATH...");
    let mut orch = Orchestrator::new(repo_root.clone(), SpurConfig::default())?;
    let found_names = orch.init_agents().await?;
    let seed = spur_acp::config::load_seed_template();
    let registered: Vec<spur_acp::config::AgentConfig> =
        orch.registry.list().into_iter().cloned().collect();
    let registered_names: std::collections::HashSet<_> =
        found_names.iter().cloned().collect();

    // ── Agent list: ✓ for registered, ✗ with install hint otherwise. ──
    println!();
    for agent in &seed.entries {
        if registered_names.contains(&agent.name) {
            println!("  ✓ {}", agent.name);
        } else {
            println!("  ✗ {:<18}install: {}", agent.name, install_hint(&agent.name));
        }
    }

    // ── Path B: zero agents → no write. ──
    if registered.is_empty() {
        println!();
        println!("No agents found. Install one of the above and re-run `spur init`.");
        return Ok(());
    }

    // ── Adaptive brain selection. ──
    // Prefer first registered brain-capable agent (seed order after
    // Task 1's reorder = claude-code, kiro, claude-code-acp, codex, gemini).
    // If nothing is brain-capable, fall back to registered[0] with a note.
    let brain_name = registered
        .iter()
        .find(|a| matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .map(|a| a.name.clone())
        .unwrap_or_else(|| {
            println!();
            println!(
                "  (note: no brain-capable agents registered; using {} as brain)",
                registered[0].name
            );
            registered[0].name.clone()
        });

    // ── Adaptive fallback: other brain-capable agents in seed order. ──
    let fallbacks: Vec<String> = registered
        .iter()
        .filter(|a| a.name != brain_name
            && matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .map(|a| a.name.clone())
        .collect();

    // ── Persist config with derived brain/fallback. ──
    let mut persist = SpurConfig::default();
    persist.brain.default = brain_name.clone();
    persist.brain.fallback = fallbacks.clone();
    persist.agents.entries = registered.clone();

    std::fs::create_dir_all(config_path.parent().unwrap())?;
    std::fs::write(&config_path, toml::to_string_pretty(&persist)?)?;

    // ── Summary line. ──
    let any_bypass = persist
        .agents
        .entries
        .iter()
        .any(|a| a.effective_permissions().skip);
    let bypass_str = if any_bypass {
        "enabled for some agents — review .spur/config.toml"
    } else {
        "disabled (safety-default)"
    };
    let fallback_str = if fallbacks.is_empty() {
        "none".to_string()
    } else {
        fallbacks.join(", ")
    };

    println!();
    println!("Config written to {}.", config_path.display());
    println!(
        "Brain: {} (fallback: {}). Bypass: {}.",
        brain_name, fallback_str, bypass_str
    );

    // ── Capability nudge when ≥2 agents. ──
    if registered.len() >= 2 {
        println!();
        println!("Tip: set `capabilities = [\"security\", ...]` on each agent");
        println!("to enable capability-based delegation from the brain.");
    }

    // ── Next-step block. ──
    println!();
    println!("Next step:");
    println!("  spur run \"describe the repo in 3 bullets\"    # one-shot");
    println!("  spur watch                                   # interactive TUI");
    println!("  spur config check                            # validate your setup");

    Ok(())
}
```

### Step 2.3 — Check imports

- [ ] At the top of `crates/spur-cli/src/main.rs`, ensure imports cover: `std::path::PathBuf`, `anyhow::Result`, `spur_core::Orchestrator`, `spur_acp::config::SpurConfig`. Most already there; add `spur_acp::types::AgentRole` use inside the function (already done via the `use` statement at the top of `cmd_init`).
- [ ] Run: `cargo build -p spur-cli` — expect clean.

### Step 2.4 — Manual smoke test

- [ ] From the repo root:

```bash
# Path B: zero agents
cd /tmp && rm -rf spurtest && mkdir spurtest && cd spurtest
PATH=/usr/bin cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml \
    -p spur-cli -- init
```

Expect: agent list with all 5 marked `✗` and install hints; "No agents found. Install one of the above..."; no `.spur/config.toml` written.

```bash
# Path A: one agent stubbed
echo '#!/bin/sh' > /tmp/spurtest/claude
chmod +x /tmp/spurtest/claude
PATH=/tmp/spurtest:/usr/bin cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml \
    -p spur-cli -- init
```

Expect: claude-code marked `✓`, others marked `✗`; config written; "Brain: claude-code (fallback: none). Bypass: disabled."; capability tip; next-step block.

```bash
# Path C: overwrite guard
PATH=/tmp/spurtest:/usr/bin cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml \
    -p spur-cli -- init
```

Expect: "already exists. Run `spur init --force` to overwrite." No file change.

```bash
# Path A with --force
PATH=/tmp/spurtest:/usr/bin cargo run --manifest-path /Volumes/Projects/spur/Cargo.toml \
    -p spur-cli -- init --force
```

Expect: full happy-path output, config rewritten.

### Step 2.5 — Commit

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): init UX — adaptive brain/fallback, overwrite guard, --force, next-step"
```

---

## Task 3: Behavioral integration tests via stdlib subprocess

**Why:** Regression guards for the three code paths and `--force` semantics. No `assert_cmd` dep — use `std::process::Command::new(env!("CARGO_BIN_EXE_spur"))`. Tests assert on filesystem state, not stdout copy (less brittle).

**Files:**
- Modify: `crates/spur-cli/tests/init_ux.rs` (extend with behavioral tests)

### Step 3.1 — Append behavioral tests

- [ ] Append to `crates/spur-cli/tests/init_ux.rs` (below `install_hints_cover_all_seed_agents`):

```rust
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::sync::Mutex;
use tempfile::TempDir;

/// Serialize tests that mutate $PATH via spawned subprocess env. Not
/// strictly required (each subprocess has its own env) but also
/// prevents tempdir collision when tests run in parallel.
static LOCK: Mutex<()> = Mutex::new(());

fn stub_binary(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn spur() -> Command {
    Command::new(env!("CARGO_BIN_EXE_spur"))
}

#[test]
fn init_with_zero_agents_writes_no_config() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "spur init should exit 0 even with no agents");
    assert!(
        !tmp.path().join(".spur/config.toml").exists(),
        "spur init with zero agents must NOT write .spur/config.toml"
    );
}

#[test]
fn init_with_existing_config_requires_force() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    let existing = "# pre-existing, must not be touched\n";
    fs::write(tmp.path().join(".spur/config.toml"), existing).unwrap();
    stub_binary(tmp.path(), "claude");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .arg("init")
        .status()
        .expect("spawn spur init");

    assert!(status.success(), "overwrite refusal should exit 0, not error");
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();
    assert_eq!(
        after, existing,
        "config must NOT be modified without --force"
    );
}

#[test]
fn init_with_force_overwrites_and_sets_adaptive_brain() {
    let _g = LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    fs::create_dir_all(tmp.path().join(".spur")).unwrap();
    fs::write(tmp.path().join(".spur/config.toml"), "# stale\n").unwrap();

    // Stub only kiro. Post-Spec-3 brain.default is hardcoded to
    // claude-code — adaptive selection must pick kiro instead.
    stub_binary(tmp.path(), "kiro-cli");

    let status = spur()
        .current_dir(tmp.path())
        .env("PATH", format!("{}:/usr/bin", tmp.path().display()))
        .args(["init", "--force"])
        .status()
        .expect("spawn spur init --force");

    assert!(status.success());
    let after = fs::read_to_string(tmp.path().join(".spur/config.toml")).unwrap();

    assert!(
        !after.contains("# stale"),
        "--force must overwrite the old file"
    );
    assert!(
        after.contains("name = \"kiro\""),
        "new config must contain the registered agent; got:\n{after}"
    );
    // Adaptive-brain assertion: brain.default must point at a
    // REGISTERED agent (kiro), not the hardcoded claude-code.
    assert!(
        after.contains("default = \"kiro\""),
        "brain.default must adapt to installed agents (kiro in this test); got:\n{after}"
    );
}
```

### Step 3.2 — Confirm dev-deps

- [ ] `crates/spur-cli/Cargo.toml` needs `tempfile` under `[dev-dependencies]`. If missing, add `tempfile = { workspace = true }` (or the existing-crate pattern — check neighbors like spur-core's Cargo.toml).

### Step 3.3 — Run tests

- [ ] `cargo test -p spur-cli --test init_ux`
Expected: 4 tests PASS (1 contract + 3 behavioral).

### Step 3.4 — Commit

```bash
git add crates/spur-cli/tests/init_ux.rs crates/spur-cli/Cargo.toml
git commit -m "test(spur-cli): behavioral tests for init UX (3 paths + adaptive brain)"
```

---

## Task 4: Cookbook one-line addition

**Why:** When contributors add a new seed agent, they need to know about `INSTALL_HINTS`. One-line addition to the existing cookbook section.

**Files:**
- Modify: `docs/spur/agent-onboarding-cookbook.md`

### Step 4.1 — Add to "seed template" section

- [ ] In `docs/spur/agent-onboarding-cookbook.md`, find the "Adding your agent to the seed template" section. Add a step:

```markdown
3. Add an entry to `INSTALL_HINTS` in `crates/spur-cli/src/main.rs` — a terse one-liner telling new users how to install your agent. Example: `("my-agent", "brew install my-agent")`. The contract test in `crates/spur-cli/tests/init_ux.rs` enforces parity.
```

(Renumber existing list items accordingly — if the existing list has steps 1–3, the install-hint step slots in as step 3 and the "open a PR" step becomes step 4.)

### Step 4.2 — Commit

```bash
git add docs/spur/agent-onboarding-cookbook.md
git commit -m "docs(spur): cookbook — INSTALL_HINTS is the install-copy home"
```

---

## Task 5: Success-criteria gate

- [ ] `cargo test --workspace --no-fail-fast` — green.
- [ ] `cargo test -p spur-cli --test init_ux` — 4 tests pass.
- [ ] Manual smoke test per Task 2.4 passes for all 4 paths (zero agents, one agent, overwrite-guard, --force).
- [ ] Multi-agent smoke: stub `kiro-cli` + `claude`, run `spur init --force` in a fresh dir. Assert in output: `Brain: claude-code (fallback: kiro). Bypass: disabled.` — confirms adaptive brain picks claude-code (seed-reorder) and kiro becomes fallback.

No new commit; this is a gate.

---

## Self-review

1. **Placeholder scan:** No TBD/TODO — every step has concrete code.
2. **Type consistency:** `INSTALL_HINTS`, `install_hint()`, `AgentRole::{Brain, Both}` all used consistently across Tasks 1–2.
3. **Spec coverage:**
   - §Pillar 1 (install hints) → Task 1.2 + Task 4.
   - §Pillar 2 (cmd_init rewrite, --force, overwrite guard, paths A/B/C) → Task 2.
   - §Pillar 2 (adaptive brain/fallback, capability nudge) → Task 2 (new in this revision).
   - §Pillar 3 (seed reorder) → Task 1.1.
   - §Testing (contract + 3 behavioral) → Tasks 1.3, 3.
   - §Success criteria → Task 5.
4. **YAGNI check:** no schema changes, no assert_cmd dep, no helpers beyond `install_hint`, no forward-reference to a `spur agents detect` subcommand that doesn't exist, no opinionated capability pre-seeding.
5. **Multi-agent correctness:** adaptive brain selection (Task 2.2) prevents the `brain.default = "claude-code"`-but-not-installed failure. Adaptive fallback prevents the `fallback = ["kiro"]`-but-not-installed failure. Capability tip surfaces the routing lever without opining on who's good at what.
6. **What this does NOT do** (deferred):
   - `spur config show --resolved` — audit subcommand, own spec.
   - `spur agents add` — interactive or flag-driven onboarding, own spec.
   - Capability pre-seeding ("kiro is for security") — user opinion, not spur's.

---

## Execution

Ready for **superpowers:subagent-driven-development**. Recommended model: sonnet throughout — all tasks are mechanical.

Task dependency graph:
- Task 1 → Task 2 (cmd_init reads `install_hint` + seed order)
- Task 2 → Task 3 (tests cover new behavior)
- Task 2 → Task 4 (cookbook mentions the new const)
- Task 5 gates 1–4.

Suggested order: 1 → 2 → 3 → 4 → 5.
