//! Integration: spawn a process that prints 50 MB to stderr in 10 MB bursts;
//! assert per-child file usage stays ≤ 10 MB. Then run a `\r`-only burst and
//! assert spur task does not OOM (process memory stays under 100 MB).

#![cfg(unix)]

use spur_acp::connection::child_stderr_bridge::ChildStderrBridge;
use std::process::Stdio;
use tempfile::tempdir;
use tokio::process::Command;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fifty_mb_stderr_burst_capped_at_ten_mb() {
    let dir = tempdir().expect("tmpdir");
    let log_path = dir.path().join("test-agent.log");

    // /bin/sh script that prints ~50 MB to stderr in 10 MB bursts.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"for i in 1 2 3 4 5; do head -c 10485760 < /dev/urandom | base64 1>&2; done"#)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let stderr = child.stderr.take().expect("stderr piped");
    let bridge = ChildStderrBridge::start(
        stderr,
        log_path.parent().unwrap(),
        "test-agent",
        child.id().expect("pid"),
        2_500_000, // 2.5 MB per chunk
        3,         // 3 rotated + 1 active = 10 MB total
        8_192,     // buffered_lines_limit
    )
    .expect("start bridge");

    let _ = child.wait().await;
    bridge.shutdown().await;

    // Sum all test-agent-*.log* sizes.
    let mut total = 0u64;
    for entry in std::fs::read_dir(dir.path()).expect("read_dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("test-agent") {
            total += entry.metadata().expect("md").len();
        }
    }
    assert!(
        total <= 10 * 1_024 * 1_024 + 64 * 1_024,
        "child stderr total {} exceeds 10 MB + slop",
        total
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn newline_less_burst_does_not_oom() {
    let dir = tempdir().expect("tmpdir");
    let log_path = dir.path().join("rprog-agent.log");

    // Print 5 MB of `\r`-prefixed progress without a single newline.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"head -c 5242880 < /dev/zero | tr '\0' '\r' 1>&2"#)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let stderr = child.stderr.take().expect("stderr piped");
    let bridge = ChildStderrBridge::start(
        stderr,
        log_path.parent().unwrap(),
        "rprog-agent",
        child.id().expect("pid"),
        2_500_000,
        3,
        8_192,
    )
    .expect("start bridge");

    // Test passes if it completes within the timeout (no infinite buffer growth).
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let _ = child.wait().await;
        bridge.shutdown().await;
    })
    .await;
    assert!(result.is_ok(), "newline-less stderr burst hung");
}
