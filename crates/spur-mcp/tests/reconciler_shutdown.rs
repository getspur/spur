//! Asserts `Reconciler::run` exits promptly when cancel is sent.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig};
use tempfile::TempDir;
use tokio::sync::Notify;

mod common;

fn br_available() -> bool {
    common::beads::br_available()
}

fn run_br_json(repo: &Path, args: &[&str]) -> String {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
}

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn reconciler_shutdown_on_cancel() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

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
