#[test]
fn notebook_daemon_child_command_uses_no_flags() {
    let spec = spur_tui::notebook_daemon::DaemonCommandSpec::for_current_installation();

    assert!(spec.program.ends_with("spur-notebook"));
    assert!(spec.args.is_empty());
}
