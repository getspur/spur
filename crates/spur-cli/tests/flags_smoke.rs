#[test]
fn flags_list_binary_runs() {
    let bin = std::env::var("CARGO_BIN_EXE_spur").unwrap_or_else(|_| {
        // Fallback for environments where the env var isn't set
        let mut path = std::env::current_exe().expect("current_exe failed");
        path.pop(); // tests/
        path.pop(); // debug/
        path.push("spur");
        path.to_string_lossy().into_owned()
    });
    let output = std::process::Command::new(&bin)
        .args(["flags", "list"])
        .output()
        .expect("failed to run spur flags list");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("kill_advanced_planner"), "stdout: {stdout}");
    assert!(stdout.contains("enable_browser_tool"), "stdout: {stdout}");
}
