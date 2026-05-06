use anyhow::Result;
use spur_acp::config::{AgentConfig, SpurConfig};
use spur_acp::types::AgentRole;
use spur_core::Orchestrator;
use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

/// User-facing install commands per seed agent. Surfaced by `spur init`
/// when a seed agent's binary is not on $PATH. Kept here (not in the
/// schema) because hints are onboarding copy — they don't belong in
/// every user's round-tripped `.spur/config.toml`.
///
/// Contract: every agent in `spur_acp::config::load_seed_template()`
/// must have an entry here. Enforced by `tests/init_ux.rs`.
pub const INSTALL_HINTS: &[(&str, &str)] = &[
    ("kiro", "brew install kiro-cli"),
    (
        "claude-code",
        "npm install -g npx   # then re-run `spur init`",
    ),
    (
        "codex-bin",
        "https://github.com/zed-industries/codex-acp/releases",
    ),
    ("codex", "npx @zed-industries/codex-acp"),
    ("gemini", "npm install -g @google/gemini-cli"),
    ("opencode", "npm install -g opencode"),
    ("kimi", "see docs/spur/agent-onboarding-cookbook.md"),
];

pub fn install_hint(name: &str) -> &'static str {
    INSTALL_HINTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, h)| *h)
        .unwrap_or("see docs/spur/agent-onboarding-cookbook.md")
}

/// Initialize SPUR: converge local config with the environment.
///
/// Philosophy: `spur init` is a config convergence tool, not a one-shot
/// writer. It is safe to run multiple times.
///
/// - Discovers agents on `$PATH` and merges them into existing config.
/// - Preserves user customizations (capabilities, review policy, delegation).
/// - Preserves all non-agent config (bot, pm, project, cost, worktree, …).
/// - Recomputes brain / fallback based on the merged agent list.
/// - Optionally guides through Telegram bot setup when running in a TTY.
/// - Validates before persisting.
///
/// `--force` resets the agent list to discovered-only (drops manually-added
/// agents) while still preserving non-agent sections.
pub async fn run(repo_root: PathBuf, force: bool, skills: bool) -> Result<()> {
    let config_path = repo_root.join(".spur").join("config.toml");

    // ── Phase 1: Environment discovery ─────────────────────────────────
    println!("[spur] Scanning agents on $PATH...");
    let mut orch = Orchestrator::new(repo_root.clone(), SpurConfig::default(), None)?;
    let found_names = orch.init_agents().await?;
    let seed = spur_acp::config::load_seed_template();

    let seed_order: Vec<&str> = seed.entries.iter().map(|e| e.name.as_str()).collect();
    let mut discovered: Vec<AgentConfig> = orch.registry.list().into_iter().cloned().collect();
    discovered.sort_by_key(|a| {
        seed_order
            .iter()
            .position(|&s| s == a.name.as_str())
            .unwrap_or(usize::MAX)
    });
    let discovered_names: std::collections::HashSet<_> = found_names.iter().cloned().collect();

    // ── Phase 2: Load existing config or start from default ────────────
    let (mut config, existed_before) = load_or_default_config(&config_path)?;

    // ── Phase 3: Agent convergence ─────────────────────────────────────
    config.agents.entries = merge_agents(&config.agents.entries, &discovered, force);

    // ── Phase 4: Recompute brain & fallback ────────────────────────────
    recompute_brain_and_fallback(&mut config);

    // ── Phase 5: Display discovery results ─────────────────────────────
    println!();
    for agent in &seed.entries {
        if discovered_names.contains(&agent.name) {
            println!("  ✓ {}", agent.name);
        } else {
            println!(
                "  ✗ {:<18}install: {}",
                agent.name,
                install_hint(&agent.name)
            );
        }
    }

    // ── Phase 7: Brain selection (interactive only in TTY) ─────────────
    if std::io::stdin().is_terminal() {
        if let Err(e) = prompt_default_brain_selection(&mut config) {
            eprintln!("[spur] default brain prompt failed: {e}; continuing");
        }
    }

    // ── Phase 8: Bot setup (interactive only in TTY) ───────────────────
    if std::io::stdin().is_terminal() {
        if let Err(e) = maybe_prompt_bot_setup(&mut config) {
            eprintln!("[spur] bot setup prompt failed: {e}; continuing");
        }
    }

    // ── Phase 9: Validate before write ─────────────────────────────────
    if let Err(e) = validate_all_agents(&config) {
        eprintln!("[spur] config validation failed: {e}");
        return Err(e);
    }

    // ── Phase 10: Early exit if no agents and no prior config ──────────
    if config.agents.entries.is_empty() && !existed_before {
        println!();
        println!("No agents found. Install one of the above and re-run `spur init`.");
        return Ok(());
    }

    // ── Phase 11: Permission-bypass safety prompt (TTY only) ───────────
    if std::io::stdin().is_terminal() {
        if let Err(e) = prompt_permission_bypass(&mut config) {
            eprintln!("[spur] permission bypass prompt failed: {e}; continuing");
        }
    }

    // ── Phase 12: Atomic persist ───────────────────────────────────────
    std::fs::create_dir_all(config_path.parent().unwrap())?;
    std::fs::write(&config_path, toml::to_string_pretty(&config)?)?;

    // ── Phase 12.5: First-time beads bootstrap ─────────────────────────
    // Run `pm init` automatically when `.beads/` is absent. This makes the
    // golden-path "git clone → spur init" produce a working tracker without
    // a second command. Idempotent: subsequent `spur init` runs see the
    // directory present and skip this phase.
    if !repo_root.join(".beads").exists() {
        println!();
        println!("[spur] no .beads/ found — bootstrapping tracker...");
        if let Err(e) = run_pm_init(repo_root.clone()).await {
            eprintln!("[spur] warning: pm init failed: {e}");
            // Do not return Err — `spur init`'s primary contract (write
            // .spur/config.toml) is already met. The user can re-run
            // `spur pm init` directly.
        }
    }

    // ── Phase 13: Skills install ────────────────────────────────────────
    // Default-on, but filtered: only fan out to adapters whose agent was
    // discovered on `$PATH` (plus `SpurHermetic` for brain prompt injection).
    // `--with-skills` forces the legacy full fanout (all 8 adapters) for
    // users who explicitly want every adapter dir materialized.
    if skills {
        if let Err(e) = run_skills_init(&repo_root) {
            eprintln!("[spur] warning: skills install failed: {e}");
        }
    } else if !config.agents.entries.is_empty() {
        let allowed = adapters_for_discovered_agents(&discovered_names);
        if let Err(e) = run_skills_init_filtered(&repo_root, &allowed) {
            eprintln!("[spur] warning: skills install failed: {e}");
        }
    }

    // ── Phase 14: Summary ──────────────────────────────────────────────
    print_summary(&config);

    Ok(())
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

fn load_or_default_config(path: &std::path::Path) -> Result<(SpurConfig, bool)> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let config: SpurConfig = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))?;
        Ok((config, true))
    } else {
        Ok((SpurConfig::default(), false))
    }
}

fn merge_agents(
    existing: &[AgentConfig],
    discovered: &[AgentConfig],
    force: bool,
) -> Vec<AgentConfig> {
    if force {
        // Reset to discovered-only, but preserve seed ordering.
        return discovered.to_vec();
    }

    // Start with existing agents (preserves user customizations).
    let mut merged: HashMap<String, AgentConfig> = existing
        .iter()
        .map(|a| (a.name.clone(), a.clone()))
        .collect();

    // Overlay newly discovered agents that aren't already present.
    for disc in discovered {
        if !merged.contains_key(&disc.name) {
            merged.insert(disc.name.clone(), disc.clone());
        }
    }

    // Sort by seed order for deterministic output.
    let seed = spur_acp::config::load_seed_template();
    let seed_order: Vec<&str> = seed.entries.iter().map(|e| e.name.as_str()).collect();
    let mut result: Vec<AgentConfig> = merged.into_values().collect();
    result.sort_by_key(|a| {
        seed_order
            .iter()
            .position(|&s| s == a.name.as_str())
            .unwrap_or(usize::MAX)
    });
    result
}

fn recompute_brain_and_fallback(config: &mut SpurConfig) {
    let entries = &config.agents.entries;

    let brain_name = entries
        .iter()
        .find(|a| a.name == "claude-code" && matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .or_else(|| {
            entries
                .iter()
                .find(|a| matches!(a.role, AgentRole::Brain | AgentRole::Both))
        })
        .map(|a| a.name.clone())
        .unwrap_or_else(|| {
            if !entries.is_empty() {
                println!();
                println!(
                    "  (note: no brain-capable agents registered; using {} as brain)",
                    entries[0].name
                );
                entries[0].name.clone()
            } else {
                String::new()
            }
        });

    let fallbacks: Vec<String> = entries
        .iter()
        .filter(|a| a.name != brain_name && matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .map(|a| a.name.clone())
        .collect();

    config.brain.default = brain_name;
    config.brain.fallback = fallbacks;
}

fn prompt_default_brain_selection(config: &mut SpurConfig) -> Result<()> {
    let brain_agents: Vec<String> = config
        .agents
        .entries
        .iter()
        .filter(|a| matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .map(|a| a.name.clone())
        .collect();

    if brain_agents.is_empty() {
        return Ok(());
    }

    let default_index = brain_agents
        .iter()
        .position(|name| name == &config.brain.default)
        .unwrap_or(0);

    println!();
    println!(
        "Select default brain [default: {}]:",
        brain_agents[default_index]
    );
    for (idx, name) in brain_agents.iter().enumerate() {
        println!("  {}) {}", idx + 1, name);
    }
    eprint!("> ");
    std::io::stderr().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let selected = match line.trim() {
        "" => brain_agents[default_index].clone(),
        input => match input.parse::<usize>() {
            Ok(choice) if (1..=brain_agents.len()).contains(&choice) => {
                brain_agents[choice - 1].clone()
            }
            _ => {
                println!(
                    "Invalid selection; keeping {}.",
                    brain_agents[default_index]
                );
                brain_agents[default_index].clone()
            }
        },
    };

    config.brain.default = selected.clone();
    config.brain.fallback = brain_agents
        .into_iter()
        .filter(|name| name != &selected)
        .collect();

    Ok(())
}

fn maybe_prompt_bot_setup(config: &mut SpurConfig) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Ok(());
    }

    let already_enabled = config.bot.telegram.enabled;

    println!();
    if already_enabled {
        println!("Telegram bot is already configured (enabled = true).");
        println!("Reconfigure? [y/N]");
    } else {
        println!("SPUR can run a Telegram bot for remote interaction.");
        println!("Configure it now? [y/N]");
    }
    eprint!("> ");
    std::io::stderr().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if !line.trim().eq_ignore_ascii_case("y") {
        return Ok(());
    }

    // Prompt for operator_user_id
    println!();
    println!("How to get your Telegram numeric user ID:");
    println!();
    println!("  1. SIMPLEST: Message @userinfobot and copy the 'id' number.");
    println!();
    println!("  2. VIA API: If @userinfobot is unavailable, send a DM to your");
    println!("     bot, then run:");
    println!("       curl \"https://api.telegram.org/bot<TOKEN>/getUpdates\"");
    println!("     and read result[0].message.from.id.");
    println!();
    println!("     NOTE: if spur bot telegram is already running, it consumes");
    println!("     updates via long polling — getUpdates may return empty.");
    println!("     Stop the bot first, and if you ever used webhooks, clear");
    println!("     them with:");
    println!("       curl \"https://api.telegram.org/bot<TOKEN>/deleteWebhook?drop_pending_updates=false\"");
    println!();
    println!("Enter your operator_user_id:");
    eprint!("> ");
    std::io::stderr().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let operator_id = line.trim().parse::<i64>().ok();

    // Token guidance — never prompt for the secret itself.
    println!();
    println!("Bot token: set SPUR_TELEGRAM_BOT_TOKEN as an environment variable");
    println!("(get your token from @BotFather). Do NOT paste it here — it would");
    println!("be stored in .spur/config.toml which is not gitignored by default.");
    println!();
    println!("Once the env var is set, start the bot with:");
    println!("  spur bot telegram");

    config.bot.telegram.enabled = true;
    if let Some(id) = operator_id {
        config.bot.telegram.operator_user_id = Some(id);
    }

    Ok(())
}

fn validate_all_agents(config: &SpurConfig) -> Result<()> {
    use spur_acp::validate_agent_config;

    let mut fatal = 0_usize;
    let mut _warn = 0_usize;

    for entry in &config.agents.entries {
        match validate_agent_config(entry) {
            Ok(()) => {}
            Err(errors) => {
                for e in errors {
                    if e.is_fatal() {
                        eprintln!("  \u{2717} {}: {}", entry.name, e);
                        fatal += 1;
                    } else {
                        eprintln!("  \u{26a0} {}: {}", entry.name, e);
                        _warn += 1;
                    }
                }
            }
        }
    }

    if fatal > 0 {
        Err(anyhow::anyhow!("{fatal} fatal agent validation error(s)"))
    } else {
        Ok(())
    }
}

fn print_summary(config: &SpurConfig) {
    let any_bypass = config
        .agents
        .entries
        .iter()
        .any(|a| a.effective_permissions().skip);
    let bypass_str = if any_bypass {
        "enabled for some agents — review .spur/config.toml"
    } else {
        "disabled (safety-default)"
    };
    let fallback_str = if config.brain.fallback.is_empty() {
        "none".to_string()
    } else {
        config.brain.fallback.join(", ")
    };

    println!();
    println!("Config written to .spur/config.toml.");
    println!(
        "Brain: {} (fallback: {}). Bypass: {}.",
        config.brain.default, fallback_str, bypass_str
    );

    if config.bot.telegram.enabled {
        println!(
            "Telegram bot: enabled (operator_user_id = {:?})",
            config.bot.telegram.operator_user_id
        );
        if config.bot.telegram.bot_token.is_none() {
            println!("  Reminder: set SPUR_TELEGRAM_BOT_TOKEN env var before running");
        }
    }

    if config.agents.entries.len() >= 2 {
        println!();
        println!("Tip: set `capabilities = [\"security\", ...]` on each agent");
        println!("to enable capability-based delegation from the brain.");
    }

    println!();
    println!("Next step:");
    println!("  spur run \"describe the repo in 3 bullets\"    # one-shot");
    println!("  spur tui                                     # interactive TUI");
    println!("  spur config check                            # validate your setup");
    if config.bot.telegram.enabled {
        println!("  spur bot telegram                            # start Telegram bot");
    }
}

/// `spur pm init` — bootstrap the beads tracker in a fresh repo.
///
/// Idempotent: safe to run multiple times. On a fresh repo, this:
///   1. creates `.beads/` and initializes `beads.db` + `issues.jsonl` via
///      `BeadsCrateAdapter::open` (which runs `init_writer_with_flush`);
///   2. appends beads-derived-file entries to `.gitignore` if missing
///      (so `issues.jsonl` stays committed and `beads.db` / locks / temps
///      do not);
///   3. ensures `[pm.beads] enabled = true` in `.spur/config.toml` if the
///      file already exists. If the config doesn't exist, prints a hint
///      pointing at `spur init` instead of fabricating one.
pub async fn run_pm_init(repo_root: PathBuf) -> Result<()> {
    use spur_pm::beads_crate::adapter::{AdapterConfig, BeadsCrateAdapter};

    let beads_dir = repo_root.join(".beads");
    let already_existed = beads_dir.exists();

    std::fs::create_dir_all(&beads_dir)
        .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", beads_dir.display()))?;

    // Bootstrap SQLite + JSONL by opening once and dropping. The adapter's
    // `open` runs `init_writer_with_flush` under `.write.lock`, which creates
    // `beads.db` (schema) and leaves an empty `issues.jsonl`.
    let _adapter = BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default())
        .await
        .map_err(|e| anyhow::anyhow!("beads adapter init failed: {e}"))?;
    drop(_adapter);

    // `init_writer_with_flush` only emits `issues.jsonl` when the DB is dirty;
    // a fresh DB skips the write. Materialize an empty file ourselves so the
    // source-of-truth artifact exists at well-known path from the first run
    // and `git add .beads/issues.jsonl` works without a follow-up command.
    let jsonl = beads_dir.join("issues.jsonl");
    if !jsonl.exists() {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&jsonl)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", jsonl.display()))?;
    }

    // Patch .gitignore — selective entries so `issues.jsonl` stays committed.
    let gitignore_added =
        ensure_gitignore_lines(&repo_root.join(".gitignore"), BEADS_GITIGNORE_LINES)?;

    // Patch .spur/config.toml only if it exists.
    let config_path = repo_root.join(".spur").join("config.toml");
    let config_touched = if config_path.exists() {
        ensure_pm_beads_enabled(&config_path)?
    } else {
        false
    };

    println!();
    if already_existed {
        println!("[pm init] .beads/ already present — verified schema is up to date.");
    } else {
        println!("[pm init] initialized .beads/ (beads.db + issues.jsonl).");
    }
    if gitignore_added > 0 {
        println!(
            "[pm init] added {gitignore_added} entr{} to .gitignore.",
            if gitignore_added == 1 { "y" } else { "ies" }
        );
    } else {
        println!("[pm init] .gitignore already up to date.");
    }
    if config_touched {
        println!("[pm init] set [pm.beads] enabled = true in .spur/config.toml.");
    } else if !config_path.exists() {
        println!("[pm init] tip: run `spur init` to create .spur/config.toml.");
    }
    println!();
    println!("Next: commit `.beads/issues.jsonl` and `.gitignore`, then create issues.");
    Ok(())
}

/// Lines added to `.gitignore` by `spur pm init`. Selective on purpose:
/// `issues.jsonl` is the source of truth and MUST stay committed.
const BEADS_GITIGNORE_LINES: &[&str] = &[
    ".beads/beads.db",
    ".beads/beads.db-*",
    ".beads/.write.lock",
    ".beads/issues.jsonl.*.tmp",
];

/// Append any of `lines` not already present in `path`. Returns count added.
/// Creates the file (with a leading section comment) if it doesn't exist.
fn ensure_gitignore_lines(path: &std::path::Path, lines: &[&str]) -> Result<usize> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let present: std::collections::HashSet<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let mut to_add: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| !present.contains(*l))
        .collect();
    if to_add.is_empty() {
        return Ok(0);
    }

    let mut out = String::with_capacity(existing.len() + 256);
    out.push_str(&existing);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !existing.contains("# spur pm (beads)") {
        out.push_str("\n# spur pm (beads) — derived files; commit issues.jsonl\n");
    }
    for line in &to_add {
        out.push_str(line);
        out.push('\n');
    }

    std::fs::write(path, out)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
    Ok(to_add.drain(..).count())
}

/// Load `.spur/config.toml`, ensure `[pm.beads] enabled = true` (preserving
/// `auto_sync`), persist atomically. Returns `true` if the file was changed.
fn ensure_pm_beads_enabled(config_path: &std::path::Path) -> Result<bool> {
    use spur_acp::config::BeadsPmConfig;

    let content = std::fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", config_path.display()))?;
    let mut config: SpurConfig = toml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", config_path.display()))?;

    let was_enabled = config.pm.beads.as_ref().map(|b| b.enabled).unwrap_or(false);
    let was_explicit = config.pm.beads.is_some();

    if was_enabled && was_explicit {
        return Ok(false);
    }

    let auto_sync = config
        .pm
        .beads
        .as_ref()
        .map(|b| b.auto_sync)
        .unwrap_or(false);
    config.pm.beads = Some(BeadsPmConfig {
        enabled: true,
        auto_sync,
    });
    std::fs::write(config_path, toml::to_string_pretty(&config)?)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", config_path.display()))?;
    Ok(true)
}

/// Run the SpurPower skills installer independently of config init.
pub fn run_skills_init(repo_root: &std::path::Path) -> Result<()> {
    match spur_core::skills::installer::run(repo_root) {
        Ok(summary) => {
            println!();
            print!("{summary}");
            print_gitattributes_advisory_if_needed(repo_root);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("skills install failed: {e}")),
    }
}

/// Filtered skills install — used by `spur init`'s default-on path so we
/// don't materialize dotfile dirs for agents the user doesn't have.
pub fn run_skills_init_filtered(
    repo_root: &std::path::Path,
    adapters: &[spur_core::skills::adapters::Adapter],
) -> Result<()> {
    match spur_core::skills::installer::run_filtered(repo_root, adapters) {
        Ok(summary) => {
            println!();
            print!("{summary}");
            print_gitattributes_advisory_if_needed(repo_root);
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("skills install failed: {e}")),
    }
}

/// Map discovered agent names to skill adapters. `SpurHermetic` is always
/// included for brain prompt injection. Unknown agent names are ignored.
fn adapters_for_discovered_agents(
    discovered: &std::collections::HashSet<String>,
) -> Vec<spur_core::skills::adapters::Adapter> {
    use spur_core::skills::adapters::Adapter;
    let mut set: std::collections::HashSet<Adapter> = std::collections::HashSet::new();
    set.insert(Adapter::SpurHermetic);
    for name in discovered {
        match name.as_str() {
            "claude-code" => {
                set.insert(Adapter::ClaudeCode);
            }
            "codex" | "codex-bin" => {
                set.insert(Adapter::Codex);
            }
            "gemini" => {
                set.insert(Adapter::Gemini);
            }
            "kiro" => {
                set.insert(Adapter::Kiro);
            }
            "opencode" => {
                set.insert(Adapter::OpenCode);
            }
            "kimi" => {
                set.insert(Adapter::Kimi);
            }
            _ => {}
        }
    }
    // Preserve `Adapter::all()` order so output is deterministic.
    Adapter::all()
        .iter()
        .copied()
        .filter(|a| set.contains(a))
        .collect()
}

fn prompt_permission_bypass(config: &mut SpurConfig) -> Result<()> {
    let bypass_agents: Vec<&str> = config
        .agents
        .entries
        .iter()
        .filter(|a| a.effective_permissions().skip)
        .map(|a| a.name.as_str())
        .collect();

    if bypass_agents.is_empty() {
        return Ok(());
    }

    println!();
    println!("WARNING: the following agents have permission bypass enabled:");
    for name in &bypass_agents {
        println!("  - {name}");
    }
    println!();
    println!("Permission bypass allows agents to execute tools without prompting.");
    println!("Keep bypass enabled? [y/N]");
    eprint!("> ");
    std::io::stderr().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim().eq_ignore_ascii_case("y") {
        return Ok(());
    }

    // User declined — disable bypass for all agents.
    for agent in &mut config.agents.entries {
        agent.permissions.skip = false;
        agent.skip_permissions = false;
    }
    println!("Permission bypass disabled for all agents.");
    Ok(())
}

fn print_gitattributes_advisory_if_needed(repo_root: &std::path::Path) {
    let path = repo_root.join(".gitattributes");
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    if !(contents.contains("*.md") && contents.contains("eol=lf")) {
        println!();
        println!("Tip: add `*.md text eol=lf` to .gitattributes for cross-platform");
        println!("     teammates. SpurPower marker files may thrash across CRLF/LF");
        println!("     systems otherwise.");
    }
}
