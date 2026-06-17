//! Integration test for `spur config check`. Builds the binary and invokes
//! it against a temporary config file — verifies exit codes and diagnostic
//! output.

use std::io::Write;
use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn write_config(contents: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let spur_dir = dir.path().join(".spur");
    std::fs::create_dir_all(&spur_dir).expect("mkdir .spur");
    let mut f = std::fs::File::create(spur_dir.join("config.toml")).expect("create toml");
    f.write_all(contents.as_bytes()).expect("write");
    dir
}

#[test]
fn config_check_passes_on_valid_config() {
    let dir = write_config(
        r#"
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[agents.entries.commands]
dispatch = "prompt_text"
"#,
    );
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "expected 0 exit; stderr = {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn config_check_fails_on_vendor_exec_without_method() {
    let dir = write_config(
        r#"
[[agents.entries]]
name = "broken-kiro"
command = "x"
transport = "acp"

[agents.entries.commands]
dispatch = "vendor_exec"
"#,
    );
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout = {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken-kiro") && stderr.contains("exec_method"),
        "expected broken-kiro/exec_method in stderr; got: {stderr}"
    );
}

// Telegram validation only runs when the bot feature is compiled in.
#[cfg(feature = "telegram-bot")]
#[test]
fn config_check_fails_when_bot_enabled_without_operator_user() {
    let dir = write_config(
        r#"
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[agents.entries.commands]
dispatch = "prompt_text"

[bot.telegram]
enabled = true
bot_token = "123:ABC"
"#,
    );

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("operator_user_id"));
}
