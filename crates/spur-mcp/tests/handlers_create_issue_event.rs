use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId, SpurEvent, SpurEventBody};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::{McpCallbackServer, McpEventSink};
use spur_pm::PmService;
use tempfile::TempDir;

mod common;

fn run_br(repo: &Path, args: &[&str]) {
    common::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"));
}

async fn test_pm_service_empty(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

#[derive(Default)]
struct CaptureSink {
    events: std::sync::Mutex<Vec<SpurEvent>>,
}

impl McpEventSink for CaptureSink {
    fn emit(&self, event: SpurEventBody) {
        self.events.lock().unwrap().push(SpurEvent::now(event));
    }

    fn try_emit(&self, event: SpurEventBody) -> Result<(), SpurEventBody> {
        self.emit(event);
        Ok(())
    }
}

#[tokio::test]
async fn create_issue_emits_issue_created_event() {
    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = test_pm_service_empty(dir.path()).await;
    let sink = Arc::new(CaptureSink::default());
    let sink_ref: Arc<dyn McpEventSink> = Arc::clone(&sink) as Arc<dyn McpEventSink>;
    let session_id = BrainSessionId::new(SessionId::new());
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        Some(Arc::clone(&pm)),
        Some(sink_ref),
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );

    let response = server
        .__test_call_tool(
            "create_issue",
            json!({
                "title": "Emit IssueCreated",
                "description": "ensure create tool broadcasts",
                "type": "task",
                "priority": 2,
                "labels": ["signal:test"],
                "assignee": "alice"
            }),
        )
        .await;

    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "create_issue should succeed: {response}"
    );

    let events = sink.events.lock().unwrap();
    let created = events.iter().find_map(|event| match &event.body {
        SpurEventBody::IssueCreated { issue } => Some(issue.clone()),
        _ => None,
    });
    let created = created.expect("expected IssueCreated event");
    assert_eq!(created.title, "Emit IssueCreated");
    assert_eq!(
        created.description.as_deref(),
        Some("ensure create tool broadcasts")
    );
    assert_eq!(created.assignee.as_deref(), Some("alice"));
    assert_eq!(created.priority, Some(2));
    assert_eq!(created.issue_type.as_deref(), Some("task"));
    assert!(created.labels.iter().any(|label| label == "signal:test"));
}
