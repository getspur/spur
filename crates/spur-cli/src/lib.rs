//! Library target for spur-cli. Used by integration tests in tests/.
//! The production binary entrypoint stays in `src/main.rs`.

pub mod commands;
pub mod log_writer;
pub mod upgrade_check;

pub use log_writer::{build_rotator, today_basepath};

pub fn pm_service_gate_allows_construction(gate: &spur_license::FeatureGate) -> bool {
    gate.has(spur_license::FeatureKey::PM_CORE_BROWSE)
}

/// Test seam: equivalent to TUI-mode `init_tracing`, but takes an explicit
/// repo root and returns the `WorkerGuard` so tests can drop it before
/// asserting on disk state.
#[cfg(any(test, feature = "test-seam"))]
pub fn init_tracing_for_test(
    repo_root: &std::path::Path,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let log_dir = repo_root.join(".spur").join("logs");
    std::fs::create_dir_all(&log_dir)?;

    let env_filter = tracing_subscriber::EnvFilter::new("warn,spur_core::orchestrator=info");

    let rotator = log_writer::build_rotator(&log_dir, 8_388_608, 3);
    let (non_blocking, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(8_192)
        .finish(rotator);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking))
        .init();

    Ok(guard)
}
