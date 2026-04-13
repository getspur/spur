use spur_acp::ReviewDecision;
use spur_core::{review_dispatcher_loop, ExecutorId, InteractiveInput, ReviewSink};

// ─── Task 9: run_gate_for_candidate tests ─────────────────────────────

#[tokio::test(start_paused = true)]
async fn approve_decision_produces_success_status() {
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::orchestrator::run_gate_for_candidate;

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
    use spur_core::orchestrator::run_gate_for_candidate;

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
    use spur_core::orchestrator::run_gate_for_candidate;

    let sink = ReviewSink::new();
    let sink_for_test = sink.clone();

    let gate = tokio::spawn(async move {
        run_gate_for_candidate(
            ExecutorId::new("e1"),
            1,
            DelegationStatus::Success,
            std::time::Duration::from_secs(300),
            TimeoutFallback::Reject {
                reason: "t".into(),
            },
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
    use spur_core::orchestrator::run_gate_for_candidate;

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
    use spur_acp::{DelegationStatus, ReviewDecision, TimeoutFallback};
    use spur_core::{orchestrator::run_gate_with_retries, ExecutorId, ReviewSink};
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
    use spur_core::{orchestrator::run_gate_with_retries, ExecutorId, ReviewSink};
    use std::time::Duration;

    let sink = ReviewSink::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ReviewDecision>(8);
    for i in 0..4 {
        tx.send(ReviewDecision::Retry {
            new_constraints: format!("try {}", i + 1),
        })
        .await
        .unwrap();
    }
    drop(tx);

    let sink_for_task = sink.clone();
    tokio::spawn(async move {
        let mut attempt = 1u32;
        while let Some(d) = rx.recv().await {
            loop {
                tokio::task::yield_now().await;
                if sink_for_task
                    .submit(ExecutorId::new("e1"), attempt, d.clone())
                    .await
                {
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
        3,
        sink,
    )
    .await;
    match final_status {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded"), "got: {}", error);
            assert!(error.contains("3"), "got: {}", error);
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}

#[tokio::test]
async fn dispatcher_routes_submit_review_to_sink() {
    let sink = ReviewSink::new();
    let rx = sink
        .register(ExecutorId::new("e1"), 1)
        .await
        .expect("registered");
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
        text: "hi".into(),
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
