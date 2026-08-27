//! Leakage-safe query contracts and backend boundary.
//!
//! Duplicate observations are merged deterministically by canonical
//! [`SourceIdentity`]: the highest finite score wins, source kinds are united,
//! per-observation costs take their maximum, ambiguity flags are combined, and
//! the worst staleness state wins. Result-level costs still aggregate every
//! dispatch.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::{Component, Path, PathBuf},
    pin::Pin,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use spur_analyst::mcp::AnalystMcpModule;
use spur_graph::mcp::{with_worktree_root_for_request, GraphMcpModule};

use crate::SourceIdentity;

/// Stable category for a public SPUR retrieval source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Semantic evidence returned by `knowledge_context_pack_2`.
    SemanticKnowledgePack,
    /// Exact-name candidates returned by `code_symbol_search`.
    ExactSymbolSearch,
    /// Exact symbol bodies returned by `code_read_symbol`.
    ExactSymbolRead,
    /// Exact inbound edges returned by `code_callers`.
    ExactCallers,
    /// Exact outbound edges returned by `code_callees`.
    ExactCallees,
}

/// Stable answer state retained in benchmark denominators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStatus {
    /// At least one usable evidence hit was returned.
    Answered,
    /// Usable evidence and typed invalid evidence were both returned.
    Partial,
    /// No evidence was returned.
    NoEvidence,
    /// Evidence was returned, but none of it was valid.
    InvalidEvidence,
}

/// Stable staleness state propagated from public MCP metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// The response explicitly matches the active graph/worktree state.
    Fresh,
    /// The response explicitly reports stale graph or file state.
    Stale,
    /// The response supplied no decisive staleness signal.
    Unknown,
}

impl Staleness {
    const fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Stale, _) | (_, Self::Stale) => Self::Stale,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            (Self::Fresh, Self::Fresh) => Self::Fresh,
        }
    }
}

/// Typed reason an evidence row remains denominator-visible but unranked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceIssueKind {
    /// The public response did not have its documented structural shape.
    MalformedResponse,
    /// The row's score was NaN or infinite.
    NonFiniteScore,
    /// The row named a path outside the validated source root.
    OutOfRoot,
    /// The row could not be mapped to a canonical source identity.
    Unidentifiable,
}

/// Denominator-visible invalid evidence retained with its public source kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceIssue {
    source_kind: SourceKind,
    kind: EvidenceIssueKind,
    message: String,
}

impl EvidenceIssue {
    /// Returns the public retrieval source that produced the issue.
    #[must_use]
    pub const fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    /// Returns the stable issue category.
    #[must_use]
    pub const fn kind(&self) -> EvidenceIssueKind {
        self.kind
    }

    /// Returns the deterministic diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One stable, ranked source identity with complete cost and quality metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceHit {
    identity: SourceIdentity,
    score: f64,
    source_kinds: Vec<SourceKind>,
    latency_micros: u64,
    response_bytes: u64,
    estimated_tokens: u64,
    ambiguous: bool,
    staleness: Staleness,
    answer_status: AnswerStatus,
}

impl EvidenceHit {
    /// Returns the canonical path, byte span, and optional stable symbol ID.
    #[must_use]
    pub const fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    /// Returns the finite backend score used for ranking.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }

    /// Returns sorted, deduplicated public sources that identified this hit.
    #[must_use]
    pub fn source_kinds(&self) -> &[SourceKind] {
        &self.source_kinds
    }

    /// Returns measured backend latency attributed to the winning observation.
    #[must_use]
    pub const fn latency_micros(&self) -> u64 {
        self.latency_micros
    }

    /// Returns the compact response size attributed to this hit.
    #[must_use]
    pub const fn response_bytes(&self) -> u64 {
        self.response_bytes
    }

    /// Returns the deterministic byte-based token estimate.
    #[must_use]
    pub const fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    /// Returns whether any merged observation reported ambiguity.
    #[must_use]
    pub const fn ambiguous(&self) -> bool {
        self.ambiguous
    }

    /// Returns the worst merged staleness state.
    #[must_use]
    pub const fn staleness(&self) -> Staleness {
        self.staleness
    }

    /// Returns the denominator-visible status of the containing result.
    #[must_use]
    pub const fn answer_status(&self) -> AnswerStatus {
        self.answer_status
    }
}

/// Leakage category rejected before the first backend call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakageKind {
    /// A forbidden target symbol name appeared as complete identifier tokens.
    TargetName,
    /// Hidden completion text appeared as a complete canonical token sequence.
    HiddenCompletion,
    /// Both endpoints of a gold call edge appeared as complete token sequences.
    GoldCallEdge,
}

/// One gold caller-to-callee relationship that retrieval input must not expose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldCallEdge {
    caller: String,
    callee: String,
}

impl GoldCallEdge {
    /// Creates a gold call edge from two non-empty endpoint names.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidRequest`] if either endpoint has no
    /// identifier tokens.
    pub fn new(
        source_endpoint: impl Into<String>,
        target_endpoint: impl Into<String>,
    ) -> Result<Self, QueryError> {
        let edge = Self {
            caller: source_endpoint.into(),
            callee: target_endpoint.into(),
        };
        require_tokens("gold_call_edge.caller", &edge.caller)?;
        require_tokens("gold_call_edge.callee", &edge.callee)?;
        Ok(edge)
    }
}

/// Forbidden case material checked before any public MCP dispatch.
///
/// Matching is deterministic and token based: Unicode alphanumeric characters
/// and `_` form tokens, tokens are lowercased, and only complete token
/// sequences match. Thus a forbidden name such as `cat` does not reject
/// `concatenate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakagePolicy {
    target_names: Vec<Vec<String>>,
    hidden_completions: Vec<Vec<String>>,
    gold_call_edges: Vec<(Vec<String>, Vec<String>)>,
}

impl LeakagePolicy {
    /// Creates a canonical leakage policy.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidRequest`] when supplied material has no
    /// identifier tokens.
    pub fn new(
        target_names: Vec<String>,
        hidden_completions: Vec<String>,
        gold_call_edges: Vec<GoldCallEdge>,
    ) -> Result<Self, QueryError> {
        let target_names = canonical_material("target_name", target_names)?;
        let hidden_completions = canonical_material("hidden_completion", hidden_completions)?;
        let gold_call_edges = gold_call_edges
            .into_iter()
            .map(|edge| {
                (
                    canonical_tokens(&edge.caller),
                    canonical_tokens(&edge.callee),
                )
            })
            .collect();
        Ok(Self {
            target_names,
            hidden_completions,
            gold_call_edges,
        })
    }

    fn target_leakage(&self, query_tokens: &[String]) -> Option<String> {
        self.target_names
            .iter()
            .find(|target| contains_tokens(query_tokens, target))
            .map(|target| target.join(" "))
    }

    fn hidden_completion_leakage(&self, query_tokens: &[String]) -> Option<String> {
        self.hidden_completions
            .iter()
            .find(|completion| contains_tokens(query_tokens, completion))
            .map(|completion| completion.join(" "))
    }

    fn gold_call_edge_leakage(&self, query_tokens: &[String]) -> Option<String> {
        self.gold_call_edges
            .iter()
            .find(|(caller, callee)| {
                contains_tokens(query_tokens, caller) && contains_tokens(query_tokens, callee)
            })
            .map(|(caller, callee)| format!("{} -> {}", caller.join(" "), callee.join(" ")))
    }
}

/// One typed call made through [`QueryBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCall {
    source_kind: SourceKind,
    tool_name: &'static str,
    arguments: Value,
}

impl BackendCall {
    /// Returns the stable source category for this call.
    #[must_use]
    pub const fn source_kind(&self) -> SourceKind {
        self.source_kind
    }

    /// Returns the public MCP tool name.
    #[must_use]
    pub const fn tool_name(&self) -> &'static str {
        self.tool_name
    }

    /// Returns the root-independent MCP arguments.
    #[must_use]
    pub const fn arguments(&self) -> &Value {
        &self.arguments
    }
}

/// Raw backend response plus measured wall-clock latency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendResponse {
    body: Value,
    latency: Duration,
}

impl BackendResponse {
    /// Wraps one public MCP response.
    #[must_use]
    pub const fn new(body: Value, latency: Duration) -> Self {
        Self { body, latency }
    }

    /// Returns the public MCP response body.
    #[must_use]
    pub const fn body(&self) -> &Value {
        &self.body
    }

    /// Returns measured wall-clock dispatch latency.
    #[must_use]
    pub const fn latency(&self) -> Duration {
        self.latency
    }
}

/// Boxed future returned by the object-safe backend boundary.
pub type QueryBackendFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BackendResponse, QueryError>> + Send + 'a>>;

/// Injectable boundary for network-free query-adapter tests.
pub trait QueryBackend: Send + Sync {
    /// Dispatches one root-scoped public MCP call.
    fn dispatch<'a>(&'a self, source_root: &'a Path, call: BackendCall) -> QueryBackendFuture<'a>;
}

/// Production backend composed exclusively from public SPUR MCP modules.
pub struct SpurQueryBackend {
    graph: GraphMcpModule,
    analyst: AnalystMcpModule,
}

impl Default for SpurQueryBackend {
    fn default() -> Self {
        Self::new(GraphMcpModule::default(), AnalystMcpModule::default())
    }
}

impl SpurQueryBackend {
    /// Composes caller-supplied public graph and analyst modules.
    #[must_use]
    pub const fn new(graph: GraphMcpModule, analyst: AnalystMcpModule) -> Self {
        Self { graph, analyst }
    }
}

impl QueryBackend for SpurQueryBackend {
    fn dispatch<'a>(&'a self, source_root: &'a Path, call: BackendCall) -> QueryBackendFuture<'a> {
        Box::pin(async move {
            let source_kind = call.source_kind;
            let started = Instant::now();
            let dispatch = async move {
                match source_kind {
                    SourceKind::SemanticKnowledgePack => self
                        .analyst
                        .dispatch(call.tool_name, call.arguments)
                        .await
                        .map_err(|error| error.to_string()),
                    SourceKind::ExactSymbolSearch
                    | SourceKind::ExactSymbolRead
                    | SourceKind::ExactCallers
                    | SourceKind::ExactCallees => {
                        match self.graph.dispatch(call.tool_name, call.arguments).await {
                            Ok(body) => Ok(body),
                            Err(error) => Err(error.into_error_response().await.message),
                        }
                    }
                }
            };
            let body = with_worktree_root_for_request(source_root.to_path_buf(), dispatch)
                .await
                .map_err(|message| QueryError::Backend {
                    source_kind,
                    message,
                })?;
            Ok(BackendResponse::new(body, started.elapsed()))
        })
    }
}

/// Validated input for one semantic-plus-exact retrieval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalRequest {
    source_root: PathBuf,
    query: String,
    exact_symbol_query: String,
    top_k: usize,
    exact_followup_limit: usize,
    leakage: LeakagePolicy,
}

impl RetrievalRequest {
    /// Creates a request scoped to an existing materialized source root.
    ///
    /// `top_k` and `exact_followup_limit` are caller supplied; the adapter does
    /// not replace either with a suite-specific constant.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a missing root, empty query, or zero bound.
    pub fn new(
        source_root: impl AsRef<Path>,
        query: impl Into<String>,
        exact_symbol_query: impl Into<String>,
        top_k: usize,
        exact_followup_limit: usize,
        leakage: LeakagePolicy,
    ) -> Result<Self, QueryError> {
        let requested_root = source_root.as_ref();
        let source_root =
            std::fs::canonicalize(requested_root).map_err(|error| QueryError::InvalidRoot {
                root: requested_root.to_path_buf(),
                message: error.to_string(),
            })?;
        if !source_root.is_dir() {
            return Err(QueryError::InvalidRoot {
                root: source_root,
                message: "source root is not a directory".to_owned(),
            });
        }
        let query = query.into();
        require_tokens("query", &query)?;
        let exact_symbol_query = exact_symbol_query.into();
        require_tokens("exact_symbol_query", &exact_symbol_query)?;
        if top_k == 0 {
            return Err(invalid_request("top_k", "must be greater than zero"));
        }
        if exact_followup_limit == 0 {
            return Err(invalid_request(
                "exact_followup_limit",
                "must be greater than zero",
            ));
        }
        Ok(Self {
            source_root,
            query,
            exact_symbol_query,
            top_k,
            exact_followup_limit,
            leakage,
        })
    }
}

/// Normalized result for one retrieval request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalResult {
    hits: Vec<EvidenceHit>,
    issues: Vec<EvidenceIssue>,
    answer_status: AnswerStatus,
    score: Option<f64>,
    latency_micros: u64,
    response_bytes: u64,
    estimated_tokens: u64,
    ambiguous: bool,
    staleness: Staleness,
}

impl RetrievalResult {
    /// Returns ranked evidence after canonical deduplication and truncation.
    #[must_use]
    pub fn hits(&self) -> &[EvidenceHit] {
        &self.hits
    }

    /// Returns typed invalid evidence retained for denominator accounting.
    #[must_use]
    pub fn issues(&self) -> &[EvidenceIssue] {
        &self.issues
    }

    /// Returns the denominator-visible answer state.
    #[must_use]
    pub const fn answer_status(&self) -> AnswerStatus {
        self.answer_status
    }

    /// Returns the best finite hit score, when one exists.
    #[must_use]
    pub const fn score(&self) -> Option<f64> {
        self.score
    }

    /// Returns total measured latency across public dispatches.
    #[must_use]
    pub const fn latency_micros(&self) -> u64 {
        self.latency_micros
    }

    /// Returns total serialized bytes across compact public responses.
    #[must_use]
    pub const fn response_bytes(&self) -> u64 {
        self.response_bytes
    }

    /// Returns the deterministic byte-based response token estimate.
    #[must_use]
    pub const fn estimated_tokens(&self) -> u64 {
        self.estimated_tokens
    }

    /// Returns whether any public response reported ambiguity.
    #[must_use]
    pub const fn ambiguous(&self) -> bool {
        self.ambiguous
    }

    /// Returns the worst staleness state across public responses.
    #[must_use]
    pub const fn staleness(&self) -> Staleness {
        self.staleness
    }
}

/// Typed failure from request validation, leakage checking, or MCP dispatch.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum QueryError {
    /// A request field violated its stable contract.
    #[error("invalid retrieval request field {field}: {message}")]
    InvalidRequest {
        /// Stable field name.
        field: &'static str,
        /// Deterministic rejection reason.
        message: String,
    },
    /// The source root could not be validated.
    #[error("invalid retrieval source root {root}: {message}")]
    InvalidRoot {
        /// Rejected path.
        root: PathBuf,
        /// Filesystem validation reason.
        message: String,
    },
    /// Forbidden case material was found before dispatch.
    #[error("forbidden {kind:?} material in retrieval input: {canonical_material}")]
    ForbiddenLeakage {
        /// Stable leakage category.
        kind: LeakageKind,
        /// Canonical tokens that matched.
        canonical_material: String,
    },
    /// A public MCP module rejected a dispatch.
    #[error("{source_kind:?} backend dispatch failed: {message}")]
    Backend {
        /// Public source category being queried.
        source_kind: SourceKind,
        /// Bounded public error text.
        message: String,
    },
}

/// Runs the leakage guard before handing any request to `backend`.
///
/// # Errors
///
/// Returns [`QueryError::ForbiddenLeakage`] before backend dispatch when the
/// semantic or exact query contains forbidden material.
pub async fn retrieve<B: QueryBackend + ?Sized>(
    backend: &B,
    request: &RetrievalRequest,
) -> Result<RetrievalResult, QueryError> {
    for input in [&request.query, &request.exact_symbol_query] {
        let tokens = canonical_tokens(input);
        if let Some(canonical_material) = request.leakage.target_leakage(&tokens) {
            return Err(QueryError::ForbiddenLeakage {
                kind: LeakageKind::TargetName,
                canonical_material,
            });
        }
        if let Some(canonical_material) = request.leakage.hidden_completion_leakage(&tokens) {
            return Err(QueryError::ForbiddenLeakage {
                kind: LeakageKind::HiddenCompletion,
                canonical_material,
            });
        }
        if let Some(canonical_material) = request.leakage.gold_call_edge_leakage(&tokens) {
            return Err(QueryError::ForbiddenLeakage {
                kind: LeakageKind::GoldCallEdge,
                canonical_material,
            });
        }
    }

    let semantic = backend
        .dispatch(
            &request.source_root,
            BackendCall {
                source_kind: SourceKind::SemanticKnowledgePack,
                tool_name: "knowledge_context_pack_2",
                arguments: serde_json::json!({
                    "query": request.query,
                    "intent": "explain",
                    "scope": "code",
                    "max_symbol_bodies": 0,
                    "response_format": "compact"
                }),
            },
        )
        .await?;
    let exact_search = backend
        .dispatch(
            &request.source_root,
            BackendCall {
                source_kind: SourceKind::ExactSymbolSearch,
                tool_name: "code_symbol_search",
                arguments: serde_json::json!({
                    "query": request.exact_symbol_query,
                    "mode": "substring",
                    "response_format": "compact"
                }),
            },
        )
        .await?;

    let followups = exact_followups(&semantic.body, &exact_search.body)
        .into_iter()
        .take(request.exact_followup_limit);
    let mut responses = vec![
        (SourceKind::SemanticKnowledgePack, semantic),
        (SourceKind::ExactSymbolSearch, exact_search),
    ];
    for followup in followups {
        let source_kind = followup.source_kind;
        let response = backend
            .dispatch(
                &request.source_root,
                BackendCall {
                    source_kind,
                    tool_name: followup.tool_name,
                    arguments: followup.arguments(),
                },
            )
            .await?;
        responses.push((source_kind, response));
    }

    Ok(normalize_result(request, &responses))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ExactFollowup {
    source_kind: SourceKind,
    tool_name: &'static str,
    selector: String,
}

impl ExactFollowup {
    fn arguments(&self) -> Value {
        if self.source_kind == SourceKind::ExactSymbolRead {
            serde_json::json!({
                "stable_symbol_id": self.selector,
                "response_format": "compact"
            })
        } else {
            serde_json::json!({
                "selector": self.selector,
                "response_format": "compact"
            })
        }
    }
}

fn exact_followups(semantic: &Value, exact_search: &Value) -> Vec<ExactFollowup> {
    let mut followups = Vec::new();
    if let Some(recommended) = semantic
        .get("recommended_next_tools")
        .and_then(Value::as_array)
    {
        for entry in recommended {
            let Some(tool_name) = entry.get("tool").and_then(Value::as_str) else {
                continue;
            };
            let Some(selector) = entry
                .get("selector")
                .or_else(|| entry.get("stable_symbol_id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(source_kind) = source_kind_for_followup(tool_name) else {
                continue;
            };
            followups.push(ExactFollowup {
                source_kind,
                tool_name: tool_name_for_source(source_kind),
                selector: selector.to_owned(),
            });
        }
    }
    if let Some(candidates) = exact_search.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            let Some(selector) = candidate
                .get("uri")
                .or_else(|| candidate.get("selector"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            followups.push(ExactFollowup {
                source_kind: SourceKind::ExactSymbolRead,
                tool_name: "code_read_symbol",
                selector: selector.to_owned(),
            });
        }
    }

    let mut seen = BTreeSet::new();
    followups.retain(|followup| seen.insert((followup.source_kind, followup.selector.clone())));
    followups
}

const fn source_kind_for_followup(tool_name: &str) -> Option<SourceKind> {
    match tool_name.as_bytes() {
        b"code_read_symbol" => Some(SourceKind::ExactSymbolRead),
        b"code_callers" => Some(SourceKind::ExactCallers),
        b"code_callees" => Some(SourceKind::ExactCallees),
        _ => None,
    }
}

const fn tool_name_for_source(source_kind: SourceKind) -> &'static str {
    match source_kind {
        SourceKind::ExactSymbolRead => "code_read_symbol",
        SourceKind::ExactCallers => "code_callers",
        SourceKind::ExactCallees => "code_callees",
        SourceKind::SemanticKnowledgePack => "knowledge_context_pack_2",
        SourceKind::ExactSymbolSearch => "code_symbol_search",
    }
}

fn normalize_result(
    request: &RetrievalRequest,
    responses: &[(SourceKind, BackendResponse)],
) -> RetrievalResult {
    let mut hits = Vec::new();
    let mut issues = Vec::new();
    let mut known_identities = BTreeMap::new();

    for (source_kind, response) in responses
        .iter()
        .filter(|(kind, _)| *kind != SourceKind::SemanticKnowledgePack)
    {
        normalize_response(
            &request.source_root,
            *source_kind,
            response,
            &mut known_identities,
            &mut hits,
            &mut issues,
        );
    }
    for (source_kind, response) in responses
        .iter()
        .filter(|(kind, _)| *kind == SourceKind::SemanticKnowledgePack)
    {
        normalize_response(
            &request.source_root,
            *source_kind,
            response,
            &mut known_identities,
            &mut hits,
            &mut issues,
        );
    }

    let mut deduplicated = BTreeMap::<SourceIdentity, EvidenceHit>::new();
    for hit in hits {
        match deduplicated.entry(hit.identity.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(hit);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                merge_duplicate_hit(entry.get_mut(), hit);
            }
        }
    }
    let mut hits = deduplicated.into_values().collect::<Vec<_>>();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.identity.cmp(&right.identity))
    });
    hits.truncate(request.top_k);

    let answer_status = match (hits.is_empty(), issues.is_empty()) {
        (false, true) => AnswerStatus::Answered,
        (false, false) => AnswerStatus::Partial,
        (true, true) => AnswerStatus::NoEvidence,
        (true, false) => AnswerStatus::InvalidEvidence,
    };
    for hit in &mut hits {
        hit.answer_status = answer_status;
    }

    let mut latency_micros = 0_u64;
    let mut response_bytes = 0_u64;
    let mut ambiguous = false;
    let mut staleness = Staleness::Fresh;
    for (_, response) in responses {
        let metadata = response_metadata(response);
        latency_micros = latency_micros.saturating_add(metadata.latency_micros);
        response_bytes = response_bytes.saturating_add(metadata.response_bytes);
        ambiguous |= metadata.ambiguous;
        staleness = staleness.merge(metadata.staleness);
    }
    let estimated_tokens = estimate_tokens(response_bytes);
    let score = hits.first().map(EvidenceHit::score);

    RetrievalResult {
        hits,
        issues,
        answer_status,
        score,
        latency_micros,
        response_bytes,
        estimated_tokens,
        ambiguous,
        staleness,
    }
}

#[derive(Debug, Clone, Copy)]
struct ResponseMetadata {
    latency_micros: u64,
    response_bytes: u64,
    estimated_tokens: u64,
    ambiguous: bool,
    staleness: Staleness,
}

fn response_metadata(response: &BackendResponse) -> ResponseMetadata {
    let response_bytes = usize_to_u64(response.body.to_string().len());
    ResponseMetadata {
        latency_micros: u128_to_u64(response.latency.as_micros()),
        response_bytes,
        estimated_tokens: estimate_tokens(response_bytes),
        ambiguous: response_is_ambiguous(&response.body),
        staleness: response_staleness(&response.body),
    }
}

fn normalize_response(
    source_root: &Path,
    source_kind: SourceKind,
    response: &BackendResponse,
    known_identities: &mut BTreeMap<String, SourceIdentity>,
    hits: &mut Vec<EvidenceHit>,
    issues: &mut Vec<EvidenceIssue>,
) {
    let metadata = response_metadata(response);
    let rows = match response_rows(source_kind, &response.body) {
        Ok(rows) => rows,
        Err(message) => {
            issues.push(EvidenceIssue {
                source_kind,
                kind: EvidenceIssueKind::MalformedResponse,
                message,
            });
            return;
        }
    };

    for row in rows {
        let score = match evidence_score(row, source_kind) {
            Ok(score) => score,
            Err((kind, message)) => {
                issues.push(EvidenceIssue {
                    source_kind,
                    kind,
                    message,
                });
                continue;
            }
        };
        let identity = match source_identity_from_row(source_root, row, known_identities) {
            Ok(identity) => identity,
            Err((kind, message)) => {
                issues.push(EvidenceIssue {
                    source_kind,
                    kind,
                    message,
                });
                continue;
            }
        };
        if let Some(symbol_id) = identity.symbol_id() {
            known_identities.insert(symbol_id.to_owned(), identity.clone());
        }
        hits.push(EvidenceHit {
            identity,
            score,
            source_kinds: vec![source_kind],
            latency_micros: metadata.latency_micros,
            response_bytes: metadata.response_bytes,
            estimated_tokens: metadata.estimated_tokens,
            ambiguous: metadata.ambiguous,
            staleness: metadata.staleness,
            answer_status: AnswerStatus::Answered,
        });
    }
}

fn response_rows(source_kind: SourceKind, body: &Value) -> Result<Vec<&Value>, String> {
    let array_field = match source_kind {
        SourceKind::SemanticKnowledgePack => Some("primary_evidence"),
        SourceKind::ExactSymbolSearch => Some("candidates"),
        SourceKind::ExactCallers => Some("callers"),
        SourceKind::ExactCallees => Some("callees"),
        SourceKind::ExactSymbolRead => None,
    };
    if let Some(field) = array_field {
        return body
            .get(field)
            .and_then(Value::as_array)
            .map(|rows| rows.iter().collect())
            .ok_or_else(|| format!("missing array field `{field}`"));
    }
    body.get("symbol")
        .filter(|symbol| symbol.is_object())
        .map(|symbol| vec![symbol])
        .ok_or_else(|| "missing object field `symbol`".to_owned())
}

fn evidence_score(
    row: &Value,
    source_kind: SourceKind,
) -> Result<f64, (EvidenceIssueKind, String)> {
    let Some(value) = row.get("score") else {
        return Ok(0.0);
    };
    let score = value
        .as_f64()
        .or_else(|| {
            let text = value.as_str()?;
            text.parse::<f64>().ok()
        })
        .ok_or_else(|| {
            (
                EvidenceIssueKind::MalformedResponse,
                format!("{source_kind:?} score is not numeric"),
            )
        })?;
    if score.is_finite() {
        Ok(score)
    } else {
        Err((
            EvidenceIssueKind::NonFiniteScore,
            format!("{source_kind:?} score is not finite"),
        ))
    }
}

fn source_identity_from_row(
    source_root: &Path,
    row: &Value,
    known_identities: &BTreeMap<String, SourceIdentity>,
) -> Result<SourceIdentity, (EvidenceIssueKind, String)> {
    let symbol_id = row
        .get("stable_symbol_id")
        .or_else(|| row.get("uri"))
        .or_else(|| row.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let path = row
        .get("file")
        .or_else(|| row.get("file_path"))
        .and_then(Value::as_str);

    if let Some(path) = path {
        let (canonical_file, canonical_relative) = canonical_evidence_file(source_root, path)?;
        if let Some((start_line, end_line)) = row.get("line_range").and_then(parse_line_range) {
            let source = std::fs::read(&canonical_file).map_err(|error| {
                (
                    EvidenceIssueKind::Unidentifiable,
                    format!("cannot read evidence file `{canonical_relative}`: {error}"),
                )
            })?;
            let (byte_start, byte_end) = byte_span_for_lines(&source, start_line, end_line)?;
            return SourceIdentity::new(canonical_relative, byte_start, byte_end, symbol_id)
                .map_err(|error| (EvidenceIssueKind::Unidentifiable, error.to_string()));
        }
        if let Some(known) = symbol_id.as_deref().and_then(|id| known_identities.get(id)) {
            if known.path() == canonical_relative {
                return Ok(known.clone());
            }
        }
        return Err((
            EvidenceIssueKind::Unidentifiable,
            format!("evidence row for `{canonical_relative}` has no valid line range"),
        ));
    }

    symbol_id
        .as_deref()
        .and_then(|id| known_identities.get(id))
        .cloned()
        .ok_or_else(|| {
            (
                EvidenceIssueKind::Unidentifiable,
                "evidence row has neither a source path nor a previously resolved symbol ID"
                    .to_owned(),
            )
        })
}

fn canonical_evidence_file(
    source_root: &Path,
    reported_path: &str,
) -> Result<(PathBuf, String), (EvidenceIssueKind, String)> {
    let reported = Path::new(reported_path);
    if reported.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err((
            EvidenceIssueKind::OutOfRoot,
            format!("evidence path `{reported_path}` is not a safe relative path"),
        ));
    }
    let canonical = std::fs::canonicalize(source_root.join(reported)).map_err(|error| {
        (
            EvidenceIssueKind::Unidentifiable,
            format!("cannot resolve evidence path `{reported_path}`: {error}"),
        )
    })?;
    let relative = canonical
        .strip_prefix(source_root)
        .map_err(|_outside_root| {
            (
                EvidenceIssueKind::OutOfRoot,
                format!("evidence path `{reported_path}` resolves outside the source root"),
            )
        })?;
    if !canonical.is_file() {
        return Err((
            EvidenceIssueKind::Unidentifiable,
            format!("evidence path `{reported_path}` is not a file"),
        ));
    }
    let canonical_relative = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    Ok((canonical, canonical_relative))
}

fn parse_line_range(value: &Value) -> Option<(u64, u64)> {
    if let Some(range) = value.as_array() {
        return Some((range.first()?.as_u64()?, range.get(1)?.as_u64()?));
    }
    Some((value.get("start")?.as_u64()?, value.get("end")?.as_u64()?))
}

fn byte_span_for_lines(
    source: &[u8],
    start_line: u64,
    end_line: u64,
) -> Result<(u64, u64), (EvidenceIssueKind, String)> {
    if start_line == 0 || end_line < start_line {
        return Err((
            EvidenceIssueKind::Unidentifiable,
            format!("invalid one-based line range [{start_line}, {end_line}]"),
        ));
    }
    let mut line_starts = vec![0_usize];
    line_starts.extend(
        source
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'\n')
            .map(|(index, _)| index + 1),
    );
    let start_index = usize::try_from(start_line - 1).ok();
    let end_index = usize::try_from(end_line).ok();
    let Some(byte_start) = start_index.and_then(|index| line_starts.get(index).copied()) else {
        return Err((
            EvidenceIssueKind::Unidentifiable,
            format!("line range starts beyond file at line {start_line}"),
        ));
    };
    let byte_end = end_index
        .and_then(|index| line_starts.get(index).copied())
        .unwrap_or(source.len());
    if byte_start >= byte_end {
        return Err((
            EvidenceIssueKind::Unidentifiable,
            format!("line range [{start_line}, {end_line}] has an empty byte span"),
        ));
    }
    Ok((usize_to_u64(byte_start), usize_to_u64(byte_end)))
}

fn merge_duplicate_hit(existing: &mut EvidenceHit, duplicate: EvidenceHit) {
    existing.score = existing.score.max(duplicate.score);
    existing.source_kinds.extend(duplicate.source_kinds);
    existing.source_kinds.sort_unstable();
    existing.source_kinds.dedup();
    existing.latency_micros = existing.latency_micros.max(duplicate.latency_micros);
    existing.response_bytes = existing.response_bytes.max(duplicate.response_bytes);
    existing.estimated_tokens = existing.estimated_tokens.max(duplicate.estimated_tokens);
    existing.ambiguous |= duplicate.ambiguous;
    existing.staleness = existing.staleness.merge(duplicate.staleness);
}

fn response_is_ambiguous(body: &Value) -> bool {
    body.get("ambiguous").and_then(Value::as_bool) == Some(true)
        || body.get("ambiguity").is_some_and(|value| !value.is_null())
        || body.get("status").and_then(Value::as_str) == Some("ambiguous")
}

fn response_staleness(body: &Value) -> Staleness {
    if body.get("stale").and_then(Value::as_bool) == Some(true)
        || body.get("worktree_dirty").and_then(Value::as_bool) == Some(true)
        || body
            .get("response_file_oids_match")
            .and_then(Value::as_bool)
            == Some(false)
        || body
            .pointer("/staleness/analyst_matches_exact_graph")
            .and_then(Value::as_bool)
            == Some(false)
    {
        Staleness::Stale
    } else if body.get("stale").and_then(Value::as_bool) == Some(false)
        || body
            .get("response_file_oids_match")
            .and_then(Value::as_bool)
            == Some(true)
        || body
            .pointer("/staleness/analyst_matches_exact_graph")
            .and_then(Value::as_bool)
            == Some(true)
    {
        Staleness::Fresh
    } else {
        Staleness::Unknown
    }
}

const fn estimate_tokens(response_bytes: u64) -> u64 {
    response_bytes.div_ceil(4)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn u128_to_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn canonical_material(
    field: &'static str,
    material: Vec<String>,
) -> Result<Vec<Vec<String>>, QueryError> {
    material
        .into_iter()
        .map(|value| {
            let tokens = canonical_tokens(&value);
            if tokens.is_empty() {
                Err(invalid_request(field, "must contain identifier tokens"))
            } else {
                Ok(tokens)
            }
        })
        .collect()
}

fn require_tokens(field: &'static str, value: &str) -> Result<(), QueryError> {
    if canonical_tokens(value).is_empty() {
        Err(invalid_request(field, "must contain identifier tokens"))
    } else {
        Ok(())
    }
}

fn invalid_request(field: &'static str, message: &str) -> QueryError {
    QueryError::InvalidRequest {
        field,
        message: message.to_owned(),
    }
}

fn canonical_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    for character in value.chars() {
        if character.is_alphanumeric() || character == '_' {
            token.extend(character.to_lowercase());
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token));
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn contains_tokens(haystack: &[String], needle: &[String]) -> bool {
    token_sequence_position(haystack, needle).is_some()
}

fn token_sequence_position(haystack: &[String], needle: &[String]) -> Option<usize> {
    (needle.len() <= haystack.len())
        .then(|| {
            haystack
                .windows(needle.len())
                .position(|window| window == needle)
        })
        .flatten()
}
