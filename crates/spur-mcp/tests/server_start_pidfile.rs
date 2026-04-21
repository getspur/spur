use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use spur_pm::PmService;
use tempfile::TempDir;

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

#[tokio::test]
async fn beads_backed_start_requires_repo_root_before_listener_boot() {
    if !br_available() {
        eprintln!(
            "skipping beads_backed_start_requires_repo_root_before_listener_boot: `br` not on PATH"
        );
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = beads_pm(dir.path()).await;
    let brain_sid = BrainSessionId::new(SessionId::new());

    let (server, _channel) =
        McpCallbackServer::new(&brain_sid, Some(pm), None, test_continuation_ctx());
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
