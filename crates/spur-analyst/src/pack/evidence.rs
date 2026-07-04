use serde_json::{json, Value};

use crate::KnowledgeCandidate;

use super::{code_next_tools, KnowledgeContextPackRequest, KnowledgeIntent};

pub(crate) const POPULAR_SINK_CALLERS_THRESHOLD: u64 = 30;
pub(crate) const MAX_IMPACT_SYMBOLS: usize = 2;
pub(crate) const MAX_IMPACT_NEIGHBORS: usize = 2;

#[derive(Debug, Clone)]
pub(crate) struct SymbolImpactSummary {
    pub(crate) selector: String,
    pub(crate) callers_count: u64,
    pub(crate) callees_count: u64,
    pub(crate) caller_neighbors: Vec<Value>,
    pub(crate) callee_neighbors: Vec<Value>,
}

pub(crate) fn split_evidence(
    candidates: &[KnowledgeCandidate],
    request: &KnowledgeContextPackRequest,
) -> (Vec<Value>, Vec<Value>) {
    let mut primary = Vec::new();
    let mut docs = Vec::new();
    let max_primary = request.limit as usize;

    for candidate in candidates {
        if !request.include_tests && is_test_file(&candidate.file_path) {
            continue;
        }
        let evidence = evidence_from_candidate(candidate, request.intent);
        if candidate.kind == "doc" {
            docs.push(evidence);
        } else if primary.len() < max_primary {
            primary.push(evidence);
        } else {
            docs.push(evidence);
        }
    }

    (primary, docs)
}

fn evidence_from_candidate(candidate: &KnowledgeCandidate, intent: KnowledgeIntent) -> Value {
    let is_code = candidate.kind == "code" || candidate.kind == "symbol";
    let next = if is_code {
        code_next_tools(intent)
    } else if let Some(root) = candidate.stable_symbol_id.as_deref() {
        vec![json!({ "tool": "doc_navigate", "root": root })]
    } else {
        vec![json!({ "tool": "code_semantic_search", "query": candidate.title })]
    };
    let stable_symbol_id = candidate.stable_symbol_id.as_ref().map(|id| {
        if is_code {
            normalized_code_selector(id)
        } else {
            id.clone()
        }
    });
    json!({
        "kind": if is_code { "symbol" } else { "doc" },
        "title": candidate.title,
        "file": candidate.file_path,
        "stable_symbol_id": stable_symbol_id,
        "symbol_kind": candidate.symbol_kind,
        "score": candidate.score,
        "signal": candidate.signal,
        "neighbor_kind": candidate.neighbor_kind,
        "edge_bind_method": candidate.edge_bind_method,
        "grounding": candidate.grounding,
        "why_relevant": build_why_relevant(candidate),
        "next": next
    })
}

fn build_why_relevant(candidate: &KnowledgeCandidate) -> String {
    let mut parts = vec![format!(
        "{} {:.1}",
        grounding_score_prefix(&candidate.grounding),
        candidate.score
    )];
    if let Some(signal) = &candidate.signal {
        parts.push(signal.clone());
    }
    if let Some(kind) = &candidate.symbol_kind {
        parts.push(format!("kind={kind}"));
    }
    parts.push(format!("grounding={}", candidate.grounding));
    parts.join(", ")
}

fn grounding_score_prefix(grounding: &str) -> &str {
    match grounding {
        "bm25-code" | "bm25-doc" => "BM25",
        "bm25-graph" => "BM25+graph",
        "bm25-graph-expanded" => "graph",
        "ann-embedding" => "ANN",
        _ if grounding.starts_with("bm25-") => "BM25",
        _ => grounding,
    }
}

pub(crate) fn normalized_code_selector(stable_symbol_id: &str) -> String {
    format!("graph://symbol/{}", raw_stable_symbol_id(stable_symbol_id))
}

pub(crate) fn raw_stable_symbol_id(stable_symbol_id: &str) -> &str {
    stable_symbol_id
        .strip_prefix("graph://symbol/")
        .unwrap_or(stable_symbol_id)
}

pub(crate) fn is_test_file(file_path: &str) -> bool {
    file_path.contains("/tests/")
        || file_path.ends_with("_test.rs")
        || file_path.ends_with("_tests.rs")
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
