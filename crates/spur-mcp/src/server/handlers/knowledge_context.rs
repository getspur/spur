use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spur_analyst::{
    query_context_candidates, KnowledgeCandidate, KnowledgeQueryOptions, KnowledgeQueryResult,
    KnowledgeSearchScope,
};
use spur_graph::resolve_worktree_root_from;

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

const POPULAR_SINK_CALLERS_THRESHOLD: u64 = 30;
const MAX_IMPACT_NEIGHBORS: usize = 3;

impl McpCallbackServer {
    pub(crate) async fn handle_knowledge_context_pack(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        match knowledge_context_pack(&args).await {
            Ok(result) => {
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(McpHandlerError::InvalidParams(error)) => {
                JsonRpcResponse::invalid_params(id, error)
            }
            Err(error) => JsonRpcResponse::internal_error(id, error.to_string()),
        }
    }
}

pub(crate) async fn knowledge_context_pack(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackRequest::parse(args)?;
    let db_path = analyst_db_path()?;
    if !db_path.exists() {
        return Ok(unavailable_pack(&request, &db_path));
    }

    let query_result = query_context_candidates(
        &db_path,
        &request.query,
        request.scope.as_analyst_scope(),
        KnowledgeQueryOptions {
            limit: request.limit as usize,
        },
    )
    .map_err(|error| {
        McpHandlerError::Internal(format!(
            "knowledge_context_pack failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;

    let exact_context = exact_graph_context_for_result(&request, &query_result).await;
    Ok(pack_query_result_with_exact_context(
        &request,
        query_result,
        exact_context,
    ))
}

struct KnowledgeContextPackRequest {
    query: String,
    intent: KnowledgeIntent,
    scope: KnowledgeScope,
    limit: u64,
    include_tests: bool,
    max_symbol_bodies: u64,
}

impl KnowledgeContextPackRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "knowledge_context_pack requires non-empty string field 'query'".into(),
                )
            })?
            .to_string();
        let intent = KnowledgeIntent::parse(parse_enum(
            args,
            "intent",
            &["explain", "change", "review", "debug", "plan"],
            "explain",
        )?);
        let scope = KnowledgeScope::parse(parse_enum(
            args,
            "scope",
            &["all", "docs", "code", "graph"],
            "all",
        )?);
        let limit = parse_u64(args, "limit", 8, 1, 20)?;
        let include_tests = args
            .get("include_tests")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "knowledge_context_pack field 'include_tests' must be a boolean".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(true);
        let max_symbol_bodies = parse_u64(args, "max_symbol_bodies", 3, 0, 5)?;

        Ok(Self {
            query,
            intent,
            scope,
            limit,
            include_tests,
            max_symbol_bodies,
        })
    }
}

#[derive(Clone, Copy)]
enum KnowledgeIntent {
    Explain,
    Change,
    Review,
    Debug,
    Plan,
}

impl KnowledgeIntent {
    fn parse(value: String) -> Self {
        match value.as_str() {
            "change" => Self::Change,
            "review" => Self::Review,
            "debug" => Self::Debug,
            "plan" => Self::Plan,
            _ => Self::Explain,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Change => "change",
            Self::Review => "review",
            Self::Debug => "debug",
            Self::Plan => "plan",
        }
    }
}

#[derive(Clone, Copy)]
enum KnowledgeScope {
    All,
    Docs,
    Code,
    Graph,
}

impl KnowledgeScope {
    fn parse(value: String) -> Self {
        match value.as_str() {
            "docs" => Self::Docs,
            "code" => Self::Code,
            "graph" => Self::Graph,
            _ => Self::All,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Docs => "docs",
            Self::Code => "code",
            Self::Graph => "graph",
        }
    }

    fn as_analyst_scope(self) -> KnowledgeSearchScope {
        match self {
            Self::All => KnowledgeSearchScope::All,
            Self::Docs => KnowledgeSearchScope::Docs,
            Self::Code => KnowledgeSearchScope::Code,
            Self::Graph => KnowledgeSearchScope::Graph,
        }
    }
}

fn parse_enum(
    args: &Value,
    field: &str,
    allowed: &[&str],
    default: &str,
) -> Result<String, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default.to_string());
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be a string"
        ))
    })?;
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be one of {}",
            allowed.join("|")
        )))
    }
}

fn parse_u64(
    args: &Value,
    field: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default);
    };
    let value = value.as_u64().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be an integer"
        ))
    })?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be between {min} and {max}"
        )))
    }
}

fn analyst_db_path() -> Result<PathBuf, McpHandlerError> {
    Ok(current_repo_root()?.join(".spur").join("analyst.duckdb"))
}

fn current_repo_root() -> Result<PathBuf, McpHandlerError> {
    if let Some(worktree) = super::code_graph::scoped_worktree_root() {
        return Ok(worktree);
    }
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}

fn unavailable_pack(request: &KnowledgeContextPackRequest, db_path: &Path) -> Value {
    base_pack(
        request,
        None,
        json!({ "available": false, "reason": "analyst_db_missing" }),
    )
    .with_error(json!({
        "code": "analyst_unavailable",
        "message": format!("analyst DB not found at {}", db_path.display()),
        "db_path": db_path.display().to_string()
    }))
}

#[cfg(test)]
fn pack_query_result(request: &KnowledgeContextPackRequest, result: KnowledgeQueryResult) -> Value {
    pack_query_result_with_exact_context(request, result, ExactGraphContext::default())
}

#[derive(Debug, Clone, Default)]
struct ExactGraphContext {
    graph_content_hash: Option<String>,
    response_file_oids_match: Option<bool>,
    impact: Option<SymbolImpactSummary>,
}

#[derive(Debug, Clone)]
struct SymbolImpactSummary {
    selector: String,
    callers_count: u64,
    callees_count: u64,
    caller_neighbors: Vec<Value>,
    callee_neighbors: Vec<Value>,
}

async fn exact_graph_context_for_result(
    request: &KnowledgeContextPackRequest,
    result: &KnowledgeQueryResult,
) -> ExactGraphContext {
    let Some(selector) = top_code_selector(&result.candidates, request) else {
        return ExactGraphContext::default();
    };

    let symbol_info = super::code_graph::code_symbol_info(&json!({
        "selector": selector,
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
            impact: None,
        },
        Err(_) => return ExactGraphContext::default(),
    };

    context.impact = impact_summary_for_selector(&selector).await;
    context
}

async fn impact_summary_for_selector(selector: &str) -> Option<SymbolImpactSummary> {
    let callers = super::code_graph::code_callers(&json!({
        "selector": selector,
        "include_unresolved": true,
    }))
    .await
    .ok()?;
    let callees = super::code_graph::code_callees(&json!({
        "selector": selector,
        "include_unresolved": true,
    }))
    .await
    .ok()?;

    let callers_count = array_len(&callers, "callers")?;
    let callees_count = array_len(&callees, "callees")?;
    let popular_sink = callers_count > POPULAR_SINK_CALLERS_THRESHOLD;

    Some(SymbolImpactSummary {
        selector: selector.to_string(),
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

fn top_code_selector(
    candidates: &[KnowledgeCandidate],
    request: &KnowledgeContextPackRequest,
) -> Option<String> {
    candidates
        .iter()
        .filter(|candidate| request.include_tests || !is_test_file(&candidate.file_path))
        .filter(|candidate| candidate.kind == "code" || candidate.kind == "symbol")
        .filter_map(|candidate| candidate.stable_symbol_id.as_deref())
        .map(normalized_code_selector)
        .next()
}

fn normalized_code_selector(stable_symbol_id: &str) -> String {
    format!(
        "graph://symbol/{}",
        stable_symbol_id
            .strip_prefix("graph://symbol/")
            .unwrap_or(stable_symbol_id)
    )
}

fn pack_query_result_with_exact_context(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
) -> Value {
    let (primary_evidence, supporting_docs) = split_evidence(&result.candidates, request);
    let recommended_next_tools = recommended_next_tools(request.intent, &primary_evidence);
    let answerable = !primary_evidence.is_empty() || !supporting_docs.is_empty();
    let confidence = if answerable { "medium" } else { "low" };
    let impact = impact_value(exact_context.impact.as_ref());
    let staleness = staleness_value(&result, &exact_context);
    let mut pack = base_pack(request, result.graph_content_hash.clone(), staleness);

    if let Some(object) = pack.as_object_mut() {
        object.insert("answerable".into(), json!(answerable));
        object.insert("confidence".into(), json!(confidence));
        object.insert(
            "primary_evidence".into(),
            Value::Array(primary_evidence_with_impact(
                primary_evidence,
                exact_context.impact.as_ref(),
            )),
        );
        object.insert("supporting_docs".into(), Value::Array(supporting_docs));
        object.insert("impact".into(), impact);
        object.insert(
            "recommended_next_tools".into(),
            Value::Array(recommended_next_tools),
        );
    }
    pack
}

fn staleness_value(result: &KnowledgeQueryResult, exact_context: &ExactGraphContext) -> Value {
    let analyst_hash = result.graph_content_hash.clone();
    let exact_hash = exact_context.graph_content_hash.clone();
    let analyst_matches_exact_graph = match (analyst_hash.as_deref(), exact_hash.as_deref()) {
        (Some(analyst), Some(exact)) => Value::Bool(analyst == exact),
        _ => Value::Null,
    };

    json!({
        "available": analyst_hash.is_some(),
        "analyst_db": result.db_path.clone(),
        "analyst_graph_content_hash": analyst_hash.clone(),
        "graph_hash_present": result.graph_content_hash.is_some(),
        "exact_graph_hash": exact_hash.clone(),
        "exact_graph_verified": exact_context.graph_content_hash.is_some(),
        "analyst_matches_exact_graph": analyst_matches_exact_graph,
        "response_file_oids_match": exact_context.response_file_oids_match,
        "exact_graph_note": "Exact graph tools remain the source-of-truth follow-up for current working tree source and impact."
    })
}

fn primary_evidence_with_impact(
    mut primary_evidence: Vec<Value>,
    impact: Option<&SymbolImpactSummary>,
) -> Vec<Value> {
    let Some(impact) = impact else {
        return primary_evidence;
    };
    if let Some(evidence) = primary_evidence.iter_mut().find(|evidence| {
        evidence.get("stable_symbol_id").and_then(Value::as_str) == Some(impact.selector.as_str())
    }) {
        if let Some(object) = evidence.as_object_mut() {
            object.insert("impact".into(), impact_value(Some(impact)));
        }
    }
    primary_evidence
}

fn impact_value(impact: Option<&SymbolImpactSummary>) -> Value {
    match impact {
        Some(impact) => {
            let popular_sink = impact.callers_count > POPULAR_SINK_CALLERS_THRESHOLD;
            json!({
                "summary": if popular_sink {
                    "popular sink counted but not expanded"
                } else {
                    "bounded exact graph impact summary"
                },
                "selector": impact.selector.clone(),
                "callers_count": impact.callers_count,
                "callees_count": impact.callees_count,
                "popular_sink": popular_sink,
                "caller_neighbors": if popular_sink {
                    Vec::<Value>::new()
                } else {
                    impact.caller_neighbors.clone()
                },
                "callee_neighbors": if popular_sink {
                    Vec::<Value>::new()
                } else {
                    impact.callee_neighbors.clone()
                }
            })
        }
        None => json!({
            "summary": "impact counts are deferred to exact graph follow-up tools",
            "callers_count": null,
            "callees_count": null,
            "popular_sink": null
        }),
    }
}

fn base_pack(
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

trait PackErrorExt {
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

fn split_evidence(
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
        "why_relevant": "Matched analyst candidate for query",
        "next": next
    })
}

fn recommended_next_tools(intent: KnowledgeIntent, primary_evidence: &[Value]) -> Vec<Value> {
    let top_symbol = primary_evidence
        .iter()
        .find_map(|evidence| evidence.get("stable_symbol_id").and_then(Value::as_str));

    match (intent, top_symbol) {
        (KnowledgeIntent::Change, Some(selector)) => vec![
            json!({ "tool": "code_callers", "selector": selector, "reason": "Find direct change impact before editing." }),
            json!({ "tool": "code_callees", "selector": selector, "reason": "Trace direct dependencies for the selected symbol." }),
            json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body." }),
        ],
        (_, Some(selector)) => vec![json!({
            "tool": "code_read_symbol",
            "selector": selector,
            "reason": "Read exact current symbol body for grounded follow-up."
        })],
        _ => vec![json!({
            "tool": "code_semantic_search",
            "query": "",
            "reason": "No symbol evidence was available; broaden retrieval with semantic search."
        })],
    }
}

fn code_next_tools(intent: KnowledgeIntent) -> Vec<Value> {
    match intent {
        KnowledgeIntent::Change => vec![
            json!({ "tool": "code_callers" }),
            json!({ "tool": "code_callees" }),
            json!({ "tool": "code_read_symbol" }),
        ],
        _ => vec![json!({ "tool": "code_read_symbol" })],
    }
}

fn is_test_file(file_path: &str) -> bool {
    file_path.contains("/tests/")
        || file_path.ends_with("_test.rs")
        || file_path.ends_with("_tests.rs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn knowledge_context_pack_missing_analyst_db_returns_structured_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".spur")).expect("create .spur");

        let result =
            super::super::code_graph::with_worktree_root_for_request(repo.clone(), async {
                knowledge_context_pack(&json!({ "query": "semantic search" })).await
            })
            .await
            .expect("structured unavailable response");

        assert_eq!(result["query"], "semantic search");
        assert_eq!(result["intent"], "explain");
        assert_eq!(result["scope"], "all");
        assert_eq!(result["answerable"], false);
        assert_eq!(result["confidence"], "low");
        assert_eq!(result["graph_content_hash"], Value::Null);
        assert_eq!(result["staleness"]["available"], false);
        assert_eq!(result["error"]["code"], "analyst_unavailable");
        assert!(result["error"]["message"]
            .as_str()
            .expect("error message")
            .contains(".spur/analyst.duckdb"));
    }

    #[tokio::test]
    async fn knowledge_context_pack_rejects_empty_query() {
        let error = knowledge_context_pack(&json!({ "query": "   " }))
            .await
            .expect_err("empty query must be rejected");
        assert_eq!(error.json_rpc_code(), -32602);
        assert!(
            error.to_string().contains("non-empty string field 'query'"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn knowledge_context_pack_returns_grounded_evidence_and_followups() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search",
            "intent": "change",
            "scope": "code",
            "limit": 4,
            "include_tests": false,
            "max_symbol_bodies": 1
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "query_context_candidates".into(),
                    file_path: "crates/spur-analyst/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-1".into()),
                    symbol_kind: Some("function".into()),
                    score: 7.5,
                    signal: Some("stable".into()),
                    neighbor_kind: Some("primary".into()),
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
                KnowledgeCandidate {
                    kind: "doc".into(),
                    title: "Knowledge Context API".into(),
                    file_path: "docs/context.md".into(),
                    stable_symbol_id: Some("doc-1".into()),
                    symbol_kind: Some("section".into()),
                    score: 3.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-doc".into(),
                },
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "test helper".into(),
                    file_path: "crates/spur-mcp/tests/context_tests.rs".into(),
                    stable_symbol_id: Some("test-sym".into()),
                    symbol_kind: Some("function".into()),
                    score: 1.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
            ],
        };

        let pack = pack_query_result(&request, result);

        assert_eq!(pack["query"], "semantic search");
        assert_eq!(pack["intent"], "change");
        assert_eq!(pack["scope"], "code");
        assert_eq!(pack["answerable"], true);
        assert_eq!(pack["confidence"], "medium");
        assert_eq!(pack["graph_content_hash"], "fixture-hash");
        assert_eq!(pack["staleness"]["graph_hash_present"], true);
        assert_eq!(pack["primary_evidence"][0]["kind"], "symbol");
        assert_eq!(
            pack["primary_evidence"][0]["stable_symbol_id"],
            "graph://symbol/sym-1"
        );
        assert_eq!(pack["supporting_docs"][0]["kind"], "doc");
        assert_eq!(pack["supporting_docs"][0]["stable_symbol_id"], "doc-1");
        assert_eq!(
            pack["supporting_docs"][0]["next"][0]["tool"],
            "doc_navigate"
        );
        assert_eq!(pack["recommended_next_tools"][0]["tool"], "code_callers");
        assert_eq!(
            pack["recommended_next_tools"][0]["selector"],
            "graph://symbol/sym-1"
        );
        assert_eq!(pack["impact"]["popular_sink"], Value::Null);
        assert_eq!(
            pack["primary_evidence"]
                .as_array()
                .expect("primary evidence")
                .len(),
            1,
            "include_tests=false should filter test evidence"
        );
    }

    #[test]
    fn knowledge_context_pack_includes_bounded_impact_for_top_code_evidence() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "change impact",
            "intent": "change",
            "scope": "code"
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![KnowledgeCandidate {
                kind: "code".into(),
                title: "top_symbol".into(),
                file_path: "crates/spur-mcp/src/lib.rs".into(),
                stable_symbol_id: Some("sym-top".into()),
                symbol_kind: Some("function".into()),
                score: 9.0,
                signal: Some("stable".into()),
                neighbor_kind: Some("primary".into()),
                edge_bind_method: None,
                grounding: "bm25-code".into(),
            }],
        };

        let pack = pack_query_result_with_exact_context(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("fixture-hash".into()),
                response_file_oids_match: Some(true),
                impact: Some(SymbolImpactSummary {
                    selector: "graph://symbol/sym-top".into(),
                    callers_count: 4,
                    callees_count: 2,
                    caller_neighbors: vec![json!({ "title": "caller_a" })],
                    callee_neighbors: vec![json!({ "title": "callee_a" })],
                }),
            },
        );

        assert_eq!(pack["impact"]["callers_count"], 4);
        assert_eq!(pack["impact"]["callees_count"], 2);
        assert_eq!(pack["impact"]["popular_sink"], false);
        assert_eq!(
            pack["staleness"]["analyst_graph_content_hash"],
            "fixture-hash"
        );
        assert_eq!(pack["staleness"]["exact_graph_hash"], "fixture-hash");
        assert_eq!(pack["staleness"]["analyst_matches_exact_graph"], true);
        assert_eq!(pack["primary_evidence"][0]["impact"]["callers_count"], 4);
        assert_eq!(pack["primary_evidence"][0]["impact"]["callees_count"], 2);
        assert_eq!(pack["primary_evidence"][0]["impact"]["popular_sink"], false);
    }

    #[test]
    fn knowledge_context_pack_marks_popular_sink_without_expanding_neighbors() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "popular impact",
            "intent": "change",
            "scope": "code"
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![KnowledgeCandidate {
                kind: "code".into(),
                title: "sink_symbol".into(),
                file_path: "crates/spur-mcp/src/lib.rs".into(),
                stable_symbol_id: Some("sym-sink".into()),
                symbol_kind: Some("function".into()),
                score: 9.0,
                signal: Some("load-bearing wall".into()),
                neighbor_kind: Some("primary".into()),
                edge_bind_method: None,
                grounding: "bm25-code".into(),
            }],
        };

        let pack = pack_query_result_with_exact_context(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("fixture-hash".into()),
                response_file_oids_match: Some(true),
                impact: Some(SymbolImpactSummary {
                    selector: "graph://symbol/sym-sink".into(),
                    callers_count: 31,
                    callees_count: 2,
                    caller_neighbors: vec![json!({ "title": "caller_a" })],
                    callee_neighbors: vec![json!({ "title": "callee_a" })],
                }),
            },
        );

        assert_eq!(pack["impact"]["callers_count"], 31);
        assert_eq!(pack["impact"]["popular_sink"], true);
        assert_eq!(
            pack["impact"]["caller_neighbors"].as_array().unwrap().len(),
            0
        );
        assert_eq!(
            pack["impact"]["callee_neighbors"].as_array().unwrap().len(),
            0
        );
    }
}
