#[cfg(test)]
mod cancel_delegation_tests {
    use spur_acp::{CancelOutcome, CancellationControl};

    /// INV-6: CancellationControl.cancel returns Cancelled the first time
    /// and NotFound on a second call (token was removed on first cancel).
    #[tokio::test]
    async fn cancel_returns_cancelled_then_not_found() {
        let cc = CancellationControl::new();
        let token = cc.register("req-1".into()).await;

        assert!(!token.is_cancelled(), "token should not be cancelled yet");

        let outcome = cc.cancel("req-1").await;
        assert_eq!(outcome, CancelOutcome::Cancelled);
        assert!(
            token.is_cancelled(),
            "token must be cancelled after cancel()"
        );

        // Second cancel: token was removed, so NotFound.
        let outcome2 = cc.cancel("req-1").await;
        assert_eq!(outcome2, CancelOutcome::NotFound);
    }

    /// INV-6: cancel on an unknown id returns NotFound.
    #[tokio::test]
    async fn cancel_unknown_id_returns_not_found() {
        let cc = CancellationControl::new();
        let outcome = cc.cancel("no-such-id").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }

    /// INV-6: remove() cleans up without cancelling the token.
    #[tokio::test]
    async fn remove_cleans_up_without_cancelling() {
        let cc = CancellationControl::new();
        let token = cc.register("req-2".into()).await;
        cc.remove("req-2").await;
        assert!(!token.is_cancelled(), "remove must not cancel the token");
        // After remove, cancel returns NotFound.
        let outcome = cc.cancel("req-2").await;
        assert_eq!(outcome, CancelOutcome::NotFound);
    }
}

#[cfg(test)]
mod retirement_state_tests {
    use std::future::pending;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use spur_acp::{BrainSessionId, SessionId};
    use tokio::sync::{oneshot, Notify};

    fn no_op_ctx() -> super::DetachedContinuationCtx {
        super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn test_server_mark_retiring_rejects_new_delegations() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        server.mark_retiring();

        let single = server
            .__test_call_delegate_to_worker("codex", "should reject")
            .await;
        assert_eq!(single["error"]["message"], "SessionRetiring");

        let parallel = server
            .__test_call_delegate_parallel(vec![("codex", "parallel should reject")])
            .await;
        assert_eq!(parallel["error"]["message"], "SessionRetiring");
    }

    #[tokio::test]
    async fn test_server_cancel_in_flight_signals_token() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        assert!(
            !server.cancel_token.is_cancelled(),
            "fresh servers must start with an active cancellation token"
        );

        server.cancel_in_flight_workers();

        assert!(
            server.cancel_token.is_cancelled(),
            "cancel_in_flight_workers must signal the shared cancellation token"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_idempotent() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let dropped = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());

        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            let started = Arc::clone(&started);
            async move {
                let _flag = DropFlag(dropped);
                started.notify_one();
                pending::<()>().await;
            }
        }));

        started.notified().await;
        server.force_abort();
        server.force_abort();
        tokio::time::timeout(Duration::from_millis(200), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("force_abort should eventually abort the stored root task");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must abort the stored root task"
        );
        assert!(
            server.root_handle.lock().unwrap().is_none(),
            "force_abort must take the root handle so repeated calls stay idempotent"
        );
    }

    #[tokio::test]
    async fn test_server_force_abort_after_shutdown_partial_progress() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let server = Arc::new(server);

        let release = Arc::new(Notify::new());
        server.task_tracker.spawn({
            let release = Arc::clone(&release);
            async move {
                release.notified().await;
            }
        });

        let dropped = Arc::new(AtomicBool::new(false));
        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                pending::<()>().await;
            }
        }));

        let shutdown = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                server.shutdown().await;
            }
        });

        tokio::task::yield_now().await;
        server.force_abort();
        release.notify_waiters();

        tokio::time::timeout(Duration::from_millis(200), shutdown)
            .await
            .expect("shutdown should complete once tracked work finishes")
            .expect("shutdown task must not panic");

        assert!(
            dropped.load(Ordering::SeqCst),
            "force_abort must still abort the root task after shutdown has already started"
        );
    }

    #[tokio::test]
    async fn test_server_shutdown_signals_root_listener() {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let server = Arc::new(server);

        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let (root_shutdown_tx, root_shutdown_rx) = oneshot::channel();
        *server.root_shutdown_tx.lock().unwrap() = Some(root_shutdown_tx);
        *server.root_handle.lock().unwrap() = Some(tokio::spawn({
            let started = Arc::clone(&started);
            let dropped = Arc::clone(&dropped);
            async move {
                let _flag = DropFlag(dropped);
                started.notify_one();
                let _ = root_shutdown_rx.await;
            }
        }));

        started.notified().await;
        server.shutdown().await;
        tokio::time::timeout(Duration::from_millis(200), async {
            while !dropped.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown should signal and finish the root listener task");
    }
}

#[cfg(test)]
mod continuation_producer_tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::AtomicU32;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use chrono::Utc;
    use spur_acp::domain::artifact::{ArtifactKind as WorkerArtifactKind, WorkerArtifact};
    use spur_acp::domain::continuation::ArtifactKind as ContinuationArtifactKind;
    use spur_acp::domain::events::{DiffSummary, SpurEventBody};
    use spur_acp::domain::{
        BrainContinuation, ContinuationSource, DelegationResult, DelegationStatus,
    };
    use spur_acp::{DelegationId, SessionId};
    use tokio_util::sync::CancellationToken;
    use tokio_util::task::TaskTracker;

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<SpurEventBody>>,
    }

    impl crate::events::McpEventSink for RecordingSink {
        fn emit(&self, event: SpurEventBody) {
            self.events.lock().unwrap().push(event);
        }
    }

    async fn capture_continuation(
        delegation_id: DelegationId,
        result: DelegationResult,
        attempt: u32,
        brain_session: SessionId,
        event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    ) -> BrainContinuation {
        let tracker = TaskTracker::new();
        let active = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
        let completed = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let captured = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let captured_for_ctx = Arc::clone(&captured);
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let materializer = super::OutcomeMaterializer::new(store);

        let detached = Some(super::DetachedCompletionHandle {
            ctx: Arc::new(super::DetachedContinuationCtx {
                on_complete: Arc::new(move |cont, _worker_session| {
                    let captured = Arc::clone(&captured_for_ctx);
                    Box::pin(async move {
                        captured.lock().await.push(cont);
                    })
                }),
            }),
            source_kind: super::DetachedSourceKind::BlockTimeout,
            attempt_tracker: Arc::new(AtomicU32::new(attempt)),
            brain_session,
            event_sink,
            materializer,
        });

        let (tx, rx) = tokio::sync::oneshot::channel();
        super::McpCallbackServer::spawn_result_collector(
            &tracker,
            delegation_id,
            rx,
            CancellationToken::new(),
            active,
            completed,
            detached,
        );

        tx.send(result).expect("send continuation result");
        tracker.close();
        tracker.wait().await;

        let captured = captured.lock().await;
        assert_eq!(
            captured.len(),
            1,
            "collector should emit exactly one continuation"
        );
        captured[0].clone()
    }

    fn success_result(
        summary: Option<String>,
        diff_summary: Option<DiffSummary>,
        artifact: Option<WorkerArtifact>,
    ) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary,
            summary,
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-test".into()),
            artifact,
        }
    }

    #[tokio::test]
    async fn build_detached_continuation_populates_artifact_id_via_materializer() {
        use spur_blob_store::MemoryOutcomeStore;

        let store: Arc<dyn spur_blob_store::OutcomeStore> = Arc::new(MemoryOutcomeStore::new());
        let mat = crate::outcome_materializer::OutcomeMaterializer::new(store);
        let result = DelegationResult {
            status: DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("done".into()),
            estimated_cost_usd: 0.0,
            worker_branch: Some("spur/worker-x".into()),
            artifact: None,
        };
        let delegation_id = DelegationId::from("deadbeef-1111-2222-3333-444455556666");
        let brain_session = SessionId("550e8400-e29b-41d4-a716-446655440000".into());

        let cont = super::build_detached_continuation(
            &delegation_id,
            &result,
            spur_acp::domain::ContinuationSource::BlockTimeout,
            1,
            brain_session,
            None,
            &mat,
        )
        .await;
        assert!(
            cont.payload.artifact_id.is_some(),
            "Phase 3 wires artifact_id"
        );
    }

    #[tokio::test]
    async fn test_producer_materializes_oversized_summary_with_fetch_hint() {
        let delegation_id: DelegationId = "del-oversized".into();
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let original_summary = "x".repeat(super::PRODUCER_MAX_FIELD_BYTES + 64);

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(Some(original_summary.clone()), None, None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        let clipped = continuation
            .payload
            .summary
            .as_ref()
            .expect("summary should still be present after clipping");
        assert!(
            clipped.len() <= super::PRODUCER_MAX_FIELD_BYTES,
            "clipped summary must stay within the producer byte budget"
        );
        assert!(
            clipped.ends_with('…'),
            "clipped summary should carry the ellipsis marker"
        );
        assert!(
            continuation.payload.artifact_id.is_some(),
            "full result should be fetchable from the outcome store"
        );
        assert!(
            continuation
                .payload
                .fetch_hint
                .as_deref()
                .is_some_and(|hint| hint.contains("Summary truncated")),
            "fetch hint should tell the brain that the summary was clipped"
        );

        assert!(
            sink.events.lock().unwrap().is_empty(),
            "primary materializer path persists the full result instead of emitting a truncation event"
        );
    }

    #[tokio::test]
    async fn test_producer_diff_summary_handled() {
        let sink = Arc::new(RecordingSink::default());
        let sink_obj: Arc<dyn crate::events::McpEventSink> = sink.clone();
        let diff_summary = DiffSummary {
            files_changed: 2,
            insertions: 8,
            deletions: 3,
            files: vec!["src/main.rs".into(), "src/lib.rs".into()],
        };

        let continuation = capture_continuation(
            "del-diff-summary".into(),
            success_result(Some("ok".into()), Some(diff_summary.clone()), None),
            1,
            SessionId("brain".into()),
            Some(sink_obj),
        )
        .await;

        assert_eq!(continuation.payload.diff_summary, Some(diff_summary));
        assert!(
            sink.events.lock().unwrap().is_empty(),
            "structured diff_summary should not emit truncation events when no string field is clipped"
        );
    }

    #[tokio::test]
    async fn test_continuation_construction_brain_session_attempt_created_at() {
        let delegation_id: DelegationId = "del-cont-1".into();
        let brain_session = SessionId("brain-session-7".into());
        let before_wall = Utc::now();
        let before_mono = Instant::now();

        let continuation = capture_continuation(
            delegation_id.clone(),
            success_result(
                Some("done".into()),
                None,
                Some(WorkerArtifact {
                    object_ref: "refs/spur/artifacts/abc123".into(),
                    blob_sha: "0".repeat(40),
                    size_bytes: 1_234,
                    kind: WorkerArtifactKind::Diagnostic,
                }),
            ),
            7,
            brain_session.clone(),
            None,
        )
        .await;

        let after_mono = Instant::now();
        let after_wall = Utc::now();

        assert_eq!(continuation.delegation_id, delegation_id);
        assert_eq!(continuation.attempt, 7);
        assert_eq!(continuation.brain_session, brain_session);
        assert_eq!(continuation.source, ContinuationSource::BlockTimeout);
        assert!(continuation.created_at_wall >= before_wall);
        assert!(continuation.created_at_wall <= after_wall);
        assert!(continuation.created_at_mono >= before_mono);
        assert!(continuation.created_at_mono <= after_mono);

        let artifact_ref = continuation
            .payload
            .artifact_ref
            .as_ref()
            .expect("worker artifacts should map to continuation artifact refs");
        assert_eq!(
            artifact_ref.kind,
            ContinuationArtifactKind::Other("worker_artifact".into())
        );
        assert_eq!(artifact_ref.uri, "spur://artifact/del-cont-1");
        assert_eq!(artifact_ref.byte_size, 1_234);
        assert_eq!(
            artifact_ref.sha256.as_deref(),
            Some("0".repeat(40).as_str())
        );
        assert_eq!(
            artifact_ref.git_object_ref.as_deref(),
            Some("refs/spur/artifacts/abc123")
        );
        assert_eq!(
            artifact_ref.git_blob_sha.as_deref(),
            Some("0".repeat(40).as_str())
        );
    }
}

#[cfg(test)]
mod fetch_outcome_artifact_tests {
    //! End-to-end tests for the `fetch_outcome_artifact` MCP tool.
    //!
    //! Seeds the outcome store with serialized `DelegationResult` blobs,
    //! then calls the JSON-RPC tool dispatcher and asserts the section
    //! projection returned to the brain.

    use super::{DetachedContinuationCtx, McpCallbackServer};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use spur_acp::domain::{ContinuationSource, DelegationResult, DelegationStatus, OutcomeKey};
    use spur_acp::{BrainSessionId, DelegationId, SessionId};
    use spur_blob_store::{ContentType, OutcomeMetadata, OutcomeStore};
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn init_git_repo(path: &Path) {
        let init = tokio::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(path)
            .output()
            .await
            .expect("git init must run");
        assert!(init.status.success(), "git init failed: {init:?}");

        for kv in &[("user.email", "test@example.com"), ("user.name", "test")] {
            let out = tokio::process::Command::new("git")
                .args(["config", kv.0, kv.1])
                .current_dir(path)
                .output()
                .await
                .expect("git config must run");
            assert!(out.status.success(), "git config {} failed", kv.0);
        }
    }

    fn no_op_continuation_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_cont, _worker_session| Box::pin(async {})),
        }
    }

    async fn build_test_server(repo_root: &Path, session_id: &str) -> McpCallbackServer {
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let outcome_store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        build_test_server_with_store(repo_root, brain_session, outcome_store).await
    }

    async fn build_test_server_with_store(
        repo_root: &Path,
        brain_session: BrainSessionId,
        outcome_store: Arc<dyn spur_blob_store::OutcomeStore>,
    ) -> McpCallbackServer {
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&brain_session),
            None,
            None,
            no_op_continuation_ctx(),
            outcome_store,
            super::community_feature_gate(),
        );
        server.set_repo_root(repo_root.to_path_buf());
        server
    }

    fn sha256_hex(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").expect("hex write infallible");
        }
        hex
    }

    fn outcome_metadata(content: &[u8]) -> OutcomeMetadata {
        OutcomeMetadata {
            created_at: chrono::Utc::now(),
            content_type: ContentType::Json,
            original_byte_size: content.len() as u64,
            stored_byte_size: content.len() as u64,
            sha256: sha256_hex(content),
        }
    }

    async fn put_outcome(
        store: &Arc<dyn OutcomeStore>,
        brain_session: &BrainSessionId,
        delegation_id: DelegationId,
        attempt: u32,
        result: &DelegationResult,
    ) {
        let bytes = serde_json::to_vec(result).expect("serialize result");
        let metadata = outcome_metadata(&bytes);
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id,
            attempt,
        };
        store
            .put(&key, &bytes, &metadata)
            .await
            .expect("put outcome");
    }

    fn success_result(summary: &str, diff: &str, cost: f64) -> DelegationResult {
        DelegationResult {
            status: DelegationStatus::Success,
            summary: Some(summary.into()),
            diff: Some(diff.into()),
            diff_summary: None,
            estimated_cost_usd: cost,
            worker_branch: None,
            artifact: None,
        }
    }

    fn dispatch_args(name: &str, args: Value) -> Value {
        json!({ "name": name, "arguments": args })
    }

    fn response_text(response: &super::JsonRpcResponse) -> &str {
        response.result.as_ref().expect("expected success response")["content"][0]["text"]
            .as_str()
            .expect("text content")
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_persisted_blob_text() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let result = success_result("ok", "line one\nline two\n", 0.0);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_id.as_str() }),
                ),
            )
            .await;

        let text = response_text(&response);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result json");
        assert_eq!(parsed.summary.as_deref(), Some("ok"));
        assert_eq!(parsed.diff.as_deref(), Some("line one\nline two\n"));
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_status_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-status-only".into();
        let result = success_result("summary must stay out", "diff must stay out", 1.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "status_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["estimated_cost_micros"], 1_250_000);
        assert!(projected.get("summary").is_none());
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_summary_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-summary".into();
        let result = success_result("summary included", "diff must stay out", 0.5);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["attempt"], 1);
        assert_eq!(projected["brain_session"], session_id);
        assert_eq!(projected["summary"], "summary included");
        assert_eq!(projected["estimated_cost_micros"], 500_000);
        assert!(projected.get("diff").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_diff_only_section() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-diff-only".into();
        let result = success_result("summary must stay out", "diff included", 0.25);
        put_outcome(&store, &brain_session, delegation_id.clone(), 1, &result).await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "diff_only"
                    }),
                ),
            )
            .await;

        let projected: Value = serde_json::from_str(response_text(&response)).expect("json");
        assert_eq!(projected["status"], "Success");
        assert_eq!(projected["diff"], "diff included");
        assert!(projected.get("diff_summary").is_some());
        assert!(projected.get("summary").is_none());
        assert!(projected.get("attempt").is_none());
        assert!(projected.get("estimated_cost_micros").is_none());
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_pins_specific_attempt() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        let delegation_id: DelegationId = "deadbeef-attempts".into();
        server
            .materializer
            .materialize(
                success_result("attempt one", "diff one", 0.0),
                delegation_id.clone(),
                1,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;
        server
            .materializer
            .materialize(
                success_result("attempt two", "diff two", 0.0),
                delegation_id.clone(),
                2,
                brain_session.clone(),
                ContinuationSource::BlockTimeout,
                None,
            )
            .await;

        let latest_response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let latest: Value = serde_json::from_str(response_text(&latest_response)).expect("json");
        assert_eq!(latest["attempt"], 2);
        assert_eq!(latest["summary"], "attempt two");

        let pinned_response = server
            .handle_tool_call(
                Value::Number(2.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let pinned: Value = serde_json::from_str(response_text(&pinned_response)).expect("json");
        assert_eq!(pinned["attempt"], 1);
        assert_eq!(pinned["summary"], "attempt one");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_invalid_attempt_arg() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        for invalid in [json!(-1), json!("two"), json!(0), json!(false)] {
            let response = server
                .handle_tool_call(
                    Value::Number(1.into()),
                    dispatch_args(
                        "fetch_outcome_artifact",
                        json!({
                            "delegation_id": "deadbeef-1111-2222-3333-444455556666",
                            "attempt": invalid,
                        }),
                    ),
                )
                .await;
            let error = response
                .error
                .as_ref()
                .unwrap_or_else(|| panic!("expected InvalidParams for attempt={invalid:?}"));
            assert_eq!(error.code, -32602);
            assert!(
                error.message.contains("Invalid 'attempt'"),
                "expected attempt rejection, got: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_internal_error_on_corrupted_blob() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_id = "550e8400-e29b-41d4-a716-446655440000";
        let brain_session = BrainSessionId::new(SessionId(session_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        let server =
            build_test_server_with_store(td.path(), brain_session.clone(), store.clone()).await;

        // Seed the store with bytes that ARE valid UTF-8 but NOT a valid
        // DelegationResult — exercises ProjectionError::InvalidResult on
        // a non-Full projection.
        let delegation_id: DelegationId = "deadbeef-1111-2222-3333-444455556666".into();
        let key = OutcomeKey {
            brain_session_id: brain_session.clone(),
            delegation_id: delegation_id.clone(),
            attempt: 1,
        };
        let bytes = b"not a delegation result";
        let metadata = outcome_metadata(bytes);
        store.put(&key, bytes, &metadata).await.expect("put");

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": delegation_id.as_str(),
                        "attempt": 1,
                        "section": "summary"
                    }),
                ),
            )
            .await;
        let error = response
            .error
            .as_ref()
            .expect("expected InternalError on corrupted blob");
        assert_eq!(error.code, -32603, "InternalError JSON-RPC code");
        assert!(
            error.message.to_lowercase().contains("projection")
                || error.message.contains("DelegationResult"),
            "expected projection-error context: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_returns_clean_error_for_unknown_delegation() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": "nonexistent-delegation-id" }),
                ),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        // Phase 2 Task 10: a missing artifact is reported as Unauthorized
        // rather than NotFound so that a caller cannot probe whether a given
        // (delegation_id, attempt) exists in another brain session.
        assert_eq!(error.code, -32001);
        assert!(
            error.message.contains("not accessible"),
            "error message must mention not-accessible: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_unknown_section_cleanly() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({
                        "delegation_id": "any-id",
                        "section": "not_a_section"
                    }),
                ),
            )
            .await;

        let error = response
            .error
            .as_ref()
            .expect("expected InvalidParams error");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
        assert!(
            error
                .message
                .contains("Must be one of: status_only, summary, diff_only, full"),
            "unknown sections must be rejected cleanly: {error:?}"
        );
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_rejects_empty_delegation_id() {
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let server = build_test_server(td.path(), "any-session").await;

        let response = server
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args("fetch_outcome_artifact", json!({ "delegation_id": "" })),
            )
            .await;

        let error = response.error.as_ref().expect("expected error response");
        assert_eq!(error.code, -32602, "InvalidParams JSON-RPC code");
    }

    #[tokio::test]
    async fn fetch_outcome_artifact_completed_delegations_are_per_session() {
        // Two MCP servers share the same store, but each binds fetches to
        // its own brain_session_id. Server B asks for the same delegation_id
        // under its session and must not see Server A's outcome.
        let td = TempDir::new().unwrap();
        init_git_repo(td.path()).await;

        let session_a_id = "550e8400-e29b-41d4-a716-446655440000";
        let session_b_id = "550e8400-e29b-41d4-a716-aaaaaaaaaaaa";
        let brain_session_a = BrainSessionId::new(SessionId(session_a_id.into()));
        let brain_session_b = BrainSessionId::new(SessionId(session_b_id.into()));
        let store: Arc<dyn spur_blob_store::OutcomeStore> =
            Arc::new(spur_blob_store::MemoryOutcomeStore::new());

        let server_a =
            build_test_server_with_store(td.path(), brain_session_a.clone(), store.clone()).await;
        let server_b =
            build_test_server_with_store(td.path(), brain_session_b, store.clone()).await;

        let delegation_a: DelegationId = "delegation-belonging-to-a".into();
        let result_a = success_result("secret stdout for session A", "secret diff", 0.0);
        put_outcome(&store, &brain_session_a, delegation_a.clone(), 1, &result_a).await;

        // Server A can fetch its own delegation.
        let resp_a = server_a
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let text = response_text(&resp_a);
        let parsed: DelegationResult = serde_json::from_str(text).expect("full result");
        assert_eq!(
            parsed.summary.as_deref(),
            Some("secret stdout for session A")
        );

        // Server B fetches under its own brain_session_id and is denied as
        // Unauthorized — the store-miss is deliberately indistinguishable
        // from a "different session" miss to prevent cross-session probing.
        let resp_b = server_b
            .handle_tool_call(
                Value::Number(1.into()),
                dispatch_args(
                    "fetch_outcome_artifact",
                    json!({ "delegation_id": delegation_a.as_str() }),
                ),
            )
            .await;
        let err = resp_b.error.as_ref().expect("server B must error");
        assert_eq!(err.code, -32001);
        assert!(
            err.message.contains("not accessible"),
            "Server B must not expose Server A's delegations: {err:?}"
        );
    }
}
