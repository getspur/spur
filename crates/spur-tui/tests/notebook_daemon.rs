#[test]
fn notebook_daemon_child_command_uses_headless_flag() {
    let spec = spur_tui::notebook_daemon::DaemonCommandSpec::for_current_installation();

    assert!(spec.program.ends_with("spur-notebook"));
    assert_eq!(spec.args, vec!["--headless"]);
}
