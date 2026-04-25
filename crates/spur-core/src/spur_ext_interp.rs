//! `_spur/*` ExtNotification interpreter (Phase S5).
//!
//! Consumes `ExtNotificationPayload` from `NativeAcpConnection`'s
//! ext_notification channel, parses the method (e.g.
//! `_spur/progress_milestone`) and params JSON, and emits the
//! corresponding `SpurEventBody` variant through the event funnel.

use spur_acp::connection::ExtNotificationPayload;
use spur_acp::domain::events::{FileTouchKind, SpurEventBody};

use crate::event_funnel::FunnelHandle;

/// Caller supplies `brain_session_id` and `executor_id` from the worker's
/// delegation context — both are in-scope inside `run_one_worker_attempt`
/// where the per-worker consumer task is spawned.
pub fn interpret(
    payload: ExtNotificationPayload,
    brain_session_id: spur_acp::types::SessionId,
    executor_id: String,
    funnel: &FunnelHandle,
) {
    match payload.method.as_str() {
        "_spur/heartbeat" => {
            let worker_ts = payload
                .params
                .get("ts")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            funnel.emit(SpurEventBody::WorkerHeartbeat {
                brain_session_id,
                executor_id,
                worker_ts,
            });
        }
        "_spur/progress_milestone" => {
            let name = payload
                .params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                tracing::warn!(
                    method = %payload.method,
                    "_spur/*: missing or empty 'name' param"
                );
                return;
            }
            let pct = payload
                .params
                .get("pct")
                .and_then(|v| v.as_u64())
                .and_then(|u| u8::try_from(u).ok());
            funnel.emit(SpurEventBody::WorkerProgress {
                brain_session_id,
                executor_id,
                name,
                pct,
            });
        }
        "_spur/file_touched" => {
            let path = match payload.params.get("path").and_then(|v| v.as_str()) {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    tracing::warn!("_spur/file_touched: missing 'path' param");
                    return;
                }
            };
            let kind = match payload.params.get("kind").and_then(|v| v.as_str()) {
                Some("read") => FileTouchKind::Read,
                Some("write") => FileTouchKind::Write,
                other => {
                    tracing::warn!(kind = ?other,
                        "_spur/file_touched: unknown 'kind'");
                    return;
                }
            };
            funnel.emit(SpurEventBody::WorkerFileTouched {
                brain_session_id,
                executor_id,
                path,
                kind,
            });
        }
        "_spur/peer_message" => {
            // Source identity is stamped from orchestrator context. The
            // worker payload is intentionally not trusted for source fields.
            funnel.emit(SpurEventBody::AgentExtNotification {
                session: spur_acp::types::SessionId(executor_id),
                method: "_spur/peer_message".into(),
                params: payload.params,
            });
        }
        "_spur/peer_message_consumed" | "_spur/peer_message_ignored" => {
            funnel.emit(SpurEventBody::AgentExtNotification {
                session: spur_acp::types::SessionId(executor_id),
                method: payload.method,
                params: payload.params,
            });
        }
        other => {
            tracing::debug!(method = other, "ignoring unknown _spur/* method");
        }
    }
}

pub async fn interpret_peer_message(
    router: &std::sync::Arc<crate::peer_mailbox::router::PeerMailboxRouter>,
    snapshot: &std::sync::Arc<spur_mcp::plan::scope_snapshot::PlanScopeSnapshot>,
    source_delegation_id: spur_acp::domain::delegation::DelegationId,
    source_executor_id: String,
    source_issue_id: String,
    source_plan_task_id: String,
    payload: serde_json::Value,
) -> Result<crate::peer_mailbox::router::Acceptance, crate::peer_mailbox::router::RouterError> {
    use crate::peer_mailbox::router::RouterError;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope, PeerMessageId};

    let message_id: PeerMessageId =
        serde_json::from_value(payload["message_id"].clone()).map_err(|e| {
            RouterError::Rejected {
                reason: format!("malformed_message_id: {e}"),
            }
        })?;
    let target_delegation_id: String = payload["target_delegation_id"]
        .as_str()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_target_delegation_id".into(),
        })?
        .into();
    let target_issue_id: String = payload["target_issue_id"].as_str().unwrap_or("").into();
    let target_plan_task_id: String = payload["target_plan_task_id"].as_str().unwrap_or("").into();
    let kind: MessageKind =
        serde_json::from_value(payload["kind"].clone()).map_err(|e| RouterError::Rejected {
            reason: format!("malformed_kind: {e}"),
        })?;
    let body: String = payload["body"].as_str().unwrap_or("").into();
    let sequence: u64 = payload["sequence"].as_u64().unwrap_or(0);

    let envelope = PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id,
        source_delegation_id,
        target_delegation_id: DelegationId(target_delegation_id),
        source_issue_id,
        target_issue_id,
        source_plan_task_id,
        target_plan_task_id,
        source_executor_id,
        plan_version: snapshot.plan_version,
        kind,
        body,
        sequence,
    };

    router.accept_or_reject(envelope, snapshot).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::broadcast;

    fn harness() -> (
        FunnelHandle,
        broadcast::Receiver<spur_acp::domain::events::SpurEvent>,
    ) {
        let (tx, rx) = broadcast::channel(64);
        let seq = Arc::new(AtomicU64::new(0));
        let h = crate::event_funnel::spawn_funnel(tx, seq);
        (h, rx)
    }

    fn test_brain() -> spur_acp::types::SessionId {
        spur_acp::types::SessionId("brain-1".to_string())
    }

    #[tokio::test]
    async fn progress_milestone_synthesizes_event() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/progress_milestone".into(),
                params: json!({"name": "tests_starting", "pct": 60}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::WorkerProgress {
                brain_session_id,
                executor_id,
                name,
                pct,
            } => {
                assert_eq!(brain_session_id, test_brain());
                assert_eq!(executor_id, "exec-1");
                assert_eq!(name, "tests_starting");
                assert_eq!(pct, Some(60));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn file_touched_parses_kind() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/file_touched".into(),
                params: json!({"path": "src/foo.rs", "kind": "write"}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::WorkerFileTouched {
                brain_session_id,
                executor_id,
                path,
                kind,
            } => {
                assert_eq!(brain_session_id, test_brain());
                assert_eq!(executor_id, "exec-1");
                assert_eq!(path, std::path::PathBuf::from("src/foo.rs"));
                assert_eq!(kind, FileTouchKind::Write);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_method_does_not_emit() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/no-such-thing".into(),
                params: json!({}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(rx.try_recv().is_err(), "unknown method should not emit");
    }

    #[tokio::test]
    async fn peer_message_method_routes_to_router() {
        use crate::peer_mailbox::guard::GuardOutcome;
        use crate::peer_mailbox::ledger::InMemoryLedger;
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::router::{Acceptance, PeerMailboxRouter};
        use spur_acp::domain::delegation::DelegationId;
        use spur_acp::domain::peer_message::{MessageKind, TerminalOutcome};
        use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
        use std::collections::{HashMap, HashSet};
        use tokio::sync::mpsc::unbounded_channel;

        let ledger = Arc::new(InMemoryLedger::new());
        let (funnel, mut events) = crate::event_funnel::test_channel();
        let (tx, _rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger,
            funnel.clone(),
            tx,
            Limits::default(),
            "bs".into(),
        ));

        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId("src".into()), "ta".into());
        delegation_to_task.insert(DelegationId("tgt".into()), "tb".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("ta".into(), "tb".into()));
        let snapshot = Arc::new(PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        });

        let payload = json!({
            "schema": "spur-peer-message/v1",
            "message_id": serde_json::from_str::<serde_json::Value>(
                "\"00000000-0000-0000-0000-000000000301\""
            )
            .unwrap(),
            "target_delegation_id": "tgt",
            "target_issue_id": "i2",
            "target_plan_task_id": "tb",
            "kind": "question",
            "body": "test",
            "sequence": 1
        });

        interpret(
            ExtNotificationPayload {
                method: "_spur/peer_message".into(),
                params: payload.clone(),
            },
            test_brain(),
            "exec-1".into(),
            &funnel,
        );
        match events.recv().await.unwrap() {
            SpurEventBody::AgentExtNotification {
                session,
                method,
                params,
            } => {
                assert_eq!(session, spur_acp::types::SessionId("exec-1".into()));
                assert_eq!(method, "_spur/peer_message");
                assert_eq!(params, payload);
            }
            other => panic!("unexpected passthrough event: {other:?}"),
        }

        let result = interpret_peer_message(
            &router,
            &snapshot,
            DelegationId("src".into()),
            "ex".into(),
            "i1".into(),
            "ta".into(),
            payload,
        )
        .await;

        let guard = match result.unwrap() {
            Acceptance::Created(guard) => guard,
            Acceptance::AlreadyAccepted => panic!("expected fresh acceptance"),
        };
        guard
            .finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed))
            .await;

        match events.recv().await.unwrap() {
            SpurEventBody::WorkerPeerMessageAccepted {
                brain_session_id,
                source_delegation_id,
                target_delegation_id,
                kind,
                sequence,
                ..
            } => {
                assert_eq!(brain_session_id, "bs");
                assert_eq!(source_delegation_id, DelegationId("src".into()));
                assert_eq!(target_delegation_id, DelegationId("tgt".into()));
                assert_eq!(kind, MessageKind::Question);
                assert_eq!(sequence, 1);
            }
            other => panic!("unexpected router event: {other:?}"),
        }
    }
}
