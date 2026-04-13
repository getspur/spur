//! Golden-text snapshot: confirm recursive traversal renders depth > 1.

use spur_acp::{SessionId, SpurEvent};
use spur_core::ExecutorLineage;

#[test]
fn recursive_tree_renders_depth_two() {
    let mut lineage = ExecutorLineage::new();
    // brain -> worker -> sub-worker
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "b".into(),
        parent_id: None,
        session_id: SessionId("b".into()),
        agent: "kiro".into(),
        role: "Brain".into(),
        task_spec: "root".into(),
    });
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "w".into(),
        parent_id: Some("b".into()),
        session_id: SessionId("w".into()),
        agent: "worker".into(),
        role: "Executor".into(),
        task_spec: "child".into(),
    });
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "sw".into(),
        parent_id: Some("w".into()),
        session_id: SessionId("sw".into()),
        agent: "sub".into(),
        role: "SubExecutor".into(),
        task_spec: "grandchild".into(),
    });

    let lines = spur_tui::components::agents_tree::render_lineage_to_strings(&lineage, None);

    assert!(lines.iter().any(|l| l.contains("kiro")));
    assert!(lines.iter().any(|l| l.contains("worker")));
    assert!(
        lines.iter().any(|l| l.contains("sub")),
        "sub-executor must appear in output"
    );
    // Depth indentation: sub-executor line should start with more whitespace than worker line.
    let worker_indent = lines
        .iter()
        .find(|l| l.contains("worker"))
        .and_then(|l| Some(l.len() - l.trim_start().len()))
        .unwrap_or(0);
    let sub_indent = lines
        .iter()
        .find(|l| l.contains("sub"))
        .and_then(|l| Some(l.len() - l.trim_start().len()))
        .unwrap_or(0);
    assert!(sub_indent > worker_indent, "sub-executor must be indented deeper than worker");
}
