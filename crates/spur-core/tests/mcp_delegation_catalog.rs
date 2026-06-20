use spur_core::mcp::delegation::{DelegationMcpDeps, DelegationMcpModule};
use spur_mcp::ToolModule;

const DELEGATION_TOOLS: &[&str] = &[
    "delegate_to_worker",
    "delegate_parallel",
    "check_delegation_status",
    "fetch_outcome_artifact",
    "cancel_delegation",
    "list_available_workers",
];

#[test]
fn delegation_module_advertises_delegation_tools_in_compatibility_order() {
    let module = DelegationMcpModule::new(DelegationMcpDeps::catalog_only());
    let names: Vec<String> = module.tools().into_iter().map(|tool| tool.name).collect();
    let expected: Vec<String> = DELEGATION_TOOLS
        .iter()
        .map(|tool| tool.to_string())
        .collect();

    assert_eq!(names, expected);
}

#[test]
fn spur_mcp_legacy_catalog_no_longer_owns_delegation_tools() {
    let legacy_names: Vec<String> = spur_mcp::tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();

    for tool_name in DELEGATION_TOOLS {
        assert!(
            !legacy_names.iter().any(|name| name == tool_name),
            "{tool_name} must be owned by spur_core::mcp::delegation, not spur-mcp/tools.rs",
        );
    }
}

#[test]
fn fetch_outcome_artifact_schema_advertises_phase3_sections() {
    let module = DelegationMcpModule::new(DelegationMcpDeps::catalog_only());
    let def = module
        .tools()
        .into_iter()
        .find(|tool| tool.name == "fetch_outcome_artifact")
        .expect("fetch_outcome_artifact definition");
    let props = def
        .input_schema
        .get("properties")
        .and_then(|value| value.as_object())
        .expect("properties");
    let section = props.get("section").expect("section property");
    let enum_values: Vec<&str> = section
        .get("enum")
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(
        enum_values,
        vec!["status_only", "summary", "diff_only", "full"],
        "Phase 3 must advertise all fetchable sections"
    );

    let required: Vec<&str> = def
        .input_schema
        .get("required")
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_str()).collect())
        .unwrap_or_default();
    assert!(
        required.contains(&"delegation_id"),
        "delegation_id must be required"
    );
}
