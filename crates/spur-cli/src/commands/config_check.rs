//! `spur config check` — validates `.spur/config.toml` without starting
//! any agents. Exit 0 if all entries pass; exit 1 if any entry produces
//! a fatal ConfigError. Warnings are reported to stderr but do not flip
//! the exit code.
//!
//! The validation logic itself is in `spur_acp::validate_agent_config`;
//! this module only loads the config, iterates, and formats output.

use std::path::Path;

use spur_acp::{config::load_layered, validate_agent_config};
#[cfg(feature = "telegram-bot")]
use spur_bot::telegram::config::{
    resolve_from_env as resolve_telegram_env, validate as validate_telegram_config,
};

/// Returns the exit code: 0 on success, 1 on any fatal error.
pub fn run(repo_root: &Path) -> anyhow::Result<i32> {
    // The Telegram path is the only mutator of `cfg`; bind without `mut`
    // when the bot feature is compiled out so `-D warnings` stays clean.
    #[cfg(feature = "telegram-bot")]
    let mut cfg = load_layered(repo_root)?;
    #[cfg(not(feature = "telegram-bot"))]
    let cfg = load_layered(repo_root)?;
    #[cfg(feature = "telegram-bot")]
    resolve_telegram_env(&mut cfg.bot.telegram);

    if cfg.agents.entries.is_empty() {
        eprintln!("no agents configured in .spur/config.toml");
        return Ok(0);
    }

    let mut fatal_count = 0_usize;
    let mut warn_count = 0_usize;

    for entry in &cfg.agents.entries {
        match validate_agent_config(entry) {
            Ok(()) => {
                println!("\u{2713} {}", entry.name);
            }
            Err(errors) => {
                for e in errors {
                    if e.is_fatal() {
                        eprintln!("\u{2717} {}", e);
                        fatal_count += 1;
                    } else {
                        eprintln!("\u{26a0} {}", e);
                        warn_count += 1;
                    }
                }
            }
        }
    }

    #[cfg(feature = "telegram-bot")]
    if let Err(error) = validate_telegram_config(&cfg.bot.telegram) {
        eprintln!("\u{2717} {error}");
        fatal_count += 1;
    }

    if fatal_count > 0 {
        eprintln!("\nconfig check FAILED: {fatal_count} fatal, {warn_count} warning(s)");
        Ok(1)
    } else {
        if warn_count > 0 {
            eprintln!("\nconfig check OK with {warn_count} warning(s)");
        }
        Ok(0)
    }
}
