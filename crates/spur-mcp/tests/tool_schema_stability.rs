use schemars::schema_for;
use serde_json::json;
use spur_mcp::tool_schemas::{
    DelegateParallelInput, DelegateParallelTaskInput, DelegateToWorkerInput,
};

#[test]
fn test_delegate_to_worker_schema_stability() {
    let schema = schema_for!(DelegateToWorkerInput);
    let json = serde_json::to_value(&schema).unwrap();
    assert_eq!(json["title"], "DelegateToWorkerInput");
    assert!(json["properties"].get("agent").is_some());
    assert!(json["properties"].get("task").is_some());
    assert!(json["properties"].get("delegation_plan").is_some());
    assert_eq!(json["additionalProperties"], json!(false));
}

#[test]
fn test_delegate_parallel_schema_stability() {
    let schema = schema_for!(DelegateParallelInput);
    let json = serde_json::to_value(&schema).unwrap();
    assert_eq!(json["title"], "DelegateParallelInput");
    assert!(json["properties"].get("tasks").is_some());
    assert!(json["properties"].get("delegation_plan").is_none());
    assert_eq!(json["additionalProperties"], json!(false));
}

#[test]
fn test_delegate_parallel_task_schema_stability() {
    let schema = schema_for!(DelegateParallelTaskInput);
    let json = serde_json::to_value(&schema).unwrap();
    assert_eq!(json["title"], "DelegateParallelTaskInput");
    assert!(json["properties"].get("agent").is_some());
    assert!(json["properties"].get("task").is_some());
    assert!(json["properties"].get("delegation_plan").is_some());
    assert_eq!(json["additionalProperties"], json!(false));
}
