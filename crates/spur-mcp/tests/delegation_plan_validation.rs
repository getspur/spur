use serde_json::json;

#[test]
fn parse_delegation_plan_accepts_absent_and_null() {
    assert!(spur_mcp::parse_delegation_plan(&json!({}))
        .expect("absent plan should parse")
        .is_none(),);
    assert!(
        spur_mcp::parse_delegation_plan(&json!({ "delegation_plan": null }))
            .expect("null plan should parse")
            .is_none(),
    );
}

#[test]
fn parse_delegation_plan_accepts_valid_object() {
    let parsed = spur_mcp::parse_delegation_plan(&json!({
        "delegation_plan": {
            "chosen": "claude-code-acp",
            "rationale": "best fit",
            "candidates": [{ "agent": "claude-code-acp" }]
        }
    }))
    .expect("valid plan should parse")
    .expect("plan should be present");

    assert_eq!(parsed.chosen.as_deref(), Some("claude-code-acp"));
    assert_eq!(parsed.rationale.as_deref(), Some("best fit"));
    assert_eq!(parsed.candidates.len(), 1);
}

#[test]
fn parse_delegation_plan_rejects_malformed_object() {
    let err = spur_mcp::parse_delegation_plan(&json!({
        "delegation_plan": {
            "chosen": 7
        }
    }))
    .expect_err("malformed plan must be rejected");

    assert!(
        err.contains("invalid delegation_plan:"),
        "error should mention invalid delegation_plan: {err}",
    );
    assert!(
        err.contains("expected a string"),
        "error should preserve the serde message: {err}",
    );
}

#[test]
fn parse_parallel_tasks_rejects_malformed_per_task_delegation_plan() {
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
        err.contains("invalid delegation_plan:"),
        "error should mention invalid delegation_plan: {err}",
    );
    assert!(
        err.contains("expected a string"),
        "error should preserve the serde message: {err}",
    );
}
