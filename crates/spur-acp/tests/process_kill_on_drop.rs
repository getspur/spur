//! Phase 0a integration tests — verify worker child processes die when
//! the spawning Tokio Command is dropped (kill_on_drop semantics).

use std::path::Path;
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

/// Build a `NativeAcpConnection` whose agent is `/bin/sh -c 'sleep 30'`,
/// rooted under `repo_root` so the pgid registry writes there. The mock
/// agent doesn't speak ACP, so `initialize()` will hang past the SDK
/// handshake — but the child spawn (and the registry write) runs first
/// inside the ACP thread, so a short timeout is enough to have a live
/// child + an on-disk record by the time we return.
async fn make_test_native_connection(repo_root: &Path) -> spur_acp::NativeAcpConnection {
    use agent_client_protocol::schema::{InitializeRequest, ProtocolVersion};
    use spur_acp::connection::AgentConnection;
    use spur_acp::NativeAcpConnection;

    let mut conn = NativeAcpConnection::new(
        "mock-sleep",
        "/bin/sh",
        vec!["-c".to_string(), "sleep 30".to_string()],
        None,
    );
    conn.set_repo_root(repo_root.to_path_buf());

    // initialize() never returns against /bin/sh (no ACP handshake), but the
    // spawn block (incl. registry write) executes before the SDK handshake,
    // so 500 ms is plenty for the .toml to land on disk.
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST)),
    )
    .await;

    conn
}

#[tokio::test]
async fn spawn_writes_pgid_toml_drop_deletes_it() {
    use spur_acp::orphan_registry::PgidRegistry;

    let dir = tempfile::tempdir().expect("tmpdir");
    let pgids = dir.path().join(".spur").join("pgids");

    // Spawn a NativeAcpConnection in this temp root with a cheap mock
    // command that sleeps. (Use the existing test helper if one exists;
    // otherwise stand up a minimal one.)
    let conn = make_test_native_connection(dir.path()).await;

    // After spawn, exactly one record must exist.
    let registry = PgidRegistry::new(&pgids);
    let recs = registry.load_all().expect("load");
    assert_eq!(
        recs.len(),
        1,
        "expected 1 record after spawn, got {:?}",
        recs
    );
    let pgid = recs[0].pgid;

    drop(conn); // triggers killpg + .toml delete

    // Allow drop's spawn_blocking to finalize.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let recs = registry.load_all().expect("load");
    assert_eq!(
        recs.len(),
        0,
        "expected 0 records after drop, got {:?}",
        recs
    );
    let _ = pgid;
}
