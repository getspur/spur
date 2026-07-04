use serde_json::{json, Value};

use crate::KnowledgeQueryResult;

use super::ExactGraphContext;

#[derive(Clone, Debug)]
pub(crate) struct PackStaleness {
    pub(crate) delta_applied: bool,
    pub(crate) algo_as_of: Option<String>,
}

impl PackStaleness {
    pub(crate) fn base_only(algo_as_of: Option<String>) -> Self {
        Self {
            delta_applied: false,
            algo_as_of,
        }
    }

    pub(crate) fn default_for_result(result: &KnowledgeQueryResult) -> Self {
        Self::base_only(result.graph_content_hash.clone())
    }
}

pub(crate) fn staleness_value(
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
    pack_staleness: &PackStaleness,
) -> Value {
    let analyst_hash = result.graph_content_hash.clone();
    let exact_hash = exact_context.graph_content_hash.clone();
    let analyst_matches_exact_graph = analyst_matches_exact_graph(result, exact_context)
        .map(Value::Bool)
        .unwrap_or(Value::Null);

    json!({
        "available": analyst_hash.is_some(),
        "analyst_db": result.db_path.clone(),
        "analyst_graph_content_hash": analyst_hash.clone(),
        "graph_hash_present": result.graph_content_hash.is_some(),
        "exact_graph_hash": exact_hash.clone(),
        "exact_graph_verified": exact_context.graph_content_hash.is_some(),
        "analyst_matches_exact_graph": analyst_matches_exact_graph,
        "response_file_oids_match": exact_context.response_file_oids_match,
        "delta_applied": pack_staleness.delta_applied,
        "algo_as_of": pack_staleness.algo_as_of.clone(),
        "exact_graph_note": "Exact graph tools remain the source-of-truth follow-up for current working tree source and impact."
    })
}

pub(crate) fn analyst_matches_exact_graph(
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
) -> Option<bool> {
    Some(result.graph_content_hash.as_deref()? == exact_context.graph_content_hash.as_deref()?)
}
