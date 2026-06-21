//! SIT P0 C1: rehydrate a plan from beads audit comments after a server
//! restart, then dispatch a downstream task. The rehydrated plan must carry
//! dispatched_base_oid for each approved dep so that the closure walk works
//! without the in-memory PlanState.

use serde_json::json;

mod common;

use common::g_strict_harness::TestHarness;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_restart_then_dispatch_walks_rehydrated_overlay() {
    let mut harness = TestHarness::new().await;
    let plan_id = harness
        .submit_plan_with_tasks(
            "G-strict restart recovery closure reproducer",
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
                    "task": "create b.rs with `pub struct B;`",
                    "depends_on": [],
                },
                {
                    "task_id": "T3",
                    "agent": "mock",
                    "task": "create c.rs using both A and B",
                    "depends_on": ["T1", "T2"],
                },
            ]),
        )
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T1", |worktree| {
            std::fs::write(worktree.join("a.rs"), "pub struct A;\n").expect("write T1 a.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T2", |worktree| {
            std::fs::write(worktree.join("b.rs"), "pub struct B;\n").expect("write T2 b.rs");
        })
        .await;

    let beads_path = harness.beads_db_path();
    let repo_path = harness.repo_root();
    let mut harness = harness.reopen_existing_beads(beads_path, repo_path).await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T3", |worktree| {
            assert!(
                worktree.join("a.rs").exists(),
                "T3 worker must see T1's a.rs after restart"
            );
            assert!(
                worktree.join("b.rs").exists(),
                "T3 worker must see T2's b.rs after restart"
            );

            std::fs::write(
                worktree.join("c.rs"),
                "mod a;\nmod b;\npub fn combine() -> (a::A, b::B) { (a::A, b::B) }\n",
            )
            .expect("write T3 c.rs");
        })
        .await;

    let merge_status = harness.merge_plan(&plan_id).await;
    assert_eq!(
        merge_status["merge"]["status"], "succeeded",
        "merge_plan must succeed: {merge_status}"
    );
    let merge_branch = merge_status["merge"]["merge_branch"]
        .as_str()
        .expect("merge branch");

    let a = harness.show(merge_branch, "a.rs");
    assert!(
        a.contains("pub struct A"),
        "merged a.rs must retain T1 contribution, got: {a}"
    );
    let b = harness.show(merge_branch, "b.rs");
    assert!(
        b.contains("pub struct B"),
        "merged b.rs must retain T2 contribution, got: {b}"
    );
    let c = harness.show(merge_branch, "c.rs");
    assert!(
        c.contains("combine"),
        "merged c.rs must retain T3 contribution, got: {c}"
    );
}
