use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

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
