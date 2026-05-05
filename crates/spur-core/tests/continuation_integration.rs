//! Integration tests for async-continuation scheduling.
//! These exercise the bridge + orchestrator with a mock brain.

use chrono::Utc;
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::types::SessionId;
use spur_core::continuation_bridge::{
    new_overflow_buf, render_autonomous_turn_with_spill_v2, render_merged_turn_with_spill_v2,
    ContinuationEventSink, MERGE_BUDGET_DEFAULT_BYTES,
};
use spur_core::orchestrator::InteractiveInput;
use spur_core::scheduler::BrainScheduler;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

fn mk_cont(id: &str) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        attempt: 1,
        brain_session: SessionId("brain-session-1".into()),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("ok".into()),
            diff_summary: None,
            worker_branch: None,
            artifact_ref: None,
            estimated_cost_micros: None,
            artifact_id: None,
            fetch_hint: None,
        },
        created_at_wall: Utc::now(),
        created_at_mono: Instant::now(),
    }
}

struct NoopSink;

impl ContinuationEventSink for NoopSink {
    fn emit(&self, _body: SpurEventBody) {}
}

#[tokio::test]
async fn backpressure_overflow_on_full_channel() {
    let (tx, mut rx) = mpsc::channel::<InteractiveInput>(1);
    let overflow = new_overflow_buf();

    // Fill the channel.
    tx.try_send(InteractiveInput::Message {
        blocks: vec![],
        interrupt: false,
    })
    .unwrap();

    // Simulate bridge calls — try_send into a full channel and overflow.
    for i in 0..5 {
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: mk_cont(&format!("id-{i}")),
        };
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(input) {
            overflow
                .lock()
                .await
                .push_back((SessionId::new(), mk_cont(&format!("id-{i}"))));
        }
    }

    // All 5 should have overflowed (channel cap=1, already full).
    assert_eq!(overflow.lock().await.len(), 5);

    // Drain channel once → overflow still holds them until drained by scheduler.
    let _ = rx.recv().await;
    assert_eq!(overflow.lock().await.len(), 5);
}

#[test]
fn session_swap_drops_all_pending_continuations() {
    let mut s = BrainScheduler::new(Some(SessionId::new().into()), Arc::new(NoopSink));
    s.push_continuation(mk_cont("id-1"));
    s.push_continuation(mk_cont("id-2"));
    let overflow = new_overflow_buf();
    s.note_session_swap(Some(SessionId::new().into()), &overflow);
    // Scheduler is now empty.
    let action = s.next(Instant::now());
    assert!(matches!(
        action,
        spur_core::scheduler::ScheduledAction::IdleUntil { deadline: None }
    ));
}

#[test]
fn merged_turn_has_user_block_at_front_and_self_describing_marker() {
    use agent_client_protocol::schema::{ContentBlock, TextContent};
    let user = vec![ContentBlock::Text(TextContent::new("what is the plan?"))];
    let outcome =
        render_merged_turn_with_spill_v2(&user, &[mk_cont("id-1")], MERGE_BUDGET_DEFAULT_BYTES);
    assert!(outcome.deferred_spill.is_empty());
    assert!(outcome.dropped_oversized.is_empty());
    // User block present byte-exact at position 0.
    assert_eq!(outcome.blocks[0], user[0]);
    // Separator marker present.
    let has_marker = outcome
        .blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::Text(t) if t.text.contains("[SPUR:background]")));
    assert!(has_marker, "merged turn must carry self-describing marker");
    // Resource with spur:// URI present.
    let has_resource = outcome
        .blocks
        .iter()
        .any(|b| format!("{b:?}").contains("spur://continuation/id-1"));
    assert!(
        has_resource,
        "merged turn must carry spur://continuation/ resource"
    );
}

#[test]
fn autonomous_turn_is_self_describing() {
    let blocks =
        render_autonomous_turn_with_spill_v2(&[mk_cont("id-42")], MERGE_BUDGET_DEFAULT_BYTES)
            .blocks;
    let joined = format!("{blocks:?}");
    assert!(joined.contains("[SPUR:background]"), "must carry marker");
    assert!(
        joined.contains("spur://continuation/id-42"),
        "must carry resource URI"
    );
}

/// INV-ASYNC-1: "For any given delegation_id, the brain receives the result
/// through exactly one of: (a) inline MCP response, (b) continuation turn.
/// Never zero, never both."
///
/// This test exercises the race where a worker completes strictly AFTER the
/// MCP inline block window expires. The delegation falls through to the
/// detached path; the brain must get the `DelegationResult` via the
/// continuation callback ONLY — not via a leftover `completed_delegations`
/// entry that a subsequent `check_delegation_status` would redeliver.
///
/// Expected states across the migration:
/// - Current main:  collector writes only the map (detached=None at server.rs:836)
///   → continuation never fires → FAILS "never zero" assertion.
/// - After Phase 1a (detached=Some with BlockTimeout, map write unconditional):
///   → both map and callback fire → FAILS "never both" assertion.
/// - After Phase 1c (collector skips map write when detached):
///   → callback fires, map is clean → PASSES.
///
/// Time is driven via `tokio::time::pause()` / auto-advance so the race is
/// deterministic without wall-clock sleeps.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_no_double_delivery_on_block_timeout() {
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::{BrainSessionId, DelegationResult};
    use spur_mcp::server::DetachedContinuationCtx;
    use spur_mcp::{McpCallbackServer, WorkerInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::Duration;

    // DELEGATION_BLOCK_TIMEOUT in spur-mcp/src/server.rs is 90s. Delay the
    // fake worker 91s so it falls just outside the inline window — this is the
    // block-timeout / detached branch.
    const BLOCK_TIMEOUT_SECS: u64 = 90;
    const WORKER_DELAY_SECS: u64 = 91;

    // Tracking callback counts how many times a continuation was delivered.
    let callback_count = Arc::new(AtomicUsize::new(0));
    let cb = Arc::clone(&callback_count);
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |_cont, _worker| {
            let cb = Arc::clone(&cb);
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
            })
        }),
    };

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    server.set_workers(vec![WorkerInfo {
        name: "worker-slow".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    let server = Arc::new(server);

    // Fake worker: consume the delegation request; send the oneshot reply
    // AFTER the inline block window. Returns the delegation_id so we can
    // observe map state by id.
    let worker_handle = tokio::spawn(async move {
        let req = channel.request_rx.recv().await.expect("delegation request");
        let delegation_id: String = req.id.clone().into();
        tokio::time::sleep(Duration::from_secs(WORKER_DELAY_SECS)).await;
        let _ = req.respond_to.send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("worker done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        });
        delegation_id
    });

    // Brain dispatches delegate_to_worker. Under start_paused, the handler's
    // internal 250ms poll loop auto-advances virtual time until the 90s
    // deadline fires.
    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_to_worker("worker-slow", "slow task")
            .await
    });

    let mcp_resp = mcp_resp_handle.await.expect("mcp task join");
    let delegation_id = worker_handle.await.expect("worker task join");

    // Wait for the spawned collector to drain the oneshot and run its
    // post-completion writes (map insert + optional detached callback).
    // `shutdown()` closes the TaskTracker and awaits every spawned collector.
    server.shutdown().await;

    // Observe the three possible delivery channels:
    //   inline_has_result: did the delegate_to_worker response carry a
    //                      DelegationResult payload (success path)?
    //   callback_fired:    did the detached continuation callback fire?
    //   map_has_entry:     is the result waiting in the polled map for a
    //                      future check_delegation_status to consume?
    let text = mcp_resp
        .pointer("/result/content/0/text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    // Inline success response from handle_delegate_to_worker serializes the
    // DelegationResult JSON pretty. "still running" text lacks `"status"`.
    let inline_has_result = text.contains("\"status\"") && text.contains("Success");
    let callback_fired = callback_count.load(Ordering::SeqCst) > 0;
    let map_has_entry = server.__test_completed_has(&delegation_id).await;

    // INV-ASYNC-1 — never zero.
    assert!(
        callback_fired || inline_has_result,
        "INV-ASYNC-1 'never zero': worker completed past the {BLOCK_TIMEOUT_SECS}s \
         block window, so the brain MUST get the result via a detached \
         continuation. Got zero deliveries \
         (inline_has_result={inline_has_result} callback_fired={callback_fired} \
         map_has_entry={map_has_entry}). Current main: collector writes only the \
         map at spur-mcp/src/server.rs:600; Phase 1a wires Some(handle) at :836 \
         to flip this green.",
    );

    // INV-ASYNC-1 — never both.
    let inline_count = u32::from(inline_has_result);
    let continuation_count = u32::from(callback_fired);
    let polled_count = u32::from(map_has_entry);
    assert_eq!(
        inline_count + continuation_count + polled_count,
        1,
        "INV-ASYNC-1 'never both': expected EXACTLY one delivery path but got \
         inline={inline_has_result} callback={callback_fired} map={map_has_entry}. \
         A leftover `completed_delegations` entry alongside a detached \
         continuation means a subsequent check_delegation_status would \
         redeliver the same result — that's the double-delivery failure mode \
         Phase 1c (skip map write when detached) closes.",
    );
}

/// INV-ASYNC-1 companion: fast-path (worker completes *within* the inline
/// window). The brain must receive the `DelegationResult` via the inline MCP
/// response ONLY — no detached callback, no map write.
///
/// Response-shape contract (spec §8.3, post-review revision): `content[0].text`
/// is PURE JSON — no human-readable shadow prefix. Parsing the text with
/// `serde_json::from_str` must succeed and yield the fields documented below.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn test_no_double_delivery_on_fast_path() {
    use serde_json::Value;
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::{BrainSessionId, DelegationResult};
    use spur_mcp::server::DetachedContinuationCtx;
    use spur_mcp::{McpCallbackServer, WorkerInfo};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::Duration;

    // Inline window 5s; worker responds at t=1s — strictly within the window.
    const INLINE_WAIT_MS: u64 = 5_000;
    const WORKER_DELAY_SECS: u64 = 1;

    let callback_count = Arc::new(AtomicUsize::new(0));
    let cb = Arc::clone(&callback_count);
    let ctx = DetachedContinuationCtx {
        on_complete: Arc::new(move |_cont, _worker| {
            let cb = Arc::clone(&cb);
            Box::pin(async move {
                cb.fetch_add(1, Ordering::SeqCst);
            })
        }),
    };

    let brain_sid = BrainSessionId::new(SessionId::new());
    let (mut server, mut channel) = McpCallbackServer::new(
        Some(&brain_sid),
        None,
        None,
        ctx,
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        spur_mcp::server::community_feature_gate(),
    );
    server.set_workers(vec![WorkerInfo {
        name: "worker-fast".into(),
        tier: Some("generalist".into()),
        ..Default::default()
    }]);
    server.set_inline_wait(Duration::from_millis(INLINE_WAIT_MS));
    let server = Arc::new(server);

    let worker_handle = tokio::spawn(async move {
        let req = channel.request_rx.recv().await.expect("delegation request");
        let delegation_id: String = req.id.clone().into();
        tokio::time::sleep(Duration::from_secs(WORKER_DELAY_SECS)).await;
        let _ = req.respond_to.send(DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("worker done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        });
        delegation_id
    });

    let server_ref = Arc::clone(&server);
    let mcp_resp_handle = tokio::spawn(async move {
        server_ref
            .__test_call_delegate_to_worker("worker-fast", "fast task")
            .await
    });

    let mcp_resp = mcp_resp_handle.await.expect("mcp task join");
    let delegation_id = worker_handle.await.expect("worker task join");

    server.shutdown().await;

    // content[0].text MUST be pure JSON — parse it strictly.
    let text = mcp_resp
        .pointer("/result/content/0/text")
        .and_then(|t| t.as_str())
        .expect("mcp response must carry content[0].text");
    let payload: Value = serde_json::from_str(text).expect(
        "Phase 1c response-shape contract: content[0].text must be pure JSON, \
         no shadow prefix. If this fails, the handler is still prepending a \
         human-readable sentence — brains doing json.loads(text) will break.",
    );

    assert_eq!(
        payload["status"].as_str(),
        Some("completed"),
        "fast path must report status=completed",
    );
    assert_eq!(
        payload["continuation_will_fire"].as_bool(),
        Some(false),
        "fast path must declare no continuation will fire",
    );
    assert!(
        payload.get("result").is_some(),
        "fast path must embed the DelegationResult under `result`",
    );
    assert_eq!(
        payload["result"]["status"].as_str(),
        Some("Success"),
        "embedded DelegationResult must reflect worker success",
    );

    // INV-ASYNC-1 'never both' specialized for fast path:
    // (a) no detached continuation callback fired,
    // (b) completed_delegations map is empty (fast arm never writes it).
    assert_eq!(
        callback_count.load(Ordering::SeqCst),
        0,
        "fast path must NOT fire a detached continuation callback",
    );
    assert!(
        !server.__test_completed_has(&delegation_id).await,
        "fast path must NOT leave a `completed_delegations` entry behind",
    );
}
