use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use spur_pm::PmService;
use tempfile::TempDir;

mod common;

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(
            None,  // no github_repo
            true,  // beads_enabled
            false, // github_enabled
            repo, None, // closed_status default
        )
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init"),
    )
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn beads_backed_start_requires_repo_root_before_listener_boot() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = beads_pm(dir.path()).await;
    let brain_sid = BrainSessionId::new(SessionId::new());

    let (server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(pm),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    let err = Arc::new(server)
        .start()
        .await
        .expect_err("missing repo_root must fail before the callback server starts");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("repo_root not set on McpCallbackServer"),
        "unexpected error: {msg}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn dropping_server_handle_releases_pidfile_for_next_start() {
    assert!(br_available(), "this test requires `br` on PATH; run with `cargo test -- --ignored`");
    skip_if_no_loopback!("dropping_server_handle_releases_pidfile_for_next_start");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = beads_pm(dir.path()).await;
    let brain_sid = BrainSessionId::new(SessionId::new());

    let (mut server, _channel) = McpCallbackServer::new(
        &brain_sid,
        Some(pm.clone()),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());
    server.set_reconciler_enabled(true, None);

    let (_url, handle) = Arc::new(server)
        .start()
        .await
        .expect("initial start should succeed");

    // Regression: dropping the start() handle must not detach a live server
    // that keeps holding `.beads/.spur-brain.pid`.
    drop(handle);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    loop {
        let (mut next_server, _channel) = McpCallbackServer::new(
            &brain_sid,
            Some(pm.clone()),
            None,
            test_continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            common::server_builder::pro_feature_gate(),
        );
        next_server.set_repo_root(dir.path().to_path_buf());
        next_server.set_reconciler_enabled(true, None);

        match Arc::new(next_server).start().await {
            Ok((_url, next_handle)) => {
                drop(next_handle);
                return;
            }
            Err(error)
                if format!("{error:#}")
                    .contains("another SPUR brain session already owns this .beads/")
                    && tokio::time::Instant::now() < deadline =>
            {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!(
                "dropping the server handle must release the pidfile for the next start: {error:#}"
            ),
        }
    }
}
