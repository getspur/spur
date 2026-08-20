use serde_json::json;

use crate::KnowledgeCandidate;
use crate::KnowledgeQueryResult;

use super::super::graph_reasoning::GraphReasoningSections;
use super::super::impact::SymbolImpactSummary;
use super::super::request::PackResponseFormat;
use super::*;

fn code_candidate(id: &str, title: &str) -> KnowledgeCandidate {
    KnowledgeCandidate {
        kind: "code".into(),
        title: title.into(),
        file_path: "crates/foo/src/lib.rs".into(),
        stable_symbol_id: Some(id.into()),
        symbol_kind: Some("function".into()),
        score: 0.9,
        signal: Some("leaf".into()),
        neighbor_kind: Some("primary".into()),
        edge_bind_method: None,
        grounding: "hybrid-code".into(),
    }
}

#[tokio::test]
async fn compact_pack_omits_empty_sections_and_duplicate_evidence() {
    let request = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "intent": "explain",
        "scope": "code"
    }))
    .expect("request");
    assert_eq!(request.response_format, PackResponseFormat::Compact);

    let result = KnowledgeQueryResult {
        db_path: "/tmp/analyst.duckdb".into(),
        graph_content_hash: Some("hash-one".into()),
        candidates: vec![code_candidate("sym-one", "symbol_one")],
    };
    let exact_context = ExactGraphContext {
        graph_content_hash: Some("hash-one".into()),
        response_file_oids_match: Some(true),
        impacts: vec![Some(SymbolImpactSummary {
            selector: "graph://symbol/sym-one".into(),
            callers_count: 2,
            callees_count: 3,
            caller_neighbors: vec![json!({
                "uri": "graph://symbol/caller",
                "entity_name": "caller_fn"
            })],
            callee_neighbors: vec![json!({
                "uri": "graph://symbol/callee",
                "entity_name": "callee_fn"
            })],
        })],
    };
    let sections = GraphReasoningSections {
        risk_scorecard: vec![json!({
            "stable_symbol_id": "sym-one",
            "posture": "leaf",
            "churn_90d": 1,
            "blast_radius_score": 0.5
        })],
        community_context: vec![json!({
            "stable_symbol_id": "sym-one",
            "community_id": 7,
            "component_size": 3
        })],
        temporal_context: vec![json!({
            "stable_symbol_id": "sym-one",
            "churn_90d": 1,
            "last_touched": "2026-07-06"
        })],
        ..GraphReasoningSections::default()
    };

    let pack =
        pack_query_result_v2_with_graph_sections(&request, result, exact_context, sections).await;

    assert!(pack.get("graph_paths").is_none());
    assert!(pack.get("risk_scorecard").is_none());
    assert!(pack.get("community_context").is_none());
    assert!(pack.get("temporal_context").is_none());
    assert!(pack.get("caveats").is_none());
    assert!(pack.get("supporting_docs").is_none());
    assert!(pack.get("staleness").is_none());
    let evidence = &pack["primary_evidence"][0];
    assert!(evidence.get("why_relevant").is_none());
    assert!(evidence.get("next").is_none());
    assert_eq!(evidence["posture"], "leaf");
    assert_eq!(evidence["community_id"], 7);
    assert_eq!(evidence["churn_90d"], 1);
    assert_eq!(evidence["impact"]["callers_count"], 2);
    assert!(pack["impact"].get("caller_neighbors").is_none());
    assert!(pack["impact"].get("callee_neighbors").is_none());
    assert_eq!(
        pack["recommended_next_tools"][0]["selector"],
        "graph://symbol/sym-one"
    );
}

#[tokio::test]
async fn full_pack_keeps_verbose_sections() {
    let request = KnowledgeContextPackV2Request::parse(&json!({
        "query": "semantic search",
        "response_format": "full",
        "graph_reasoning": { "risk": true, "communities": true }
    }))
    .expect("request");
    assert_eq!(request.response_format, PackResponseFormat::Full);

    let result = KnowledgeQueryResult {
        db_path: "/tmp/analyst.duckdb".into(),
        graph_content_hash: Some("hash-one".into()),
        candidates: vec![code_candidate("sym-one", "symbol_one")],
    };
    let pack = pack_query_result_v2_with_graph_sections(
        &request,
        result,
        ExactGraphContext {
            graph_content_hash: Some("hash-one".into()),
            response_file_oids_match: Some(true),
            impacts: Vec::new(),
        },
        GraphReasoningSections::default(),
    )
    .await;

    assert_eq!(pack["graph_paths"], json!([]));
    assert_eq!(pack["risk_scorecard"], json!([]));
    assert_eq!(pack["community_context"], json!([]));
    assert_eq!(pack["temporal_context"], json!([]));
    assert!(pack["primary_evidence"][0].get("why_relevant").is_some());
    assert!(pack.get("staleness").is_some());
}

#[test]
fn compact_staleness_preserves_unavailable_and_unverified_state() {
    let mut pack = json!({
        "staleness": {
            "available": false,
            "exact_graph_verified": false,
            "analyst_matches_exact_graph": null,
            "delta_applied": false
        }
    });

    compact_staleness(&mut pack);

    assert_eq!(pack["staleness"]["available"], false);
    assert_eq!(pack["staleness"]["exact_graph_verified"], false);
}
