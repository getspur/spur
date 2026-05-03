//! Integration tests for delegate_parallel per-task field plumbing
//! (T1.2/A1 + T1.3/R3/A5/A3).
//!
//! These tests exercise parse_parallel_tasks by calling it directly and
//! asserting each DelegationRequest carries the right per-task fields.

use rmcp::{
    model::{CallToolRequestParams, JsonObject},
    serve_server, ServiceExt,
};
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::tools::{BaseSpec, BaseTarget, OverlayCommit};
use spur_mcp::{server::DetachedContinuationCtx, McpCallbackServer};
use std::sync::Arc;

mod common;

#[test]
fn per_task_context_files_survive_to_delegation_requests() {
    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "Task A", "context_files": ["src/a1.rs", "src/a2.rs"] },
            { "agent": "claude-code-acp", "task": "Task B", "context_files": ["src/b1.rs"] }
        ]
    });

    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("test-brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0].context_files,
        vec!["src/a1.rs".to_string(), "src/a2.rs".to_string()]
    );
    assert_eq!(parsed[1].context_files, vec!["src/b1.rs".to_string()]);
}

#[test]
fn per_task_issue_id_and_delegation_plan_survive_unshared() {
    let args = json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "Task A",
                "issue_id": "bd-1",
                "delegation_plan": { "chosen": "claude-code-acp", "rationale": "A rationale" }
            },
            {
                "agent": "gpt-5-acp",
                "task": "Task B",
                "issue_id": "bd-2",
                "delegation_plan": { "chosen": "gpt-5-acp", "rationale": "B rationale" }
            }
        ],
        "delegation_plan": { "chosen": "batch-top-level", "rationale": "SHOULD NOT propagate" }
    });

    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("test-brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    assert_eq!(parsed.len(), 2);

    assert_eq!(parsed[0].issue_id.as_deref(), Some("bd-1"));
    assert_eq!(parsed[1].issue_id.as_deref(), Some("bd-2"));

    // Per-task plans are distinct.
    let p0 = parsed[0].delegation_plan.as_ref().expect("plan A present");
    let p1 = parsed[1].delegation_plan.as_ref().expect("plan B present");
    assert_eq!(p0.chosen.as_deref(), Some("claude-code-acp"));
    assert_eq!(p1.chosen.as_deref(), Some("gpt-5-acp"));

    // Top-level plan from the args MUST NOT have been propagated.
    assert!(
        p0.chosen.as_deref() != Some("batch-top-level"),
        "top-level delegation_plan leaked into per-task request",
    );
    assert!(p1.chosen.as_deref() != Some("batch-top-level"));
}

#[test]
fn duplicate_non_none_issue_id_is_rejected() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A", "issue_id": "bd-1" },
            { "agent": "x", "task": "B", "issue_id": "bd-1" }
        ]
    });
    let err =
        spur_mcp::validate_parallel_args(&args).expect_err("duplicate issue_id must be rejected");
    assert!(
        err.contains("issue_id"),
        "error should mention issue_id: {err}",
    );
}

#[test]
fn duplicate_none_issue_id_across_tasks_is_allowed() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A" },
            { "agent": "x", "task": "B" }
        ]
    });
    spur_mcp::validate_parallel_args(&args).expect("None-id twice is fine");
}

#[test]
fn distinct_issue_ids_pass() {
    let args = json!({
        "tasks": [
            { "agent": "x", "task": "A", "issue_id": "bd-1" },
            { "agent": "x", "task": "B", "issue_id": "bd-2" }
        ]
    });
    spur_mcp::validate_parallel_args(&args).expect("distinct ids pass");
}

#[test]
fn parse_parallel_tasks_requires_brain_session_id() {
    use spur_acp::{BrainSessionId, SessionId};

    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "T" }
        ]
    });
    let brain_sid = BrainSessionId::new(SessionId("brain-xyz".into()));

    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");

    assert_eq!(parsed.len(), 1);
    assert_eq!(
        parsed[0].brain_session_id.as_session_id().0,
        "brain-xyz",
        "brain_session_id must be threaded through, not defaulted to SessionId::new()"
    );
    // Negative intent: prior to INV-2, parse_parallel_tasks stamped a
    // fresh random UUID. If anyone re-introduces that fallback, this
    // assertion catches the value diverging from what the caller passed.
    assert_ne!(
        parsed[0].brain_session_id.as_session_id().0,
        spur_acp::SessionId::new().0,
        "brain_session_id must equal the caller-supplied value, not a fresh UUID"
    );
}

// ─── bd-1u8p: BaseSpec per-task plumbing ──────────────────────────────

#[test]
fn per_task_base_repo_main_survives() {
    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "T", "base": { "kind": "repo_main" } }
        ]
    });
    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    assert_eq!(parsed[0].base, Some(BaseSpec::RepoMain));
}

#[test]
fn per_task_base_branch_survives() {
    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "T", "base": { "kind": "branch", "name": "feat/foo" } }
        ]
    });
    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    assert_eq!(
        parsed[0].base,
        Some(BaseSpec::Branch {
            name: "feat/foo".into()
        })
    );
}

#[test]
fn per_task_base_commit_survives() {
    let oid = "0000000000000000000000000000000000000000";
    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "T", "base": { "kind": "commit", "oid": oid } }
        ]
    });
    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    assert_eq!(parsed[0].base, Some(BaseSpec::Commit { oid: oid.into() }));
}

#[test]
fn per_task_base_with_overlay_survives() {
    let args = json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "T",
                "base": {
                    "kind": "with_overlay",
                    "base": { "kind": "repo_main" },
                    "overlays": [
                        {
                            "base_oid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "tip_oid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "source_task_id": "test-overlay"
                        }
                    ]
                }
            }
        ]
    });
    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
    let parsed = spur_mcp::parse_parallel_tasks(&args, &brain_sid).expect("parse ok");
    match &parsed[0].base {
        Some(BaseSpec::WithOverlay { base, overlays }) => {
            assert_eq!(*base, BaseTarget::RepoMain);
            assert_eq!(overlays.len(), 1);
            assert_eq!(
                overlays[0],
                OverlayCommit {
                    base_oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                    tip_oid: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                    source_task_id: "test-overlay".into(),
                }
            );
        }
        other => panic!("expected WithOverlay, got {:?}", other),
    }
}

#[test]
fn per_task_delegation_plan_rejects_malformed_object() {
    let args = json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "Task A",
                "delegation_plan": { "chosen": 7 }
            }
        ]
    });

    let brain_sid = spur_acp::BrainSessionId::new(spur_acp::SessionId("test-brain".into()));
    let err = spur_mcp::parse_parallel_tasks(&args, &brain_sid)
        .expect_err("malformed per-task delegation_plan must be rejected");

    assert!(
        err.contains("Invalid task arguments:"),
        "error should mention Invalid task arguments: {err}",
    );
    assert!(
        err.contains("expected a string"),
        "error should preserve the serde message: {err}",
    );
}

// ─── bd-1u8p: rmcp transport regression for stringified per-task `base` ───
//
// Mirrors `base_branch_survives_when_sent_as_string` in
// `delegate_to_worker_fields.rs`. The other per-task BaseSpec tests above
// call `parse_parallel_tasks` directly; this one drives `delegate_parallel`
// over the rmcp duplex transport so any layer that might re-stringify or
// reject the field (envelope deserialization, schema validation, args
// shaping) is also covered.

fn json_object(value: Value) -> JsonObject {
    value
        .as_object()
        .cloned()
        .expect("tool arguments must be a JSON object")
}

fn mock_server() -> (McpCallbackServer, spur_mcp::DelegationChannel) {
    let brain_sid = BrainSessionId::new(SessionId::new());
    McpCallbackServer::new(
        &brain_sid,
        None,
        None,
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        },
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::community_feature_gate(),
    )
}

async fn call_delegate_parallel(args: Value) -> Vec<spur_mcp::tools::DelegationRequest> {
    let (server, mut channel) = mock_server();
    let server = Arc::new(server);
    let (client_io, server_io) = tokio::io::duplex(16 * 1024);

    let server_service_task = tokio::spawn({
        let server = Arc::clone(&server);
        async move { serve_server(server, server_io).await }
    });

    let mut client = ().serve(client_io).await.expect("client init");

    let task_count = args
        .get("tasks")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .expect("args must include a `tasks` array");

    let _result = client
        .call_tool(CallToolRequestParams::new("delegate_parallel").with_arguments(json_object(args)))
        .await
        .expect("call_tool should succeed");

    let mut requests = Vec::with_capacity(task_count);
    for _ in 0..task_count {
        let req = channel
            .request_rx
            .recv()
            .await
            .expect("delegation request should be sent");
        requests.push(req);
    }

    let _ = client.close().await;
    let mut server_service = server_service_task
        .await
        .expect("server bootstrap must not panic")
        .expect("server service ok");
    let _ = server_service.close().await;

    requests
}

#[tokio::test]
async fn per_task_base_branch_survives_when_sent_as_string() {
    let requests = call_delegate_parallel(json!({
        "tasks": [
            {
                "agent": "claude-code-acp",
                "task": "test task",
                "base": "{\"kind\":\"branch\",\"name\":\"feat/from-string\"}"
            }
        ]
    }))
    .await;

    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].base,
        Some(BaseSpec::Branch {
            name: "feat/from-string".into()
        })
    );
}
