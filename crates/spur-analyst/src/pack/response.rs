use futures::future::join_all;
use serde_json::{json, Value};

use crate::search::hybrid::evidence_confidence;
use crate::KnowledgeQueryResult;

use super::evidence::{
    aggregate_impact_value, primary_evidence_with_impact, split_evidence, SymbolImpactSummary,
};
use super::next_tools::recommended_next_tools;
use super::staleness::{staleness_value, PackStaleness};
use super::{KnowledgeContextPackRequest, KnowledgeContextPackV2Request};

#[derive(Debug, Clone, Default)]
pub(crate) struct ExactGraphContext {
    pub(crate) graph_content_hash: Option<String>,
    pub(crate) response_file_oids_match: Option<bool>,
    pub(crate) impacts: Vec<Option<SymbolImpactSummary>>,
}

#[derive(Default)]
pub(crate) struct GraphReasoningSections {
    pub(crate) graph_paths: Vec<Value>,
    pub(crate) risk_scorecard: Vec<Value>,
    pub(crate) community_context: Vec<Value>,
    pub(crate) temporal_context: Vec<Value>,
    pub(crate) caveats: Vec<Value>,
}

impl GraphReasoningSections {
    pub(crate) fn with_caveat(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            caveats: vec![super::caveat_value(code, message, None)],
            ..Self::default()
        }
    }
}

#[cfg(test)]
pub(crate) async fn pack_query_result(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
) -> Value {
    pack_query_result_with_exact_context(request, result, ExactGraphContext::default()).await
}

pub(crate) async fn pack_query_result_with_exact_context(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
) -> Value {
    let staleness = PackStaleness::default_for_result(&result);
    pack_query_result_with_exact_context_and_staleness(request, result, exact_context, staleness)
        .await
}

pub(crate) async fn pack_query_result_with_exact_context_and_staleness(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    pack_staleness: PackStaleness,
) -> Value {
    let (mut primary_evidence, supporting_docs) = split_evidence(&result.candidates, request);
    let total_candidates = result.candidates.len();
    let total_code = result
        .candidates
        .iter()
        .filter(|candidate| candidate.kind == "code" || candidate.kind == "symbol")
        .count();
    let total_docs = result
        .candidates
        .iter()
        .filter(|candidate| candidate.kind == "doc")
        .count();
    if request.max_symbol_bodies > 0 {
        attach_symbol_bodies(&mut primary_evidence, request.max_symbol_bodies).await;
    }

    let recommended_next_tools =
        recommended_next_tools(request.intent, &primary_evidence, &supporting_docs);
    let answerable = !primary_evidence.is_empty() || !supporting_docs.is_empty();
    let confidence = if answerable {
        evidence_confidence(&primary_evidence, &supporting_docs)
    } else {
        "low"
    };
    let impact = aggregate_impact_value(&exact_context.impacts);
    let staleness = staleness_value(&result, &exact_context, &pack_staleness);
    let mut pack = base_pack(request, result.graph_content_hash.clone(), staleness);
    let returned_primary = primary_evidence.len();
    let returned_supporting_docs = supporting_docs.len();

    if let Some(object) = pack.as_object_mut() {
        object.insert("answerable".into(), json!(answerable));
        object.insert("confidence".into(), json!(confidence));
        object.insert(
            "primary_evidence".into(),
            Value::Array(primary_evidence_with_impact(
                primary_evidence,
                &exact_context.impacts,
            )),
        );
        object.insert("supporting_docs".into(), Value::Array(supporting_docs));
        object.insert("impact".into(), impact);
        object.insert(
            "recommended_next_tools".into(),
            Value::Array(recommended_next_tools),
        );
        object.insert(
            "candidates".into(),
            json!({
                "total": total_candidates,
                "returned_primary": returned_primary,
                "returned_supporting_docs": returned_supporting_docs,
                "total_code": total_code,
                "total_docs": total_docs,
            }),
        );
    }
    pack
}

async fn attach_symbol_bodies(primary_evidence: &mut [Value], max_symbol_bodies: u64) {
    let body_selectors: Vec<(String, usize)> = primary_evidence
        .iter()
        .enumerate()
        .take(max_symbol_bodies as usize)
        .filter_map(|(index, evidence)| {
            evidence
                .get("stable_symbol_id")
                .and_then(Value::as_str)
                .map(|selector| (selector.to_owned(), index))
        })
        .collect();

    let body_results = join_all(
        body_selectors
            .into_iter()
            .map(|(selector, index)| async move {
                (
                    index,
                    spur_graph::mcp::code_read_symbol(&json!({
                        "selector": selector,
                    }))
                    .await,
                )
            }),
    )
    .await;

    for (index, body_result) in body_results {
        let Ok(body) = body_result else {
            continue;
        };
        let Some(source) = body.get("source").and_then(Value::as_str) else {
            continue;
        };
        let Some(evidence) = primary_evidence.get_mut(index) else {
            continue;
        };
        let Some(object) = evidence.as_object_mut() else {
            continue;
        };
        object.insert("source".into(), json!(source));
        if let Some(line_range) = body.get("line_range") {
            object.insert("line_range".into(), line_range.clone());
        }
    }
}

#[cfg(test)]
pub(crate) async fn pack_query_result_v2_with_graph_sections(
    request: &KnowledgeContextPackV2Request,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    graph_sections: GraphReasoningSections,
) -> Value {
    let staleness = PackStaleness::default_for_result(&result);
    pack_query_result_v2_with_graph_sections_and_staleness(
        request,
        result,
        exact_context,
        graph_sections,
        staleness,
    )
    .await
}

pub(crate) async fn pack_query_result_v2_with_graph_sections_and_staleness(
    request: &KnowledgeContextPackV2Request,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    graph_sections: GraphReasoningSections,
    staleness: PackStaleness,
) -> Value {
    let mut pack = pack_query_result_with_exact_context_and_staleness(
        &request.base,
        result,
        exact_context,
        staleness,
    )
    .await;
    insert_v2_sections(&mut pack, graph_sections);
    pack
}

pub(crate) fn insert_v2_sections(pack: &mut Value, sections: GraphReasoningSections) {
    if let Some(object) = pack.as_object_mut() {
        object.insert("graph_paths".into(), Value::Array(sections.graph_paths));
        object.insert(
            "risk_scorecard".into(),
            Value::Array(sections.risk_scorecard),
        );
        object.insert(
            "community_context".into(),
            Value::Array(sections.community_context),
        );
        object.insert(
            "temporal_context".into(),
            Value::Array(sections.temporal_context),
        );
        object.insert("caveats".into(), Value::Array(sections.caveats));
        object.entry("candidates").or_insert_with(|| {
            json!({
                "total": 0,
                "returned_primary": 0,
                "returned_supporting_docs": 0,
                "total_code": 0,
                "total_docs": 0,
            })
        });
    }
}

pub(crate) fn base_pack(
    request: &KnowledgeContextPackRequest,
    graph_content_hash: Option<String>,
    staleness: Value,
) -> Value {
    json!({
        "query": request.query,
        "intent": request.intent.as_str(),
        "scope": request.scope.as_str(),
        "limit": request.limit,
        "include_tests": request.include_tests,
        "max_symbol_bodies": request.max_symbol_bodies,
        "answerable": false,
        "confidence": "low",
        "graph_content_hash": graph_content_hash,
        "staleness": staleness,
        "primary_evidence": [],
        "supporting_docs": [],
        "impact": {
            "summary": "no analyst evidence available",
            "callers_count": null,
            "callees_count": null,
            "popular_sink": null
        },
        "recommended_next_tools": []
    })
}

pub(crate) trait PackErrorExt {
    fn with_error(self, error: Value) -> Value;
}

impl PackErrorExt for Value {
    fn with_error(mut self, error: Value) -> Value {
        if let Some(object) = self.as_object_mut() {
            object.insert("error".into(), error);
        }
        self
    }
}
