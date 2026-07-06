use spur_analyst::*;

fn exported_type<T>() -> &'static str {
    std::any::type_name::<T>()
}

#[test]
fn public_api_types_remain_exported() {
    let exported = [
        exported_type::<KnowledgeSearchScope>(),
        exported_type::<KnowledgeQueryIntent>(),
        exported_type::<KnowledgeQueryOptions>(),
        exported_type::<KnowledgeCandidate>(),
        exported_type::<KnowledgeQueryResult>(),
        exported_type::<SymbolEvidenceStatus>(),
        exported_type::<SymbolEvidenceCaveat>(),
        exported_type::<SymbolRiskScorecardRow>(),
        exported_type::<SymbolCommunityContextRow>(),
        exported_type::<SymbolGraphMetrics>(),
        exported_type::<SymbolRiskCommunityResult>(),
        exported_type::<KnowledgePathEngine>(),
        exported_type::<KnowledgePathStatus>(),
        exported_type::<KnowledgePathOptions>(),
        exported_type::<KnowledgePathRow>(),
        exported_type::<KnowledgePathResult>(),
        exported_type::<mcp::ToolDefinition>(),
        exported_type::<mcp::McpHandlerError>(),
        exported_type::<mcp::AnalystMcpModule>(),
    ];

    assert_eq!(exported.len(), 19);
}
