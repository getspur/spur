//! Integration: `submit_plan` with an explicit `base: Branch{...}` succeeds
//! even when the brain WT is dirty, and the resulting plan dispatches off
//! the explicit branch's HEAD, not a stash-derived snapshot.

use std::process::Command;

use serde_json::{json, Value};
use spur_mcp::tools::{BaseSpec, BaseTarget};

mod common;

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn submit_plan_explicit_branch_base_with_dirty_wt() {
    let mut h = common::g_strict_harness::TestHarness::new().await;
    let repo = h.repo_root();

    run_git(&repo, &["checkout", "-q", "-b", "phase0"]);
    std::fs::write(repo.join("phase0.txt"), "phase0\n").unwrap();
    run_git(&repo, &["add", "phase0.txt"]);
    run_git(&repo, &["commit", "-q", "-m", "phase0 work"]);
    let phase0_oid = run_git(&repo, &["rev-parse", "HEAD"]);
    run_git(&repo, &["checkout", "-q", "main"]);

    std::fs::write(repo.join("brain_wt_dirty.txt"), "dirty\n").unwrap();
    std::fs::write(repo.join("README.md"), "dirty seed\n").unwrap();

    let response: Value = h
        .server_test_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "br-osl explicit base test",
            "base": { "kind": "branch", "name": "phase0" },
            "tasks": [
                { "task_id": "T1", "agent": "mock", "task": "do work", "depends_on": [] }
            ]
        }))
        .await;

    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "submit_plan must succeed with explicit base + dirty WT; got: {response}"
    );

    h.tick_until_request_or_timeout().await;
    let request = h
        .take_next_dispatch()
        .expect("reconciler must dispatch T1 with the explicit base");

    match request.base.as_ref().expect("base must be Some") {
        BaseSpec::WithOverlay {
            base: BaseTarget::Branch { name },
            overlays,
        } => {
            assert!(
                name.starts_with("spur/brain-snapshot-"),
                "reconciler still wraps the snapshot ref; got {name}"
            );
            assert!(
                overlays.is_empty(),
                "T1 has no approved deps so no overlays expected; got {overlays:?}"
            );
            let snap_oid = run_git(&repo, &["rev-parse", "--verify", name.as_str()]);
            assert_eq!(
                snap_oid, phase0_oid,
                "explicit base must materialize as a snapshot ref pointing at the named branch's OID"
            );
        }
        other => panic!("unexpected base shape: {other:?}"),
    }
}

#[tokio::test]
async fn submit_plan_unknown_base_branch_returns_error() {
    let h = common::g_strict_harness::TestHarness::new().await;

    let response: Value = h
        .server_test_submit_plan(json!({
            "persist_as_epic": true,
            "epic_title": "br-osl bad base",
            "base": { "kind": "branch", "name": "does-not-exist" },
            "tasks": [
                { "task_id": "T1", "agent": "mock", "task": "do work", "depends_on": [] }
            ]
        }))
        .await;

    let err = response.get("error").cloned().unwrap_or(Value::Null);
    assert!(
        !err.is_null(),
        "submit_plan must reject unknown base branch; got: {response}"
    );
    let msg = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        msg.contains("does-not-exist") || msg.contains("base"),
        "error message must mention the bad ref or 'base'; got: {msg}"
    );
}
