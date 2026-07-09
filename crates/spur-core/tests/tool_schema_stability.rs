use schemars::schema_for;
use serde_json::json;
use spur_core::mcp::tools_list;
use spur_core::tool_schemas::{
    schema_value, DelegateParallelInput, DelegateParallelTaskInput, DelegateToWorkerInput,
};

#[test]
fn test_delegate_to_worker_schema_stability() {
    let schema = schema_for!(DelegateToWorkerInput);
    let json = serde_json::to_value(&schema).unwrap();
    assert_eq!(json["title"], "DelegateToWorkerInput");
    assert!(json["properties"].get("agent").is_some());
    assert!(json["properties"].get("task").is_some());
    assert!(json["properties"].get("config_overrides").is_some());
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
    assert!(json["properties"].get("config_overrides").is_some());
    assert!(json["properties"].get("delegation_plan").is_some());
    assert_eq!(json["additionalProperties"], json!(false));
}

#[test]
fn published_tool_schemas_are_ref_free() {
    for def in tools_list() {
        assert_no_refs(&def.input_schema, &def.name);
    }
}

#[test]
fn schema_value_inlines_all_defs_refs() {
    let schema = schema_value::<DelegateToWorkerInput>();
    assert!(
        schema.get("$defs").is_none() && schema.get("definitions").is_none(),
        "schema_value must drop top-level defs once refs are inlined"
    );
    assert_no_refs(&schema, "DelegateToWorkerInput");
}

#[test]
fn delegation_schemas_are_ref_free() {
    let delegate_to_worker = schema_value::<DelegateToWorkerInput>();
    assert_no_refs(&delegate_to_worker, "DelegateToWorkerInput");

    let delegate_parallel = schema_value::<DelegateParallelInput>();
    assert_no_refs(&delegate_parallel, "DelegateParallelInput");
}

fn assert_no_refs(value: &serde_json::Value, context: &str) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(reference) = map.get("$ref") {
                panic!("{context} contains $ref: {reference}");
            }

            for nested in map.values() {
                assert_no_refs(nested, context);
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                assert_no_refs(nested, context);
            }
        }
        _ => {}
    }
}
