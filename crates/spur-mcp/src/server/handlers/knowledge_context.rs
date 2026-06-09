use std::path::{Path, PathBuf};
#[cfg(feature = "embed")]
use std::sync::OnceLock;

use futures::future::join_all;
use serde_json::{json, Value};
use spur_analyst::{
    query_context_candidates, query_graph_candidates, KnowledgeCandidate, KnowledgeQueryIntent,
    KnowledgeQueryOptions, KnowledgeQueryResult, KnowledgeSearchScope,
};
use spur_graph::{resolve_worktree_root_from, EMBEDDING_VECTOR_DIMENSIONS};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

const POPULAR_SINK_CALLERS_THRESHOLD: u64 = 30;
const MAX_IMPACT_SYMBOLS: usize = 2;
const MAX_IMPACT_NEIGHBORS: usize = 2;
const BM25_HIGH_CONFIDENCE_SCORE: f64 = 8.0;
const BM25_MEDIUM_CONFIDENCE_SCORE: f64 = 3.0;
const HYBRID_HIGH_CONFIDENCE_SCORE: f64 = 0.80;
const HYBRID_MEDIUM_CONFIDENCE_SCORE: f64 = 0.55;

#[cfg(feature = "embed")]
static EMBED_MODEL: OnceLock<Option<fastembed::TextEmbedding>> = OnceLock::new();

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

    let query_vec = embed_query(&request.query).await.map(Vec::from);
    let analyst_intent = request.intent.as_analyst_intent();
    let mut query_result = query_context_candidates(
        &db_path,
        &request.query,
        request.scope.as_analyst_scope(),
        KnowledgeQueryOptions {
            limit: request.limit as usize,
            intent: analyst_intent,
            query_vec,
        },
    )
    .map_err(|error| {
        McpHandlerError::Internal(format!(
            "knowledge_context_pack failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;

    if request.should_query_graph_candidates() {
        match query_graph_candidates(
            &db_path,
            &request.query,
            KnowledgeQueryOptions {
                limit: request.limit as usize,
                intent: analyst_intent,
                query_vec: None,
            },
        ) {
            Ok(graph_result) => merge_graph_candidates(&mut query_result, graph_result),
            Err(error) => tracing::warn!(
                db_path = %db_path.display(),
                error = %error,
                "knowledge_context_pack failed to query graph candidates; continuing with context candidates"
            ),
        }
    }

    let exact_context = exact_graph_context_for_result(&request, &query_result).await;
    Ok(pack_query_result_with_exact_context(&request, query_result, exact_context).await)
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

    fn should_query_graph_candidates(&self) -> bool {
        matches!(self.scope, KnowledgeScope::Graph)
            || (matches!(self.scope, KnowledgeScope::All)
                && matches!(
                    self.intent,
                    KnowledgeIntent::Debug | KnowledgeIntent::Change
                ))
    }
}

fn merge_graph_candidates(result: &mut KnowledgeQueryResult, graph_result: KnowledgeQueryResult) {
    result.candidates.extend(graph_result.candidates);

    let mut deduped = Vec::with_capacity(result.candidates.len());
    for candidate in result.candidates.drain(..) {
        let Some(stable_symbol_id) = candidate.stable_symbol_id.as_deref() else {
            deduped.push(candidate);
            continue;
        };

        if let Some(existing) = deduped
            .iter_mut()
            .find(|existing| existing.stable_symbol_id.as_deref() == Some(stable_symbol_id))
        {
            if candidate.score > existing.score {
                *existing = candidate;
            }
        } else {
            deduped.push(candidate);
        }
    }

    result.candidates = deduped;
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

    fn as_analyst_intent(self) -> KnowledgeQueryIntent {
        match self {
            Self::Explain => KnowledgeQueryIntent::Explain,
            Self::Change => KnowledgeQueryIntent::Change,
            Self::Review => KnowledgeQueryIntent::Review,
            Self::Debug => KnowledgeQueryIntent::Debug,
            Self::Plan => KnowledgeQueryIntent::Plan,
        }
    }
}

#[cfg(feature = "embed")]
fn get_embed_model() -> Option<&'static fastembed::TextEmbedding> {
    EMBED_MODEL
        .get_or_init(|| {
            tracing::info!(
                "Loading embedding model BGEBaseENV15 for knowledge_context_pack hybrid search"
            );
            fastembed::TextEmbedding::try_new(
                fastembed::InitOptions::new(fastembed::EmbeddingModel::BGEBaseENV15)
                    .with_show_download_progress(false),
            )
            .ok()
        })
        .as_ref()
}

#[cfg(feature = "embed")]
pub fn warm_embed_model() {
    std::thread::spawn(|| {
        tracing::info!("Pre-warming BGEBaseENV15 embedding model for knowledge_context_pack");
        match get_embed_model() {
            Some(_) => tracing::info!("BGEBaseENV15 embedding model loaded successfully"),
            None => tracing::warn!(
                "BGEBaseENV15 embedding model failed to load; hybrid search will be unavailable"
            ),
        }
    });
}

#[cfg(not(feature = "embed"))]
pub fn warm_embed_model() {}

#[cfg(feature = "embed")]
async fn embed_query(query: &str) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> {
    let query = query.to_owned();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || {
            let model = get_embed_model()?;
            let embeddings = model.embed(vec![query.as_str()], None).ok()?;
            let embedding = embeddings.into_iter().next()?;
            embedding.try_into().ok()
        }),
    )
    .await;
    match result {
        Ok(Ok(embedding)) => embedding,
        Ok(Err(_join_error)) => None,
        Err(_timeout) => {
            tracing::warn!("embed_query timed out after 5s; degrading to BM25-only search");
            None
        }
    }
}

#[cfg(not(feature = "embed"))]
async fn embed_query(_query: &str) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> {
    None
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
async fn pack_query_result(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
) -> Value {
    pack_query_result_with_exact_context(request, result, ExactGraphContext::default()).await
}

#[derive(Debug, Clone, Default)]
struct ExactGraphContext {
    graph_content_hash: Option<String>,
    response_file_oids_match: Option<bool>,
    impacts: Vec<Option<SymbolImpactSummary>>,
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
    let selectors = top_n_code_selectors(&result.candidates, request);
    let Some(first_selector) = selectors.first() else {
        return ExactGraphContext::default();
    };

    let symbol_info = super::code_graph::code_symbol_info(&json!({
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
        super::code_graph::code_callers(&callers_args),
        super::code_graph::code_callees(&callees_args)
    );
    let callers = callers.ok()?;
    let callees = callees.ok()?;

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

fn normalized_code_selector(stable_symbol_id: &str) -> String {
    format!(
        "graph://symbol/{}",
        stable_symbol_id
            .strip_prefix("graph://symbol/")
            .unwrap_or(stable_symbol_id)
    )
}

async fn pack_query_result_with_exact_context(
    request: &KnowledgeContextPackRequest,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
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
        let body_selectors: Vec<(String, usize)> = primary_evidence
            .iter()
            .enumerate()
            .take(request.max_symbol_bodies as usize)
            .filter_map(|(index, evidence)| {
                evidence
                    .get("stable_symbol_id")
                    .and_then(Value::as_str)
                    .map(|selector| (selector.to_string(), index))
            })
            .collect();

        let body_results = join_all(body_selectors.into_iter().map(
            |(selector, index)| async move {
                (
                    index,
                    super::code_graph::code_read_symbol(&json!({
                        "selector": selector,
                    }))
                    .await,
                )
            },
        ))
        .await;

        for (index, body_result) in body_results {
            if let Ok(body) = body_result {
                if let Some(source) = body.get("source").and_then(Value::as_str) {
                    if let Some(evidence) = primary_evidence.get_mut(index) {
                        if let Some(object) = evidence.as_object_mut() {
                            object.insert("source".into(), json!(source));
                            if let Some(line_range) = body.get("line_range") {
                                object.insert("line_range".into(), line_range.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    let recommended_next_tools =
        recommended_next_tools(request.intent, &primary_evidence, &supporting_docs);
    let answerable = !primary_evidence.is_empty() || !supporting_docs.is_empty();
    let confidence = if !answerable {
        "low"
    } else {
        let top_evidence = primary_evidence.first();
        let top_score = top_evidence
            .and_then(|evidence| evidence.get("score").and_then(Value::as_f64))
            .unwrap_or(0.0);
        let top_grounding =
            top_evidence.and_then(|evidence| evidence.get("grounding").and_then(Value::as_str));
        let (high_score, medium_score) = confidence_score_thresholds(top_grounding);
        let evidence_count = primary_evidence.len() + supporting_docs.len();
        if top_score > high_score && evidence_count >= 3 {
            "high"
        } else if top_score > medium_score && evidence_count >= 2 {
            "medium"
        } else {
            "low"
        }
    };
    let impact = aggregate_impact_value(&exact_context.impacts);
    let staleness = staleness_value(&result, &exact_context);
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

fn confidence_score_thresholds(grounding: Option<&str>) -> (f64, f64) {
    match grounding {
        Some(grounding) if grounding.starts_with("hybrid-") => {
            (HYBRID_HIGH_CONFIDENCE_SCORE, HYBRID_MEDIUM_CONFIDENCE_SCORE)
        }
        _ => (BM25_HIGH_CONFIDENCE_SCORE, BM25_MEDIUM_CONFIDENCE_SCORE),
    }
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

fn aggregate_impact_value(impacts: &[Option<SymbolImpactSummary>]) -> Value {
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

fn recommended_next_tools(
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
        (KnowledgeIntent::Change, Some(selector)) => vec![
            json!({ "tool": "code_callers", "selector": selector, "reason": "Find direct change impact before editing." }),
            json!({ "tool": "code_callees", "selector": selector, "reason": "Trace direct dependencies for the selected symbol." }),
            json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body." }),
        ],
        (KnowledgeIntent::Debug, Some(selector)) => vec![
            json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body before debugging." }),
            json!({ "tool": "code_symbol_history", "selector": selector, "reason": "Inspect recent edits that may explain the failure." }),
            json!({ "tool": "code_subgraph", "selector": selector, "radius": 2, "reason": "Map nearby dependencies and callers around the failing symbol." }),
        ],
        (KnowledgeIntent::Review, Some(selector)) => vec![
            json!({ "tool": "code_read_symbol", "selector": selector, "reason": "Read exact current symbol body for review." }),
            json!({ "tool": "code_callers", "selector": selector, "reason": "Verify behavioral impact from direct callers." }),
        ],
        (KnowledgeIntent::Plan, Some(_)) => {
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
        (KnowledgeIntent::Explain, Some(selector)) => vec![json!({
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

fn is_test_file(file_path: &str) -> bool {
    file_path.contains("/tests/")
        || file_path.ends_with("_test.rs")
        || file_path.ends_with("_tests.rs")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard};

    use super::*;
    use duckdb::Connection;

    use spur_graph::store::{write_sections_dataset, SECTIONS_DATASET_DIR};
    use spur_graph::{artifact_from_facts, build_facts, EMBEDDING_VECTOR_DIMENSIONS};

    const INIT_SEARCH_SQL: &str =
        include_str!("../../../../spur-context/poc/duckdb-analyst/init_search.sql");
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct HybridConfidenceFixture {
        _temp_dir: tempfile::TempDir,
        db_path: PathBuf,
        query_vec: Vec<f32>,
    }

    #[test]
    fn hybrid_confidence_thresholds_match_bge_base_scores() {
        assert_eq!(
            confidence_score_thresholds(Some("hybrid-code")),
            (0.80, 0.55)
        );
    }

    fn candidate(stable_symbol_id: Option<&str>, title: &str, score: f64) -> KnowledgeCandidate {
        KnowledgeCandidate {
            kind: "code".into(),
            title: title.into(),
            file_path: "crates/spur-mcp/src/lib.rs".into(),
            stable_symbol_id: stable_symbol_id.map(str::to_string),
            symbol_kind: Some("function".into()),
            score,
            signal: None,
            neighbor_kind: None,
            edge_bind_method: None,
            grounding: "test".into(),
        }
    }

    fn context_candidate_macro_sql() -> String {
        INIT_SEARCH_SQL
            .split("CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE")
            .nth(1)
            .and_then(|rest| rest.split("-- Graph-augmented:").next())
            .map(|body| {
                let start = "CREATE OR REPLACE MACRO search_context_candidates(q, requested_scope, intent) AS TABLE";
                format!("{start}{body}")
            })
            .expect("context candidate macro should be present in init_search.sql")
    }

    fn context_candidate_macro_sql_with_artifact_dir(artifact_dir: &Path) -> String {
        context_candidate_macro_sql().replace(
            "__SPUR_GRAPH_ARTIFACT_DIR__",
            &sql_escape_path(artifact_dir),
        )
    }

    fn semantic_query_vec() -> Vec<f32> {
        let mut query_vec = vec![0.0; EMBEDDING_VECTOR_DIMENSIONS];
        query_vec[0] = 1.0;
        query_vec
    }

    fn format_query_vec_sql(query_vec: &[f32]) -> String {
        let values = query_vec
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{values}]::FLOAT[{EMBEDDING_VECTOR_DIMENSIONS}]")
    }

    fn seed_section_vectors(
        conn: &Connection,
        semantic_rows: &[(&str, &[f32])],
        lexical_rows: &[(&str, &[f32])],
    ) {
        if semantic_rows.is_empty() && lexical_rows.is_empty() {
            return;
        }
        let overrides = semantic_rows
            .iter()
            .chain(lexical_rows.iter())
            .map(|(file_path, vector)| {
                format!(
                    "('{}', {})",
                    file_path.replace('\'', "''"),
                    format_query_vec_sql(vector)
                )
            })
            .collect::<Vec<_>>();
        let sql = format!(
            r#"
            CREATE OR REPLACE TABLE lance_ns.main.section_bodies AS
            SELECT s.stable_symbol_id,
                   s.file_path,
                   s.qualified_name,
                   s.heading_level,
                   s.body_text,
                   s.body_byte_start,
                   s.body_byte_end,
                   s.child_count,
                   s.parent_stable_id,
                   s.content_hash,
                   COALESCE(o.vector, s.vector) AS vector
            FROM lance_ns.main.section_bodies AS s
            LEFT JOIN (
                SELECT col0 AS stable_symbol_id, col1 AS vector
                FROM (VALUES {})
            ) AS o USING (stable_symbol_id);
            "#,
            overrides.join(",\n                  ")
        );
        conn.execute_batch(&sql)
            .expect("seed fixture section vectors");
    }

    fn sql_escape_path(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().expect("env lock")
    }

    fn parse_vector_json_to_f32(raw: &str) -> Vec<f32> {
        serde_json::from_str::<Vec<f64>>(raw)
            .unwrap_or_default()
            .into_iter()
            .map(|value| value as f32)
            .collect()
    }

    fn build_hybrid_confidence_fixture() -> HybridConfidenceFixture {
        let _lock = env_lock();

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let root = temp_dir.path().join("repo");
        fs::create_dir_all(root.join("src")).expect("create src dir");
        fs::create_dir_all(root.join("docs")).expect("create docs dir");
        fs::write(
            root.join("src/hybrid.rs"),
            r#"
pub fn ranking_beacon_router() {
    // Ranking beacon: this symbol intentionally repeats the target query phrase.
    let ranking_beacon = "ranking beacon ranking beacon ranking beacon";
    println!("{ranking_beacon}");
}

pub fn lexical_signal_anchor() {
    println!("lexical fallback utility");
}
"#,
        )
        .expect("write strong hybrid code");
        fs::write(
            root.join("docs/strong_hybrid.md"),
            "# Strong Hybrid\n\nranking beacon ranking beacon ranking beacon.\n",
        )
        .expect("write strong hybrid doc");
        fs::write(
            root.join("docs/lexical_hybrid.md"),
            "# Lexical Rival\n\nranking beacon appears often ranking beacon.\n",
        )
        .expect("write lexical rival doc");
        fs::write(
            root.join("docs/weak_hybrid.md"),
            "# Weak Only\n\nprivate lexical-only weakness signal.\n",
        )
        .expect("write weak-only doc");

        let facts = build_facts(&root, None).expect("build fixture facts").0;
        let artifact = artifact_from_facts(&facts, &root).expect("build fixture artifact");
        let artifact_dir = temp_dir.path().join("artifact");
        write_sections_dataset(&artifact, &root, &artifact_dir).expect("write Lance sidecar");

        let db_path = temp_dir.path().join("analyst.duckdb");
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch("INSTALL fts; LOAD fts; LOAD icu; INSTALL lance; LOAD lance;")
            .expect("load fixture extensions");
        conn.execute_batch(&format!(
            "ATTACH '{}' AS lance_ns (TYPE LANCE);",
            sql_escape_path(&artifact_dir.join(SECTIONS_DATASET_DIR))
        ))
        .expect("attach sections dataset");
        conn.execute_batch(&format!(
            "ATTACH '{}' AS code_ns (TYPE LANCE);",
            sql_escape_path(&artifact_dir)
        ))
        .expect("attach code dataset");
        let mut symbol_row_stmt = conn
            .prepare(
                "
            SELECT stable_symbol_id
            FROM code_ns.main.code_symbols
            WHERE file_path = 'src/hybrid.rs'
            ORDER BY stable_symbol_id
            LIMIT 1
            ",
            )
            .expect("query code symbol id");
        let strong_symbol_id: String = symbol_row_stmt
            .query_row([], |row| row.get(0))
            .expect("query strong symbol id");
        let mut symbol_vec_stmt = conn
            .prepare("SELECT to_json(vector) FROM code_ns.main.code_symbols WHERE stable_symbol_id = ? LIMIT 1")
            .expect("query strong symbol vector");
        let symbol_vector_json = symbol_vec_stmt
            .query_row([&strong_symbol_id], |row| {
                row.get::<usize, Option<String>>(0)
            })
            .expect("query code symbol vector");
        let query_vec = symbol_vector_json
            .and_then(|value| (!value.is_empty()).then_some(value))
            .map(|value| parse_vector_json_to_f32(&value))
            .filter(|query_vec| query_vec.len() == EMBEDDING_VECTOR_DIMENSIONS)
            .unwrap_or_else(semantic_query_vec);
        seed_section_vectors(
            &conn,
            &[("docs/strong_hybrid.md", query_vec.as_slice())],
            &[],
        );

        conn.execute_batch(
            r#"
            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('hybrid-fixture-hash');

            CREATE TABLE sections_search AS
            SELECT stable_symbol_id, qualified_name, file_path, heading_level, content_hash, body_text
            FROM lance_ns.main.section_bodies;

            CREATE TABLE symbol_text AS
            SELECT stable_symbol_id,
                   entity_name,
                   qualified_name,
                   file_path,
                   symbol_kind,
                   embed_text AS doc_text
            FROM code_ns.main.code_symbols;

            CREATE TABLE v_symbol_scorecard AS
            SELECT stable_symbol_id,
                   entity_name,
                   file_path,
                   symbol_kind,
                   0.01 AS pagerank,
                   3::BIGINT AS churn_90d,
                   'stable' AS posture,
                   1::BIGINT AS component_size,
                   2::BIGINT AS callers
            FROM symbol_text;

            CREATE TABLE v_symbol_inbound AS
            SELECT stable_symbol_id, 1::BIGINT AS callers
            FROM symbol_text;
            "#
        )
        .expect("create fixture schema");
        conn.execute_batch(
            r#"
            PRAGMA create_fts_index('main.sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');
            PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);
            "#,
        )
        .expect("create fixture fts indexes");
        let macro_sql = context_candidate_macro_sql_with_artifact_dir(&artifact_dir);
        conn.execute_batch(&macro_sql)
            .expect("define search context macro");
        drop(conn);

        HybridConfidenceFixture {
            _temp_dir: temp_dir,
            db_path,
            query_vec,
        }
    }

    #[test]
    fn recommended_next_tools_are_intent_adaptive() {
        let primary = vec![json!({
            "stable_symbol_id": "graph://symbol/sym-1",
            "file": "crates/spur-mcp/src/lib.rs"
        })];
        let docs = vec![json!({
            "kind": "doc",
            "stable_symbol_id": "doc-1",
            "file": "docs/context.md"
        })];

        let debug_tools = recommended_next_tools(KnowledgeIntent::Debug, &primary, &[]);
        assert_eq!(
            debug_tools
                .iter()
                .map(|tool| tool["tool"].as_str().expect("tool name"))
                .collect::<Vec<_>>(),
            vec!["code_read_symbol", "code_symbol_history", "code_subgraph"]
        );
        assert_eq!(debug_tools[2]["radius"], 2);

        let review_tools = recommended_next_tools(KnowledgeIntent::Review, &primary, &[]);
        assert_eq!(
            review_tools
                .iter()
                .map(|tool| tool["tool"].as_str().expect("tool name"))
                .collect::<Vec<_>>(),
            vec!["code_read_symbol", "code_callers"]
        );

        let plan_tools = recommended_next_tools(KnowledgeIntent::Plan, &primary, &docs);
        assert_eq!(
            plan_tools
                .iter()
                .map(|tool| tool["tool"].as_str().expect("tool name"))
                .collect::<Vec<_>>(),
            vec!["doc_navigate", "code_file_symbols"]
        );
        assert_eq!(plan_tools[0]["root"], "doc-1");
        assert_eq!(plan_tools[1]["file"], "crates/spur-mcp/src/lib.rs");

        let fallback = recommended_next_tools(KnowledgeIntent::Debug, &[], &[]);
        assert_eq!(fallback[0]["tool"], "code_semantic_search");
    }

    #[test]
    fn code_next_tools_are_intent_adaptive() {
        let tools = |intent| {
            code_next_tools(intent)
                .into_iter()
                .map(|tool| tool["tool"].as_str().expect("tool name").to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            tools(KnowledgeIntent::Debug),
            vec!["code_read_symbol", "code_symbol_history"]
        );
        assert_eq!(
            tools(KnowledgeIntent::Review),
            vec!["code_read_symbol", "code_callers"]
        );
        assert_eq!(
            tools(KnowledgeIntent::Plan),
            vec!["code_read_symbol", "code_file_symbols"]
        );
        assert_eq!(tools(KnowledgeIntent::Explain), vec!["code_read_symbol"]);
    }

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

    #[test]
    fn knowledge_context_pack_queries_graph_for_graph_scope_or_change_debug_all_scope() {
        for (scope, intent, expected) in [
            ("graph", "explain", true),
            ("all", "debug", true),
            ("all", "change", true),
            ("all", "explain", false),
            ("code", "debug", false),
            ("docs", "change", false),
        ] {
            let request = KnowledgeContextPackRequest::parse(&json!({
                "query": "semantic search",
                "scope": scope,
                "intent": intent
            }))
            .expect("request");

            assert_eq!(
                request.should_query_graph_candidates(),
                expected,
                "scope={scope} intent={intent}"
            );
        }
    }

    #[test]
    fn merge_graph_candidates_deduplicates_stable_symbols_by_higher_score() {
        let mut result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-dup"), "bm25 duplicate", 3.0),
                candidate(None, "bm25 no symbol", 2.0),
                candidate(Some("sym-bm25"), "bm25 unique", 5.0),
            ],
        };
        let graph_result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-dup"), "graph duplicate", 8.0),
                candidate(Some("sym-bm25"), "graph lower duplicate", 1.0),
                candidate(Some("sym-graph"), "graph unique", 4.0),
                candidate(None, "graph no symbol", 6.0),
            ],
        };

        merge_graph_candidates(&mut result, graph_result);

        let titles = result
            .candidates
            .iter()
            .map(|candidate| candidate.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            titles,
            vec![
                "graph duplicate",
                "bm25 no symbol",
                "bm25 unique",
                "graph unique",
                "graph no symbol"
            ]
        );
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

    #[tokio::test]
    async fn knowledge_context_pack_explains_why_evidence_is_relevant() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search"
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![KnowledgeCandidate {
                kind: "code".into(),
                title: "query_context_candidates".into(),
                file_path: "crates/spur-analyst/src/lib.rs".into(),
                stable_symbol_id: Some("sym-1".into()),
                symbol_kind: Some("function".into()),
                score: 7.5,
                signal: Some("stable".into()),
                neighbor_kind: Some("primary".into()),
                edge_bind_method: None,
                grounding: "bm25-graph-expanded".into(),
            }],
        };

        let pack = pack_query_result(&request, result).await;
        let why_relevant = pack["primary_evidence"][0]["why_relevant"]
            .as_str()
            .expect("why relevant");

        assert!(why_relevant.starts_with("graph 7.5"));
        assert!(why_relevant.contains("stable"));
        assert!(why_relevant.contains("kind=function"));
        assert!(why_relevant.contains("grounding=bm25-graph-expanded"));
    }

    #[tokio::test]
    async fn knowledge_context_pack_reports_high_confidence_for_strong_evidence_set() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search",
            "limit": 3
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "top_symbol".into(),
                    file_path: "crates/spur-mcp/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-top".into()),
                    symbol_kind: Some("function".into()),
                    score: 9.2,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "supporting_symbol".into(),
                    file_path: "crates/spur-core/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-support".into()),
                    symbol_kind: Some("function".into()),
                    score: 4.0,
                    signal: None,
                    neighbor_kind: None,
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
            ],
        };

        let pack = pack_query_result(&request, result).await;

        assert_eq!(pack["confidence"], "high");
    }

    #[tokio::test]
    async fn knowledge_context_pack_uses_lower_high_threshold_for_hybrid_evidence() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search",
            "limit": 3
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "top_symbol".into(),
                    file_path: "crates/spur-mcp/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-top".into()),
                    symbol_kind: Some("function".into()),
                    score: 1.1,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "hybrid-code".into(),
                },
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "supporting_symbol".into(),
                    file_path: "crates/spur-core/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-support".into()),
                    symbol_kind: Some("function".into()),
                    score: 0.8,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "hybrid-code".into(),
                },
                KnowledgeCandidate {
                    kind: "doc".into(),
                    title: "Knowledge Context API".into(),
                    file_path: "docs/context.md".into(),
                    stable_symbol_id: Some("doc-1".into()),
                    symbol_kind: Some("section".into()),
                    score: 0.4,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "hybrid-doc".into(),
                },
            ],
        };

        let pack = pack_query_result(&request, result).await;

        assert_eq!(pack["confidence"], "high");
    }

    #[tokio::test]
    async fn knowledge_context_pack_reports_low_confidence_for_weak_evidence_set() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search"
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "weak_symbol".into(),
                    file_path: "crates/spur-mcp/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-weak".into()),
                    symbol_kind: Some("function".into()),
                    score: 2.5,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
                KnowledgeCandidate {
                    kind: "doc".into(),
                    title: "Weak Context API".into(),
                    file_path: "docs/context.md".into(),
                    stable_symbol_id: Some("doc-weak".into()),
                    symbol_kind: Some("section".into()),
                    score: 2.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-doc".into(),
                },
            ],
        };

        let pack = pack_query_result(&request, result).await;

        assert_eq!(pack["confidence"], "low");
    }

    #[tokio::test]
    async fn knowledge_context_pack_reports_candidate_totals() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search",
            "limit": 3
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                KnowledgeCandidate {
                    kind: "code".into(),
                    title: "code_symbol".into(),
                    file_path: "crates/spur-mcp/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-code".into()),
                    symbol_kind: Some("function".into()),
                    score: 7.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
                KnowledgeCandidate {
                    kind: "symbol".into(),
                    title: "graph_symbol".into(),
                    file_path: "crates/spur-graph/src/lib.rs".into(),
                    stable_symbol_id: Some("sym-graph".into()),
                    symbol_kind: Some("struct".into()),
                    score: 6.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-code".into(),
                },
                KnowledgeCandidate {
                    kind: "doc".into(),
                    title: "Knowledge Context API".into(),
                    file_path: "docs/context.md".into(),
                    stable_symbol_id: Some("doc-1".into()),
                    symbol_kind: Some("section".into()),
                    score: 5.0,
                    signal: None,
                    neighbor_kind: None,
                    edge_bind_method: None,
                    grounding: "bm25-doc".into(),
                },
            ],
        };

        let pack = pack_query_result(&request, result).await;

        assert_eq!(pack["candidates"]["total"], 3);
        assert_eq!(pack["candidates"]["returned_primary"], 2);
        assert_eq!(pack["candidates"]["returned_supporting_docs"], 1);
        assert_eq!(pack["candidates"]["total_code"], 2);
        assert_eq!(pack["candidates"]["total_docs"], 1);
    }

    #[tokio::test]
    async fn knowledge_context_pack_returns_grounded_evidence_and_followups() {
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

        let pack = pack_query_result(&request, result).await;

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

    #[tokio::test]
    async fn knowledge_context_pack_includes_bounded_impact_for_top_code_evidence() {
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
                impacts: vec![Some(SymbolImpactSummary {
                    selector: "graph://symbol/sym-top".into(),
                    callers_count: 4,
                    callees_count: 2,
                    caller_neighbors: vec![json!({ "title": "caller_a" })],
                    callee_neighbors: vec![json!({ "title": "callee_a" })],
                })],
            },
        )
        .await;

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

    #[tokio::test]
    async fn knowledge_context_pack_attaches_aggregate_impact_for_top_two_code_evidence() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "change impact",
            "intent": "change",
            "scope": "code",
            "limit": 4
        }))
        .expect("request");
        let candidates = ["one", "two", "three", "four"]
            .into_iter()
            .enumerate()
            .map(|(index, suffix)| KnowledgeCandidate {
                kind: "code".into(),
                title: format!("symbol_{suffix}"),
                file_path: "crates/spur-mcp/src/lib.rs".into(),
                stable_symbol_id: Some(format!("sym-{suffix}")),
                symbol_kind: Some("function".into()),
                score: 9.0 - index as f64,
                signal: Some("stable".into()),
                neighbor_kind: Some("primary".into()),
                edge_bind_method: None,
                grounding: "bm25-code".into(),
            })
            .collect();
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates,
        };

        let pack = pack_query_result_with_exact_context(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("fixture-hash".into()),
                response_file_oids_match: Some(true),
                impacts: vec![
                    Some(SymbolImpactSummary {
                        selector: "graph://symbol/sym-one".into(),
                        callers_count: 4,
                        callees_count: 2,
                        caller_neighbors: vec![json!({ "title": "caller_a" })],
                        callee_neighbors: vec![json!({ "title": "callee_a" })],
                    }),
                    Some(SymbolImpactSummary {
                        selector: "graph://symbol/sym-two".into(),
                        callers_count: 31,
                        callees_count: 3,
                        caller_neighbors: vec![json!({ "title": "caller_b" })],
                        callee_neighbors: vec![json!({ "title": "callee_b" })],
                    }),
                ],
            },
        )
        .await;

        assert_eq!(pack["impact"]["callers_count"], 35);
        assert_eq!(pack["impact"]["callees_count"], 5);
        assert_eq!(pack["impact"]["popular_sink"], true);
        assert_eq!(
            pack["impact"]["caller_neighbors"].as_array().unwrap().len(),
            0
        );
        assert_eq!(
            pack["impact"]["callee_neighbors"].as_array().unwrap().len(),
            0
        );
        assert_eq!(pack["primary_evidence"][0]["impact"]["callers_count"], 4);
        assert_eq!(pack["primary_evidence"][1]["impact"]["callers_count"], 31);
        assert_eq!(pack["primary_evidence"][2].get("impact"), None);
        assert_eq!(pack["primary_evidence"][3].get("impact"), None);
        assert_eq!(
            pack["primary_evidence"][0]["impact"]
                .as_object()
                .unwrap()
                .len(),
            3
        );
    }

    #[tokio::test]
    async fn knowledge_context_pack_marks_popular_sink_without_expanding_neighbors() {
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
                impacts: vec![Some(SymbolImpactSummary {
                    selector: "graph://symbol/sym-sink".into(),
                    callers_count: 31,
                    callees_count: 2,
                    caller_neighbors: vec![json!({ "title": "caller_a" })],
                    callee_neighbors: vec![json!({ "title": "callee_a" })],
                })],
            },
        )
        .await;

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

    #[tokio::test]
    async fn knowledge_context_pack_reports_confidence_from_real_hybrid_fusion() {
        let fixture = build_hybrid_confidence_fixture();

        let strong_request = KnowledgeContextPackRequest::parse(&json!({
            "query": "ranking beacon",
            "scope": "all",
            "limit": 3
        }))
        .expect("request");
        let strong_result = query_context_candidates(
            &fixture.db_path,
            "ranking beacon",
            KnowledgeSearchScope::All,
            KnowledgeQueryOptions {
                limit: 3,
                intent: KnowledgeQueryIntent::Explain,
                query_vec: Some(fixture.query_vec.clone()),
            },
        )
        .expect("query strong hybrid candidates");
        let strong_primary = strong_result
            .candidates
            .iter()
            .filter(|candidate| !candidate.grounding.starts_with("bm25"))
            .collect::<Vec<_>>();
        assert!(
            !strong_primary.is_empty(),
            "expected strong-query hybrid candidates, got {:?}",
            strong_result.candidates
        );
        let strong_pack = pack_query_result(&strong_request, strong_result).await;
        println!(
            "strong pack: {}",
            serde_json::to_string_pretty(&strong_pack).unwrap_or_else(|_| strong_pack.to_string())
        );
        let strong_top = strong_pack["primary_evidence"]
            .as_array()
            .and_then(|values| values.first())
            .expect("strong result should include primary evidence");
        let strong_score = strong_top["score"].as_f64().expect("strong top score");
        let strong_grounding = strong_top["grounding"].as_str().unwrap_or("<missing>");
        let strong_confidence = strong_pack["confidence"]
            .as_str()
            .expect("strong confidence");

        assert!(
            strong_grounding.starts_with("hybrid-"),
            "strong top grounding should be hybrid, got {strong_grounding}"
        );
        assert!(
            strong_score >= 0.55,
            "strong hybrid top score={strong_score:.6}, grounding={strong_grounding}"
        );
        assert!(
            strong_pack["candidates"]["returned_primary"]
                .as_u64()
                .unwrap_or(0)
                >= 1,
            "expected at least one primary candidate, got {:?}",
            strong_pack["candidates"]
        );
        assert!(
            matches!(strong_confidence, "medium" | "high"),
            "cross-signal hybrid should not be reported as low confidence, got {strong_confidence}"
        );

        let weak_request = KnowledgeContextPackRequest::parse(&json!({
            "query": "private lexical-only weakness signal",
            "scope": "docs",
            "limit": 3
        }))
        .expect("request");
        let weak_result = query_context_candidates(
            &fixture.db_path,
            "private lexical-only weakness signal",
            KnowledgeSearchScope::Docs,
            KnowledgeQueryOptions {
                limit: 1,
                intent: KnowledgeQueryIntent::Explain,
                query_vec: None,
            },
        )
        .expect("query weak hybrid candidates");
        let weak_primary = weak_result
            .candidates
            .iter()
            .filter(|candidate| candidate.kind == "doc")
            .collect::<Vec<_>>();
        assert!(
            !weak_primary.is_empty(),
            "expected weak-query doc candidates, got {:?}",
            weak_result.candidates
        );
        let weak_pack = pack_query_result(&weak_request, weak_result).await;
        println!(
            "weak pack: {}",
            serde_json::to_string_pretty(&weak_pack).unwrap_or_else(|_| weak_pack.to_string())
        );
        let weak_top = weak_pack["supporting_docs"]
            .as_array()
            .and_then(|values| values.first())
            .expect("weak result should include supporting docs");
        assert_eq!(
            weak_pack["candidates"]["returned_primary"]
                .as_u64()
                .unwrap_or(0),
            0
        );
        let weak_score = weak_top["score"].as_f64().expect("weak top score");
        let weak_grounding = weak_top["grounding"].as_str().unwrap_or("<missing>");

        println!(
            "measured top scores: strong={:.6}, weak={:.6}",
            strong_score, weak_score
        );
        assert_eq!(weak_grounding, "bm25-doc");
        assert_eq!(weak_pack["confidence"], "low");
    }
}
