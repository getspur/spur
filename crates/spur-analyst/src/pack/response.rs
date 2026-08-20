use futures::future::join_all;
use serde_json::{json, Value};

use crate::search::hybrid::evidence_confidence;
use crate::KnowledgeQueryResult;

use super::evidence::split_evidence;
use super::graph_reasoning::{insert_v2_sections, GraphReasoningSections};
use super::impact::{
    aggregate_impact_value, primary_evidence_with_impact, raw_stable_symbol_id, ExactGraphContext,
};
use super::next_tools::recommended_next_tools;
use super::staleness::{staleness_value, PackStaleness};
use super::{KnowledgeContextPackRequest, KnowledgeContextPackV2Request};

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
    apply_v2_response_format(request, &mut pack, graph_sections);
    pack
}

pub(crate) fn apply_v2_response_format(
    request: &KnowledgeContextPackV2Request,
    pack: &mut Value,
    graph_sections: GraphReasoningSections,
) {
    if request.response_format.is_compact() {
        apply_compact_pack(pack, graph_sections);
    } else {
        insert_v2_sections(pack, graph_sections);
    }
}

fn apply_compact_pack(pack: &mut Value, sections: GraphReasoningSections) {
    compact_primary_evidence(pack);
    fold_graph_sections_into_primary(pack, &sections);
    compact_impact(pack);
    compact_staleness(pack);
    omit_empty_array(pack, "supporting_docs");
    insert_nonempty_array(pack, "graph_paths", sections.graph_paths);
    insert_nonempty_array(pack, "caveats", sections.caveats);
}

fn compact_primary_evidence(pack: &mut Value) {
    let Some(Value::Array(evidence)) = pack.get_mut("primary_evidence") else {
        return;
    };
    for row in evidence {
        let Some(object) = row.as_object_mut() else {
            continue;
        };
        object.remove("why_relevant");
        object.remove("next");
        object.remove("neighbor_kind");
        object.remove("edge_bind_method");
    }
}

fn fold_graph_sections_into_primary(pack: &mut Value, sections: &GraphReasoningSections) {
    let Some(Value::Array(evidence)) = pack.get_mut("primary_evidence") else {
        return;
    };
    for row in evidence {
        let Some(id) = row
            .get("stable_symbol_id")
            .and_then(Value::as_str)
            .map(raw_stable_symbol_id)
            .map(str::to_owned)
        else {
            continue;
        };
        let Some(object) = row.as_object_mut() else {
            continue;
        };
        if let Some(risk) = find_section_row(&sections.risk_scorecard, &id) {
            copy_if_present(object, risk, "posture");
            copy_if_present(object, risk, "churn_90d");
            copy_if_present(object, risk, "last_touched");
            copy_if_present(object, risk, "blast_radius_score");
        }
        if let Some(community) = find_section_row(&sections.community_context, &id) {
            copy_if_present(object, community, "community_id");
            copy_if_present(object, community, "component_size");
        }
        if let Some(temporal) = find_section_row(&sections.temporal_context, &id) {
            copy_if_present(object, temporal, "churn_90d");
            copy_if_present(object, temporal, "last_touched");
            copy_if_present(object, temporal, "posture");
        }
    }
}

fn find_section_row<'a>(rows: &'a [Value], stable_symbol_id: &str) -> Option<&'a Value> {
    rows.iter().find(|row| {
        row.get("stable_symbol_id")
            .and_then(Value::as_str)
            .map(raw_stable_symbol_id)
            == Some(stable_symbol_id)
    })
}

fn copy_if_present(dest: &mut serde_json::Map<String, Value>, source: &Value, key: &str) {
    if let Some(value) = source.get(key) {
        if !value.is_null() {
            dest.insert(key.to_owned(), value.clone());
        }
    }
}

fn compact_impact(pack: &mut Value) {
    let Some(object) = pack.get_mut("impact").and_then(Value::as_object_mut) else {
        return;
    };
    object.remove("caller_neighbors");
    object.remove("callee_neighbors");
}

fn compact_staleness(pack: &mut Value) {
    let Some(staleness) = pack.get("staleness").cloned() else {
        return;
    };
    let mut compact = serde_json::Map::new();
    if staleness.get("available") == Some(&Value::Bool(false)) {
        compact.insert("available".into(), json!(false));
    }
    if staleness.get("exact_graph_verified") == Some(&Value::Bool(false)) {
        compact.insert("exact_graph_verified".into(), json!(false));
    }
    if staleness.get("delta_applied") == Some(&Value::Bool(true)) {
        compact.insert("delta_applied".into(), json!(true));
        if let Some(as_of) = staleness.get("algo_as_of") {
            compact.insert("algo_as_of".into(), as_of.clone());
        }
    }
    if staleness.get("analyst_matches_exact_graph") == Some(&Value::Bool(false)) {
        for key in [
            "analyst_matches_exact_graph",
            "analyst_graph_content_hash",
            "exact_graph_hash",
        ] {
            if let Some(value) = staleness.get(key) {
                compact.insert(key.to_owned(), value.clone());
            }
        }
    }
    if staleness.get("response_file_oids_match") == Some(&Value::Bool(false)) {
        compact.insert("response_file_oids_match".into(), json!(false));
    }
    let Some(object) = pack.as_object_mut() else {
        return;
    };
    if compact.is_empty() {
        object.remove("staleness");
    } else {
        object.insert("staleness".into(), Value::Object(compact));
    }
}

fn omit_empty_array(pack: &mut Value, key: &str) {
    let empty = pack
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if empty {
        if let Some(object) = pack.as_object_mut() {
            object.remove(key);
        }
    }
}

fn insert_nonempty_array(pack: &mut Value, key: &str, values: Vec<Value>) {
    if values.is_empty() {
        return;
    }
    if let Some(object) = pack.as_object_mut() {
        object.insert(key.to_owned(), Value::Array(values));
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

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
