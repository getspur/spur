//! SIT P0 C2: a panic between overlay application and dispatched-base-OID
//! publication must fail loudly instead of persisting stale base metadata.

use serde_json::json;
use spur_mcp::plan::audit_sentinel::AuditSentinelKind;

mod common;

use common::g_strict_harness::{br_available, FaultInjectionHooks, TestHarness};

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orchestrator_panic_mid_oid_send_fails_loud() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let mut harness = TestHarness::new().await;
    let plan_id = harness
        .submit_plan_with_tasks(
            "SIT P0 C2 panic mid OID send reproducer",
            json!([
                {
                    "task_id": "T1",
                    "agent": "mock",
                    "task": "create a.rs with `pub struct A;`",
                    "depends_on": [],
                },
                {
                    "task_id": "T2",
                    "agent": "mock",
                    "task": "create b.rs after reading a.rs",
                    "depends_on": ["T1"],
                },
                {
                    "task_id": "T3",
                    "agent": "mock",
                    "task": "create c.rs after reading b.rs",
                    "depends_on": ["T2"],
                },
            ]),
        )
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T1", |worktree| {
            std::fs::write(worktree.join("a.rs"), "pub struct A;\n").expect("write T1 a.rs");
        })
        .await;

    harness.set_fault_injection(FaultInjectionHooks {
        panic_after_overlay_apply: Some("SIT P0 C2 fault inject".into()),
    });
    let panic_message = harness.dispatch_and_panic_after_overlay_apply("T2").await;
    assert!(
        panic_message.contains("SIT P0 C2 fault inject"),
        "panic payload should identify the injected fault: {panic_message}"
    );

    let t2 = harness.wait_for_terminal(&plan_id, "T2").await;
    assert_eq!(
        t2["status"], "failed",
        "T2 must fail loudly instead of awaiting review or remaining dispatched: {t2}"
    );
    let error = t2["error"].as_str().expect("T2 failed error");
    assert!(
        error.contains("orchestrator disconnected") || error.contains("dropped"),
        "T2 diagnostic should mention the dropped/disconnected completion channel: {error}"
    );

    let t2_audit = harness.latest_completion_audit_for("T2");
    let t2_delegation_id = match t2_audit {
        AuditSentinelKind::Completion {
            delegation_id,
            dispatched_base_oid,
            ..
        } => {
            assert!(
                dispatched_base_oid.is_none(),
                "T2 completion audit must persist dispatched_base_oid: None, not stale data"
            );
            delegation_id
        }
        other => panic!("expected T2 completion audit, got {other:?}"),
    };

    harness.add_audit_comment_for_task(
        "T2",
        &AuditSentinelKind::Approval {
            delegation_id: t2_delegation_id,
        },
    );
    let downstream_error = harness
        .tick_reconciler()
        .await
        .expect_err("downstream dispatch must fail loudly when an approved dep has no base OID")
        .to_string();
    assert!(
        downstream_error.contains("approved dependency T2 is missing dispatched_base_oid"),
        "downstream diagnostic should name T2's missing base OID: {downstream_error}"
    );
}
