//! Asserts `Reconciler::run` exits promptly when cancel is sent.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig};
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br_json(repo: &Path, args: &[&str]) -> String {
    let mut full_args: Vec<&str> = args.to_vec();
    full_args.push("--json");
    let out = Command::new("br")
        .args(&full_args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        String::from_utf8_lossy(&out.stdout).to_string()
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
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

#[tokio::test]
async fn reconciler_shutdown_on_cancel() {
    if !br_available() {
        eprintln!("skipping reconciler_shutdown_on_cancel: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    for idx in 0..5 {
        run_br_json(
            dir.path(),
            &[
                "create",
                "--type",
                "task",
                "--title",
                &format!("Shutdown task {idx}"),
                "--priority",
                "2",
            ],
        );
    }

    let pm = spur_pm::PmService::try_new(None, true, false, dir.path(), None)
        .await
        .expect("PmService::try_new failed")
        .expect("expected Some(PmService) — beads dir must exist after br init");
    let pm = Arc::new(pm);

    let cfg = ReconcilerConfig {
        base_interval: Duration::from_millis(5),
        idle_ceiling: Duration::from_millis(50),
        backoff_factor: 2,
        ..Default::default()
    };
    let reconciler = Reconciler::new(
        cfg,
        pm,
        Arc::new(Notify::new()),
        None,
        None,
        common::server_builder::pro_feature_gate(),
    );

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(async move { reconciler.run(cancel_rx).await });

    tokio::time::sleep(Duration::from_millis(100)).await;
    cancel_tx.send(()).expect("cancel receiver alive");

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("reconciler must shut down within 5s of cancel")
        .expect("reconciler task must not panic");
}
