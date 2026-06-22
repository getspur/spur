//! `_spur/*` ExtNotification interpreter (Phase S5).
//!
//! Consumes `ExtNotificationPayload` from `NativeAcpConnection`'s
//! ext_notification channel, parses the method (e.g.
//! `_spur/progress_milestone`) and params JSON, and emits the
//! corresponding `SpurEventBody` variant through the event funnel.
//!
//! Worker-supplied `_spur/peer_message_ignored` reasons are capped at this
//! boundary. `REASON_ALLOWLIST` is the low-cardinality contract dashboards may
//! group directly; every other reason collapses to a fixed worker bucket so
//! funnel metrics stay strictly bounded. Worker-side logs and OTel keep any raw
//! diagnostic detail that operators need.

use spur_acp::connection::ExtNotificationPayload;
use spur_acp::domain::events::{FileTouchKind, SpurEventBody};

use crate::event_funnel::FunnelHandle;

pub(crate) const REASON_ALLOWLIST: &[&str] = &[
    "worker_ignored",
    "drain_timeout",
    "drain_capped",
    "out_of_scope",
    "duplicate",
    "stale_plan_version",
];

pub(crate) const REASON_OVERSIZED_BYTES: usize = 128;

pub(crate) fn cap_reason(raw: &str) -> String {
    if REASON_ALLOWLIST.contains(&raw) {
        return raw.to_string();
    }
    if raw.len() > REASON_OVERSIZED_BYTES {
        "worker:other_oversized".to_string()
    } else {
        "worker:other".to_string()
    }
}

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

pub async fn interpret_peer_message_terminal(
    method: &str,
    params: serde_json::Value,
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    ack_tx: &tokio::sync::mpsc::UnboundedSender<()>,
    funnel: &FunnelHandle,
    brain_session_id: &str,
    source_executor_id: &str,
) {
    use spur_acp::domain::peer_message::{PeerMessageId, TerminalOutcome};

    let message_id: PeerMessageId = match serde_json::from_value(params["message_id"].clone()) {
        Ok(id) => id,
        Err(err) => {
            let reason = if params.get("message_id").is_none() {
                "missing_message_id".to_string()
            } else {
                format!("malformed_message_id: {err}")
            };
            tracing::debug!(
                method = %method,
                error = %err,
                "_spur/*: malformed peer terminal 'message_id' param"
            );
            funnel.emit(SpurEventBody::WorkerPeerMessageMalformed {
                brain_session_id: brain_session_id.to_string(),
                source_executor_id: source_executor_id.to_string(),
                method: method.to_string(),
                reason,
            });
            let _ = ack_tx.send(());
            return;
        }
    };

    let outcome = match method {
        "_spur/peer_message_consumed" => TerminalOutcome::Consumed,
        "_spur/peer_message_ignored" => TerminalOutcome::Ignored {
            reason: cap_reason(params["reason"].as_str().unwrap_or("worker_ignored")),
        },
        _ => return,
    };

    if let Err(err) = bundle
        .router
        .record_terminal(brain_session_id, &message_id, outcome)
        .await
    {
        tracing::warn!(
            method = %method,
            message_id = ?message_id,
            error = %err,
            "peer mailbox: failed to record worker terminal ack"
        );
    }
    let _ = ack_tx.send(());
}

/// Schema string the helper accepts. Anything else is rejected at the boundary.
pub const PEER_MESSAGE_SCHEMA_V1: &str = "spur-peer-message/v1";

/// Hard upper bound on body bytes copied out of the raw JSON value. Defends
/// the parse step against a malicious worker who would otherwise force a
/// multi-megabyte `String` allocation before the router's body-size check
/// fires. Picked to be one order of magnitude above `Limits::default()` so
/// legitimate boundary cases still reach the router for a typed reject.
const BODY_HARD_CEILING_BYTES: usize = 64 * 1024;

// TODO(tech-debt): refactor when extracting interpreter inputs into smaller types.
#[allow(clippy::too_many_arguments)]
pub async fn interpret_peer_message(
    router: &std::sync::Arc<crate::peer_mailbox::router::PeerMailboxRouter>,
    snapshot: &std::sync::Arc<crate::plan::scope_snapshot::PlanScopeSnapshot>,
    source_delegation_id: spur_acp::domain::delegation::DelegationId,
    source_executor_id: String,
    source_issue_id: String,
    source_plan_task_id: String,
    brain_session_id: &str,
    payload: serde_json::Value,
) -> Result<crate::peer_mailbox::router::Acceptance, crate::peer_mailbox::router::RouterError> {
    use crate::peer_mailbox::router::RouterError;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope, PeerMessageId};

    // Reject unknown schemas at the boundary so the v1 router never sees
    // a v2-shaped payload masquerading as v1. Absent `schema` is rejected
    // for the same reason — protocol versioning is not optional.
    let schema = payload["schema"]
        .as_str()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_schema".into(),
        })?;
    if schema != PEER_MESSAGE_SCHEMA_V1 {
        return Err(RouterError::Rejected {
            reason: format!("unsupported_schema: {schema}"),
        });
    }

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
    let target_issue_id: String = payload["target_issue_id"]
        .as_str()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_target_issue_id".into(),
        })?
        .into();
    let target_plan_task_id: String = payload["target_plan_task_id"]
        .as_str()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_target_plan_task_id".into(),
        })?
        .into();

    let kind: MessageKind =
        serde_json::from_value(payload["kind"].clone()).map_err(|e| RouterError::Rejected {
            reason: format!("malformed_kind: {e}"),
        })?;
    // `MessageKind` carries `#[serde(other)] Unknown` for forward-compat;
    // accepting it would let workers ship payloads of unspecified intent.
    // Reject explicitly so semantics stay machine-checked.
    if matches!(kind, MessageKind::Unknown) {
        return Err(RouterError::Rejected {
            reason: "unsupported_message_kind".into(),
        });
    }

    // Body: validate length on the borrowed `&str` before allocating an
    // owned `String`. The router enforces the configured per-message cap;
    // this is a hard parse-layer ceiling that protects against allocation
    // DoS regardless of router config.
    let body_str = payload["body"]
        .as_str()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_body".into(),
        })?;
    if body_str.len() > BODY_HARD_CEILING_BYTES {
        return Err(RouterError::Rejected {
            reason: "body_size_exceeded".into(),
        });
    }
    let body: String = body_str.into();

    let sequence: u64 = payload["sequence"]
        .as_u64()
        .ok_or_else(|| RouterError::Rejected {
            reason: "missing_sequence".into(),
        })?;

    let envelope = PeerMessageEnvelope {
        schema: PEER_MESSAGE_SCHEMA_V1.into(),
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

    router
        .accept_or_reject(brain_session_id, envelope, snapshot)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use tokio::sync::mpsc::UnboundedReceiver;

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

    async fn drain_test_events(
        events: &mut UnboundedReceiver<SpurEventBody>,
    ) -> Vec<SpurEventBody> {
        let mut out = Vec::new();
        while let Ok(Some(event)) =
            tokio::time::timeout(std::time::Duration::from_millis(10), events.recv()).await
        {
            out.push(event);
        }
        out
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
        use crate::plan::scope_snapshot::PlanScopeSnapshot;
        use spur_acp::domain::delegation::DelegationId;
        use spur_acp::domain::peer_message::{MessageKind, TerminalOutcome};
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
            "bs",
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

    #[tokio::test]
    async fn peer_message_consumed_method_emits_passthrough() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/peer_message_consumed".into(),
                params: json!({"message_id": "00000000-0000-0000-0000-000000000302"}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::AgentExtNotification { method, .. } => {
                assert_eq!(method, "_spur/peer_message_consumed");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_message_ignored_method_emits_passthrough() {
        let (h, mut rx) = harness();
        interpret(
            ExtNotificationPayload {
                method: "_spur/peer_message_ignored".into(),
                params: json!({"message_id": "00000000-0000-0000-0000-000000000303"}),
            },
            test_brain(),
            "exec-1".into(),
            &h,
        );
        let event = rx.recv().await.unwrap();
        match event.body {
            SpurEventBody::AgentExtNotification { method, .. } => {
                assert_eq!(method, "_spur/peer_message_ignored");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    async fn helper_fixture() -> (
        std::sync::Arc<crate::peer_mailbox::router::PeerMailboxRouter>,
        std::sync::Arc<crate::plan::scope_snapshot::PlanScopeSnapshot>,
    ) {
        use crate::peer_mailbox::ledger::InMemoryLedger;
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::router::PeerMailboxRouter;
        use crate::plan::scope_snapshot::PlanScopeSnapshot;
        use spur_acp::domain::delegation::DelegationId;
        use std::collections::{HashMap, HashSet};
        use tokio::sync::mpsc::unbounded_channel;

        let ledger = std::sync::Arc::new(InMemoryLedger::new());
        let (funnel, _events) = crate::event_funnel::test_channel();
        let (tx, _rx) = unbounded_channel();
        let router = std::sync::Arc::new(PeerMailboxRouter::new(
            ledger,
            funnel,
            tx,
            Limits::default(),
        ));
        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId("src".into()), "ta".into());
        delegation_to_task.insert(DelegationId("tgt".into()), "tb".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("ta".into(), "tb".into()));
        let snapshot = std::sync::Arc::new(PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        });
        (router, snapshot)
    }

    async fn helper_call(
        router: &std::sync::Arc<crate::peer_mailbox::router::PeerMailboxRouter>,
        snapshot: &std::sync::Arc<crate::plan::scope_snapshot::PlanScopeSnapshot>,
        payload: serde_json::Value,
    ) -> Result<crate::peer_mailbox::router::Acceptance, crate::peer_mailbox::router::RouterError>
    {
        use spur_acp::domain::delegation::DelegationId;
        interpret_peer_message(
            router,
            snapshot,
            DelegationId("src".into()),
            "ex".into(),
            "i1".into(),
            "ta".into(),
            "bs",
            payload,
        )
        .await
    }

    #[tokio::test]
    async fn helper_rejects_missing_schema() {
        let (router, snapshot) = helper_fixture().await;
        let err = helper_call(
            &router,
            &snapshot,
            json!({
                "message_id": "00000000-0000-0000-0000-000000000401",
                "target_delegation_id": "tgt",
                "target_issue_id": "i2",
                "target_plan_task_id": "tb",
                "kind": "question",
                "body": "hi",
                "sequence": 1
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            crate::peer_mailbox::router::RouterError::Rejected { reason } if reason == "missing_schema"
        ));
    }

    #[tokio::test]
    async fn helper_rejects_unknown_schema_version() {
        let (router, snapshot) = helper_fixture().await;
        let err = helper_call(
            &router,
            &snapshot,
            json!({
                "schema": "spur-peer-message/v2",
                "message_id": "00000000-0000-0000-0000-000000000402",
                "target_delegation_id": "tgt",
                "target_issue_id": "i2",
                "target_plan_task_id": "tb",
                "kind": "question",
                "body": "hi",
                "sequence": 1
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            crate::peer_mailbox::router::RouterError::Rejected { reason } if reason.starts_with("unsupported_schema")
        ));
    }

    #[tokio::test]
    async fn helper_rejects_unknown_message_kind() {
        let (router, snapshot) = helper_fixture().await;
        let err = helper_call(
            &router,
            &snapshot,
            json!({
                "schema": "spur-peer-message/v1",
                "message_id": "00000000-0000-0000-0000-000000000403",
                "target_delegation_id": "tgt",
                "target_issue_id": "i2",
                "target_plan_task_id": "tb",
                "kind": "future_kind_v9",
                "body": "hi",
                "sequence": 1
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            crate::peer_mailbox::router::RouterError::Rejected { reason } if reason == "unsupported_message_kind"
        ));
    }

    #[tokio::test]
    async fn helper_rejects_oversized_body_before_allocation() {
        let (router, snapshot) = helper_fixture().await;
        // 200 KiB > 64 KiB hard ceiling. Reject must surface at parse layer.
        let err = helper_call(
            &router,
            &snapshot,
            json!({
                "schema": "spur-peer-message/v1",
                "message_id": "00000000-0000-0000-0000-000000000404",
                "target_delegation_id": "tgt",
                "target_issue_id": "i2",
                "target_plan_task_id": "tb",
                "kind": "question",
                "body": "x".repeat(200 * 1024),
                "sequence": 1
            }),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            crate::peer_mailbox::router::RouterError::Rejected { reason } if reason == "body_size_exceeded"
        ));
    }

    #[tokio::test]
    async fn helper_rejects_missing_required_fields() {
        let (router, snapshot) = helper_fixture().await;
        for (missing_field, payload) in [
            (
                "missing_target_issue_id",
                json!({
                    "schema": "spur-peer-message/v1",
                    "message_id": "00000000-0000-0000-0000-000000000405",
                    "target_delegation_id": "tgt",
                    "target_plan_task_id": "tb",
                    "kind": "question",
                    "body": "hi",
                    "sequence": 1
                }),
            ),
            (
                "missing_target_plan_task_id",
                json!({
                    "schema": "spur-peer-message/v1",
                    "message_id": "00000000-0000-0000-0000-000000000406",
                    "target_delegation_id": "tgt",
                    "target_issue_id": "i2",
                    "kind": "question",
                    "body": "hi",
                    "sequence": 1
                }),
            ),
            (
                "missing_sequence",
                json!({
                    "schema": "spur-peer-message/v1",
                    "message_id": "00000000-0000-0000-0000-000000000407",
                    "target_delegation_id": "tgt",
                    "target_issue_id": "i2",
                    "target_plan_task_id": "tb",
                    "kind": "question",
                    "body": "hi"
                }),
            ),
        ] {
            let err = helper_call(&router, &snapshot, payload).await.unwrap_err();
            assert!(
                matches!(
                    &err,
                    crate::peer_mailbox::router::RouterError::Rejected { reason } if reason == missing_field
                ),
                "expected {missing_field}, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn consumed_helper_records_terminal_and_signals_ack() {
        use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
        use crate::peer_mailbox::router::{Acceptance, PeerMailboxRouter};
        use crate::peer_mailbox::PeerMailboxBundle;
        use crate::plan::scope_snapshot::PlanScopeSnapshot;
        use spur_acp::domain::delegation::DelegationId;
        use spur_acp::domain::events::SpurEventBody;
        use spur_acp::domain::peer_message::LedgerState;
        use std::collections::{HashMap, HashSet};
        use tokio::sync::mpsc::unbounded_channel;

        let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
        let (funnel, mut events) = crate::event_funnel::test_channel();
        let (reconciler_tx, _reconciler_rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger.clone(),
            funnel.clone(),
            reconciler_tx,
            Limits::default(),
        ));
        let bundle = PeerMailboxBundle {
            router: router.clone(),
            builder: Arc::new(PeerPromptContextBuilder::new(ledger.clone())),
            ledger: ledger.clone(),
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        };

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
            "message_id": "00000000-0000-0000-0000-000000000501",
            "target_delegation_id": "tgt",
            "target_issue_id": "i2",
            "target_plan_task_id": "tb",
            "kind": "question",
            "body": "test",
            "sequence": 1
        });
        let guard = match interpret_peer_message(
            &router,
            &snapshot,
            DelegationId("src".into()),
            "ex".into(),
            "i1".into(),
            "ta".into(),
            "bs",
            payload,
        )
        .await
        .unwrap()
        {
            Acceptance::Created(guard) => guard,
            Acceptance::AlreadyAccepted => panic!("expected fresh acceptance"),
        };
        let message_id = *guard.message_id();
        ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::Delivered)
            .await
            .unwrap();

        let (ack_tx, mut ack_rx) = unbounded_channel();
        interpret_peer_message_terminal(
            "_spur/peer_message_consumed",
            json!({"message_id": message_id}),
            &bundle,
            &ack_tx,
            &funnel,
            "bs",
            "exec-1",
        )
        .await;

        assert_eq!(
            ledger.get(&message_id).await.unwrap().state,
            LedgerState::Consumed
        );
        ack_rx.try_recv().unwrap();
        let consumed = drain_test_events(&mut events)
            .await
            .into_iter()
            .any(|event| {
                matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageConsumed { message_id: id, .. } if id == message_id
                )
            });
        assert!(consumed, "expected WorkerPeerMessageConsumed event");
    }

    #[tokio::test]
    async fn ignored_helper_records_terminal_reason_and_signals_ack() {
        use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
        use crate::peer_mailbox::router::{Acceptance, PeerMailboxRouter};
        use crate::peer_mailbox::PeerMailboxBundle;
        use crate::plan::scope_snapshot::PlanScopeSnapshot;
        use spur_acp::domain::delegation::DelegationId;
        use spur_acp::domain::events::SpurEventBody;
        use spur_acp::domain::peer_message::LedgerState;
        use std::collections::{HashMap, HashSet};
        use tokio::sync::mpsc::unbounded_channel;

        let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
        let (funnel, mut events) = crate::event_funnel::test_channel();
        let (reconciler_tx, _reconciler_rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger.clone(),
            funnel.clone(),
            reconciler_tx,
            Limits::default(),
        ));
        let bundle = PeerMailboxBundle {
            router: router.clone(),
            builder: Arc::new(PeerPromptContextBuilder::new(ledger.clone())),
            ledger: ledger.clone(),
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        };

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
            "message_id": "00000000-0000-0000-0000-000000000502",
            "target_delegation_id": "tgt",
            "target_issue_id": "i2",
            "target_plan_task_id": "tb",
            "kind": "question",
            "body": "test",
            "sequence": 1
        });
        let guard = match interpret_peer_message(
            &router,
            &snapshot,
            DelegationId("src".into()),
            "ex".into(),
            "i1".into(),
            "ta".into(),
            "bs",
            payload,
        )
        .await
        .unwrap()
        {
            Acceptance::Created(guard) => guard,
            Acceptance::AlreadyAccepted => panic!("expected fresh acceptance"),
        };
        let message_id = *guard.message_id();
        ledger
            .transition(&message_id, LedgerState::Queued)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        ledger
            .transition(&message_id, LedgerState::Delivered)
            .await
            .unwrap();

        let (ack_tx, mut ack_rx) = unbounded_channel();
        interpret_peer_message_terminal(
            "_spur/peer_message_ignored",
            json!({"message_id": message_id, "reason": "not_needed"}),
            &bundle,
            &ack_tx,
            &funnel,
            "bs",
            "exec-1",
        )
        .await;

        assert_eq!(
            ledger.get(&message_id).await.unwrap().state,
            LedgerState::Ignored
        );
        ack_rx.try_recv().unwrap();
        let ignored = drain_test_events(&mut events)
            .await
            .into_iter()
            .any(|event| {
                matches!(
                    event,
                    SpurEventBody::WorkerPeerMessageIgnored { message_id: id, reason, .. }
                        if id == message_id && reason == "worker:other"
                )
            });
        assert!(ignored, "expected WorkerPeerMessageIgnored event");
    }

    #[tokio::test]
    async fn terminal_helper_emits_malformed_event_on_bad_message_id() {
        use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
        use crate::peer_mailbox::router::PeerMailboxRouter;
        use crate::peer_mailbox::PeerMailboxBundle;
        use tokio::sync::mpsc::unbounded_channel;

        let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
        let (funnel, mut events) = crate::event_funnel::test_channel();
        let (reconciler_tx, _reconciler_rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger.clone(),
            funnel.clone(),
            reconciler_tx,
            Limits::default(),
        ));
        let bundle = PeerMailboxBundle {
            router,
            builder: Arc::new(PeerPromptContextBuilder::new(ledger.clone())),
            ledger,
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        };

        let (ack_tx, mut ack_rx) = unbounded_channel();
        interpret_peer_message_terminal(
            "_spur/peer_message_consumed",
            json!({"message_id": "not-a-uuid"}),
            &bundle,
            &ack_tx,
            &funnel,
            "bs",
            "exec-1",
        )
        .await;

        let events = drain_test_events(&mut events).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            SpurEventBody::WorkerPeerMessageMalformed {
                brain_session_id,
                source_executor_id,
                method,
                reason,
            } => {
                assert_eq!(brain_session_id, "bs");
                assert_eq!(source_executor_id, "exec-1");
                assert_eq!(method, "_spur/peer_message_consumed");
                assert!(reason.starts_with("malformed_message_id"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        ack_rx.try_recv().unwrap();
    }

    #[tokio::test]
    async fn terminal_helper_emits_malformed_event_on_missing_message_id() {
        use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
        use crate::peer_mailbox::limits::Limits;
        use crate::peer_mailbox::prompt_builder::PeerPromptContextBuilder;
        use crate::peer_mailbox::router::PeerMailboxRouter;
        use crate::peer_mailbox::PeerMailboxBundle;
        use tokio::sync::mpsc::unbounded_channel;

        let ledger: Arc<dyn PeerMailboxLedger> = Arc::new(InMemoryLedger::new());
        let (funnel, mut events) = crate::event_funnel::test_channel();
        let (reconciler_tx, _reconciler_rx) = unbounded_channel();
        let router = Arc::new(PeerMailboxRouter::new(
            ledger.clone(),
            funnel.clone(),
            reconciler_tx,
            Limits::default(),
        ));
        let bundle = PeerMailboxBundle {
            router,
            builder: Arc::new(PeerPromptContextBuilder::new(ledger.clone())),
            ledger,
            brain_session_id_slot: Arc::new(tokio::sync::RwLock::new(Some("bs".into()))),
        };

        let (ack_tx, mut ack_rx) = unbounded_channel();
        interpret_peer_message_terminal(
            "_spur/peer_message_consumed",
            json!({}),
            &bundle,
            &ack_tx,
            &funnel,
            "bs",
            "exec-1",
        )
        .await;

        let events = drain_test_events(&mut events).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            SpurEventBody::WorkerPeerMessageMalformed { reason, .. } => {
                assert!(reason.starts_with("missing_message_id"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        ack_rx.try_recv().unwrap();
    }

    #[test]
    fn cap_reason_passes_allowlist_through() {
        for reason in REASON_ALLOWLIST {
            assert_eq!(cap_reason(reason), *reason);
        }
    }

    #[test]
    fn cap_reason_collapses_unknown_short_string_to_other() {
        assert_eq!(cap_reason("plan_diverged"), "worker:other");
    }

    #[test]
    fn cap_reason_collapses_oversized_string_to_other_oversized() {
        assert_eq!(cap_reason(&"x".repeat(200)), "worker:other_oversized");
    }

    #[test]
    fn cap_reason_bounds_cardinality_strictly() {
        let mut buckets = std::collections::HashSet::new();
        for reason in REASON_ALLOWLIST {
            buckets.insert(cap_reason(reason));
        }
        for i in 0..1000 {
            let raw = if i % 2 == 0 {
                format!("random_reason_{i}")
            } else {
                format!("random_reason_{i}_{}", "x".repeat(160))
            };
            buckets.insert(cap_reason(&raw));
        }
        assert!(buckets.len() <= REASON_ALLOWLIST.len() + 2);
    }
}
