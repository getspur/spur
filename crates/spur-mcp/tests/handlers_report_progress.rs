//! T13: report_progress handler — fire-and-forget event emission.

use std::sync::Mutex;

use serde_json::json;
use spur_acp::SpurEventBody;
use spur_mcp::events::McpEventSink;
use spur_mcp::handlers::{report_progress, McpHandlerError, WorkerCallContext};

#[derive(Default)]
struct RecordingSink {
    events: Mutex<Vec<SpurEventBody>>,
}

impl McpEventSink for RecordingSink {
    fn emit(&self, event: SpurEventBody) {
        self.events.lock().unwrap().push(event);
    }
}

fn ctx() -> WorkerCallContext {
    WorkerCallContext {
        delegation_id: "del-42".into(),
        brain_session_id: "brain-1".into(),
    }
}

#[tokio::test]
async fn report_progress_emits_one_event_with_message_and_percent() {
    let sink = RecordingSink::default();
    let args = json!({ "message": "compiling crate spur-mcp", "percent": 42.5 });

    let value = report_progress(&sink, &ctx(), args)
        .await
        .expect("report_progress should succeed");

    assert_eq!(value["ok"].as_bool(), Some(true));

    let events = sink.events.lock().unwrap();
    assert_eq!(events.len(), 1, "exactly one event must be emitted");
    match &events[0] {
        SpurEventBody::WorkerReportProgress {
            delegation_id,
            message,
            percent,
        } => {
            assert_eq!(delegation_id, "del-42");
            assert_eq!(message, "compiling crate spur-mcp");
            assert_eq!(*percent, Some(42.5));
        }
        other => panic!("unexpected event variant: {other:?}"),
    }
}

#[tokio::test]
async fn report_progress_omits_percent_when_absent() {
    let sink = RecordingSink::default();
    let args = json!({ "message": "still working" });

    report_progress(&sink, &ctx(), args)
        .await
        .expect("report_progress should succeed without percent");

    let events = sink.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    match &events[0] {
        SpurEventBody::WorkerReportProgress {
            message, percent, ..
        } => {
            assert_eq!(message, "still working");
            assert!(percent.is_none(), "percent must be None when not supplied");
        }
        other => panic!("unexpected event variant: {other:?}"),
    }
}

#[tokio::test]
async fn report_progress_missing_message_returns_invalid_params_and_emits_nothing() {
    let sink = RecordingSink::default();
    let args = json!({ "percent": 10.0 });

    let err = report_progress(&sink, &ctx(), args)
        .await
        .expect_err("missing 'message' must be an InvalidParams error");
    assert!(
        matches!(err, McpHandlerError::InvalidParams(_)),
        "expected InvalidParams, got {err:?}"
    );

    assert!(
        sink.events.lock().unwrap().is_empty(),
        "no event must be emitted when args are invalid"
    );
}

#[tokio::test]
async fn report_progress_non_string_message_returns_invalid_params() {
    let sink = RecordingSink::default();
    let args = json!({ "message": 123 });

    let err = report_progress(&sink, &ctx(), args)
        .await
        .expect_err("non-string 'message' must be rejected");
    assert!(matches!(err, McpHandlerError::InvalidParams(_)));
    assert!(sink.events.lock().unwrap().is_empty());
}
