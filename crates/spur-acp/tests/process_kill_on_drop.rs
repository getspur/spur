//! Phase 0a integration tests — verify worker child processes die when
//! the spawning Tokio Command is dropped (kill_on_drop semantics).

use std::time::{Duration, Instant};
use tokio::process::Command;

async fn pid_alive(pid: u32) -> bool {
    // POSIX: kill -0 returns 0 if process exists, errors if not.
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn native_worker_dies_on_drop() {
    use spur_acp::connection::native::spawn_native_worker_for_test;

    // Spawn a long-running child via the production helper.
    let child = spawn_native_worker_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");

    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await, "child should be alive after spawn");

    drop(child); // Drop the Command/Child.

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await {
            return; // PASS — child died.
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("child PID {pid} still alive 500ms after Drop; kill_on_drop missing");
}

#[tokio::test]
async fn stdio_adapter_dies_on_drop() {
    use spur_acp::connection::stdio_adapter::spawn_stdio_for_test;

    let child = spawn_stdio_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);

    drop(child);

    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("stdio child {pid} still alive 500ms after Drop");
}

#[tokio::test]
async fn cli_wrap_dies_on_drop() {
    use spur_acp::connection::cli_wrap_adapter::spawn_cli_wrap_for_test;
    let child = spawn_cli_wrap_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);
    drop(child);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("cli_wrap child {pid} still alive 500ms after Drop");
}

#[tokio::test]
async fn stream_json_dies_on_drop() {
    use spur_acp::connection::stream_json_adapter::spawn_stream_json_for_test;
    let child = spawn_stream_json_for_test("/bin/sh", &["-c", "sleep 60"])
        .await
        .expect("spawn child");
    let pid = child.id().expect("pid present");
    assert!(pid_alive(pid).await);
    drop(child);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if !pid_alive(pid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("stream_json child {pid} still alive 500ms after Drop");
}
