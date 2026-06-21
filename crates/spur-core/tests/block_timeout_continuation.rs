use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use spur_acp::domain::{BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus};
use spur_acp::SessionId;
use spur_core::server::{DetachedCompletionHandle, DetachedContinuationCtx, DetachedSourceKind};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

fn test_materializer() -> spur_core::outcome_materializer::OutcomeMaterializer {
    spur_core::outcome_materializer::OutcomeMaterializer::new(Arc::new(
        spur_blob_store::MemoryOutcomeStore::new(),
    ))
}

#[tokio::test]
async fn test_block_timeout_fires_continuation() {
    let result_received = Arc::new(tokio::sync::Mutex::new(false));
    let result_received_clone = Arc::clone(&result_received);
    let expected_brain_session = SessionId("brain-session-1".into());

    let test_ctx = Arc::new(DetachedContinuationCtx {
        on_complete: Arc::new(move |cont: BrainContinuation, _worker_session: String| {
            let result_received = Arc::clone(&result_received_clone);
            let expected_brain_session = expected_brain_session.clone();
            Box::pin(async move {
                assert_eq!(
                    cont.source,
                    ContinuationSource::BlockTimeout,
                    "Continuation should have BlockTimeout source when detached handle uses BlockTimeout source_kind"
                );
                assert_eq!(
                    cont.payload.status,
                    DelegationStatus::Success,
                    "Continuation payload should reflect worker result status"
                );
                assert_eq!(cont.attempt, 1, "collector should preserve attempt number");
                assert_eq!(
                    cont.brain_session, expected_brain_session,
                    "collector should preserve the brain session"
                );
                *result_received.lock().await = true;
            })
        }),
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<DelegationResult>();
    let tracker = TaskTracker::new();

    let active = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
    let completed = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    let detached = Some(DetachedCompletionHandle {
        ctx: test_ctx,
        source_kind: DetachedSourceKind::BlockTimeout,
        attempt_tracker: Arc::new(AtomicU32::new(1)),
        brain_session: SessionId("brain-session-1".into()),
        event_sink: None,
        materializer: test_materializer(),
    });

    let request_id: spur_acp::DelegationId = "test-request-123".into();

    spur_core::server::McpCallbackServer::spawn_result_collector(
        &tracker,
        request_id.clone(),
        rx,
        CancellationToken::new(),
        Arc::clone(&active),
        Arc::clone(&completed),
        detached,
    );

    tx.send(DelegationResult {
        status: DelegationStatus::Success,
        summary: Some("Test completed".to_string()),
        diff: None,
        diff_summary: None,
        estimated_cost_usd: 0.0,
        worker_branch: Some("main".to_string()),
        artifact: None,
    })
    .expect("send should succeed");

    tracker.close();
    tracker.wait().await;

    let received = *result_received.lock().await;
    assert!(
        received,
        "Continuation callback should have been invoked with BlockTimeout source"
    );
}

#[tokio::test]
async fn test_attempt_threaded_into_continuation() {
    let observed_attempt = Arc::new(tokio::sync::Mutex::new(0u32));
    let observed_attempt_clone = Arc::clone(&observed_attempt);
    let attempt_tracker = Arc::new(AtomicU32::new(1));

    let test_ctx = Arc::new(DetachedContinuationCtx {
        on_complete: Arc::new(move |cont: BrainContinuation, _worker_session: String| {
            let observed_attempt = Arc::clone(&observed_attempt_clone);
            Box::pin(async move {
                *observed_attempt.lock().await = cont.attempt;
            })
        }),
    });

    let (tx, rx) = tokio::sync::oneshot::channel::<DelegationResult>();
    let tracker = TaskTracker::new();
    let active = Arc::new(tokio::sync::Mutex::new(std::collections::HashSet::new()));
    let completed = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

    let detached = Some(DetachedCompletionHandle {
        ctx: test_ctx,
        source_kind: DetachedSourceKind::BlockTimeout,
        attempt_tracker: Arc::clone(&attempt_tracker),
        brain_session: SessionId("brain-session-2".into()),
        event_sink: None,
        materializer: test_materializer(),
    });

    let request_id: spur_acp::DelegationId = "test-request-456".into();
    spur_core::server::McpCallbackServer::spawn_result_collector(
        &tracker,
        request_id,
        rx,
        CancellationToken::new(),
        active,
        completed,
        detached,
    );

    attempt_tracker.store(2, Ordering::SeqCst);

    tx.send(DelegationResult {
        status: DelegationStatus::Success,
        summary: Some("retry completed".to_string()),
        diff: None,
        diff_summary: None,
        estimated_cost_usd: 0.0,
        worker_branch: Some("retry-branch".to_string()),
        artifact: None,
    })
    .expect("send should succeed");

    tracker.close();
    tracker.wait().await;

    assert_eq!(*observed_attempt.lock().await, 2);
}
