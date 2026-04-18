//! Integration tests for delegate_parallel per-task field plumbing
//! (T1.2/A1 + T1.3/R3/A5/A3).
//!
//! These tests exercise parse_parallel_tasks by calling it directly and
//! asserting each DelegationRequest carries the right per-task fields.

use serde_json::json;

#[test]
fn per_task_context_files_survive_to_delegation_requests() {
    let args = json!({
        "tasks": [
            { "agent": "claude-code-acp", "task": "Task A", "context_files": ["src/a1.rs", "src/a2.rs"] },
            { "agent": "claude-code-acp", "task": "Task B", "context_files": ["src/b1.rs"] }
        ]
    });

    let parsed = spur_mcp::parse_parallel_tasks(&args).expect("parse ok");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].context_files, vec!["src/a1.rs".to_string(), "src/a2.rs".to_string()]);
    assert_eq!(parsed[1].context_files, vec!["src/b1.rs".to_string()]);
}
