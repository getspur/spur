//! SIT P0 C2: a panic between overlay application and dispatched-base-OID
//! publication must fail loudly instead of persisting stale base metadata.

use serde_json::json;
use spur_mcp::plan::audit_sentinel::AuditSentinelKind;

mod common;

use common::g_strict_harness::{FaultInjectionHooks, TestHarness};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn orchestrator_panic_mid_oid_send_fails_loud_and_retries_without_stale_base_oid() {
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

    let status = harness.wait_for_task_status(&plan_id, "T2", "ready").await;
    let t2 = harness
        .task_status_entry(&status, "T2")
        .expect("T2 status entry");
    assert_eq!(
        t2["attempt"], 2,
        "T2 should auto-retry once after the loud failure: {t2}"
    );
    assert_eq!(
        t2["history_count"], 1,
        "T2 should retain the failed attempt in history: {t2}"
    );

    let t2_audit = harness.latest_completion_audit_for("T2");
    match t2_audit {
        AuditSentinelKind::Completion {
            dispatched_base_oid,
            ..
        } => {
            assert!(
                dispatched_base_oid.is_none(),
                "T2 completion audit must persist dispatched_base_oid: None, not stale data"
            );
        }
        other => panic!("expected T2 completion audit, got {other:?}"),
    };
}
