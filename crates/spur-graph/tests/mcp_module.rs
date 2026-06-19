use spur_graph::mcp::tool_definitions;

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
}
