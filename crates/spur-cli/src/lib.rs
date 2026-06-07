//! Library target for spur-cli. Used by integration tests in tests/.
//! The production binary entrypoint stays in `src/main.rs`.

#![expect(
    clippy::allow_attributes,
    reason = "legacy CLI modules still contain localized allow attributes"
)]
#![expect(
    clippy::branches_sharing_code,
    reason = "legacy graph command keeps write-stage branches locally structured"
)]
#![expect(
    clippy::bool_to_int_with_if,
    reason = "legacy CLI exit-code mapping uses explicit boolean branches"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy CLI docs contain domain terms that are not consistently backticked yet"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy CLI output builders append formatted strings directly"
)]
#![expect(
    clippy::future_not_send,
    reason = "legacy CLI async commands may capture non-Send terminal locks"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy CLI init code iterates discovered adapter maps"
)]
#![expect(
    clippy::literal_string_with_formatting_args,
    reason = "legacy CLI progress templates intentionally contain brace syntax"
)]
#![expect(
    clippy::manual_is_multiple_of,
    reason = "legacy CLI formatting uses modulo arithmetic for grouping"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy CLI exit-code mapping keeps error classes explicit"
)]
#![expect(
    clippy::option_option,
    reason = "legacy upgrade parsing uses nested Option to distinguish missing and invalid fields"
)]
#![expect(
    clippy::ref_patterns,
    reason = "legacy CLI code still uses explicit ref bindings"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy CLI JSON/config extraction uses and_then chains"
)]
#![expect(
    clippy::semicolon_if_nothing_returned,
    reason = "legacy CLI async join handling omits semicolons in unit-returning expressions"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy CLI code has many &str to String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy CLI formatting has not all moved to captured format args"
)]
#![expect(
    clippy::unnecessary_debug_formatting,
    reason = "legacy CLI context messages use Debug formatting for paths"
)]
#![expect(
    clippy::unnecessary_wraps,
    reason = "legacy CLI command helpers preserve Result return shapes"
)]
#![expect(
    clippy::unused_async,
    reason = "legacy CLI command signatures preserve async call-site compatibility"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy CLI modules import extension traits by name"
)]

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
