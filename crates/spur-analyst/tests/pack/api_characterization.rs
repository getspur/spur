use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use spur_analyst::mcp::{AnalystMcpModule, McpHandlerError, ToolDefinition};
use spur_analyst::{
    KnowledgeCandidate, KnowledgePathEngine, KnowledgePathOptions, KnowledgePathResult,
    KnowledgePathRow, KnowledgePathStatus, KnowledgeQueryIntent, KnowledgeQueryOptions,
    KnowledgeQueryResult, KnowledgeSearchScope, SymbolCommunityContextRow, SymbolEvidenceCaveat,
    SymbolEvidenceStatus, SymbolGraphMetrics, SymbolRiskCommunityResult, SymbolRiskScorecardRow,
};

const PUBLIC_API_TYPES: &[&str] = &[
    "KnowledgeSearchScope",
    "KnowledgeQueryIntent",
    "KnowledgeQueryOptions",
    "KnowledgeCandidate",
    "KnowledgeQueryResult",
    "SymbolEvidenceStatus",
    "SymbolEvidenceCaveat",
    "SymbolRiskScorecardRow",
    "SymbolCommunityContextRow",
    "SymbolGraphMetrics",
    "SymbolRiskCommunityResult",
    "KnowledgePathEngine",
    "KnowledgePathStatus",
    "KnowledgePathOptions",
    "KnowledgePathRow",
    "KnowledgePathResult",
    "ToolDefinition",
    "McpHandlerError",
    "AnalystMcpModule",
];

#[test]
fn public_api_type_inventory_matches_step_1_safety_net() {
    assert_eq!(PUBLIC_API_TYPES.len(), 19);
    assert_eq!(
        PUBLIC_API_TYPES,
        [
            "KnowledgeSearchScope",
            "KnowledgeQueryIntent",
            "KnowledgeQueryOptions",
            "KnowledgeCandidate",
            "KnowledgeQueryResult",
            "SymbolEvidenceStatus",
            "SymbolEvidenceCaveat",
            "SymbolRiskScorecardRow",
            "SymbolCommunityContextRow",
            "SymbolGraphMetrics",
            "SymbolRiskCommunityResult",
            "KnowledgePathEngine",
            "KnowledgePathStatus",
            "KnowledgePathOptions",
            "KnowledgePathRow",
            "KnowledgePathResult",
            "ToolDefinition",
            "McpHandlerError",
            "AnalystMcpModule",
        ]
    );
}

#[test]
fn serde_round_trips_public_data_contracts() {
    let candidate = KnowledgeCandidate {
        kind: "symbol".into(),
        title: "dispatch_plan".into(),
        file_path: "src/dispatch.rs".into(),
        stable_symbol_id: Some("sym-dispatch".into()),
        symbol_kind: Some("function".into()),
        score: 7.5,
        signal: Some("active".into()),
        neighbor_kind: Some("primary".into()),
        edge_bind_method: Some("singleton".into()),
        grounding: "bm25-code".into(),
    };
    round_trip_json("KnowledgeCandidate", &candidate);

    round_trip_json(
        "KnowledgeQueryResult",
        &KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![candidate.clone()],
        },
    );

    for status in [
        SymbolEvidenceStatus::Available,
        SymbolEvidenceStatus::MissingSymbol,
        SymbolEvidenceStatus::Unavailable,
    ] {
        round_trip_json("SymbolEvidenceStatus", &status);
    }

    let caveat = SymbolEvidenceCaveat {
        stable_symbol_id: Some("sym-dispatch".into()),
        code: "fixture_caveat".into(),
        message: "fixture caveat message".into(),
    };
    round_trip_json("SymbolEvidenceCaveat", &caveat);

    let risk_row = SymbolRiskScorecardRow {
        input_index: 0,
        stable_symbol_id: "sym-dispatch".into(),
        status: SymbolEvidenceStatus::Available,
        entity_name: Some("dispatch_plan".into()),
        qualified_name: Some("fixture::dispatch_plan".into()),
        symbol_kind: Some("function".into()),
        file_path: Some("src/dispatch.rs".into()),
        pagerank: Some(0.42),
        in_degree: Some(7),
        out_degree: Some(3),
        callers: Some(11),
        importers: Some(2),
        inbound_total: Some(13),
        churn_90d: Some(9),
        last_touched: Some("2026-06-17 12:00:00".into()),
        blast_radius_score: Some(0.91),
        posture: Some("load-bearing wall".into()),
        caveats: vec![caveat.clone()],
    };
    round_trip_json("SymbolRiskScorecardRow", &risk_row);

    let community_row = SymbolCommunityContextRow {
        input_index: 0,
        stable_symbol_id: "sym-dispatch".into(),
        status: SymbolEvidenceStatus::Available,
        component_id: Some(10),
        component_size: Some(2),
        community_id: Some(20),
        caveats: vec![caveat.clone()],
    };
    round_trip_json("SymbolCommunityContextRow", &community_row);

    let graph_metrics = SymbolGraphMetrics {
        calls_edges: Some(1),
        connected_nodes: Some(2),
        components: Some(1),
        largest_component: Some(2),
        communities: Some(1),
        density: Some(0.5),
    };
    round_trip_json("SymbolGraphMetrics", &graph_metrics);

    round_trip_json(
        "SymbolRiskCommunityResult",
        &SymbolRiskCommunityResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            max_symbols: 40,
            truncated: false,
            risk_scorecard: vec![risk_row],
            community_context: vec![community_row],
            graph_metrics: Some(graph_metrics),
            caveats: vec![caveat],
        },
    );

    for engine in [
        KnowledgePathEngine::DuckPgq,
        KnowledgePathEngine::RecursiveSql,
        KnowledgePathEngine::Unavailable,
    ] {
        round_trip_json("KnowledgePathEngine", &engine);
    }

    for status in [
        KnowledgePathStatus::PathFound,
        KnowledgePathStatus::NoPath,
        KnowledgePathStatus::Unavailable,
    ] {
        round_trip_json("KnowledgePathStatus", &status);
    }

    let path_row = KnowledgePathRow {
        path_index: 0,
        hop_index: 0,
        source_stable_id: "sym-dispatch".into(),
        target_stable_id: "sym-review".into(),
        relation: Some("calls".into()),
        edge_kind: Some("calls".into()),
        confidence: Some("syntax_exact".into()),
        bind_method: Some("singleton".into()),
        direction: Some("forward".into()),
        engine: KnowledgePathEngine::RecursiveSql,
        status: KnowledgePathStatus::PathFound,
        caveat: None,
    };
    round_trip_json("KnowledgePathRow", &path_row);

    round_trip_json(
        "KnowledgePathResult",
        &KnowledgePathResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            max_hops: 4,
            max_paths: 6,
            engine: KnowledgePathEngine::RecursiveSql,
            status: KnowledgePathStatus::PathFound,
            caveat: None,
            rows: vec![path_row],
        },
    );

    round_trip_json(
        "ToolDefinition",
        &ToolDefinition {
            name: "query".into(),
            description: "Execute read-only DuckDB SQL.".into(),
            input_schema: json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" }
                }
            }),
        },
    );
}

#[test]
fn public_default_impls_are_stable() {
    let query_options = KnowledgeQueryOptions::default();
    assert_eq!(query_options.limit, 20);
    assert_eq!(query_options.intent, KnowledgeQueryIntent::Explain);
    assert_eq!(query_options.query_vec, None);

    let path_options = KnowledgePathOptions::default();
    assert_eq!(path_options.max_hops, 4);
    assert_eq!(path_options.max_paths, 6);
    assert!(!path_options.undirected);

    let default_module = AnalystMcpModule::default();
    assert_eq!(
        tool_names(&default_module),
        tool_names(&AnalystMcpModule::new())
    );
    assert_eq!(
        tool_names(&default_module),
        tool_names(&AnalystMcpModule::read_only())
    );
}

#[test]
fn non_serde_public_api_behaviors_are_stable() {
    assert_eq!(
        KnowledgeSearchScope::try_from("all").unwrap(),
        KnowledgeSearchScope::All
    );
    assert_eq!(
        KnowledgeSearchScope::try_from("docs").unwrap(),
        KnowledgeSearchScope::Docs
    );
    assert_eq!(
        KnowledgeSearchScope::try_from("code").unwrap(),
        KnowledgeSearchScope::Code
    );
    assert_eq!(
        KnowledgeSearchScope::try_from("graph").unwrap(),
        KnowledgeSearchScope::Graph
    );
    assert!(KnowledgeSearchScope::try_from("invalid")
        .expect_err("invalid scope must fail")
        .to_string()
        .contains("all|docs|code|graph"));

    let options = KnowledgeQueryOptions {
        limit: 3,
        intent: KnowledgeQueryIntent::Review,
        query_vec: Some(vec![0.1, 0.2]),
    };
    let cloned = options.clone();
    assert_eq!(cloned.limit, 3);
    assert_eq!(cloned.intent, KnowledgeQueryIntent::Review);
    assert_eq!(cloned.query_vec, Some(vec![0.1, 0.2]));

    let path_options = KnowledgePathOptions {
        max_hops: 2,
        max_paths: 1,
        undirected: true,
    };
    assert_eq!(path_options.max_hops, 2);
    assert_eq!(path_options.max_paths, 1);
    assert!(path_options.undirected);

    let error_cases = [
        (
            McpHandlerError::InvalidParams("bad params".into()),
            -32602,
            "invalid params",
        ),
        (
            McpHandlerError::NotFound("missing".into()),
            -32004,
            "not found",
        ),
        (
            McpHandlerError::Unauthorized("denied".into()),
            -32001,
            "unauthorized",
        ),
        (
            McpHandlerError::UpstreamPm("pm failed".into()),
            -32603,
            "upstream PM failure",
        ),
        (McpHandlerError::Internal("boom".into()), -32603, "internal"),
    ];
    for (error, code, display_prefix) in error_cases {
        assert_eq!(error.json_rpc_code(), code);
        assert!(
            error.to_string().starts_with(display_prefix),
            "unexpected display for {error:?}"
        );
    }
}

fn round_trip_json<T>(name: &str, value: &T)
where
    T: Serialize + DeserializeOwned,
{
    let encoded = serde_json::to_value(value).unwrap_or_else(|error| {
        panic!("{name} should serialize to JSON: {error}");
    });
    let decoded: T = serde_json::from_value(encoded.clone()).unwrap_or_else(|error| {
        panic!("{name} should deserialize from JSON: {error}");
    });
    let reencoded = serde_json::to_value(decoded).unwrap_or_else(|error| {
        panic!("{name} should reserialize to JSON: {error}");
    });
    assert_eq!(reencoded, encoded, "{name} JSON round-trip drifted");
}

fn tool_names(module: &AnalystMcpModule) -> Vec<String> {
    module.tools().into_iter().map(|tool| tool.name).collect()
}
