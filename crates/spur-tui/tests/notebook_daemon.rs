#[test]
fn notebook_daemon_child_command_uses_no_flags() {
    let spec = spur_tui::notebook_daemon::DaemonCommandSpec::for_current_installation();

    #[cfg(target_os = "macos")]
    assert!(
        spec.program.ends_with("spur-notebook")
            || spec.program.ends_with("Jute.app/Contents/MacOS/Jute")
    );
    #[cfg(not(target_os = "macos"))]
    assert!(spec.program.ends_with("spur-notebook"));
    assert!(spec.args.is_empty());
}
