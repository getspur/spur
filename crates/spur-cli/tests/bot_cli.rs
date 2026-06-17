//! `spur bot` CLI surface. The command is gated behind the `telegram-bot`
//! feature: present (and tested for help output) when enabled, and absent
//! (guarded by `bot_command_absent_by_default`) under the default build.

use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

/// Default build must not expose the Telegram bot at all.
#[cfg(not(feature = "telegram-bot"))]
#[test]
fn bot_command_absent_by_default() {
    let out = Command::new(spur_binary())
        .args(["bot", "telegram", "--help"])
        .output()
        .expect("spawn");

    assert!(
        !out.status.success(),
        "`spur bot` must be compiled out under the default feature set"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("unexpected argument"),
        "expected clap to reject `bot`; got stderr:\n{stderr}"
    );
}

#[cfg(feature = "telegram-bot")]
#[test]
fn bot_telegram_help_smoke() {
    let out = Command::new(spur_binary())
        .args(["bot", "telegram", "--help"])
        .output()
        .expect("spawn");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Launch the Telegram bot frontend"));
}

#[cfg(feature = "telegram-bot")]
#[test]
fn bot_telegram_help_still_exposes_the_command() {
    let out = Command::new(spur_binary())
        .args(["bot", "telegram", "--help"])
        .output()
        .expect("spawn");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("bot telegram"));
}
