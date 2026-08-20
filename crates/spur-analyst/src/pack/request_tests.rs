use serde_json::json;

use super::*;

#[test]
fn knowledge_context_pack_rejects_empty_query() {
    let error = KnowledgeContextPackRequest::parse(&json!({ "query": "   " }))
        .expect_err("empty query must be rejected");

    assert_eq!(error.json_rpc_code(), -32602);
    assert!(
        error.to_string().contains("non-empty string field 'query'"),
        "unexpected error: {error}"
    );
}

#[test]
fn knowledge_context_pack_queries_graph_for_graph_scope_or_change_debug_all_scope() {
    for (scope, intent, expected) in [
        ("graph", "explain", true),
        ("all", "debug", true),
        ("all", "change", true),
        ("all", "explain", false),
        ("code", "debug", false),
        ("docs", "change", false),
    ] {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search",
            "scope": scope,
            "intent": intent
        }))
        .expect("request");

        assert_eq!(
            request.should_query_graph_candidates(),
            expected,
            "scope={scope} intent={intent}"
        );
    }
}

#[test]
fn knowledge_context_pack_2_parser_clamps_graph_reasoning_budgets() {
    let high = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "review",
        "graph_reasoning": {
            "paths": true,
            "communities": true,
            "risk": true,
            "max_path_hops": 999,
            "max_paths": 999,
            "anchors": ["graph://symbol/anchor-one"]
        }
    }))
    .expect("high budget request");

    assert_eq!(high.base.intent.as_str(), "review");
    assert!(high.graph_reasoning.paths);
    assert!(high.graph_reasoning.communities);
    assert!(high.graph_reasoning.risk);
    assert_eq!(
        high.graph_reasoning.max_path_hops,
        crate::MAX_CONTEXT_PATH_HOPS
    );
    assert_eq!(high.graph_reasoning.max_paths, crate::MAX_CONTEXT_PATHS);
    assert_eq!(
        high.graph_reasoning.anchors,
        vec!["graph://symbol/anchor-one".to_owned()]
    );

    let low = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "graph_reasoning": {
            "max_path_hops": 0,
            "max_paths": 0
        }
    }))
    .expect("low budget request");
    assert_eq!(low.graph_reasoning.max_path_hops, 1);
    assert_eq!(low.graph_reasoning.max_paths, 1);
}

#[test]
fn knowledge_context_pack_2_parser_defaults_graph_reasoning_by_intent_and_scope() {
    let change = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "change"
    }))
    .expect("change request");
    assert!(change.graph_reasoning.paths);
    assert!(change.graph_reasoning.risk);
    assert!(change.graph_reasoning.communities);

    let explain_docs = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "explain",
        "scope": "docs"
    }))
    .expect("docs request");
    assert!(!explain_docs.graph_reasoning.paths);
    assert!(!explain_docs.graph_reasoning.risk);
    assert!(!explain_docs.graph_reasoning.communities);
}

#[test]
fn knowledge_context_pack_2_explain_defaults_disable_risk_and_communities() {
    let explain = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "explain",
        "scope": "code"
    }))
    .expect("explain request");
    assert!(!explain.graph_reasoning.paths);
    assert!(!explain.graph_reasoning.risk);
    assert!(!explain.graph_reasoning.communities);
    assert!(!explain.graph_reasoning.communities_explicit);

    let plan = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "plan"
    }))
    .expect("plan request");
    assert!(!plan.graph_reasoning.risk);
    assert!(!plan.graph_reasoning.communities);
}

#[test]
fn knowledge_context_pack_defaults_max_symbol_bodies_to_zero() {
    let request = KnowledgeContextPackRequest::parse(&json!({
        "query": "semantic search"
    }))
    .expect("request");
    assert_eq!(request.max_symbol_bodies, 0);
}

#[test]
fn knowledge_context_pack_2_defaults_response_format_to_compact() {
    let request = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search"
    }))
    .expect("request");
    assert_eq!(request.response_format, PackResponseFormat::Compact);

    let full = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "response_format": "full"
    }))
    .expect("full request");
    assert_eq!(full.response_format, PackResponseFormat::Full);
}
