use spur_graph::mcp::{tool_definitions, GraphMcpDeps, GraphMcpModule};

#[test]
fn graph_mcp_module_owns_code_tool_definitions() {
    let names: Vec<String> = tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect();

    assert_eq!(
        names,
        [
            "code_resolve",
            "code_file_symbols",
            "code_symbol_info",
            "code_read_symbol",
            "code_callers",
            "code_callees",
            "code_symbol_search",
            "code_subgraph",
            "code_symbol_history",
        ]
    );
    assert!(
        !names.iter().any(|name| name == "code_search"),
        "code_search is a registry alias and must not be advertised"
    );

    let module_definitions = GraphMcpModule::new(GraphMcpDeps::default()).tools();
    assert_eq!(
        serde_json::to_value(&module_definitions).expect("serialize module definitions"),
        serde_json::to_value(tool_definitions()).expect("serialize default definitions")
    );
    assert!(module_definitions.iter().all(|definition| definition
        .input_schema
        .pointer("/properties/project")
        .is_none()));
}
