// The `Send` proof for spawned server futures traverses deep dependency
// type chains (lance_io/moka/portable_atomic) inside spur-context; the
// chain exceeds the default trait-solver recursion limit (E0275).
#![recursion_limit = "256"]

use spur_acp::ReviewDecision;
use spur_core::{review_dispatcher_loop, ExecutorId, InteractiveInput, ReviewSink};

// ─── Task 9: run_gate_for_candidate tests ─────────────────────────────

#[tokio::test(start_paused = true)]
async fn approve_decision_produces_success_status() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::test_support::run_gate_for_candidate;

    let sink = ReviewSink::new();
    let sink_for_test = sink.clone();

    let gate = tokio::spawn(async move {
        run_gate_for_candidate(
            ExecutorId::new("e1"),
            /* attempt_n */ 1,
            /* candidate */ DelegationStatus::Success,
            /* review_timeout */ std::time::Duration::from_secs(300),
            /* timeout_fallback */
            TimeoutFallback::Reject { reason: "t".into() },
            sink,
        )
        .await
    });

    // Yield so the spawned gate task gets polled and reaches
    // `register_gate` before we submit.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let routed = sink_for_test
        .submit(ExecutorId::new("e1"), 1, ReviewDecision::Approve)
        .await;
    assert!(routed);

    let status = gate.await.unwrap();
    assert!(matches!(status, DelegationStatus::Success));
}

#[tokio::test(start_paused = true)]
async fn timeout_produces_timed_out_status_and_removes_entry() {
    use spur_acp::{DelegationStatus, TimeoutFallback};
    use spur_core::test_support::run_gate_for_candidate;

    let sink = ReviewSink::new();
    let sink_for_test = sink.clone();

    let gate = tokio::spawn(async move {
        run_gate_for_candidate(
            ExecutorId::new("e1"),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(60),
            TimeoutFallback::Reject {
                reason: "review timeout".into(),
            },
            sink,
        )
        .await
    });

    // Yield so the spawned gate task registers and begins its select!
    // loop before we advance virtual time past the timeout.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(120)).await;

    let status = gate.await.unwrap();
    match status {
        DelegationStatus::TimedOut {
            waited_for,
            fallback: TimeoutFallback::Reject { reason },
        } => {
            assert_eq!(waited_for, std::time::Duration::from_secs(60));
            assert_eq!(reason, "review timeout");
        }
        other => panic!("expected TimedOut, got {:?}", other),
    }

    // Post-timeout: entry must be gone (explicit-remove contract).
    let stale = sink_for_test
        .submit(ExecutorId::new("e1"), 1, spur_acp::ReviewDecision::Approve)
        .await;
    assert!(!stale, "timeout path must remove the entry");
}

#[tokio::test(start_paused = true)]
async fn reject_decision_produces_rejected_status() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::test_support::run_gate_for_candidate;

    let sink = ReviewSink::new();
    let sink_for_test = sink.clone();

    let gate = tokio::spawn(async move {
        run_gate_for_candidate(
            ExecutorId::new("e1"),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(300),
            TimeoutFallback::Reject { reason: "t".into() },
            sink,
        )
        .await
    });

    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    sink_for_test
        .submit(
            ExecutorId::new("e1"),
            1,
            ReviewDecision::Reject {
                reason: "too large".into(),
            },
        )
        .await;

    let status = gate.await.unwrap();
    match status {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "too large"),
        other => panic!("expected Rejected, got {:?}", other),
    }
}

#[tokio::test(start_paused = true)]
async fn modify_decision_produces_modified_status() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::test_support::run_gate_for_candidate;

    let sink = ReviewSink::new();
    let sink_for_test = sink.clone();

    let gate = tokio::spawn(async move {
        run_gate_for_candidate(
            ExecutorId::new("e1"),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(300),
            TimeoutFallback::Reject { reason: "t".into() },
            sink,
        )
        .await
    });

    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    sink_for_test
        .submit(
            ExecutorId::new("e1"),
            1,
            ReviewDecision::Modify {
                note: "fix naming".into(),
            },
        )
        .await;

    let status = gate.await.unwrap();
    match status {
        DelegationStatus::Modified { reviewer_note } => assert_eq!(reviewer_note, "fix naming"),
        other => panic!("expected Modified, got {:?}", other),
    }
}

// ─── Task 10: run_gate_with_retries tests ──────────────────────────────

#[tokio::test(start_paused = true)]
async fn retry_then_approve_produces_success() {
    // 2 Retrys then Approve. With max_review_retries = 3 and `>` check:
    // attempts 1 (Retry→2), 2 (Retry→3), 3 (Approve). Final status: Success.
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::{test_support::run_gate_with_retries, ExecutorId, ReviewSink};
    use std::time::Duration;

    let sink = ReviewSink::new();

    let (decisions_tx, mut decisions_rx) = tokio::sync::mpsc::channel::<ReviewDecision>(8);
    decisions_tx
        .send(ReviewDecision::Retry {
            new_constraints: "try harder".into(),
        })
        .await
        .unwrap();
    decisions_tx
        .send(ReviewDecision::Retry {
            new_constraints: "try harder 2".into(),
        })
        .await
        .unwrap();
    decisions_tx.send(ReviewDecision::Approve).await.unwrap();
    drop(decisions_tx);

    let sink_for_task = sink.clone();
    tokio::spawn(async move {
        let mut attempt = 1u32;
        while let Some(d) = decisions_rx.recv().await {
            loop {
                tokio::task::yield_now().await;
                if sink_for_task
                    .submit(ExecutorId::new("e1"), attempt, d.clone())
                    .await
                {
                    break;
                }
            }
            if matches!(d, ReviewDecision::Retry { .. }) {
                attempt += 1;
            }
        }
    });

    let final_status = run_gate_with_retries(
        ExecutorId::new("e1"),
        DelegationStatus::Success,
        Duration::from_secs(60),
        TimeoutFallback::Reject { reason: "t".into() },
        3,
        sink,
    )
    .await;
    assert!(matches!(final_status, DelegationStatus::Success));
}

#[tokio::test(start_paused = true)]
async fn retry_limit_exceeded_produces_failed() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::{test_support::run_gate_with_retries, ExecutorId, ReviewSink};
    use std::sync::Arc;
    use std::time::Duration;

    let sink = ReviewSink::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ReviewDecision>(8);
    // Send 3 Retrys with max_review_retries = 2. Expected: first 2 bump
    // attempt_n (1→2, 2→3), and the 3rd Retry (arriving at attempt_n=3)
    // fails with "retry limit exceeded after 2 attempts".
    for i in 0..3 {
        tx.send(ReviewDecision::Retry {
            new_constraints: format!("try {}", i + 1),
        })
        .await
        .unwrap();
    }
    drop(tx);

    let attempts_consumed = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let attempts_for_task = Arc::clone(&attempts_consumed);

    let sink_for_task = sink.clone();
    let dispatcher = tokio::spawn(async move {
        let mut attempt = 1u32;
        while let Some(d) = rx.recv().await {
            loop {
                tokio::task::yield_now().await;
                if sink_for_task
                    .submit(ExecutorId::new("e1"), attempt, d.clone())
                    .await
                {
                    attempts_for_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
            }
            attempt += 1;
        }
    });

    let final_status = run_gate_with_retries(
        ExecutorId::new("e1"),
        DelegationStatus::Success,
        Duration::from_secs(60),
        TimeoutFallback::Reject { reason: "t".into() },
        2, // max_review_retries
        sink,
    )
    .await;

    match final_status {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded"), "got: {}", error);
            // max_review_retries=2; bound fires at attempt_n=3 (1 original
            // + 2 retries, then a 3rd Retry decision exceeds the bound).
            // The error reports `attempt_n` (the count that ran), not
            // `max_review_retries`.
            assert!(error.contains("3"), "got: {}", error);
        }
        other => panic!("expected Failed, got {:?}", other),
    }

    // Wait for the dispatcher task to finish consuming. All 3 Retry
    // submits must have been routed: 2 that bumped attempt_n and the
    // 3rd that triggered the limit-exceeded failure.
    dispatcher.await.unwrap();
    assert_eq!(
        attempts_consumed.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "all 3 Retry decisions should have been consumed (2 bumps + 1 fail)"
    );
}

#[tokio::test]
async fn dispatcher_routes_submit_review_to_sink() {
    let sink = ReviewSink::new();
    let rx = sink
        .register_handle(ExecutorId::new("e1"), 1)
        .await
        .expect("registered")
        .into_rx();
    let (tx, input_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(4);

    let sink_for_task = sink.clone();
    let handle = tokio::spawn(review_dispatcher_loop(input_rx, sink_for_task));

    tx.send(InteractiveInput::SubmitReview {
        executor_id: "e1".into(),
        attempt_n: 1,
        decision: ReviewDecision::Approve,
    })
    .await
    .unwrap();

    let decision = rx.await.expect("decision delivered");
    assert!(matches!(decision, ReviewDecision::Approve));

    drop(tx);
    handle.await.unwrap();
}

#[tokio::test]
async fn dispatcher_ignores_non_review_variants() {
    // Sends Message / ListSessions / etc into the dispatcher channel;
    // assert the ReviewSink has no registered entry and the dispatcher
    // does not panic.
    let sink = ReviewSink::new();
    let (tx, input_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(4);
    let handle = tokio::spawn(review_dispatcher_loop(input_rx, sink.clone()));

    tx.send(InteractiveInput::Message {
        blocks: vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(
            "hi".to_string(),
        ))],
        interrupt: false,
    })
    .await
    .unwrap();
    tx.send(InteractiveInput::ListSessions).await.unwrap();

    // Give the dispatcher a chance to process and ignore.
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    drop(tx);
    handle.await.unwrap();
    // No assertion beyond "did not panic" + "handle completed".
}

// ─── Task 11: should_preserve_worktree tests ───────────────────────────

#[test]
fn should_preserve_worktree_matches_expected_variants() {
    use spur_acp::{DelegationStatus, TimeoutFallback};
    use spur_core::orchestrator::should_preserve_worktree;
    use std::path::PathBuf;

    // Non-preserved: Success / Failed / Conflict / Timeout (worker-hang) / Modified.
    assert!(!should_preserve_worktree(&DelegationStatus::Success));
    assert!(!should_preserve_worktree(&DelegationStatus::Failed {
        error: "e".into(),
    }));
    assert!(!should_preserve_worktree(&DelegationStatus::Conflict {
        files: vec![PathBuf::from("a")]
    }));
    assert!(!should_preserve_worktree(&DelegationStatus::Timeout));
    assert!(!should_preserve_worktree(&DelegationStatus::Modified {
        reviewer_note: "n".into(),
    }));

    // Preserved: Rejected (human feedback — worker's work needs inspection).
    assert!(should_preserve_worktree(&DelegationStatus::Rejected {
        reason: "r".into()
    }));

    // TimedOut with Reject or Abandon fallback: preserve for inspection
    // (no human reviewed and the configured policy says "treat as no" or
    // "abandon" — operator may still want to read the diff).
    assert!(should_preserve_worktree(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Reject { reason: "r".into() },
    }));
    assert!(should_preserve_worktree(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    }));

    // TimedOut with Approve fallback: auto-approved — commit + remove,
    // NOT preserved. Matches spec's "retained as if reviewed" semantics.
    assert!(!should_preserve_worktree(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Approve,
    }));

    // Cancelled (INV-6): preserve partial work for inspection.
    assert!(should_preserve_worktree(&DelegationStatus::Cancelled {
        reason: "brain requested cancel".into(),
    }));
}

#[test]
fn should_commit_worker_diff_matches_expected_variants() {
    use spur_acp::{DelegationStatus, TimeoutFallback};
    use spur_core::orchestrator::should_commit_worker_diff;
    use std::path::PathBuf;

    // Commit: Success, Modified (human approval/annotation),
    // TimedOut { Approve } (auto-approve fallback "retained as if reviewed").
    assert!(should_commit_worker_diff(&DelegationStatus::Success));
    assert!(should_commit_worker_diff(&DelegationStatus::Modified {
        reviewer_note: "n".into()
    }));
    assert!(should_commit_worker_diff(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Approve,
    }));

    // No commit: TimedOut { Reject | Abandon } (preserved for inspection).
    assert!(!should_commit_worker_diff(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Reject { reason: "r".into() },
    }));
    assert!(!should_commit_worker_diff(&DelegationStatus::TimedOut {
        waited_for: std::time::Duration::from_secs(60),
        fallback: TimeoutFallback::Abandon,
    }));

    // No commit: Rejected (human said no).
    assert!(!should_commit_worker_diff(&DelegationStatus::Rejected {
        reason: "r".into()
    }));

    // No commit: Failed / Conflict / Timeout (worker hang) — no clean
    // diff to merge.
    assert!(!should_commit_worker_diff(&DelegationStatus::Failed {
        error: "e".into()
    }));
    assert!(!should_commit_worker_diff(&DelegationStatus::Conflict {
        files: vec![PathBuf::from("a")]
    }));
    assert!(!should_commit_worker_diff(&DelegationStatus::Timeout));

    // No commit: Cancelled (INV-6) — partial work preserved but not merged.
    assert!(!should_commit_worker_diff(&DelegationStatus::Cancelled {
        reason: "brain requested cancel".into(),
    }));
}

// ─── Fix 1: ExecutorReviewCancelled on timeout / sender-drop ─────────

/// Verifies that applying `ExecutorReviewCancelled { reason: "review timeout" }`
/// clears `pending_review` in the lineage projection — the guard against the
/// TUI review card staying open indefinitely after a review timeout.
#[test]
fn review_cancelled_with_timeout_reason_clears_pending_review() {
    use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ExecutorLineage};

    let mut lineage = ExecutorLineage::new();

    // Spawn an executor.
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-timeout".into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "some task".into(),
    }));

    // Request review — simulates the orchestrator entering the review gate.
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-timeout".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ready".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));

    let n = lineage.node(&ExecutorId::new("exec-timeout")).unwrap();
    assert!(
        n.pending_review.is_some(),
        "pending_review must be set after review requested"
    );
    assert_eq!(lineage.pending_reviews().len(), 1);

    // Simulate the timeout branch emitting ExecutorReviewCancelled.
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: "exec-timeout".into(),
        reason: "review timeout".to_string(),
    }));

    let n = lineage.node(&ExecutorId::new("exec-timeout")).unwrap();
    assert!(
        n.pending_review.is_none(),
        "pending_review must be cleared after ExecutorReviewCancelled(timeout)"
    );
    assert_eq!(
        lineage.pending_reviews().len(),
        0,
        "executor must be removed from pending_review_order"
    );
}

/// Verifies that applying `ExecutorReviewCancelled { reason: "review sender dropped" }`
/// also clears `pending_review` — covers the sender-drop branch.
#[test]
fn review_cancelled_with_sender_dropped_reason_clears_pending_review() {
    use spur_acp::{ReviewKind, ReviewPayload, Role, SessionId, SpurEvent, SpurEventBody};
    use spur_core::{ExecutorId, ExecutorLineage};

    let mut lineage = ExecutorLineage::new();

    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "exec-drop".into(),
        parent_id: None,
        session_id: SessionId::new(),
        agent: "worker".into(),
        role: Role::Executor,
        task_spec: "task".into(),
    }));
    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
        id: "exec-drop".into(),
        attempt_n: 1,
        kind: ReviewKind::Completion,
        payload: ReviewPayload {
            summary: "ok".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        },
    }));

    assert!(lineage
        .node(&ExecutorId::new("exec-drop"))
        .unwrap()
        .pending_review
        .is_some());

    lineage.apply(&SpurEvent::now(SpurEventBody::ExecutorReviewCancelled {
        id: "exec-drop".into(),
        reason: "review sender dropped".to_string(),
    }));

    let n = lineage.node(&ExecutorId::new("exec-drop")).unwrap();
    assert!(
        n.pending_review.is_none(),
        "pending_review must be cleared after ExecutorReviewCancelled(sender dropped)"
    );
    assert!(
        !lineage.pending_reviews().iter().any(|e| e.0 == "exec-drop"),
        "executor must be removed from pending_review_order"
    );
}

// ─── Task 12: brain-cancellation audit event ──────────────────────────

#[tokio::test(start_paused = true)]
async fn brain_cancellation_during_review_emits_review_cancelled() {
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_core::event_funnel::spawn_funnel;
    use spur_core::orchestrator::cleanup_cancelled_review;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    let sink = ReviewSink::new();
    // Register a pending review so the helper has something to clean up.
    let _handle = sink
        .register_handle(ExecutorId::new("e1"), 1)
        .await
        .unwrap();

    let (tx, mut event_rx) = broadcast::channel::<SpurEvent>(8);
    // Build a funnel pointing at `tx` so the test can observe the
    // stamped event on `event_rx`.
    let funnel = spawn_funnel(tx.clone(), Arc::new(AtomicU64::new(0)));

    cleanup_cancelled_review(
        &ExecutorId::new("e1"),
        "brain call cancelled",
        &funnel,
        &sink,
    )
    .await;

    let ev = event_rx.recv().await.expect("event");
    match ev.body {
        SpurEventBody::ExecutorReviewCancelled { id, reason } => {
            assert_eq!(id, "e1");
            assert_eq!(reason, "brain call cancelled");
        }
        other => panic!("expected ExecutorReviewCancelled, got {:?}", other),
    }

    // Sink entry must be gone.
    let stale = sink
        .submit(ExecutorId::new("e1"), 1, spur_acp::ReviewDecision::Approve)
        .await;
    assert!(!stale, "sink entry must be removed by cleanup");
}
