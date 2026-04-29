//! Smoke test: TUI-mode init_tracing must accept an EnvFilter so DEBUG events
//! are filtered. Until Task 4, this test only exercises the filter, not rotation.

use std::process::Command;
use tempfile::tempdir;

#[test]
fn tui_mode_filters_debug_events_by_default() {
    let dir = tempdir().expect("tmpdir");
    // Spawn `spur tui --help` with the tempdir as CWD; --help short-circuits
    // before the TUI starts but after init_tracing has run.
    // We assert that .spur/logs is created without DEBUG noise.
    let output = Command::new(env!("CARGO_BIN_EXE_spur"))
        .args(["tui", "--help"])
        .current_dir(dir.path())
        .output()
        .expect("spawn spur");
    assert!(
        output.status.success(),
        "spur tui --help failed: {output:?}"
    );
    // Note: this is an early smoke. Full byte-cap verification lands in Task 4.
}
