use std::sync::Arc;

use spur_acp::domain::{BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus};
use spur_acp::SessionId;
use spur_mcp::server::{DetachedCompletionHandle, DetachedContinuationCtx, DetachedSourceKind};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

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
        attempt: 1,
        brain_session: SessionId("brain-session-1".into()),
        event_sink: None,
    });

    let request_id: spur_acp::DelegationId = "test-request-123".into();

    spur_mcp::server::McpCallbackServer::spawn_result_collector(
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
