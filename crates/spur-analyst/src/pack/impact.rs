use futures::future::join_all;
use serde_json::{json, Value};

use crate::{KnowledgeCandidate, KnowledgeQueryResult};

use super::evidence::is_test_file;
use super::request::KnowledgeContextPackRequest;

pub(crate) const POPULAR_SINK_CALLERS_THRESHOLD: u64 = 30;
pub(crate) const MAX_IMPACT_SYMBOLS: usize = 2;
pub(crate) const MAX_IMPACT_NEIGHBORS: usize = 2;

#[derive(Debug, Clone, Default)]
pub(crate) struct ExactGraphContext {
    pub(crate) graph_content_hash: Option<String>,
    pub(crate) response_file_oids_match: Option<bool>,
    pub(crate) impacts: Vec<Option<SymbolImpactSummary>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolImpactSummary {
    pub(crate) selector: String,
    pub(crate) callers_count: u64,
    pub(crate) callees_count: u64,
    pub(crate) caller_neighbors: Vec<Value>,
    pub(crate) callee_neighbors: Vec<Value>,
}

pub(crate) async fn exact_graph_context_for_result(
    request: &KnowledgeContextPackRequest,
    result: &KnowledgeQueryResult,
) -> ExactGraphContext {
    let selectors = top_n_code_selectors(&result.candidates, request);
    let Some(first_selector) = selectors.first() else {
        return ExactGraphContext::default();
    };

    let symbol_info = spur_graph::mcp::code_symbol_info_rebuild_aware(&json!({
        "selector": first_selector,
    }))
    .await;
    let mut context = match symbol_info {
        Ok(body) => ExactGraphContext {
            graph_content_hash: body
                .get("graph_content_hash")
                .and_then(Value::as_str)
                .map(str::to_string),
            response_file_oids_match: body
                .get("response_file_oids_match")
                .and_then(Value::as_bool),
            impacts: Vec::new(),
        },
        Err(_) => return ExactGraphContext::default(),
    };

    context.impacts = join_all(
        selectors
            .iter()
            .map(|selector| impact_summary_for_selector(selector)),
    )
    .await;
    context
}

async fn impact_summary_for_selector(selector: &str) -> Option<SymbolImpactSummary> {
    let callers_args = json!({
        "selector": selector,
        "include_unresolved": true,
    });
    let callees_args = json!({
        "selector": selector,
        "include_unresolved": true,
    });
    let (callers, callees) = tokio::join!(
        spur_graph::mcp::code_callers(&callers_args),
        spur_graph::mcp::code_callees(&callees_args)
    );
    let callers = callers.ok()?;
    let callees = callees.ok()?;

    let callers_count = array_len(&callers, "callers")?;
    let callees_count = array_len(&callees, "callees")?;
    let popular_sink = callers_count > POPULAR_SINK_CALLERS_THRESHOLD;

    Some(SymbolImpactSummary {
        selector: selector.to_owned(),
        callers_count,
        callees_count,
        caller_neighbors: representative_neighbors(&callers, "callers", popular_sink),
        callee_neighbors: representative_neighbors(&callees, "callees", false),
    })
}

fn array_len(body: &Value, field: &str) -> Option<u64> {
    body.get(field)
        .and_then(Value::as_array)
        .map(|values| values.len() as u64)
}

fn representative_neighbors(body: &Value, field: &str, suppress: bool) -> Vec<Value> {
    if suppress {
        return Vec::new();
    }
    body.get(field)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .take(MAX_IMPACT_NEIGHBORS)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn top_n_code_selectors(
    candidates: &[KnowledgeCandidate],
    request: &KnowledgeContextPackRequest,
) -> Vec<String> {
    candidates
        .iter()
        .filter(|candidate| request.include_tests || !is_test_file(&candidate.file_path))
        .filter(|candidate| candidate.kind == "code" || candidate.kind == "symbol")
        .filter_map(|candidate| candidate.stable_symbol_id.as_deref())
        .map(normalized_code_selector)
        .take(MAX_IMPACT_SYMBOLS)
        .collect()
}

pub(crate) fn normalized_code_selector(stable_symbol_id: &str) -> String {
    format!("graph://symbol/{}", raw_stable_symbol_id(stable_symbol_id))
}

pub(crate) fn raw_stable_symbol_id(stable_symbol_id: &str) -> &str {
    stable_symbol_id
        .strip_prefix("graph://symbol/")
        .unwrap_or(stable_symbol_id)
}

pub(crate) fn primary_evidence_with_impact(
    mut primary_evidence: Vec<Value>,
    impacts: &[Option<SymbolImpactSummary>],
) -> Vec<Value> {
    for impact in impacts.iter().flatten() {
        if let Some(evidence) = primary_evidence.iter_mut().find(|evidence| {
            evidence.get("stable_symbol_id").and_then(Value::as_str)
                == Some(impact.selector.as_str())
        }) {
            if let Some(object) = evidence.as_object_mut() {
                object.insert("impact".into(), compact_impact_value(impact));
            }
        }
    }
    primary_evidence
}

fn compact_impact_value(impact: &SymbolImpactSummary) -> Value {
    json!({
        "callers_count": impact.callers_count,
        "callees_count": impact.callees_count,
        "popular_sink": impact.callers_count > POPULAR_SINK_CALLERS_THRESHOLD,
    })
}

pub(crate) fn aggregate_impact_value(impacts: &[Option<SymbolImpactSummary>]) -> Value {
    let impacts: Vec<&SymbolImpactSummary> = impacts.iter().filter_map(Option::as_ref).collect();
    if impacts.is_empty() {
        return json!({
            "summary": "impact counts are deferred to exact graph follow-up tools",
            "callers_count": null,
            "callees_count": null,
            "popular_sink": null
        });
    }

    let callers_count = impacts
        .iter()
        .map(|impact| impact.callers_count)
        .sum::<u64>();
    let callees_count = impacts
        .iter()
        .map(|impact| impact.callees_count)
        .sum::<u64>();
    let popular_sink = impacts
        .iter()
        .any(|impact| impact.callers_count > POPULAR_SINK_CALLERS_THRESHOLD);
    let caller_neighbors = aggregate_neighbors(
        impacts
            .iter()
            .flat_map(|impact| impact.caller_neighbors.iter()),
        popular_sink,
    );
    let callee_neighbors = aggregate_neighbors(
        impacts
            .iter()
            .flat_map(|impact| impact.callee_neighbors.iter()),
        popular_sink,
    );

    json!({
        "summary": if popular_sink {
            "popular sink counted but not expanded"
        } else {
            "bounded exact graph impact summary"
        },
        "callers_count": callers_count,
        "callees_count": callees_count,
        "popular_sink": popular_sink,
        "caller_neighbors": caller_neighbors,
        "callee_neighbors": callee_neighbors
    })
}

fn aggregate_neighbors<'a>(
    neighbors: impl Iterator<Item = &'a Value>,
    suppress: bool,
) -> Vec<Value> {
    if suppress {
        Vec::new()
    } else {
        neighbors.take(MAX_IMPACT_NEIGHBORS).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn aggregate_impact_suppresses_neighbors_when_any_symbol_is_popular_sink() {
        let impact = aggregate_impact_value(&[
            Some(SymbolImpactSummary {
                selector: normalized_code_selector("sym-one"),
                callers_count: 2,
                callees_count: 3,
                caller_neighbors: vec![json!({ "title": "caller" })],
                callee_neighbors: vec![json!({ "title": "callee" })],
            }),
            Some(SymbolImpactSummary {
                selector: normalized_code_selector("graph://symbol/sym-two"),
                callers_count: POPULAR_SINK_CALLERS_THRESHOLD + 1,
                callees_count: 4,
                caller_neighbors: vec![json!({ "title": "sink-caller" })],
                callee_neighbors: vec![json!({ "title": "sink-callee" })],
            }),
        ]);

        assert_eq!(impact["callers_count"], POPULAR_SINK_CALLERS_THRESHOLD + 3);
        assert_eq!(impact["callees_count"], 7);
        assert_eq!(impact["popular_sink"], true);
        assert_eq!(impact["caller_neighbors"].as_array().unwrap().len(), 0);
        assert_eq!(impact["callee_neighbors"].as_array().unwrap().len(), 0);
    }
}
