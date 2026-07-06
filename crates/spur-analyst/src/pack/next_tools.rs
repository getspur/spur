use serde_json::{json, Value};

use super::KnowledgeIntent;

pub(crate) fn recommended_next_tools(
    intent: KnowledgeIntent,
    primary_evidence: &[Value],
    supporting_docs: &[Value],
) -> Vec<Value> {
    let top_symbol = primary_evidence
        .iter()
        .find_map(|evidence| evidence.get("stable_symbol_id").and_then(Value::as_str));
    let top_file = primary_evidence
        .iter()
        .find_map(|evidence| evidence.get("file").and_then(Value::as_str));
    let top_doc_root = supporting_docs
        .iter()
        .chain(primary_evidence.iter())
        .filter(|evidence| evidence.get("kind").and_then(Value::as_str) == Some("doc"))
        .find_map(|evidence| evidence.get("stable_symbol_id").and_then(Value::as_str));

    match (intent, top_symbol) {
        (KnowledgeIntent::Change, Some(selector)) => change_next_tools(selector),
        (KnowledgeIntent::Debug, Some(selector)) => debug_next_tools(selector),
        (KnowledgeIntent::Review, Some(selector)) => review_next_tools(selector),
        (KnowledgeIntent::Plan, Some(_)) => plan_next_tools(top_doc_root, top_file),
        (KnowledgeIntent::Explain, Some(selector)) => explain_next_tools(selector),
        _ => fallback_next_tools(),
    }
}

fn change_next_tools(selector: &str) -> Vec<Value> {
    vec![
        json!({ "tool": "code_callers", "selector": selector, "reason": "Find direct change impact before editing." }),
        json!({ "tool": "code_callees", "selector": selector, "reason": "Trace direct dependencies for the selected symbol." }),
        json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body." }),
    ]
}

fn debug_next_tools(selector: &str) -> Vec<Value> {
    vec![
        json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body before debugging." }),
        json!({ "tool": "code_symbol_history", "selector": selector, "reason": "Inspect recent edits that may explain the failure." }),
        json!({ "tool": "code_subgraph", "selector": selector, "radius": 2, "reason": "Map nearby dependencies and callers around the failing symbol." }),
    ]
}

fn review_next_tools(selector: &str) -> Vec<Value> {
    vec![
        json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body for review." }),
        json!({ "tool": "code_callers", "selector": selector, "reason": "Verify behavioral impact from direct callers." }),
    ]
}

fn plan_next_tools(top_doc_root: Option<&str>, top_file: Option<&str>) -> Vec<Value> {
    let mut tools = Vec::new();
    if let Some(root) = top_doc_root {
        tools.push(json!({
            "tool": "doc_navigate",
            "root": root,
            "reason": "Start planning from the most relevant documentation evidence."
        }));
    }
    if let Some(file) = top_file {
        tools.push(json!({
            "tool": "code_file_symbols",
            "file": file,
            "reason": "Survey symbols in the relevant file before planning edits."
        }));
    }
    tools
}

fn explain_next_tools(selector: &str) -> Vec<Value> {
    vec![json!({
        "tool": "code_read_symbol",
        "selector": selector,
        "reason": "Read exact current symbol body for grounded follow-up."
    })]
}

fn fallback_next_tools() -> Vec<Value> {
    vec![json!({
        "tool": "code_semantic_search",
        "query": "",
        "reason": "No symbol evidence was available; broaden retrieval with semantic search."
    })]
}

pub(crate) fn code_next_tools(intent: KnowledgeIntent) -> Vec<Value> {
    match intent {
        KnowledgeIntent::Change => vec![
            json!({ "tool": "code_callers" }),
            json!({ "tool": "code_callees" }),
            json!({ "tool": "code_read_symbol" }),
        ],
        KnowledgeIntent::Debug => vec![
            json!({ "tool": "code_read_symbol" }),
            json!({ "tool": "code_symbol_history" }),
        ],
        KnowledgeIntent::Review => vec![
            json!({ "tool": "code_read_symbol" }),
            json!({ "tool": "code_callers" }),
        ],
        KnowledgeIntent::Plan => vec![
            json!({ "tool": "code_read_symbol" }),
            json!({ "tool": "code_file_symbols" }),
        ],
        KnowledgeIntent::Explain => vec![json!({ "tool": "code_read_symbol" })],
    }
}
