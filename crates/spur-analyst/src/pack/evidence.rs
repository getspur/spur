use serde_json::{json, Value};

use crate::KnowledgeCandidate;

use super::impact::normalized_code_selector;
use super::{code_next_tools, KnowledgeContextPackRequest, KnowledgeIntent};

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

pub(crate) fn is_test_file(file_path: &str) -> bool {
    file_path.contains("/tests/")
        || file_path.ends_with("_test.rs")
        || file_path.ends_with("_tests.rs")
}
