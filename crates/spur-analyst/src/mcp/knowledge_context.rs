use std::path::{Path, PathBuf};
#[cfg(feature = "embed")]
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use crate::{
    load_analyst_icu_extension, load_analyst_lance_extension, open_analyst_connection_read_only,
    query_context_candidates_with_conn, query_context_paths_with_conn,
    query_graph_candidates_with_conn, query_symbol_risk_community_with_conn, KnowledgeCandidate,
    KnowledgePathOptions, KnowledgePathResult, KnowledgeQueryIntent, KnowledgeQueryOptions,
    KnowledgeQueryResult, KnowledgeSearchScope, SymbolEvidenceCaveat, SymbolEvidenceStatus,
    SymbolRiskScorecardRow, MAX_CONTEXT_PATHS, MAX_CONTEXT_PATH_HOPS,
};
#[cfg(test)]
use crate::{query_context_paths, query_symbol_risk_community};
use futures::future::join_all;
use serde_json::{json, Value};
#[cfg(feature = "embed")]
use spur_graph::{
    embedding_query_text_for_model, fastembed_cache_dir, EmbeddingModelSelection, EMBED_MODEL_ENV,
};
use spur_graph::{resolve_worktree_root_from, EMBEDDING_VECTOR_DIMENSIONS};

use super::McpHandlerError;

const POPULAR_SINK_CALLERS_THRESHOLD: u64 = 30;
const MAX_IMPACT_SYMBOLS: usize = 2;
const MAX_IMPACT_NEIGHBORS: usize = 2;
const BM25_HIGH_CONFIDENCE_SCORE: f64 = 8.0;
const BM25_MEDIUM_CONFIDENCE_SCORE: f64 = 3.0;
const HYBRID_HIGH_CONFIDENCE_SCORE: f64 = 0.80;
const HYBRID_MEDIUM_CONFIDENCE_SCORE: f64 = 0.55;
const ANALYST_EMBED_MODE_ENV: &str = "SPUR_ANALYST_EMBED_MODE";

#[cfg(feature = "embed")]
const EMBED_INFERENCE_TIMEOUT: Duration = Duration::from_millis(1500);
#[cfg(feature = "embed")]
static EMBEDDING_GEMMA_EMBED_MODEL: EmbedModelCell<fastembed::TextEmbedding> =
    EmbedModelCell::new();
#[cfg(all(test, feature = "embed"))]
static DISABLE_EMBED_QUERY_FOR_TESTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnalystEmbedMode {
    Auto,
    InProcess,
    Sidecar,
    Off,
}

impl AnalystEmbedMode {
    fn current() -> Self {
        #[cfg(test)]
        if let Some(mode) =
            ANALYST_EMBED_MODE_OVERRIDE_FOR_TESTS.with(|override_mode| override_mode.get())
        {
            return mode;
        }

        Self::from_env()
    }

    fn from_env() -> Self {
        match std::env::var(ANALYST_EMBED_MODE_ENV) {
            Ok(value) => Self::parse_env_value(&value),
            Err(std::env::VarError::NotPresent) => Self::Auto,
            Err(error) => {
                tracing::warn!(
                    %error,
                    env = ANALYST_EMBED_MODE_ENV,
                    "failed to read analyst embed mode; falling back to auto"
                );
                Self::Auto
            }
        }
    }

    fn parse_env_value(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "inprocess" => Self::InProcess,
            "sidecar" => Self::Sidecar,
            "off" => Self::Off,
            _ => {
                tracing::warn!(
                    value,
                    env = ANALYST_EMBED_MODE_ENV,
                    "unknown analyst embed mode; falling back to auto"
                );
                Self::Auto
            }
        }
    }

    #[cfg(feature = "embed")]
    fn allows_in_process(self, entrypoint: &'static str) -> bool {
        match self {
            Self::Auto | Self::InProcess => true,
            Self::Off => false,
            Self::Sidecar => {
                tracing::debug!(
                    mode = "sidecar",
                    entrypoint,
                    "analyst embed sidecar mode is not yet wired; degrading to BM25-only search"
                );
                false
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static ANALYST_EMBED_MODE_OVERRIDE_FOR_TESTS: std::cell::Cell<Option<AnalystEmbedMode>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
struct AnalystEmbedModeOverrideGuard {
    previous: Option<AnalystEmbedMode>,
}

#[cfg(test)]
impl Drop for AnalystEmbedModeOverrideGuard {
    fn drop(&mut self) {
        ANALYST_EMBED_MODE_OVERRIDE_FOR_TESTS.with(|override_mode| {
            override_mode.set(self.previous);
        });
    }
}

#[cfg(test)]
fn set_analyst_embed_mode_for_test(mode: AnalystEmbedMode) -> AnalystEmbedModeOverrideGuard {
    let previous = ANALYST_EMBED_MODE_OVERRIDE_FOR_TESTS.with(|override_mode| {
        let previous = override_mode.get();
        override_mode.set(Some(mode));
        previous
    });
    AnalystEmbedModeOverrideGuard { previous }
}

pub async fn knowledge_context_pack(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackRequest::parse(args)?;
    let db_path = analyst_db_path()?;
    if !db_path.exists() {
        return Ok(unavailable_pack(&request, &db_path));
    }

    let query_vec = embed_query(&request.query).await.map(Vec::from);
    let query_result = {
        let conn = open_pack_connection(&db_path, "knowledge_context_pack")?;
        query_candidates_for_request_with_conn(
            &request,
            &db_path,
            &conn,
            "knowledge_context_pack",
            query_vec,
        )?
    };

    let exact_context = exact_graph_context_for_result(&request, &query_result).await;
    Ok(pack_query_result_with_exact_context(&request, query_result, exact_context).await)
}

pub async fn knowledge_context_pack_2(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackV2Request::parse(args)?;
    let db_path = analyst_db_path()?;
    if !db_path.exists() {
        return Ok(unavailable_pack_v2(&request, &db_path));
    }

    let query_vec = embed_query(&request.base.query).await.map(Vec::from);
    let conn = open_pack_connection(&db_path, "knowledge_context_pack_2")?;
    let query_result = query_candidates_for_request_with_conn(
        &request.base,
        &db_path,
        &conn,
        "knowledge_context_pack_2",
        query_vec,
    )?;

    let exact_context = exact_graph_context_for_result(&request.base, &query_result).await;
    let graph_sections = graph_reasoning_sections_for_pack_with_conn(
        &request,
        &query_result,
        &exact_context,
        &db_path,
        &conn,
    );
    drop(conn);
    Ok(pack_query_result_v2_with_graph_sections(
        &request,
        query_result,
        exact_context,
        graph_sections,
    )
    .await)
}

fn open_pack_connection(
    db_path: &Path,
    tool_name: &str,
) -> Result<duckdb::Connection, McpHandlerError> {
    let conn = open_analyst_connection_read_only(db_path).map_err(|error| {
        McpHandlerError::Internal(format!(
            "{tool_name} failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;
    load_analyst_icu_extension(&conn);
    load_analyst_lance_extension(&conn);
    Ok(conn)
}

fn query_candidates_for_request_with_conn(
    request: &KnowledgeContextPackRequest,
    db_path: &Path,
    conn: &duckdb::Connection,
    tool_name: &str,
    query_vec: Option<Vec<f32>>,
) -> Result<KnowledgeQueryResult, McpHandlerError> {
    let analyst_intent = request.intent.as_analyst_intent();
    let mut query_result = query_context_candidates_with_conn(
        conn,
        db_path,
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
            "{tool_name} failed to query analyst DB at {}: {error}",
            db_path.display()
        ))
    })?;

    if request.should_query_graph_candidates() {
        match query_graph_candidates_with_conn(
            conn,
            db_path,
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
                tool = tool_name,
                "knowledge context pack failed to query graph candidates; continuing with context candidates"
            ),
        }
    }

    Ok(query_result)
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
            .to_owned();
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

struct KnowledgeContextPackV2Request {
    base: KnowledgeContextPackRequest,
    graph_reasoning: GraphReasoningOptions,
}

impl KnowledgeContextPackV2Request {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let base = KnowledgeContextPackRequest::parse(args)?;
        let graph_reasoning = GraphReasoningOptions::parse(args, base.intent, base.scope)?;
        Ok(Self {
            base,
            graph_reasoning,
        })
    }
}

struct GraphReasoningOptions {
    paths: bool,
    communities: bool,
    communities_explicit: bool,
    risk: bool,
    max_path_hops: usize,
    max_paths: usize,
    anchors: Vec<String>,
}

impl GraphReasoningOptions {
    fn parse(
        args: &Value,
        intent: KnowledgeIntent,
        scope: KnowledgeScope,
    ) -> Result<Self, McpHandlerError> {
        let paths_default = matches!(
            intent,
            KnowledgeIntent::Change | KnowledgeIntent::Review | KnowledgeIntent::Debug
        );
        let risk_default = !matches!(scope, KnowledgeScope::Docs);
        let Some(value) = args.get("graph_reasoning") else {
            return Ok(Self {
                paths: paths_default,
                communities: true,
                communities_explicit: false,
                risk: risk_default,
                max_path_hops: KnowledgePathOptions::default().max_hops,
                max_paths: KnowledgePathOptions::default().max_paths,
                anchors: Vec::new(),
            });
        };
        let object = value.as_object().ok_or_else(|| {
            McpHandlerError::InvalidParams(
                "knowledge_context_pack_2 field 'graph_reasoning' must be an object".into(),
            )
        })?;
        let communities = parse_optional_bool_v2(object, "communities")?;

        Ok(Self {
            paths: parse_optional_bool_v2(object, "paths")?.unwrap_or(paths_default),
            communities: communities.unwrap_or(true),
            communities_explicit: communities.is_some(),
            risk: parse_optional_bool_v2(object, "risk")?.unwrap_or(risk_default),
            max_path_hops: parse_clamped_usize_v2(
                object,
                "max_path_hops",
                KnowledgePathOptions::default().max_hops,
                1,
                MAX_CONTEXT_PATH_HOPS,
            )?,
            max_paths: parse_clamped_usize_v2(
                object,
                "max_paths",
                KnowledgePathOptions::default().max_paths,
                1,
                MAX_CONTEXT_PATHS,
            )?,
            anchors: parse_anchor_array_v2(object)?,
        })
    }

    fn should_query_communities(&self, code_symbol_count: usize) -> bool {
        if !self.communities {
            return false;
        }
        self.communities_explicit || code_symbol_count >= 2
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
pub(crate) struct EmbedModelCell<M> {
    model: OnceLock<Arc<Mutex<M>>>,
    loading: Mutex<bool>,
}

#[cfg(feature = "embed")]
pub(crate) struct EmbedLoadPermit<'a, M> {
    cell: &'a EmbedModelCell<M>,
    completed: bool,
}

#[cfg(feature = "embed")]
impl<M> EmbedModelCell<M> {
    const fn new() -> Self {
        Self {
            model: OnceLock::new(),
            loading: Mutex::new(false),
        }
    }

    pub(crate) fn ready(&self) -> Option<Arc<Mutex<M>>> {
        self.model.get().cloned()
    }

    fn is_ready(&self) -> bool {
        self.model.get().is_some()
    }

    pub(crate) fn begin_load(&self) -> Option<EmbedLoadPermit<'_, M>> {
        if self.is_ready() {
            return None;
        }

        let mut loading = self
            .loading
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_ready() || *loading {
            return None;
        }

        *loading = true;
        Some(EmbedLoadPermit {
            cell: self,
            completed: false,
        })
    }

    #[cfg(test)]
    fn is_loading_for_test(&self) -> bool {
        *self
            .loading
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    fn load_if_idle(&self, load: impl FnOnce() -> Option<M>) -> Option<Arc<Mutex<M>>> {
        if let Some(model) = self.ready() {
            return Some(model);
        }

        let permit = self.begin_load()?;
        permit.complete(load())
    }

    fn clear_loading(&self) {
        let mut loading = self
            .loading
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *loading = false;
    }
}

#[cfg(feature = "embed")]
impl<M> EmbedLoadPermit<'_, M> {
    pub(crate) fn complete(mut self, model: Option<M>) -> Option<Arc<Mutex<M>>> {
        if let Some(model) = model {
            let _ = self.cell.model.set(Arc::new(Mutex::new(model)));
        }
        self.cell.clear_loading();
        self.completed = true;
        self.cell.ready()
    }
}

#[cfg(feature = "embed")]
impl<M> Drop for EmbedLoadPermit<'_, M> {
    fn drop(&mut self) {
        if !self.completed {
            self.cell.clear_loading();
        }
    }
}

#[cfg(feature = "embed")]
pub(crate) fn embed_model_cell(
    _embedding_model: EmbeddingModelSelection,
) -> &'static EmbedModelCell<fastembed::TextEmbedding> {
    &EMBEDDING_GEMMA_EMBED_MODEL
}

#[cfg(feature = "embed")]
pub(crate) fn load_embed_model(
    embedding_model: EmbeddingModelSelection,
) -> Result<fastembed::TextEmbedding, String> {
    tracing::info!(
        model = embedding_model.model_name(),
        "Loading embedding model for knowledge_context_pack hybrid search"
    );
    let mut init_options = fastembed::InitOptions::new(embedding_model.fastembed_model())
        .with_show_download_progress(false);
    if let Some(cache_dir) = fastembed_cache_dir() {
        init_options = init_options.with_cache_dir(cache_dir);
    }

    fastembed::TextEmbedding::try_new(init_options).map_err(|error| error.to_string())
}

#[cfg(feature = "embed")]
fn start_embed_model_load_if_needed(embedding_model: EmbeddingModelSelection) -> bool {
    if !AnalystEmbedMode::current().allows_in_process("start_embed_model_load_if_needed") {
        return false;
    }

    let Some(permit) = embed_model_cell(embedding_model).begin_load() else {
        return false;
    };

    let spawn_result = std::thread::Builder::new()
        .name("spur-mcp-embed-warm".into())
        .spawn(move || {
            tracing::info!(
                model = embedding_model.model_name(),
                "Pre-warming embedding model for knowledge_context_pack"
            );
            let load_result = load_embed_model(embedding_model);
            match load_result {
                Ok(model) => {
                    let _ = permit.complete(Some(model));
                    tracing::info!(
                        model = embedding_model.model_name(),
                        "embedding model loaded successfully"
                    );
                }
                Err(error) => {
                    let _ = permit.complete(None);
                    tracing::warn!(
                        %error,
                        model = embedding_model.model_name(),
                        "embedding model failed to load; will retry on a later warm or query"
                    );
                }
            }
        });

    match spawn_result {
        Ok(_handle) => true,
        Err(error) => {
            tracing::warn!(
                %error,
                model = embedding_model.model_name(),
                "failed to spawn embedding model warm-up thread"
            );
            false
        }
    }
}

#[cfg(feature = "embed")]
pub fn warm_embed_model() {
    if !AnalystEmbedMode::current().allows_in_process("warm_embed_model") {
        return;
    }

    let embedding_model = EmbeddingModelSelection::from_env();
    if !start_embed_model_load_if_needed(embedding_model) {
        tracing::debug!(
            model = embedding_model.model_name(),
            "embedding model warm-up skipped; already ready or loading"
        );
    }
}

#[cfg(not(feature = "embed"))]
pub fn warm_embed_model() {}

#[cfg(feature = "embed")]
async fn embed_query(query: &str) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> {
    if !AnalystEmbedMode::current().allows_in_process("embed_query") {
        return None;
    }

    #[cfg(test)]
    if DISABLE_EMBED_QUERY_FOR_TESTS.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }

    let embedding_model = EmbeddingModelSelection::from_env();
    let model_cell = embed_model_cell(embedding_model);
    if !model_cell.is_ready() {
        let load_started = start_embed_model_load_if_needed(embedding_model);
        if model_cell.is_ready() {
            return embed_query_with_ready_model(query, embedding_model).await;
        }
        tracing::debug!(
            load_started,
            model = embedding_model.model_name(),
            env = EMBED_MODEL_ENV,
            "embedding model not ready; degrading to BM25-only search"
        );
        return None;
    }

    embed_query_with_ready_model(query, embedding_model).await
}

#[cfg(feature = "embed")]
async fn embed_query_with_ready_model(
    query: &str,
    embedding_model: EmbeddingModelSelection,
) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> {
    embed_with_ready_model(
        embed_model_cell(embedding_model),
        query,
        EMBED_INFERENCE_TIMEOUT,
        move |model, query| {
            let query = embedding_query_text_for_model(query.as_str(), embedding_model);
            let mut model = model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let embeddings = model.embed(vec![query.as_ref()], None).ok()?;
            let embedding = embeddings.into_iter().next()?;
            embedding.try_into().ok()
        },
    )
    .await
}

#[cfg(feature = "embed")]
async fn embed_with_ready_model<M, F>(
    cell: &EmbedModelCell<M>,
    query: &str,
    timeout_duration: Duration,
    inference: F,
) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]>
where
    M: Send + 'static,
    F: FnOnce(Arc<Mutex<M>>, String) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> + Send + 'static,
{
    let model = cell.ready()?;
    let query = query.to_owned();
    run_embed_inference_with_timeout(timeout_duration, move || inference(model, query)).await
}

#[cfg(feature = "embed")]
async fn run_embed_inference_with_timeout<F>(
    timeout_duration: Duration,
    inference: F,
) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]>
where
    F: FnOnce() -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> + Send + 'static,
{
    let started = Instant::now();
    let result =
        tokio::time::timeout(timeout_duration, tokio::task::spawn_blocking(inference)).await;
    let elapsed = started.elapsed();
    let elapsed_ms = duration_millis(elapsed);
    let timeout_ms = duration_millis(timeout_duration);

    match result {
        Ok(Ok(Some(embedding))) => {
            tracing::debug!(
                elapsed_ms,
                timeout_ms,
                "knowledge_context_pack embed inference completed"
            );
            Some(embedding)
        }
        Ok(Ok(None)) => {
            tracing::warn!(
                elapsed_ms,
                timeout_ms,
                "knowledge_context_pack embed inference failed; degrading to BM25-only search"
            );
            None
        }
        Ok(Err(error)) => {
            tracing::warn!(
                %error,
                elapsed_ms,
                timeout_ms,
                "knowledge_context_pack embed inference task failed; degrading to BM25-only search"
            );
            None
        }
        Err(_timeout) => {
            tracing::warn!(
                elapsed_ms,
                timeout_ms,
                "knowledge_context_pack embed inference timed out; degrading to BM25-only search"
            );
            None
        }
    }
}

#[cfg(feature = "embed")]
fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(not(feature = "embed"))]
#[expect(
    clippy::unused_async,
    reason = "the disabled stub matches the embed-enabled async signature"
)]
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
        return Ok(default.to_owned());
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be a string"
        ))
    })?;
    if allowed.contains(&value) {
        Ok(value.to_owned())
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

fn parse_optional_bool_v2(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, McpHandlerError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack_2 graph_reasoning field '{field}' must be a boolean"
        ))
    })
}

fn parse_clamped_usize_v2(
    object: &serde_json::Map<String, Value>,
    field: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, McpHandlerError> {
    let Some(value) = object.get(field) else {
        return Ok(default);
    };
    let value = value.as_i64().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack_2 graph_reasoning field '{field}' must be an integer"
        ))
    })?;
    Ok(value.clamp(min as i64, max as i64) as usize)
}

fn parse_anchor_array_v2(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, McpHandlerError> {
    let Some(value) = object.get("anchors") else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams(
            "knowledge_context_pack_2 graph_reasoning field 'anchors' must be an array".into(),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|anchor| !anchor.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "knowledge_context_pack_2 graph_reasoning field 'anchors' must contain non-empty strings"
                            .into(),
                    )
                })
        })
        .collect()
}

pub(crate) fn analyst_db_path() -> Result<PathBuf, McpHandlerError> {
    let root = current_repo_root()?;
    Ok(select_analyst_db_path(&root))
}

fn current_repo_root() -> Result<PathBuf, McpHandlerError> {
    if let Some(worktree) = spur_graph::mcp::scoped_worktree_root() {
        return Ok(worktree);
    }
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
    })?;
    Ok(resolve_worktree_root_from(current_dir))
}

fn select_analyst_db_path(root: &Path) -> PathBuf {
    let local_db = root.join(".spur").join("analyst.duckdb");
    if local_db.exists() {
        return local_db;
    }
    parent_spur_worktree_analyst_db(root).unwrap_or(local_db)
}

fn parent_spur_worktree_analyst_db(root: &Path) -> Option<PathBuf> {
    for ancestor in root.ancestors() {
        if ancestor.file_name().is_some_and(|name| name == "worktrees") {
            let spur_dir = ancestor.parent()?;
            if spur_dir.file_name().is_some_and(|name| name == ".spur") {
                let db_path = spur_dir.join("analyst.duckdb");
                if db_path.exists() {
                    return Some(db_path);
                }
            }
        }
    }
    None
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

fn normalized_code_selector(stable_symbol_id: &str) -> String {
    format!("graph://symbol/{}", raw_stable_symbol_id(stable_symbol_id))
}

fn raw_stable_symbol_id(stable_symbol_id: &str) -> &str {
    stable_symbol_id
        .strip_prefix("graph://symbol/")
        .unwrap_or(stable_symbol_id)
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
                    .map(|selector| (selector.to_owned(), index))
            })
            .collect();

        let body_results = join_all(body_selectors.into_iter().map(
            |(selector, index)| async move {
                (
                    index,
                    spur_graph::mcp::code_read_symbol(&json!({
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

#[cfg(test)]
async fn pack_query_result_v2_with_graph_reasoning(
    request: &KnowledgeContextPackV2Request,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    db_path: &Path,
) -> Value {
    let graph_sections =
        graph_reasoning_sections_for_pack(request, &result, &exact_context, db_path);
    pack_query_result_v2_with_graph_sections(request, result, exact_context, graph_sections).await
}

async fn pack_query_result_v2_with_graph_sections(
    request: &KnowledgeContextPackV2Request,
    result: KnowledgeQueryResult,
    exact_context: ExactGraphContext,
    graph_sections: GraphReasoningSections,
) -> Value {
    let mut pack = pack_query_result_with_exact_context(&request.base, result, exact_context).await;
    insert_v2_sections(&mut pack, graph_sections);
    pack
}

#[cfg(test)]
fn graph_reasoning_sections_for_pack(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
    db_path: &Path,
) -> GraphReasoningSections {
    match analyst_matches_exact_graph(result, exact_context) {
        Some(false) if request.graph_reasoning.any_enabled() => {
            stale_graph_reasoning_sections(result, exact_context)
        }
        _ => graph_reasoning_sections(request, result, db_path),
    }
}

fn graph_reasoning_sections_for_pack_with_conn(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
    db_path: &Path,
    conn: &duckdb::Connection,
) -> GraphReasoningSections {
    match analyst_matches_exact_graph(result, exact_context) {
        Some(false) if request.graph_reasoning.any_enabled() => {
            stale_graph_reasoning_sections(result, exact_context)
        }
        _ => graph_reasoning_sections_with_conn(request, result, db_path, conn),
    }
}

fn unavailable_pack_v2(request: &KnowledgeContextPackV2Request, db_path: &Path) -> Value {
    let mut pack = unavailable_pack(&request.base, db_path);
    insert_v2_sections(
        &mut pack,
        GraphReasoningSections {
            caveats: vec![caveat_value(
                "analyst_unavailable",
                format!("analyst DB not found at {}", db_path.display()),
                None,
            )],
            ..GraphReasoningSections::default()
        },
    );
    pack
}

#[derive(Default)]
struct GraphReasoningSections {
    graph_paths: Vec<Value>,
    risk_scorecard: Vec<Value>,
    community_context: Vec<Value>,
    temporal_context: Vec<Value>,
    caveats: Vec<Value>,
}

impl GraphReasoningSections {
    fn with_caveat(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            caveats: vec![caveat_value(code, message, None)],
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GraphPathBudgetPlan {
    target_cap: usize,
    per_target_max_paths: usize,
}

fn path_budget_plan(num_targets: usize, max_paths: usize) -> GraphPathBudgetPlan {
    GraphPathBudgetPlan {
        target_cap: num_targets.min(max_paths),
        per_target_max_paths: max_paths,
    }
}

fn stale_graph_reasoning_sections(
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
) -> GraphReasoningSections {
    let analyst_hash = result.graph_content_hash.as_deref().unwrap_or("<missing>");
    let exact_hash = exact_context
        .graph_content_hash
        .as_deref()
        .unwrap_or("<missing>");
    GraphReasoningSections::with_caveat(
        "analyst_graph_stale",
        format!(
            "analyst DB graph hash {analyst_hash} differs from exact graph hash {exact_hash}; graph reasoning skipped until analyst DB is rebuilt"
        ),
    )
}

impl GraphReasoningOptions {
    fn any_enabled(&self) -> bool {
        self.paths || self.communities || self.risk
    }
}

#[cfg(test)]
fn graph_reasoning_sections(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    db_path: &Path,
) -> GraphReasoningSections {
    let code_symbol_ids = graph_reasoning_code_symbol_ids(&result.candidates, &request.base);
    let mut sections = GraphReasoningSections::default();
    let wants_communities = request
        .graph_reasoning
        .should_query_communities(code_symbol_ids.len());
    let wants_symbol_enrichment = request.graph_reasoning.risk || wants_communities;

    if code_symbol_ids.is_empty() {
        if request.graph_reasoning.paths || wants_symbol_enrichment {
            sections.caveats.push(caveat_value(
                "graph_reasoning_no_code_candidates",
                "graph reasoning sections require grounded code candidates",
                None,
            ));
        }
        return sections;
    }

    if request.graph_reasoning.paths {
        collect_graph_paths(db_path, request, &code_symbol_ids, &mut sections);
    }

    if wants_symbol_enrichment {
        match query_symbol_risk_community(db_path, &code_symbol_ids) {
            Ok(result) => {
                apply_symbol_enrichment_result(request, wants_communities, &mut sections, result);
            }
            Err(error) => sections.caveats.push(caveat_value(
                "symbol_enrichment_unavailable",
                format!("symbol graph enrichment unavailable: {error:#}"),
                None,
            )),
        }
    }

    sections
}

fn graph_reasoning_sections_with_conn(
    request: &KnowledgeContextPackV2Request,
    result: &KnowledgeQueryResult,
    db_path: &Path,
    conn: &duckdb::Connection,
) -> GraphReasoningSections {
    let code_symbol_ids = graph_reasoning_code_symbol_ids(&result.candidates, &request.base);
    let mut sections = GraphReasoningSections::default();
    let wants_communities = request
        .graph_reasoning
        .should_query_communities(code_symbol_ids.len());
    let wants_symbol_enrichment = request.graph_reasoning.risk || wants_communities;

    if code_symbol_ids.is_empty() {
        if request.graph_reasoning.paths || wants_symbol_enrichment {
            sections.caveats.push(caveat_value(
                "graph_reasoning_no_code_candidates",
                "graph reasoning sections require grounded code candidates",
                None,
            ));
        }
        return sections;
    }

    if request.graph_reasoning.paths {
        collect_graph_paths_with_conn(conn, db_path, request, &code_symbol_ids, &mut sections);
    }

    if wants_symbol_enrichment {
        let symbol_enrichment_error =
            match query_symbol_risk_community_with_conn(conn, db_path, &code_symbol_ids) {
                Ok(result) => {
                    apply_symbol_enrichment_result(
                        request,
                        wants_communities,
                        &mut sections,
                        result,
                    );
                    None
                }
                Err(error) => Some(error),
            };
        if let Some(error) = symbol_enrichment_error {
            sections.caveats.push(caveat_value(
                "symbol_enrichment_unavailable",
                format!("symbol graph enrichment unavailable: {error:#}"),
                None,
            ));
        }
    }

    sections
}

fn apply_symbol_enrichment_result(
    request: &KnowledgeContextPackV2Request,
    wants_communities: bool,
    sections: &mut GraphReasoningSections,
    result: crate::SymbolRiskCommunityResult,
) {
    let risk_rows = result.risk_scorecard;
    let community_rows = result.community_context;
    if request.graph_reasoning.risk {
        sections.temporal_context = temporal_context_from_risk_rows(&risk_rows);
        sections.risk_scorecard = risk_rows
            .iter()
            .filter_map(to_json_value)
            .collect::<Vec<_>>();
    } else {
        sections.temporal_context = Vec::new();
    }
    if wants_communities {
        sections.community_context = community_rows
            .iter()
            .filter_map(to_json_value)
            .collect::<Vec<_>>();
    }
    sections
        .caveats
        .extend(result.caveats.iter().map(symbol_caveat_value));
    sections.caveats.extend(risk_rows.iter().flat_map(|row| {
        row.caveats
            .iter()
            .map(symbol_caveat_value)
            .collect::<Vec<_>>()
    }));
    sections
        .caveats
        .extend(community_rows.iter().flat_map(|row| {
            row.caveats
                .iter()
                .map(symbol_caveat_value)
                .collect::<Vec<_>>()
        }));
}

fn graph_reasoning_code_symbol_ids(
    candidates: &[KnowledgeCandidate],
    request: &KnowledgeContextPackRequest,
) -> Vec<String> {
    let mut ids = Vec::new();
    for candidate in candidates {
        if ids.len() >= request.limit as usize {
            break;
        }
        if !request.include_tests && is_test_file(&candidate.file_path) {
            continue;
        }
        if candidate.kind != "code" && candidate.kind != "symbol" {
            continue;
        }
        let Some(stable_symbol_id) = candidate.stable_symbol_id.as_deref() else {
            continue;
        };
        let id = raw_stable_symbol_id(stable_symbol_id).to_owned();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

#[cfg(test)]
fn collect_graph_paths(
    db_path: &Path,
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
) {
    collect_graph_paths_with_query(
        request,
        code_symbol_ids,
        sections,
        |source, target, options| query_context_paths(db_path, source, target, options),
    );
}

fn collect_graph_paths_with_conn(
    conn: &duckdb::Connection,
    db_path: &Path,
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
) {
    collect_graph_paths_with_query(
        request,
        code_symbol_ids,
        sections,
        |source, target, options| {
            query_context_paths_with_conn(conn, db_path, source, target, options)
        },
    );
}

fn collect_graph_paths_with_query<F>(
    request: &KnowledgeContextPackV2Request,
    code_symbol_ids: &[String],
    sections: &mut GraphReasoningSections,
    mut query_paths: F,
) where
    F: FnMut(&str, &str, KnowledgePathOptions) -> anyhow::Result<KnowledgePathResult>,
{
    let Some(source) = code_symbol_ids.first() else {
        return;
    };
    let mut targets = code_symbol_ids.iter().skip(1).cloned().collect::<Vec<_>>();
    targets.extend(resolve_anchor_targets(
        source,
        &request.graph_reasoning.anchors,
        sections,
    ));
    dedupe_preserving_order(&mut targets);

    if targets.is_empty() {
        sections.caveats.push(caveat_value(
            "graph_paths_insufficient_targets",
            "graph path reasoning requires at least two grounded code candidates or a graph://symbol anchor",
            Some(source.clone()),
        ));
        return;
    }

    let budget = path_budget_plan(targets.len(), request.graph_reasoning.max_paths);
    for target in targets.into_iter().take(budget.target_cap) {
        match query_paths(
            source,
            &target,
            KnowledgePathOptions {
                max_hops: request.graph_reasoning.max_path_hops,
                max_paths: budget.per_target_max_paths,
                undirected: true,
            },
        ) {
            Ok(path_result) => {
                if let Some(caveat) = path_result.caveat.as_deref() {
                    push_graph_path_caveat(sections, caveat, source);
                }
                sections.graph_paths.push(json!({
                    "source_stable_id": source,
                    "target_stable_id": target,
                    "graph_content_hash": path_result.graph_content_hash,
                    "max_hops": path_result.max_hops,
                    "max_paths": path_result.max_paths,
                    "engine": path_result.engine,
                    "status": path_result.status,
                    "caveat": path_result.caveat,
                    "rows": path_result.rows,
                }));
            }
            Err(error) => {
                let caveat = format!("context path search unavailable: {error:#}");
                push_graph_path_caveat(sections, caveat.clone(), source);
                sections.graph_paths.push(json!({
                    "source_stable_id": source,
                    "target_stable_id": target,
                    "graph_content_hash": null,
                    "max_hops": request.graph_reasoning.max_path_hops,
                    "max_paths": budget.per_target_max_paths,
                    "engine": "unavailable",
                    "status": "unavailable",
                    "caveat": caveat,
                    "rows": [],
                }));
            }
        }
    }
}

fn push_graph_path_caveat(
    sections: &mut GraphReasoningSections,
    message: impl Into<String>,
    source: &str,
) {
    let caveat = caveat_value("graph_path_unavailable", message, Some(source.to_owned()));
    if !sections.caveats.contains(&caveat) {
        sections.caveats.push(caveat);
    }
}

fn resolve_anchor_targets(
    source: &str,
    anchors: &[String],
    sections: &mut GraphReasoningSections,
) -> Vec<String> {
    anchors
        .iter()
        .filter_map(|anchor| {
            let trimmed = anchor.trim();
            let Some(target) = stable_symbol_anchor(trimmed) else {
                sections.caveats.push(caveat_value(
                    "graph_anchor_unresolved",
                    format!(
                        "anchor {trimmed:?} is not a graph://symbol selector or bare stable symbol id"
                    ),
                    None,
                ));
                return None;
            };
            if target == source {
                sections.caveats.push(caveat_value(
                    "graph_anchor_same_as_source",
                    format!("anchor {trimmed:?} resolves to the source symbol"),
                    Some(source.to_owned()),
                ));
                return None;
            }
            Some(target.to_owned())
        })
        .collect()
}

fn stable_symbol_anchor(anchor: &str) -> Option<&str> {
    if let Some(id) = anchor.strip_prefix("graph://symbol/") {
        return (!id.is_empty()).then_some(id);
    }
    let looks_like_stable_id = anchor.len() >= 8
        && anchor
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    looks_like_stable_id.then_some(anchor)
}

fn dedupe_preserving_order(values: &mut Vec<String>) {
    let mut deduped = Vec::with_capacity(values.len());
    for value in values.drain(..) {
        if !deduped.contains(&value) {
            deduped.push(value);
        }
    }
    *values = deduped;
}

fn temporal_context_from_risk_rows(rows: &[SymbolRiskScorecardRow]) -> Vec<Value> {
    rows.iter()
        .filter(|row| row.status == SymbolEvidenceStatus::Available)
        .filter(|row| row.churn_90d.is_some() || row.last_touched.is_some())
        .map(|row| {
            json!({
                "input_index": row.input_index,
                "stable_symbol_id": row.stable_symbol_id,
                "file_path": row.file_path,
                "churn_90d": row.churn_90d,
                "last_touched": row.last_touched,
                "posture": row.posture,
            })
        })
        .collect()
}

fn insert_v2_sections(pack: &mut Value, sections: GraphReasoningSections) {
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

fn to_json_value<T: serde::Serialize>(value: &T) -> Option<Value> {
    serde_json::to_value(value).ok()
}

fn symbol_caveat_value(caveat: &SymbolEvidenceCaveat) -> Value {
    caveat_value(
        caveat.code.clone(),
        caveat.message.clone(),
        caveat.stable_symbol_id.clone(),
    )
}

fn caveat_value(
    code: impl Into<String>,
    message: impl Into<String>,
    stable_symbol_id: Option<String>,
) -> Value {
    json!({
        "code": code.into(),
        "message": message.into(),
        "stable_symbol_id": stable_symbol_id,
    })
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
        "exact_graph_note": "Exact graph tools remain the source-of-truth follow-up for current working tree source and impact."
    })
}

fn analyst_matches_exact_graph(
    result: &KnowledgeQueryResult,
    exact_context: &ExactGraphContext,
) -> Option<bool> {
    Some(result.graph_content_hash.as_deref()? == exact_context.graph_content_hash.as_deref()?)
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
    use std::process::Command;
    #[cfg(feature = "embed")]
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    #[cfg(feature = "embed")]
    use std::time::Duration;
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    use super::*;
    use crate::query_context_candidates;
    use duckdb::Connection;

    use spur_graph::store::{write_sections_dataset, SECTIONS_DATASET_DIR};
    use spur_graph::{
        artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer,
        EMBEDDING_VECTOR_DIMENSIONS,
    };

    const INIT_SEARCH_SQL: &str = include_str!("../../../spur-context/analyst/init_search.sql");
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static ASYNC_ENV_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

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

    #[tokio::test]
    async fn analyst_db_path_falls_back_to_parent_repo_db_for_spur_worker_worktree() {
        let _lock = async_env_lock().await;
        let repo_dir = tempfile::tempdir().expect("repo tempdir");
        let repo_spur = repo_dir.path().join(".spur");
        let worker_dir = repo_spur.join("worktrees").join("worker-1");
        fs::create_dir_all(&worker_dir).expect("create worker dir");
        fs::write(repo_spur.join("analyst.duckdb"), b"db").expect("write repo analyst db");

        let selected = spur_graph::mcp::with_worktree_root_for_request(worker_dir, async {
            analyst_db_path()
        })
        .await
        .expect("analyst db path");

        assert_eq!(selected, repo_spur.join("analyst.duckdb"));
    }

    #[cfg(feature = "embed")]
    fn test_embedding(first_value: f32) -> [f32; EMBEDDING_VECTOR_DIMENSIONS] {
        let mut embedding = [0.0; EMBEDDING_VECTOR_DIMENSIONS];
        embedding[0] = first_value;
        embedding
    }

    #[derive(Clone, Default)]
    struct TraceCapture {
        events: Arc<Mutex<Vec<CapturedTraceEvent>>>,
    }

    impl TraceCapture {
        fn subscriber(&self) -> CaptureSubscriber {
            CaptureSubscriber {
                events: Arc::clone(&self.events),
            }
        }

        fn contains_warning(&self, needle: &str) -> bool {
            self.events
                .lock()
                .expect("trace events lock")
                .iter()
                .any(|event| event.level == "WARN" && event.fields.contains(needle))
        }
    }

    struct CapturedTraceEvent {
        level: &'static str,
        fields: String,
    }

    struct CaptureSubscriber {
        events: Arc<Mutex<Vec<CapturedTraceEvent>>>,
    }

    impl Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = TraceFieldVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("trace events lock")
                .push(CapturedTraceEvent {
                    level: event.metadata().level().as_str(),
                    fields: visitor.fields,
                });
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[derive(Default)]
    struct TraceFieldVisitor {
        fields: String,
    }

    impl tracing::field::Visit for TraceFieldVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if !self.fields.is_empty() {
                self.fields.push(' ');
            }
            self.fields.push_str(&format!("{}={value:?}", field.name()));
        }
    }

    #[cfg(feature = "embed")]
    #[test]
    fn embed_model_cell_selection_uses_single_gemma_cell() {
        assert!(std::ptr::eq(
            embed_model_cell(EmbeddingModelSelection::EmbeddingGemma300M),
            &EMBEDDING_GEMMA_EMBED_MODEL
        ));
    }

    #[cfg(feature = "embed")]
    #[tokio::test]
    async fn off_embed_mode_never_starts_in_process_model_load() {
        let _mode_guard = set_analyst_embed_mode_for_test(AnalystEmbedMode::Off);
        let model_cell = embed_model_cell(EmbeddingModelSelection::EmbeddingGemma300M);

        assert!(!model_cell.is_ready(), "test assumes model has not loaded");
        assert!(
            !model_cell.is_loading_for_test(),
            "test assumes no previous load is running"
        );

        warm_embed_model();

        assert!(
            !model_cell.is_ready(),
            "off mode must not warm the in-process model"
        );
        assert!(
            !model_cell.is_loading_for_test(),
            "off mode must not mark the model cell as loading"
        );

        assert!(embed_query("ranking beacon").await.is_none());
        assert!(
            !model_cell.is_ready(),
            "off mode query must not load the in-process model"
        );
        assert!(
            !model_cell.is_loading_for_test(),
            "off mode query must not start a background load"
        );
    }

    #[test]
    fn unknown_embed_mode_falls_back_to_auto_and_warns() {
        let captured = TraceCapture::default();

        let mode = tracing::subscriber::with_default(captured.subscriber(), || {
            AnalystEmbedMode::parse_env_value("mystery-mode")
        });

        assert_eq!(mode, AnalystEmbedMode::Auto);
        assert!(
            captured.contains_warning("unknown analyst embed mode")
                && captured.contains_warning("mystery-mode")
                && captured.contains_warning("SPUR_ANALYST_EMBED_MODE"),
            "unknown mode should emit a warning with the bad value and env var"
        );
    }

    #[cfg(feature = "embed")]
    #[test]
    fn embed_model_cell_retries_after_transient_load_failure() {
        let cell = EmbedModelCell::<u32>::new();
        let mut attempts = 0;

        assert!(cell
            .load_if_idle(|| {
                attempts += 1;
                None
            })
            .is_none());
        assert_eq!(attempts, 1);

        let model = cell
            .load_if_idle(|| {
                attempts += 1;
                Some(7)
            })
            .expect("second load should succeed");
        assert_eq!(
            *model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            7
        );
        assert_eq!(attempts, 2);

        let model = cell
            .load_if_idle(|| {
                attempts += 1;
                Some(9)
            })
            .expect("ready model should be reused");
        assert_eq!(
            *model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            7
        );
        assert_eq!(attempts, 2, "ready model should not be reloaded");
    }

    #[cfg(feature = "embed")]
    #[tokio::test]
    async fn embed_with_ready_model_falls_back_while_load_in_progress() {
        let cell = EmbedModelCell::<u32>::new();
        let _permit = cell.begin_load().expect("load should begin");
        let inference_called = Arc::new(AtomicBool::new(false));
        let called = Arc::clone(&inference_called);

        let result =
            embed_with_ready_model(&cell, "query", Duration::from_millis(25), move |_, _| {
                called.store(true, Ordering::SeqCst);
                Some(test_embedding(1.0))
            })
            .await;

        assert!(result.is_none());
        assert!(
            !inference_called.load(Ordering::SeqCst),
            "inference must not run while the model is still loading"
        );
    }

    #[cfg(feature = "embed")]
    #[tokio::test]
    async fn embed_with_ready_model_times_out_inference_only() {
        let cell = EmbedModelCell::<u32>::new();
        cell.load_if_idle(|| Some(42))
            .expect("test model should load");

        let result =
            embed_with_ready_model(&cell, "query", Duration::from_millis(10), move |_, _| {
                std::thread::sleep(Duration::from_millis(100));
                Some(test_embedding(1.0))
            })
            .await;

        assert!(result.is_none());
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

    fn doc_candidate(
        stable_symbol_id: Option<&str>,
        title: &str,
        score: f64,
    ) -> KnowledgeCandidate {
        KnowledgeCandidate {
            kind: "doc".into(),
            title: title.into(),
            file_path: "docs/context.md".into(),
            stable_symbol_id: stable_symbol_id.map(str::to_string),
            symbol_kind: Some("section".into()),
            score,
            signal: None,
            neighbor_kind: None,
            edge_bind_method: None,
            grounding: "test-doc".into(),
        }
    }

    fn minimal_analyst_db_with_meta() -> (tempfile::TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(
            r#"
            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('fixture-hash');
            "#,
        )
        .expect("create fixture meta");
        drop(conn);
        (temp_dir, db_path)
    }

    fn analyst_db_with_path_budget_fixture() -> (tempfile::TempDir, PathBuf) {
        let (temp_dir, db_path) = minimal_analyst_db_with_meta();
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(
            r#"
            CREATE TABLE edges (
                source_stable_id VARCHAR,
                target_stable_id VARCHAR,
                relation VARCHAR,
                edge_kind VARCHAR,
                confidence VARCHAR,
                bind_method VARCHAR
            );
            INSERT INTO edges VALUES
                ('sym-source', 'sym-connected', 'calls', 'calls', 'syntax_exact', 'singleton');
            "#,
        )
        .expect("create path budget fixture");
        drop(conn);
        (temp_dir, db_path)
    }

    fn analyst_db_with_graph_reasoning_views() -> (tempfile::TempDir, PathBuf) {
        let (temp_dir, db_path) = minimal_analyst_db_with_meta();
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(
            r#"
            CREATE TABLE v_symbol_scorecard (
                stable_symbol_id VARCHAR,
                entity_name VARCHAR,
                qualified_name VARCHAR,
                symbol_kind VARCHAR,
                file_path VARCHAR,
                pagerank DOUBLE,
                in_degree BIGINT,
                out_degree BIGINT,
                callers BIGINT,
                importers BIGINT,
                inbound_total BIGINT,
                churn_90d BIGINT,
                last_touched TIMESTAMP,
                blast_radius_score DOUBLE,
                posture VARCHAR
            );
            INSERT INTO v_symbol_scorecard VALUES
                ('sym-one', 'symbol_one', 'fixture::symbol_one', 'function', 'src/one.rs',
                 0.42, 7, 3, 5, 1, 6, 9, TIMESTAMP '2026-06-17 12:00:00', 0.91, 'active'),
                ('sym-two', 'symbol_two', 'fixture::symbol_two', 'function', 'src/two.rs',
                 0.21, 2, 1, 1, 0, 1, 0, NULL, 0.12, 'stable');

            CREATE TABLE v_symbol_component (
                stable_symbol_id VARCHAR,
                component_id BIGINT,
                component_size BIGINT
            );
            INSERT INTO v_symbol_component VALUES
                ('sym-one', 10, 4),
                ('sym-two', 10, 4);

            CREATE TABLE v_symbol_community (
                stable_symbol_id VARCHAR,
                community_id BIGINT
            );
            INSERT INTO v_symbol_community VALUES
                ('sym-one', 20),
                ('sym-two', 20);

            CREATE TABLE v_graph_metrics (
                calls_edges BIGINT,
                connected_nodes BIGINT,
                components BIGINT,
                largest_component BIGINT,
                communities BIGINT,
                density DOUBLE
            );
            INSERT INTO v_graph_metrics VALUES (12, 6, 1, 6, 2, 0.18);
            "#,
        )
        .expect("create graph reasoning fixture views");
        drop(conn);
        (temp_dir, db_path)
    }

    fn git(worktree: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(worktree)
            .output()
            .unwrap_or_else(|error| panic!("run git {}: {error}", args.join(" ")));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git stdout UTF-8")
    }

    fn commit_fixture(worktree: &Path) {
        git(worktree, &["init", "-q"]);
        git(worktree, &["config", "user.email", "test@spur"]);
        git(worktree, &["config", "user.name", "SPUR Test"]);
        git(worktree, &["add", "."]);
        git(worktree, &["commit", "-m", "fixture"]);
    }

    fn write_graph_artifact_for_test(worktree: &Path, artifact: &spur_graph::GraphIndexArtifact) {
        let artifact_dir = worktree.join(".spur/graph/test-artifact.parquet");
        let written = write_artifact_parquet(
            artifact,
            &artifact_dir,
            spur_graph::WriteOptions::default(),
            Vec::new(),
        )
        .expect("write graph artifact");
        write_current_pointer(worktree, &written).expect("write graph CURRENT pointer");
    }

    fn write_minimal_graph_fixture(worktree: &Path, source: &str) {
        fs::create_dir_all(worktree.join("src")).expect("create src dir");
        fs::write(
            worktree.join("Cargo.toml"),
            "[package]\nname = \"kcp-graph-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write fixture manifest");
        fs::write(worktree.join("src/lib.rs"), source).expect("write fixture source");
    }

    fn kcp2_fixture_repo(include_graph_reasoning_views: bool) -> (tempfile::TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let repo = temp_dir.path().join("repo");
        fs::create_dir_all(repo.join(".spur")).expect("create .spur");
        let db_path = repo.join(".spur").join("analyst.duckdb");
        let conn = Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(
            "INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;",
        )
        .expect("load fixture extensions");
        conn.execute_batch(
            r#"
            CREATE TABLE _meta (graph_content_hash VARCHAR);
            INSERT INTO _meta VALUES ('kcp2-fixture-hash');

            CREATE TABLE sections_search (
                stable_symbol_id VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                heading_level INTEGER,
                content_hash VARCHAR,
                body_text VARCHAR
            );
            INSERT INTO sections_search VALUES
                ('doc-dispatch', 'Dispatch Approval Reading Path', 'docs/dispatch.md', 2, 'doc-hash',
                 'dispatch approval evidence reading path');

            CREATE TABLE symbol_text (
                stable_symbol_id VARCHAR,
                entity_name VARCHAR,
                qualified_name VARCHAR,
                file_path VARCHAR,
                symbol_kind VARCHAR,
                doc_text VARCHAR
            );
            INSERT INTO symbol_text VALUES
                ('sym-dispatch', 'dispatch_plan', 'fixture::dispatch_plan',
                 'src/dispatch.rs', 'function', 'dispatch approval evidence entry point'),
                ('sym-review', 'review_approval', 'fixture::review_approval',
                 'src/review.rs', 'function', 'dispatch approval evidence review path');

            CREATE TABLE v_symbol_scorecard (
                stable_symbol_id VARCHAR,
                entity_name VARCHAR,
                qualified_name VARCHAR,
                symbol_kind VARCHAR,
                file_path VARCHAR,
                pagerank DOUBLE,
                in_degree BIGINT,
                out_degree BIGINT,
                callers BIGINT,
                importers BIGINT,
                inbound_total BIGINT,
                churn_90d BIGINT,
                last_touched TIMESTAMP,
                blast_radius_score DOUBLE,
                posture VARCHAR
            );
            INSERT INTO v_symbol_scorecard VALUES
                ('sym-dispatch', 'dispatch_plan', 'fixture::dispatch_plan', 'function', 'src/dispatch.rs',
                 0.42, 7, 3, 11, 2, 13, 9, TIMESTAMP '2026-06-17 12:00:00', 0.91, 'load-bearing wall'),
                ('sym-review', 'review_approval', 'fixture::review_approval', 'function', 'src/review.rs',
                 0.21, 2, 1, 3, 0, 3, 1, TIMESTAMP '2026-06-16 09:30:00', 0.33, 'stable');

            CREATE TABLE v_symbol_inbound (
                stable_symbol_id VARCHAR,
                callers BIGINT
            );
            INSERT INTO v_symbol_inbound VALUES
                ('sym-dispatch', 11),
                ('sym-review', 3);
            "#,
        )
        .expect("create kcp2 candidate fixture schema");
        if include_graph_reasoning_views {
            conn.execute_batch(
                r#"
                CREATE TABLE nodes (
                    stable_symbol_id VARCHAR,
                    node_id BIGINT,
                    file_path VARCHAR,
                    entity_name VARCHAR,
                    qualified_name VARCHAR,
                    symbol_kind VARCHAR
                );
                INSERT INTO nodes VALUES
                    ('sym-dispatch', 1, 'src/dispatch.rs', 'dispatch_plan', 'fixture::dispatch_plan', 'function'),
                    ('sym-review', 2, 'src/review.rs', 'review_approval', 'fixture::review_approval', 'function');

                CREATE TABLE edges (
                    source_stable_id VARCHAR,
                    target_stable_id VARCHAR,
                    src_id BIGINT,
                    dst_id BIGINT,
                    target_label VARCHAR,
                    relation VARCHAR,
                    confidence VARCHAR,
                    confidence_score FLOAT,
                    edge_kind VARCHAR,
                    bind_method VARCHAR
                );
                INSERT INTO edges VALUES
                    ('sym-dispatch', 'sym-review', 1, 2, 'review_approval', 'calls', 'syntax_exact', 1.0, 'calls', 'singleton');

                CREATE TABLE v_symbol_component (
                    stable_symbol_id VARCHAR,
                    component_id BIGINT,
                    component_size BIGINT
                );
                INSERT INTO v_symbol_component VALUES
                    ('sym-dispatch', 10, 2),
                    ('sym-review', 10, 2);

                CREATE TABLE v_symbol_community (
                    stable_symbol_id VARCHAR,
                    community_id BIGINT
                );
                INSERT INTO v_symbol_community VALUES
                    ('sym-dispatch', 20),
                    ('sym-review', 20);

                CREATE TABLE v_graph_metrics (
                    calls_edges BIGINT,
                    connected_nodes BIGINT,
                    components BIGINT,
                    largest_component BIGINT,
                    communities BIGINT,
                    density DOUBLE
                );
                INSERT INTO v_graph_metrics VALUES (1, 2, 1, 2, 1, 0.5);
                "#,
            )
            .expect("create kcp2 graph reasoning fixture schema");
        }
        conn.execute_batch(
            r#"
            PRAGMA create_fts_index('main.sections_search', 'stable_symbol_id', 'body_text', overwrite=1, stemmer='porter');
            PRAGMA create_fts_index('main.symbol_text', 'stable_symbol_id', 'doc_text', overwrite=1);
            "#,
        )
        .expect("create kcp2 fixture fts indexes");
        let macro_sql = context_candidate_macro_sql();
        conn.execute_batch(&macro_sql)
            .expect("define kcp2 fixture context search macro");
        drop(conn);
        (temp_dir, repo)
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

    async fn async_env_lock() -> tokio::sync::MutexGuard<'static, ()> {
        ASYNC_ENV_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    #[cfg(feature = "embed")]
    struct EmbedQueryDisableGuard {
        previous: bool,
    }

    #[cfg(feature = "embed")]
    impl Drop for EmbedQueryDisableGuard {
        fn drop(&mut self) {
            DISABLE_EMBED_QUERY_FOR_TESTS.store(self.previous, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[cfg(feature = "embed")]
    fn disable_embed_query_for_test() -> EmbedQueryDisableGuard {
        let previous =
            DISABLE_EMBED_QUERY_FOR_TESTS.swap(true, std::sync::atomic::Ordering::SeqCst);
        EmbedQueryDisableGuard { previous }
    }

    #[cfg(not(feature = "embed"))]
    fn disable_embed_query_for_test() {}

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
        conn.execute_batch(
            "INSTALL fts; LOAD fts; INSTALL icu; LOAD icu; INSTALL lance; LOAD lance;",
        )
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
                .map(|tool| tool["tool"].as_str().expect("tool name").to_owned())
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
        let _lock = async_env_lock().await;
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".spur")).expect("create .spur");

        let result = spur_graph::mcp::with_worktree_root_for_request(repo.clone(), async {
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
    fn knowledge_context_pack_2_parser_clamps_graph_reasoning_budgets() {
        let high = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": true,
                "max_path_hops": 999,
                "max_paths": 999,
                "anchors": ["graph://symbol/anchor-one"]
            }
        }))
        .expect("high budget request");

        assert_eq!(high.base.intent.as_str(), "review");
        assert!(high.graph_reasoning.paths);
        assert!(high.graph_reasoning.communities);
        assert!(high.graph_reasoning.risk);
        assert_eq!(
            high.graph_reasoning.max_path_hops,
            crate::MAX_CONTEXT_PATH_HOPS
        );
        assert_eq!(high.graph_reasoning.max_paths, crate::MAX_CONTEXT_PATHS);
        assert_eq!(
            high.graph_reasoning.anchors,
            vec!["graph://symbol/anchor-one".to_owned()]
        );

        let low = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "graph_reasoning": {
                "max_path_hops": 0,
                "max_paths": 0
            }
        }))
        .expect("low budget request");
        assert_eq!(low.graph_reasoning.max_path_hops, 1);
        assert_eq!(low.graph_reasoning.max_paths, 1);
    }

    #[test]
    fn path_budget_plan_caps_targets_without_shrinking_per_target_limit() {
        const MAX_PATHS: usize = 4;
        let plan = path_budget_plan(6, MAX_PATHS);

        assert_eq!(plan.target_cap, MAX_PATHS);
        assert_eq!(plan.per_target_max_paths, MAX_PATHS);

        let target_outcomes = [
            "no_path",
            "path_found",
            "no_path",
            "path_found",
            "path_found",
        ];
        let processed_limits = target_outcomes
            .iter()
            .take(plan.target_cap)
            .map(|_| plan.per_target_max_paths)
            .collect::<Vec<_>>();
        assert_eq!(
            processed_limits,
            vec![MAX_PATHS; MAX_PATHS],
            "target outcomes must not feed back into per-target path limits"
        );

        let smaller_target_set = path_budget_plan(2, MAX_PATHS);
        assert_eq!(smaller_target_set.target_cap, 2);
        assert_eq!(smaller_target_set.per_target_max_paths, MAX_PATHS);
    }

    #[test]
    fn collect_graph_paths_keeps_full_per_target_limit_after_no_path() {
        let (_temp_dir, db_path) = analyst_db_with_path_budget_fixture();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "graph_reasoning": {
                "paths": true,
                "max_path_hops": 2,
                "max_paths": 2
            }
        }))
        .expect("request");
        let mut sections = GraphReasoningSections::default();
        let code_symbol_ids = vec![
            "sym-source".to_owned(),
            "sym-disconnected".to_owned(),
            "sym-connected".to_owned(),
            "sym-late".to_owned(),
        ];

        collect_graph_paths(&db_path, &request, &code_symbol_ids, &mut sections);

        assert_eq!(
            sections.graph_paths.len(),
            2,
            "processed targets should be capped by max_paths"
        );
        assert_eq!(
            sections
                .graph_paths
                .iter()
                .map(|path| path["target_stable_id"].as_str().expect("target id"))
                .collect::<Vec<_>>(),
            vec!["sym-disconnected", "sym-connected"]
        );
        assert_eq!(sections.graph_paths[0]["status"], "no_path");
        assert_eq!(sections.graph_paths[1]["status"], "path_found");
        assert_eq!(
            sections
                .graph_paths
                .iter()
                .map(|path| path["max_paths"].as_u64().expect("max paths"))
                .collect::<Vec<_>>(),
            vec![2, 2],
            "a disconnected target must not shrink the later target's path limit"
        );
    }

    #[test]
    fn collect_graph_paths_dedupes_repeated_no_path_caveats_for_source() {
        let (_temp_dir, db_path) = analyst_db_with_path_budget_fixture();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "graph_reasoning": {
                "paths": true,
                "max_path_hops": 2,
                "max_paths": 3
            }
        }))
        .expect("request");
        let mut sections = GraphReasoningSections::default();
        let code_symbol_ids = vec![
            "sym-source".to_owned(),
            "sym-disconnected-one".to_owned(),
            "sym-disconnected-two".to_owned(),
        ];

        collect_graph_paths(&db_path, &request, &code_symbol_ids, &mut sections);

        let graph_path_caveats = sections
            .caveats
            .iter()
            .filter(|caveat| caveat["code"] == "graph_path_unavailable")
            .collect::<Vec<_>>();
        assert_eq!(
            graph_path_caveats.len(),
            1,
            "identical no_path caveats for one source should collapse"
        );
        assert_eq!(
            graph_path_caveats[0]["message"],
            "no undirected path found within 2 hops"
        );
        assert_eq!(graph_path_caveats[0]["stable_symbol_id"], "sym-source");
    }

    #[test]
    fn knowledge_context_pack_2_parser_defaults_graph_reasoning_by_intent_and_scope() {
        let change = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "change"
        }))
        .expect("change request");
        assert!(change.graph_reasoning.paths);
        assert!(change.graph_reasoning.risk);

        let explain_docs = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "explain",
            "scope": "docs"
        }))
        .expect("docs request");
        assert!(!explain_docs.graph_reasoning.paths);
        assert!(!explain_docs.graph_reasoning.risk);
    }

    #[tokio::test]
    async fn knowledge_context_pack_v1_response_omits_v2_sections() {
        let request = KnowledgeContextPackRequest::parse(&json!({
            "query": "semantic search"
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: "/repo/.spur/analyst.duckdb".into(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![candidate(Some("sym-one"), "symbol_one", 7.0)],
        };

        let pack = pack_query_result(&request, result).await;

        assert!(pack.get("graph_paths").is_none());
        assert!(pack.get("risk_scorecard").is_none());
        assert!(pack.get("community_context").is_none());
        assert!(pack.get("temporal_context").is_none());
        assert!(pack.get("caveats").is_none());
    }

    #[tokio::test]
    async fn knowledge_context_pack_uses_single_connection_for_candidate_queries() {
        let _lock = async_env_lock().await;
        let _embed_guard = disable_embed_query_for_test();
        let (_temp_dir, repo) = kcp2_fixture_repo(true);
        let db_path = repo.join(".spur").join("analyst.duckdb");
        crate::reset_analyst_connection_open_count_for_test(&db_path);

        let pack = spur_graph::mcp::with_worktree_root_for_request(repo, async {
            knowledge_context_pack(&json!({
                "query": "dispatch approval evidence",
                "intent": "change",
                "scope": "all",
                "limit": 5
            }))
            .await
        })
        .await
        .expect("v1 fixture response");

        assert!(pack.get("error").is_none(), "{pack:#}");
        assert_eq!(
            crate::analyst_connection_open_count_for_test(&db_path),
            1,
            "v1 candidate and graph retrieval should share one analyst connection"
        );
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_uses_single_connection_for_pack_request() {
        let _lock = async_env_lock().await;
        let _embed_guard = disable_embed_query_for_test();
        let (_temp_dir, repo) = kcp2_fixture_repo(true);
        let db_path = repo.join(".spur").join("analyst.duckdb");
        crate::reset_analyst_connection_open_count_for_test(&db_path);

        let pack = spur_graph::mcp::with_worktree_root_for_request(repo, async {
            knowledge_context_pack_2(&json!({
                "query": "dispatch approval evidence",
                "intent": "review",
                "scope": "all",
                "limit": 5,
                "graph_reasoning": {
                    "paths": true,
                    "communities": true,
                    "risk": true,
                    "max_path_hops": 2,
                    "max_paths": 1
                }
            }))
            .await
        })
        .await
        .expect("kcp2 fixture response");

        assert!(pack.get("error").is_none(), "{pack:#}");
        assert_eq!(
            crate::analyst_connection_open_count_for_test(&db_path),
            1,
            "v2 candidates, paths, and symbol enrichment should share one analyst connection"
        );
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_preserves_v1_fields_and_adds_empty_v2_sections_when_disabled()
    {
        let (_temp_dir, db_path) = minimal_analyst_db_with_meta();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "change",
            "scope": "code",
            "limit": 4,
            "include_tests": false,
            "max_symbol_bodies": 0,
            "graph_reasoning": {
                "paths": false,
                "communities": false,
                "risk": false
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-one"), "symbol_one", 7.5),
                doc_candidate(Some("doc-one"), "Context Doc", 3.0),
            ],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("fixture-hash".into()),
                response_file_oids_match: Some(true),
                impacts: Vec::new(),
            },
            &db_path,
        )
        .await;

        assert_eq!(pack["query"], "semantic search");
        assert_eq!(pack["intent"], "change");
        assert_eq!(pack["scope"], "code");
        assert_eq!(pack["graph_content_hash"], "fixture-hash");
        assert_eq!(
            pack["staleness"]["analyst_graph_content_hash"],
            "fixture-hash"
        );
        assert_eq!(pack["candidates"]["total"], 2);
        assert_eq!(pack["candidates"]["returned_primary"], 1);
        assert_eq!(pack["candidates"]["returned_supporting_docs"], 1);
        assert_eq!(
            pack["primary_evidence"][0]["stable_symbol_id"],
            "graph://symbol/sym-one"
        );
        assert_eq!(pack["supporting_docs"][0]["stable_symbol_id"], "doc-one");
        assert_eq!(
            pack["recommended_next_tools"][0]["selector"],
            "graph://symbol/sym-one"
        );
        assert_eq!(pack["graph_paths"], json!([]));
        assert_eq!(pack["risk_scorecard"], json!([]));
        assert_eq!(pack["community_context"], json!([]));
        assert_eq!(pack["temporal_context"], json!([]));
        assert_eq!(pack["caveats"], json!([]));
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_missing_graph_views_return_caveats_not_error() {
        let (_temp_dir, db_path) = minimal_analyst_db_with_meta();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "scope": "code",
            "limit": 2,
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": true,
                "max_path_hops": 2,
                "max_paths": 1
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-one"), "symbol_one", 8.0),
                candidate(Some("sym-two"), "symbol_two", 7.0),
            ],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext::default(),
            &db_path,
        )
        .await;

        assert!(pack.get("error").is_none(), "v2 graph failures are caveats");
        let caveat_codes = pack["caveats"]
            .as_array()
            .expect("caveats")
            .iter()
            .filter_map(|caveat| caveat["code"].as_str())
            .collect::<Vec<_>>();
        assert!(caveat_codes.contains(&"scorecard_unavailable"));
        assert!(caveat_codes.contains(&"community_unavailable"));
        assert!(caveat_codes.contains(&"graph_metrics_unavailable"));
        assert!(caveat_codes.contains(&"graph_path_unavailable"));
        assert_eq!(pack["risk_scorecard"][0]["status"], "unavailable");
        assert_eq!(pack["community_context"][0]["status"], "unavailable");
        assert_eq!(pack["graph_paths"][0]["status"], "unavailable");
        assert_eq!(pack["graph_paths"][0]["rows"][0]["status"], "unavailable");
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_returns_temporal_context_from_scorecard_when_available() {
        let (_temp_dir, db_path) = analyst_db_with_graph_reasoning_views();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "scope": "code",
            "limit": 2,
            "graph_reasoning": {
                "paths": false,
                "communities": true,
                "risk": true
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-one"), "symbol_one", 8.0),
                candidate(Some("sym-two"), "symbol_two", 7.0),
            ],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext::default(),
            &db_path,
        )
        .await;

        assert_eq!(pack["risk_scorecard"][0]["status"], "available");
        assert_eq!(pack["risk_scorecard"][0]["churn_90d"], 9);
        assert_eq!(pack["community_context"][0]["status"], "available");
        assert_eq!(pack["community_context"][0]["component_id"], 10);
        assert_eq!(pack["temporal_context"][0]["stable_symbol_id"], "sym-one");
        assert_eq!(pack["temporal_context"][0]["churn_90d"], 9);
        assert!(pack["temporal_context"][0]["last_touched"]
            .as_str()
            .expect("last touched")
            .contains("2026-06-17"));
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_suppresses_graph_reasoning_when_analyst_hash_is_stale() {
        let (_temp_dir, db_path) = analyst_db_with_graph_reasoning_views();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "scope": "code",
            "limit": 2,
            "graph_reasoning": {
                "paths": true,
                "communities": true,
                "risk": true,
                "max_path_hops": 2,
                "max_paths": 1
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-one"), "symbol_one", 8.0),
                candidate(Some("sym-two"), "symbol_two", 7.0),
            ],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("exact-graph-hash".into()),
                response_file_oids_match: Some(true),
                impacts: Vec::new(),
            },
            &db_path,
        )
        .await;

        assert_eq!(
            pack["staleness"]["analyst_graph_content_hash"],
            "fixture-hash"
        );
        assert_eq!(pack["staleness"]["exact_graph_hash"], "exact-graph-hash");
        assert_eq!(pack["staleness"]["analyst_matches_exact_graph"], false);
        assert_eq!(pack["graph_paths"], json!([]));
        assert_eq!(pack["risk_scorecard"], json!([]));
        assert_eq!(pack["community_context"], json!([]));
        assert_eq!(pack["temporal_context"], json!([]));
        assert!(pack["caveats"]
            .as_array()
            .expect("caveats")
            .iter()
            .any(|caveat| caveat["code"] == "analyst_graph_stale"));
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_staleness_uses_rebuilt_graph_hash() {
        let _lock = async_env_lock().await;
        let worktree = tempfile::tempdir().expect("worktree tempdir");
        write_minimal_graph_fixture(
            worktree.path(),
            "pub fn stable_symbol() -> bool {\n    true\n}\n",
        );
        commit_fixture(worktree.path());

        let (facts, _file_counts) = build_facts(worktree.path(), None).expect("build facts");
        let stale_artifact =
            artifact_from_facts(&facts, worktree.path()).expect("build stale graph artifact");
        let stale_graph_hash = stale_artifact.graph_content_hash.clone();
        let stable_symbol_id = stale_artifact
            .symbols
            .iter()
            .find(|symbol| symbol.entity_name == "stable_symbol")
            .expect("stable symbol is indexed")
            .stable_symbol_id
            .clone();
        write_graph_artifact_for_test(worktree.path(), &stale_artifact);

        fs::write(
            worktree.path().join("src/lib.rs"),
            "pub fn stable_symbol() -> bool {\n    true\n}\n\npub fn live_symbol() -> bool {\n    stable_symbol()\n}\n",
        )
        .expect("dirty fixture source");

        let (_db_dir, db_path) = minimal_analyst_db_with_meta();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "stable symbol",
            "intent": "review",
            "scope": "code",
            "graph_reasoning": {
                "paths": false,
                "communities": false,
                "risk": false
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some(stale_graph_hash.clone()),
            candidates: vec![candidate(Some(&stable_symbol_id), "stable_symbol", 9.0)],
        };

        let exact_context =
            spur_graph::mcp::with_worktree_root_for_request(worktree.path().to_path_buf(), async {
                exact_graph_context_for_result(&request.base, &result).await
            })
            .await;
        let pack =
            pack_query_result_v2_with_graph_reasoning(&request, result, exact_context, &db_path)
                .await;

        assert_eq!(
            pack["staleness"]["analyst_graph_content_hash"],
            stale_graph_hash
        );
        assert_ne!(
            pack["staleness"]["exact_graph_hash"], stale_graph_hash,
            "exact graph hash must come from the rebuilt live graph, not the stale loaded artifact",
        );
        assert_eq!(pack["staleness"]["exact_graph_verified"], true);
        assert_eq!(pack["staleness"]["analyst_matches_exact_graph"], false);
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_bounds_path_and_risk_output() {
        let (_temp_dir, db_path) = minimal_analyst_db_with_meta();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "semantic search",
            "intent": "review",
            "scope": "code",
            "limit": 3,
            "graph_reasoning": {
                "paths": true,
                "communities": false,
                "risk": true,
                "max_path_hops": 2,
                "max_paths": 1
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![
                candidate(Some("sym-one"), "symbol_one", 9.0),
                candidate(Some("sym-two"), "symbol_two", 8.0),
                candidate(Some("sym-three"), "symbol_three", 7.0),
                candidate(Some("sym-four"), "symbol_four", 6.0),
                candidate(Some("sym-five"), "symbol_five", 5.0),
            ],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext::default(),
            &db_path,
        )
        .await;

        assert!(
            pack["risk_scorecard"].as_array().expect("risk").len() <= 3,
            "risk rows should be bounded by the request limit"
        );
        let path_rows = pack["graph_paths"]
            .as_array()
            .expect("graph paths")
            .iter()
            .map(|path| path["rows"].as_array().map_or(0, Vec::len))
            .sum::<usize>();
        assert!(
            path_rows <= 1,
            "path rows should be bounded by graph_reasoning.max_paths"
        );
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_reads_fixture_db_end_to_end() {
        let _lock = async_env_lock().await;
        let _embed_guard = disable_embed_query_for_test();
        let (_temp_dir, repo) = kcp2_fixture_repo(true);

        let pack = spur_graph::mcp::with_worktree_root_for_request(repo, async {
            knowledge_context_pack_2(&json!({
                "query": "dispatch approval evidence",
                "intent": "review",
                "scope": "all",
                "limit": 5,
                "graph_reasoning": {
                    "paths": true,
                    "communities": true,
                    "risk": true,
                    "max_path_hops": 2,
                    "max_paths": 1
                }
            }))
            .await
        })
        .await
        .expect("kcp2 fixture response");

        assert!(pack.get("error").is_none(), "{pack:#}");
        assert_eq!(pack["query"], "dispatch approval evidence");
        assert_eq!(pack["graph_content_hash"], "kcp2-fixture-hash");
        assert_eq!(pack["answerable"], true);
        assert!(
            pack["primary_evidence"]
                .as_array()
                .expect("primary evidence")
                .iter()
                .any(|evidence| evidence["stable_symbol_id"] == "graph://symbol/sym-dispatch"),
            "primary_evidence should include the dispatch symbol: {pack:#}"
        );
        assert!(
            pack["supporting_docs"]
                .as_array()
                .expect("supporting docs")
                .iter()
                .any(|doc| doc["stable_symbol_id"] == "doc-dispatch"),
            "supporting_docs should include the fixture doc: {pack:#}"
        );
        assert_eq!(pack["graph_paths"][0]["source_stable_id"], "sym-dispatch");
        assert_eq!(pack["graph_paths"][0]["target_stable_id"], "sym-review");
        assert_eq!(pack["graph_paths"][0]["status"], "path_found");
        assert_eq!(pack["graph_paths"][0]["engine"], "recursive_sql");
        assert_eq!(pack["graph_paths"][0]["rows"][0]["relation"], "calls");
        assert_eq!(pack["risk_scorecard"][0]["status"], "available");
        assert_eq!(
            pack["risk_scorecard"][0]["stable_symbol_id"],
            "sym-dispatch"
        );
        assert_eq!(pack["risk_scorecard"][0]["churn_90d"], 9);
        assert_eq!(pack["community_context"][0]["status"], "available");
        assert_eq!(
            pack["community_context"][0]["stable_symbol_id"],
            "sym-dispatch"
        );
        assert_eq!(pack["community_context"][0]["component_id"], 10);
        assert_eq!(pack["community_context"][0]["community_id"], 20);
        assert_eq!(
            pack["recommended_next_tools"][0]["selector"],
            "graph://symbol/sym-dispatch"
        );
        assert!(
            pack["caveats"].as_array().expect("caveats").is_empty(),
            "complete fixture should not emit caveats: {pack:#}"
        );
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_missing_graph_views_keeps_candidates_and_returns_caveats() {
        let _lock = async_env_lock().await;
        let _embed_guard = disable_embed_query_for_test();
        let (_temp_dir, repo) = kcp2_fixture_repo(false);

        let pack = spur_graph::mcp::with_worktree_root_for_request(repo, async {
            knowledge_context_pack_2(&json!({
                "query": "dispatch approval evidence",
                "intent": "review",
                "scope": "all",
                "limit": 5,
                "graph_reasoning": {
                    "paths": true,
                    "communities": true,
                    "risk": true,
                    "max_path_hops": 2,
                    "max_paths": 1
                }
            }))
            .await
        })
        .await
        .expect("kcp2 missing-view fixture response");

        assert!(pack.get("error").is_none(), "{pack:#}");
        assert!(
            !pack["primary_evidence"]
                .as_array()
                .expect("primary evidence")
                .is_empty(),
            "missing graph views should not suppress retrieved candidates: {pack:#}"
        );
        assert!(
            !pack["recommended_next_tools"]
                .as_array()
                .expect("recommended next tools")
                .is_empty(),
            "candidate follow-up tools should still be present: {pack:#}"
        );
        assert_eq!(pack["risk_scorecard"][0]["status"], "available");
        assert_eq!(pack["community_context"][0]["status"], "unavailable");
        assert_eq!(pack["graph_paths"][0]["status"], "unavailable");
        let caveat_codes = pack["caveats"]
            .as_array()
            .expect("caveats")
            .iter()
            .filter_map(|caveat| caveat["code"].as_str())
            .collect::<Vec<_>>();
        assert!(caveat_codes.contains(&"community_unavailable"));
        assert!(caveat_codes.contains(&"graph_metrics_unavailable"));
        assert!(caveat_codes.contains(&"graph_path_unavailable"));
    }

    #[tokio::test]
    async fn knowledge_context_pack_2_preserves_popular_sink_impact_boundary() {
        let (_temp_dir, db_path) = minimal_analyst_db_with_meta();
        let request = KnowledgeContextPackV2Request::parse(&json!({
            "query": "popular impact",
            "intent": "change",
            "scope": "code",
            "graph_reasoning": {
                "paths": false,
                "communities": false,
                "risk": false
            }
        }))
        .expect("request");
        let result = KnowledgeQueryResult {
            db_path: db_path.display().to_string(),
            graph_content_hash: Some("fixture-hash".into()),
            candidates: vec![candidate(Some("sym-sink"), "sink_symbol", 9.0)],
        };

        let pack = pack_query_result_v2_with_graph_reasoning(
            &request,
            result,
            ExactGraphContext {
                graph_content_hash: Some("fixture-hash".into()),
                response_file_oids_match: Some(true),
                impacts: vec![Some(SymbolImpactSummary {
                    selector: "graph://symbol/sym-sink".into(),
                    callers_count: POPULAR_SINK_CALLERS_THRESHOLD + 1,
                    callees_count: 2,
                    caller_neighbors: vec![json!({ "title": "caller_a" })],
                    callee_neighbors: vec![json!({ "title": "callee_a" })],
                })],
            },
            &db_path,
        )
        .await;

        assert_eq!(pack["impact"]["popular_sink"], true);
        assert_eq!(
            pack["impact"]["caller_neighbors"].as_array().unwrap().len(),
            0
        );
        assert_eq!(
            pack["impact"]["callee_neighbors"].as_array().unwrap().len(),
            0
        );
        assert_eq!(pack["graph_paths"], json!([]));
        assert_eq!(pack["risk_scorecard"], json!([]));
        assert_eq!(pack["community_context"], json!([]));
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
