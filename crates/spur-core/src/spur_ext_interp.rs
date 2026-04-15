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
        other => {
            tracing::debug!(method = other, "ignoring unknown _spur/* method");
        }
    }
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
}
