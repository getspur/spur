//! Integration tests for delegate_parallel response shape invariants (INV-ASYNC-6).
//!
//! These tests gate the Phase 2 handler rewrite that makes per-task dispatch
//! concurrent via FuturesUnordered/JoinSet while preserving input→output order.
//!
//! RUNNING:
//!   cargo test -p spur-mcp --test parallel_response_shape -- --ignored
//!
//! EXPECTED BEHAVIOR:
//!   - Current main (serial dispatch): tests FAIL
//!   - After Phase 2 handler rewrite: tests PASS

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use spur_acp::domain::delegation::{DelegationResult, DelegationStatus};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::{McpCallbackServer, WorkerInfo};
use tokio::time::Instant;

mod common;

fn empty_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn extract_response_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn parse_results(text: &str) -> Vec<Value> {
    serde_json::from_str(text).expect("response should be valid JSON array")
}

#[allow(non_snake_case)] // INV-ASYNC-6 is the spec invariant ID; preserve in test name.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_response_length_invariant_INV_ASYNC_6() {
    const N: usize = 5;
    const COMPLETE_COUNT: usize = 3;
    const PENDING_COUNT: usize = 2;
    const INLINE_WAIT_MS: u64 = 100;

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        empty_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_inline_wait(Duration::from_millis(INLINE_WAIT_MS));
    server.set_workers(vec![WorkerInfo {
        name: "fake-worker".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    let server = Arc::new(server);

    let respond_count = Arc::new(AtomicUsize::new(0));
    let respond_count_clone = Arc::clone(&respond_count);

    let worker_handle = tokio::spawn(async move {
        // Hold the pending senders alive — dropping them would resolve the
        // oneshot with Err, which the handler's fast arm treats as inline
        // Failed completion, not as a pending slow-arm. To exercise the
        // genuine pending path we must keep the senders alive past the
        // handler's inline_wait window.
        let mut held_senders = Vec::new();
        for _ in 0..N {
            let req = channel.request_rx.recv().await.expect("delegation request");
            let respond_now = respond_count_clone.fetch_add(1, Ordering::SeqCst) < COMPLETE_COUNT;
            if respond_now {
                let _ = req.respond_to.send(DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: Some("done".into()),
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                });
            } else {
                held_senders.push(req.respond_to);
            }
        }
        // Park indefinitely so senders stay alive until test teardown.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(held_senders);
    });

    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_parallel(vec![
                ("fake-worker", "task-0"),
                ("fake-worker", "task-1"),
                ("fake-worker", "task-2"),
                ("fake-worker", "task-3"),
                ("fake-worker", "task-4"),
            ])
            .await
    });

    let mcp_resp = mcp_resp_handle.await.expect("mcp task join");
    worker_handle.abort();
    let _ = worker_handle.await; // aborted — senders held until here

    let text = extract_response_text(&mcp_resp);
    let results = parse_results(&text);

    assert_eq!(
        results.len(),
        N,
        "INV-ASYNC-6: delegate_parallel with N={} tasks MUST return array of length N",
        N
    );

    let mut completed_count = 0;
    let mut pending_count = 0;

    for (i, entry) in results.iter().enumerate() {
        assert!(
            entry.get("status").is_some(),
            "entry {} must have status field",
            i
        );
        assert!(
            entry.get("delegation_id").is_some(),
            "entry {} must have delegation_id field",
            i
        );

        let status = entry["status"].as_str().expect("status must be string");
        match status {
            "completed" => {
                completed_count += 1;
                assert!(
                    entry.get("result").is_some(),
                    "completed entry {} must have result field",
                    i
                );
                assert_eq!(
                    entry["continuation_will_fire"].as_bool(),
                    Some(false),
                    "completed entry {} should have continuation_will_fire=false",
                    i
                );
            }
            "pending" => {
                pending_count += 1;
                assert_eq!(
                    entry["continuation_will_fire"].as_bool(),
                    Some(true),
                    "pending entry {} should have continuation_will_fire=true",
                    i
                );
            }
            _ => panic!("unexpected status: {}", status),
        }
    }

    assert_eq!(
        completed_count, COMPLETE_COUNT,
        "expected {} completed entries",
        COMPLETE_COUNT
    );
    assert_eq!(
        pending_count, PENDING_COUNT,
        "expected {} pending entries",
        PENDING_COUNT
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_preserves_input_order() {
    const N: usize = 4;
    const INLINE_WAIT_MS: u64 = 100;

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        empty_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_inline_wait(Duration::from_millis(INLINE_WAIT_MS));
    server.set_workers(vec![WorkerInfo {
        name: "fake-worker".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    let server = Arc::new(server);

    let respond_indices = [0, 2];
    let respond_count = Arc::new(AtomicUsize::new(0));

    let worker_handle = tokio::spawn(async move {
        let mut held_senders = Vec::new();
        let mut received = 0usize;
        while received < N {
            let req = channel.request_rx.recv().await.expect("delegation request");
            // The request_id carries the positional index encoded by
            // __test_call_delegate_parallel's issue_id field; fall back to
            // receive order for safety.
            let task_idx = received;
            received += 1;
            let should_respond = respond_indices.contains(&task_idx);
            if should_respond {
                respond_count.fetch_add(1, Ordering::SeqCst);
                let _ = req.respond_to.send(DelegationResult {
                    status: DelegationStatus::Success,
                    diff: None,
                    diff_summary: None,
                    summary: Some(format!("task-{task_idx}")),
                    estimated_cost_usd: 0.0,
                    worker_branch: None,
                    artifact: None,
                });
            } else {
                held_senders.push(req.respond_to);
            }
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(held_senders);
    });

    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_parallel(vec![
                ("fake-worker", "task-A"),
                ("fake-worker", "task-B"),
                ("fake-worker", "task-C"),
                ("fake-worker", "task-D"),
            ])
            .await
    });

    let mcp_resp = mcp_resp_handle.await.expect("mcp task join");
    worker_handle.abort();
    let _ = worker_handle.await;

    let text = extract_response_text(&mcp_resp);
    let results = parse_results(&text);

    assert_eq!(results.len(), N, "must return N results");

    assert_eq!(
        results[0]["status"].as_str(),
        Some("completed"),
        "position 0 (A) should be completed"
    );
    assert_eq!(
        results[1]["status"].as_str(),
        Some("pending"),
        "position 1 (B) should be pending"
    );
    assert_eq!(
        results[2]["status"].as_str(),
        Some("completed"),
        "position 2 (C) should be completed"
    );
    assert_eq!(
        results[3]["status"].as_str(),
        Some("pending"),
        "position 3 (D) should be pending"
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_parallel_no_serial_dispatch_regression() {
    const N: usize = 3;
    const INLINE_WAIT_MS: u64 = 2000;

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        empty_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_inline_wait(Duration::from_millis(INLINE_WAIT_MS));
    server.set_workers(vec![WorkerInfo {
        name: "fake-worker".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    let server = Arc::new(server);

    let worker_handle = tokio::spawn(async move {
        for _ in 0..N {
            let req = channel.request_rx.recv().await.expect("delegation request");
            tokio::time::sleep(Duration::from_secs(10)).await;
            let _ = req.respond_to.send(DelegationResult {
                status: DelegationStatus::Success,
                diff: None,
                diff_summary: None,
                summary: Some("done".into()),
                estimated_cost_usd: 0.0,
                worker_branch: None,
                artifact: None,
            });
        }
    });

    let server_ref = Arc::clone(&server);
    let before = Instant::now();

    let mcp_resp = server_ref
        .__test_call_delegate_parallel(vec![
            ("fake-worker", "task-0"),
            ("fake-worker", "task-1"),
            ("fake-worker", "task-2"),
        ])
        .await;

    let elapsed = before.elapsed();

    worker_handle.abort();
    let _ = worker_handle.await;

    let text = extract_response_text(&mcp_resp);
    let results = parse_results(&text);

    assert_eq!(results.len(), N, "must return N={} results", N);

    for (i, entry) in results.iter().enumerate() {
        assert_eq!(
            entry["status"].as_str(),
            Some("pending"),
            "entry {i}: all entries should be pending since workers took > inline_wait",
        );
    }

    let elapsed_secs = elapsed.as_secs_f64();
    let max_allowed_secs = 2.5;

    assert!(
        elapsed_secs <= max_allowed_secs,
        "concurrent dispatch should complete in ≤{max_allowed_secs}s, but took {elapsed_secs:.2}s. \
         Serial dispatch would take ~{N}×{INLINE_WAIT_MS}ms = {}s. This suggests a serial-dispatch regression.",
        (N as u64) * INLINE_WAIT_MS / 1000
    );
}
