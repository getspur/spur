//! Integration: ~112 MB of tracing output, assert disk stays bounded by the
//! configured cap and at least one rotated chunk is gzipped.
//!
//! NOTE on flake-resistance: `non_blocking::lossy(true)` drops events when
//! the producer outraces the worker thread, AND file-rotate transiently
//! shows both `.N` and `.N.gz` mid-compression. So the exact rotation
//! count is non-deterministic. We assert structural bounds (≥ 2 chunks,
//! total bytes within cap, ≥ 1 gz) rather than an exact count, and yield
//! periodically so the worker thread gets scheduled.
//!
//! Gated on the `test-seam` feature so `cargo test --workspace` (which
//! does not enable per-crate features) compiles this as an empty crate.
#![cfg(feature = "test-seam")]

use spur_cli::init_tracing_for_test;
use std::time::Duration;
use tempfile::tempdir;
use tracing::info;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rotates_at_8mb_keeps_bounded_chunks() {
    let dir = tempdir().expect("tmpdir");
    let _guard = init_tracing_for_test(dir.path()).expect("init");

    let payload = "x".repeat(8_000); // 8 KB per event
    for i in 0..14_000 {
        info!(target: "spur_core::orchestrator", payload = %payload, "emit");
        // Yield so the non_blocking worker can drain instead of dropping
        // events under lossy backpressure.
        if i % 256 == 0 {
            tokio::task::yield_now().await;
        }
    }
    drop(_guard);
    // Worker needs time to flush + gzip pending rotations. 2s tolerates
    // slow CI; 500ms produced ~50% flake on dev Macs.
    tokio::time::sleep(Duration::from_secs(2)).await;

    let logs_dir = dir.path().join(".spur/logs");
    let mut total_bytes = 0u64;
    let mut chunks = vec![];
    for entry in std::fs::read_dir(&logs_dir).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("spur.log.") {
            let len = entry.metadata().expect("md").len();
            total_bytes += len;
            chunks.push((name, len));
        }
    }

    // Primary invariant: disk usage stays bounded by the configured cap.
    // 32 MB = max_files (3) × max_file_bytes (8 MB) + 1 active file. The
    // 64 KB slop covers the rotation boundary plus a tiny overshoot from
    // the gzip header on rotated chunks.
    assert!(
        total_bytes <= 32 * 1_024 * 1_024 + 64 * 1_024,
        "total bytes {} exceeds 32 MB + 64 KB slop; chunks = {:?}",
        total_bytes,
        chunks,
    );
    // At least one rotated chunk should exist (rotation actually happened).
    assert!(
        chunks.len() >= 2,
        "expected ≥ 2 chunks (active + ≥ 1 rotated), got {} = {:?}",
        chunks.len(),
        chunks,
    );
    // At least one rotated chunk should be gzipped (compression ran).
    let gz_count = chunks.iter().filter(|(n, _)| n.ends_with(".gz")).count();
    assert!(
        gz_count >= 1,
        "expected ≥ 1 .gz rotated chunk, got {} = {:?}",
        gz_count,
        chunks,
    );
}
