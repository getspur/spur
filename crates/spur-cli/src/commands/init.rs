use anyhow::Result;
use spur_acp::config::SpurConfig;
use spur_core::Orchestrator;
use std::path::PathBuf;

/// User-facing install commands per seed agent. Surfaced by `spur init`
/// when a seed agent's binary is not on $PATH. Kept here (not in the
/// schema) because hints are onboarding copy — they don't belong in
/// every user's round-tripped `.spur/config.toml`.
///
/// Contract: every agent in `spur_acp::config::load_seed_template()`
/// must have an entry here. Enforced by `tests/init_ux.rs`.
pub const INSTALL_HINTS: &[(&str, &str)] = &[
    ("claude-code", "npm install -g @anthropic-ai/claude-code"),
    ("kiro", "brew install kiro-cli"),
    (
        "claude-code-acp",
        "npm install -g npx   # then re-run `spur init`",
    ),
    (
        "codex",
        "https://github.com/zed-industries/codex-acp/releases",
    ),
    ("codex-acp", "npx @zed-industries/codex-acp"),
    ("gemini-acp", "npm install -g @google/gemini-cli"),
    ("opencode-acp", "npm install -g opencode"),
];

pub fn install_hint(name: &str) -> &'static str {
    INSTALL_HINTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, h)| *h)
        .unwrap_or("see docs/spur/agent-onboarding-cookbook.md")
}

pub async fn run(repo_root: PathBuf, force: bool) -> Result<()> {
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
    // Sort registered agents by seed order so brain selection is deterministic.
    let seed_order: Vec<&str> = seed.entries.iter().map(|e| e.name.as_str()).collect();
    let mut registered: Vec<spur_acp::config::AgentConfig> =
        orch.registry.list().into_iter().cloned().collect();
    registered.sort_by_key(|a| {
        seed_order
            .iter()
            .position(|&s| s == a.name.as_str())
            .unwrap_or(usize::MAX)
    });
    let registered_names: std::collections::HashSet<_> = found_names.iter().cloned().collect();

    // ── Agent list: ✓ for registered, ✗ with install hint otherwise. ──
    println!();
    for agent in &seed.entries {
        if registered_names.contains(&agent.name) {
            println!("  ✓ {}", agent.name);
        } else {
            println!(
                "  ✗ {:<18}install: {}",
                agent.name,
                install_hint(&agent.name)
            );
        }
    }

    // ── PM tools: detect br (beads) and bv (beads_viewer). ──
    println!();
    println!("[spur] Checking PM tools...");
    println!();

    const PM_TOOLS: &[(&str, &str)] = &[
        (
            "br",
            "cargo install --git https://github.com/Dicklesworthstone/beads_rust.git",
        ),
        ("bv", "brew install dicklesworthstone/tap/bv"),
    ];
    let beads_dir_exists = repo_root.join(".beads").is_dir();
    let mut br_found = false;

    for &(cmd, hint) in PM_TOOLS {
        let found = tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);
        if cmd == "br" {
            br_found = found;
        }
        if found {
            println!("  ✓ {cmd}");
        } else {
            println!("  ✗ {cmd:<18}install: {hint}");
        }
    }

    if beads_dir_exists && !br_found {
        println!();
        println!("  Note: .beads/ found but br is missing — issue tracking will not work.");
    }

    // ── Path B: zero agents → no write. ──
    if registered.is_empty() {
        println!();
        println!("No agents found. Install one of the above and re-run `spur init`.");
        return Ok(());
    }

    // ── Adaptive brain selection. ──
    // Prefer first registered brain-capable agent in seed order.
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
        .filter(|a| a.name != brain_name && matches!(a.role, AgentRole::Brain | AgentRole::Both))
        .map(|a| a.name.clone())
        .collect();

    // ── Persist config with derived brain/fallback. ──
    let mut persist = SpurConfig::default();
    persist.brain.default = brain_name.clone();
    persist.brain.fallback = fallbacks.clone();
    persist.agents.entries = registered.clone();

    std::fs::create_dir_all(config_path.parent().unwrap())?;
    std::fs::write(&config_path, toml::to_string_pretty(&persist)?)?;

    // ── Install SpurPower skills across agent dirs ──
    match spur_core::skills::installer::run(&repo_root) {
        Ok(summary) => {
            println!();
            print!("{summary}");
            print_gitattributes_advisory_if_needed(&repo_root);
        }
        Err(e) => {
            eprintln!("[spur] skills install failed: {e}");
            return Err(anyhow::anyhow!(e));
        }
    }

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

fn print_gitattributes_advisory_if_needed(repo_root: &std::path::Path) {
    let path = repo_root.join(".gitattributes");
    let contents = std::fs::read_to_string(&path).unwrap_or_default();
    // Advisory if the file doesn't mention LF normalization for .md files.
    if !(contents.contains("*.md") && contents.contains("eol=lf")) {
        println!();
        println!("Tip: add `*.md text eol=lf` to .gitattributes for cross-platform");
        println!("     teammates. SpurPower marker files may thrash across CRLF/LF");
        println!("     systems otherwise.");
    }
}
