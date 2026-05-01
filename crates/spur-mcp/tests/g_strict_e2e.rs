//! End-to-end test: a 3-task persisted plan where T2 modifies T1's new
//! file and T3 imports both T1 and T2's symbols.
//!
//! Without G-strict, this is the bd-2dww failure mode: downstream workers
//! are based on stale main and lose upstream-created content. With G-strict,
//! the reconciler dispatches downstream tasks with the full approved
//! dependency overlay closure.

mod common;

use common::g_strict_harness::{br_available, TestHarness};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_prevents_bd_2dww_class_loss() {
    if !br_available() {
        eprintln!("skipping g_strict_prevents_bd_2dww_class_loss: `br` not on PATH");
        return;
    }

    let mut harness = TestHarness::new().await;
    let plan_id = harness.submit_plan().await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T1", |worktree| {
            std::fs::write(worktree.join("foo.rs"), "pub struct Foo { pub n: u32 }\n")
                .expect("write T1 foo.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T2", |worktree| {
            let foo_path = worktree.join("foo.rs");
            let existing =
                std::fs::read_to_string(&foo_path).expect("T2 worker must see foo.rs from T1");
            assert!(
                existing.contains("pub struct Foo"),
                "T2 worker must see T1 struct, got: {existing}"
            );
            std::fs::write(
                foo_path,
                format!(
                    "{existing}\nimpl Foo {{ pub fn new(n: u32) -> Self {{ Self {{ n }} }} }}\n"
                ),
            )
            .expect("write T2 foo.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T3", |worktree| {
            let foo = std::fs::read_to_string(worktree.join("foo.rs"))
                .expect("T3 worker must see foo.rs from T1+T2");
            assert!(
                foo.contains("pub struct Foo"),
                "T3 worker must see T1 struct, got: {foo}"
            );
            assert!(
                foo.contains("impl Foo"),
                "T3 worker must see T2 impl, got: {foo}"
            );
            std::fs::write(
                worktree.join("main.rs"),
                "use foo::Foo;\nfn main() { let _ = Foo::new(42); }\n",
            )
            .expect("write T3 main.rs");
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

    let foo = harness.show(merge_branch, "foo.rs");
    assert!(
        foo.contains("pub struct Foo"),
        "merged foo.rs must retain T1 struct, got: {foo}"
    );
    assert!(
        foo.contains("impl Foo"),
        "merged foo.rs must retain T2 impl, got: {foo}"
    );

    let main = harness.show(merge_branch, "main.rs");
    assert!(
        main.contains("use foo::Foo"),
        "merged main.rs must retain T3 import, got: {main}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_diamond_dag_closure_walks_both_parents() {
    if !br_available() {
        eprintln!("skipping g_strict_diamond_dag_closure_walks_both_parents: `br` not on PATH");
        return;
    }

    let mut harness = TestHarness::new().await;
    let plan_id = harness.submit_diamond_plan().await;

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

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T3", |worktree| {
            let a = std::fs::read_to_string(worktree.join("a.rs"))
                .expect("T3 worker must see a.rs from T1");
            assert!(
                a.contains("pub struct A"),
                "T3 worker must see T1's a.rs, got: {a}"
            );

            let b = std::fs::read_to_string(worktree.join("b.rs"))
                .expect("T3 worker must see b.rs from T2");
            assert!(
                b.contains("pub struct B"),
                "T3 worker must see T2's b.rs, got: {b}"
            );

            std::fs::write(
                worktree.join("c.rs"),
                "mod a;\nmod b;\npub fn combine() -> (a::A, b::B) { (a::A, b::B) }\n",
            )
            .expect("write T3 c.rs");
        })
        .await;

    let t3_diff = harness.get_task_diff(&plan_id, "T3").await;
    let diff = t3_diff["diff"].as_str().expect("T3 diff text");
    assert!(
        diff.contains("c.rs"),
        "T3 diff must include its own contribution: {diff}"
    );
    assert!(
        !diff.contains("a.rs"),
        "T3 diff must exclude inherited T1 overlay: {diff}"
    );
    assert!(
        !diff.contains("b.rs"),
        "T3 diff must exclude inherited T2 overlay: {diff}"
    );

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_grandparent_depth_chain_walks_full_closure() {
    if !br_available() {
        eprintln!("skipping g_strict_grandparent_depth_chain_walks_full_closure: `br` not on PATH");
        return;
    }

    let mut harness = TestHarness::new().await;
    let plan_id = harness.submit_grandparent_plan().await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T1", |worktree| {
            std::fs::write(worktree.join("lvl1.rs"), "pub const LVL1: u8 = 1;\n")
                .expect("write T1 lvl1.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T2", |worktree| {
            let lvl1 = std::fs::read_to_string(worktree.join("lvl1.rs"))
                .expect("T2 worker must see lvl1.rs from T1");
            assert!(
                lvl1.contains("LVL1"),
                "T2 worker must see T1 contribution, got: {lvl1}"
            );

            std::fs::write(worktree.join("lvl2.rs"), "pub const LVL2: u8 = 2;\n")
                .expect("write T2 lvl2.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T3", |worktree| {
            let lvl1 = std::fs::read_to_string(worktree.join("lvl1.rs"))
                .expect("T3 worker must see lvl1.rs from T1");
            assert!(
                lvl1.contains("LVL1"),
                "T3 worker must see T1 contribution, got: {lvl1}"
            );

            let lvl2 = std::fs::read_to_string(worktree.join("lvl2.rs"))
                .expect("T3 worker must see lvl2.rs from T2");
            assert!(
                lvl2.contains("LVL2"),
                "T3 worker must see T2 contribution, got: {lvl2}"
            );

            std::fs::write(worktree.join("lvl3.rs"), "pub const LVL3: u8 = 3;\n")
                .expect("write T3 lvl3.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T4", |worktree| {
            let lvl1 = std::fs::read_to_string(worktree.join("lvl1.rs"))
                .expect("T4 worker must see lvl1.rs from T1");
            assert!(
                lvl1.contains("LVL1"),
                "T4 worker must see T1 contribution, got: {lvl1}"
            );

            let lvl2 = std::fs::read_to_string(worktree.join("lvl2.rs"))
                .expect("T4 worker must see lvl2.rs from T2");
            assert!(
                lvl2.contains("LVL2"),
                "T4 worker must see T2 contribution, got: {lvl2}"
            );

            let lvl3 = std::fs::read_to_string(worktree.join("lvl3.rs"))
                .expect("T4 worker must see lvl3.rs from T3");
            assert!(
                lvl3.contains("LVL3"),
                "T4 worker must see T3 contribution, got: {lvl3}"
            );

            std::fs::write(worktree.join("lvl4.rs"), "pub const LVL4: u8 = 4;\n")
                .expect("write T4 lvl4.rs");
        })
        .await;

    let t3_dispatched_base_oid = harness.completion_dispatched_base_oid("T3");
    let t4_dispatched_base_oid = harness.completion_dispatched_base_oid("T4");
    assert_ne!(
        t4_dispatched_base_oid, t3_dispatched_base_oid,
        "T4 dispatched_base_oid must include T3's overlay and differ from T3's base"
    );

    let merge_status = harness.merge_plan(&plan_id).await;
    assert_eq!(
        merge_status["merge"]["status"], "succeeded",
        "merge_plan must succeed: {merge_status}"
    );
    let merge_branch = merge_status["merge"]["merge_branch"]
        .as_str()
        .expect("merge branch");

    for (path, expected) in [
        ("lvl1.rs", "LVL1"),
        ("lvl2.rs", "LVL2"),
        ("lvl3.rs", "LVL3"),
        ("lvl4.rs", "LVL4"),
    ] {
        let contents = harness.show(merge_branch, path);
        assert!(
            contents.contains(expected),
            "merged {path} must retain {expected}, got: {contents}"
        );
    }
}
