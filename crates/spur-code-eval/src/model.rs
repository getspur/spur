#![allow(
    dead_code,
    reason = "Task 12 exports this module; Task 11 path-compiles it privately from integration tests"
)]

//! Deterministic advisory model-lane contracts.
//!
//! The lane borrows already-frozen contexts and returns model records in
//! memory. It has no filesystem or live-provider implementation, so a backend
//! failure cannot mutate deterministic benchmark artifacts.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const REQUEST_IDENTITY_SCHEMA: &str = "spur-code-eval-model-request-v1";
const CACHE_IDENTITY_SCHEMA: &str = "spur-code-eval-model-cache-v1";

/// A frozen context source used for final answer generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextVariant {
    /// No retrieved evidence is supplied.
    NoContext,
    /// Evidence comes from the frozen lexical BM25 baseline.
    LexicalBm25,
    /// Evidence comes from the frozen SPUR context pack.
    Spur,
    /// Evidence comes from a separately frozen Zero-Mem knowledge-context pack.
    ZeroMemSeparatedKnowledgePack,
    /// Evidence is the frozen oracle upper bound.
    Oracle,
}

impl ContextVariant {
    /// Returns the stable identity label for the variant.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoContext => "no_context",
            Self::LexicalBm25 => "lexical_bm25",
            Self::Spur => "spur",
            Self::ZeroMemSeparatedKnowledgePack => "zero_mem_separated_knowledge_pack",
            Self::Oracle => "oracle",
        }
    }
}

/// Invalid immutable model-lane configuration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelConfigError {
    /// A required pinned field is empty.
    #[error("model field `{field}` must not be empty")]
    EmptyField {
        /// Stable field name.
        field: &'static str,
    },
}

/// Immutable context bytes and their checksum identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenContext {
    case_id: String,
    variant: ContextVariant,
    contents: String,
    checksum: String,
}

impl FrozenContext {
    /// Freezes one context and computes its lowercase SHA-256 checksum.
    ///
    /// Empty contents are valid for [`ContextVariant::NoContext`].
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigError::EmptyField`] when `case_id` is empty.
    pub fn new(
        case_id: impl Into<String>,
        variant: ContextVariant,
        contents: impl Into<String>,
    ) -> Result<Self, ModelConfigError> {
        let case_id = case_id.into();
        require_nonempty("case_id", &case_id)?;
        let contents = contents.into();
        let checksum = sha256(contents.as_bytes());
        Ok(Self {
            case_id,
            variant,
            contents,
            checksum,
        })
    }

    /// Returns the benchmark case identity.
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    /// Returns the context variant.
    #[must_use]
    pub const fn variant(&self) -> ContextVariant {
        self.variant
    }

    /// Returns the immutable context contents.
    #[must_use]
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Returns the checksum of the exact frozen context bytes.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    fn is_valid(&self) -> bool {
        self.checksum == sha256(self.contents.as_bytes())
    }
}

/// Maximum input and output token counts pinned for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestBudget {
    max_input_tokens: u64,
    max_output_tokens: u64,
}

impl RequestBudget {
    /// Creates a pinned per-request token budget.
    #[must_use]
    pub const fn new(max_input_tokens: u64, max_output_tokens: u64) -> Self {
        Self {
            max_input_tokens,
            max_output_tokens,
        }
    }

    /// Returns the maximum input tokens.
    #[must_use]
    pub const fn max_input_tokens(self) -> u64 {
        self.max_input_tokens
    }

    /// Returns the maximum output tokens.
    #[must_use]
    pub const fn max_output_tokens(self) -> u64 {
        self.max_output_tokens
    }
}

/// Fully pinned settings for advisory final answer generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRunConfig {
    provider: String,
    model: String,
    prompt: String,
    tokenizer: String,
    seed: u64,
    request_budget: RequestBudget,
}

impl ModelRunConfig {
    /// Creates a complete pinned model request configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ModelConfigError::EmptyField`] for an empty textual pin.
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        prompt: impl Into<String>,
        tokenizer: impl Into<String>,
        seed: u64,
        request_budget: RequestBudget,
    ) -> Result<Self, ModelConfigError> {
        let config = Self {
            provider: provider.into(),
            model: model.into(),
            prompt: prompt.into(),
            tokenizer: tokenizer.into(),
            seed,
            request_budget,
        };
        require_nonempty("provider", &config.provider)?;
        require_nonempty("model", &config.model)?;
        require_nonempty("prompt", &config.prompt)?;
        require_nonempty("tokenizer", &config.tokenizer)?;
        Ok(config)
    }
}

/// Frozen case, variant, and checksum identity included in every cache key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextIdentity {
    case_id: String,
    variant: ContextVariant,
    checksum: String,
}

impl ContextIdentity {
    fn from_context(context: &FrozenContext) -> Self {
        Self {
            case_id: context.case_id.clone(),
            variant: context.variant,
            checksum: context.checksum.clone(),
        }
    }

    /// Returns the benchmark case identity.
    #[must_use]
    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    /// Returns the distinct frozen context variant.
    #[must_use]
    pub const fn variant(&self) -> ContextVariant {
        self.variant
    }

    /// Returns the frozen context checksum.
    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }
}

/// Owned request and cache identity persisted with a model record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRequestIdentity {
    provider: String,
    model: String,
    prompt: String,
    tokenizer: String,
    seed: u64,
    request_budget: RequestBudget,
    context: ContextIdentity,
    request_checksum: String,
    cache_identity: String,
}

impl ModelRequestIdentity {
    fn derive(config: &ModelRunConfig, context: &FrozenContext) -> Self {
        let request_checksum = request_checksum(config);
        let context = ContextIdentity::from_context(context);
        let cache_identity = cache_identity(&request_checksum, &context);
        Self {
            provider: config.provider.clone(),
            model: config.model.clone(),
            prompt: config.prompt.clone(),
            tokenizer: config.tokenizer.clone(),
            seed: config.seed,
            request_budget: config.request_budget,
            context,
            request_checksum,
            cache_identity,
        }
    }

    fn is_valid(&self) -> bool {
        let config = ModelRunConfig {
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt: self.prompt.clone(),
            tokenizer: self.tokenizer.clone(),
            seed: self.seed,
            request_budget: self.request_budget,
        };
        let request_checksum = request_checksum(&config);
        self.request_checksum == request_checksum
            && self.cache_identity == cache_identity(&request_checksum, &self.context)
    }

    /// Returns the pinned provider name.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the pinned model name.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the exact pinned prompt.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    /// Returns the pinned tokenizer identity.
    #[must_use]
    pub fn tokenizer(&self) -> &str {
        &self.tokenizer
    }

    /// Returns the pinned deterministic seed.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Returns the pinned request budget.
    #[must_use]
    pub const fn request_budget(&self) -> RequestBudget {
        self.request_budget
    }

    /// Returns the case, variant, and context checksum identity.
    #[must_use]
    pub const fn context(&self) -> &ContextIdentity {
        &self.context
    }

    /// Returns the request-only checksum.
    #[must_use]
    pub fn request_checksum(&self) -> &str {
        &self.request_checksum
    }

    /// Returns the cache identity over the request and frozen context pins.
    #[must_use]
    pub fn cache_identity(&self) -> &str {
        &self.cache_identity
    }
}

/// Borrowed final-answer request passed to an injected backend.
#[derive(Debug, Clone, Copy)]
pub struct ModelRequest<'a> {
    context: &'a FrozenContext,
    identity: &'a ModelRequestIdentity,
}

impl ModelRequest<'_> {
    /// Returns the benchmark case identity.
    #[must_use]
    pub fn case_id(&self) -> &str {
        self.context.case_id()
    }

    /// Returns the frozen context variant.
    #[must_use]
    pub const fn variant(&self) -> ContextVariant {
        self.context.variant()
    }

    /// Returns the frozen context contents without granting mutation.
    #[must_use]
    pub fn contents(&self) -> &str {
        self.context.contents()
    }

    /// Returns the complete pinned request and cache identity.
    #[must_use]
    pub const fn identity(&self) -> &ModelRequestIdentity {
        self.identity
    }
}

/// Usage attributable to model generation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    llm_calls: u64,
    input_tokens: u64,
    output_tokens: u64,
}

impl ModelUsage {
    /// Zero model usage.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Creates an exact usage counter.
    #[must_use]
    pub const fn new(llm_calls: u64, input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            llm_calls,
            input_tokens,
            output_tokens,
        }
    }

    /// Returns the number of LLM calls.
    #[must_use]
    pub const fn llm_calls(self) -> u64 {
        self.llm_calls
    }

    /// Returns the number of LLM input tokens.
    #[must_use]
    pub const fn input_tokens(self) -> u64 {
        self.input_tokens
    }

    /// Returns the number of LLM output tokens.
    #[must_use]
    pub const fn output_tokens(self) -> u64 {
        self.output_tokens
    }
}

/// A provider response before advisory status normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOutput {
    output: String,
    complete: bool,
    usage: ModelUsage,
}

impl ModelOutput {
    /// Creates a response declared complete by the backend.
    #[must_use]
    pub fn complete(output: impl Into<String>, usage: ModelUsage) -> Self {
        Self {
            output: output.into(),
            complete: true,
            usage,
        }
    }

    /// Creates an explicitly incomplete response.
    #[must_use]
    pub fn incomplete(output: impl Into<String>, usage: ModelUsage) -> Self {
        Self {
            output: output.into(),
            complete: false,
            usage,
        }
    }
}

/// Backend failures normalized by the advisory lane.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ModelBackendError {
    /// No provider credential is available.
    #[error("model credentials are unavailable")]
    MissingCredentials,
    /// The pinned request budget cannot admit this call.
    #[error("model request budget is exhausted")]
    BudgetExhausted,
    /// An HTTP transport or status failure occurred.
    #[error("model HTTP failure: {0}")]
    Http(String),
    /// The provider backend failed outside HTTP transport.
    #[error("model backend failure: {0}")]
    Backend(String),
}

/// Injected final-answer generator.
///
/// The crate intentionally provides no live implementation. Callers must opt
/// into a provider and credentials explicitly.
pub trait ModelBackend {
    /// Generates one final answer from a borrowed frozen context.
    fn generate(&mut self, request: &ModelRequest<'_>) -> Result<ModelOutput, ModelBackendError>;
}

/// A non-terminal advisory reason that may be retried with identical pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPendingReason {
    /// Required credentials were not present.
    MissingCredentials,
    /// The pinned request budget was exhausted.
    BudgetExhausted,
}

/// A failed or invalid advisory result that may be retried only when incomplete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelFailureReason {
    /// The provider returned an HTTP failure.
    HttpFailure,
    /// The provider backend failed outside HTTP transport.
    BackendFailure,
    /// The backend did not return a complete non-empty answer.
    IncompleteOutput,
    /// A prior record did not match the current pinned identity.
    IdentityMismatch,
}

/// Advisory completion state persisted with each case record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum ModelCaseStatus {
    /// Final answer generation completed successfully.
    Completed,
    /// Work remains pending for an operational reason.
    ModelPending(ModelPendingReason),
    /// Generation failed without affecting deterministic results.
    ModelFailed(ModelFailureReason),
}

/// One owned advisory record suitable for model-record serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRecord {
    identity: ModelRequestIdentity,
    status: ModelCaseStatus,
    output: Option<String>,
    usage: ModelUsage,
    detail: Option<String>,
}

impl ModelRecord {
    fn new(
        identity: ModelRequestIdentity,
        status: ModelCaseStatus,
        output: Option<String>,
        usage: ModelUsage,
        detail: Option<String>,
    ) -> Self {
        Self {
            identity,
            status,
            output,
            usage,
            detail,
        }
    }

    fn is_complete(&self) -> bool {
        matches!(self.status, ModelCaseStatus::Completed)
            && self
                .output
                .as_deref()
                .is_some_and(|output| !output.trim().is_empty())
    }

    /// Returns the request and cache identity.
    #[must_use]
    pub const fn identity(&self) -> &ModelRequestIdentity {
        &self.identity
    }

    /// Returns the normalized advisory status.
    #[must_use]
    pub const fn status(&self) -> &ModelCaseStatus {
        &self.status
    }

    /// Returns the complete or partial provider output, when present.
    #[must_use]
    pub fn output(&self) -> Option<&str> {
        self.output.as_deref()
    }

    /// Returns model usage attributable only to final answer generation.
    #[must_use]
    pub const fn usage(&self) -> ModelUsage {
        self.usage
    }

    /// Returns backend diagnostic detail when one was supplied.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

/// Runs or resumes advisory final answer generation.
///
/// Contexts are borrowed, records are returned in input order, and no file or
/// artifact handle is accepted. A valid completed record is reused only when
/// its derived request and cache identities match. Pending, failed, missing,
/// and incomplete records with matching pins are retried.
#[must_use]
pub fn run_model_lane<B: ModelBackend>(
    backend: &mut B,
    config: &ModelRunConfig,
    contexts: &[FrozenContext],
    prior_records: &[ModelRecord],
) -> Vec<ModelRecord> {
    contexts
        .iter()
        .map(|context| {
            let identity = ModelRequestIdentity::derive(config, context);
            if !context.is_valid() {
                return ModelRecord::new(
                    identity,
                    ModelCaseStatus::ModelFailed(ModelFailureReason::IdentityMismatch),
                    None,
                    ModelUsage::ZERO,
                    Some("frozen context contents do not match their checksum".to_owned()),
                );
            }
            let prior = prior_records.iter().find(|record| {
                record.identity.context.case_id == context.case_id
                    && record.identity.context.variant == context.variant
            });
            if let Some(prior) = prior {
                if !prior.identity.is_valid() || prior.identity != identity {
                    return ModelRecord::new(
                        identity,
                        ModelCaseStatus::ModelFailed(ModelFailureReason::IdentityMismatch),
                        None,
                        ModelUsage::ZERO,
                        Some(
                            "prior record does not match the pinned request and context".to_owned(),
                        ),
                    );
                }
                if prior.is_complete() {
                    return prior.clone();
                }
            }
            generate_record(backend, context, identity)
        })
        .collect()
}

fn generate_record<B: ModelBackend>(
    backend: &mut B,
    context: &FrozenContext,
    identity: ModelRequestIdentity,
) -> ModelRecord {
    let response = backend.generate(&ModelRequest {
        context,
        identity: &identity,
    });
    match response {
        Ok(output) if output.complete && !output.output.trim().is_empty() => ModelRecord::new(
            identity,
            ModelCaseStatus::Completed,
            Some(output.output),
            output.usage,
            None,
        ),
        Ok(output) => ModelRecord::new(
            identity,
            ModelCaseStatus::ModelFailed(ModelFailureReason::IncompleteOutput),
            Some(output.output),
            output.usage,
            Some("backend output was incomplete or empty".to_owned()),
        ),
        Err(ModelBackendError::MissingCredentials) => ModelRecord::new(
            identity,
            ModelCaseStatus::ModelPending(ModelPendingReason::MissingCredentials),
            None,
            ModelUsage::ZERO,
            None,
        ),
        Err(ModelBackendError::BudgetExhausted) => ModelRecord::new(
            identity,
            ModelCaseStatus::ModelPending(ModelPendingReason::BudgetExhausted),
            None,
            ModelUsage::ZERO,
            None,
        ),
        Err(ModelBackendError::Http(detail)) => ModelRecord::new(
            identity,
            ModelCaseStatus::ModelFailed(ModelFailureReason::HttpFailure),
            None,
            ModelUsage::ZERO,
            Some(detail),
        ),
        Err(ModelBackendError::Backend(detail)) => ModelRecord::new(
            identity,
            ModelCaseStatus::ModelFailed(ModelFailureReason::BackendFailure),
            None,
            ModelUsage::ZERO,
            Some(detail),
        ),
    }
}

/// One Zero-Mem operation outside final answer generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ZeroMemOperation {
    /// Capture information into memory state.
    Capture,
    /// Encode and index memory state.
    Index,
    /// Retrieve from memory state.
    Retrieve,
    /// Update memory state.
    Update,
    /// Delete memory state.
    Delete,
}

/// Encoder and index work tracked separately from LLM usage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct EncoderIndexUsage {
    encoder_calls: u64,
    encoder_input_tokens: u64,
    index_reads: u64,
    index_writes: u64,
}

impl EncoderIndexUsage {
    /// Creates exact encoder and index counters.
    #[must_use]
    pub const fn new(
        encoder_calls: u64,
        encoder_input_tokens: u64,
        index_reads: u64,
        index_writes: u64,
    ) -> Self {
        Self {
            encoder_calls,
            encoder_input_tokens,
            index_reads,
            index_writes,
        }
    }

    const fn saturating_add(self, other: Self) -> Self {
        Self::new(
            self.encoder_calls.saturating_add(other.encoder_calls),
            self.encoder_input_tokens
                .saturating_add(other.encoder_input_tokens),
            self.index_reads.saturating_add(other.index_reads),
            self.index_writes.saturating_add(other.index_writes),
        )
    }
}

/// Immutable accounting for one Zero-Mem memory operation.
///
/// Deserialization is intentionally not implemented: memory-operation LLM
/// usage is created only by this module and is always [`ModelUsage::ZERO`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ZeroMemMemoryRecord {
    operation: ZeroMemOperation,
    llm_usage: ModelUsage,
    encoder_index_usage: EncoderIndexUsage,
}

impl ZeroMemMemoryRecord {
    /// Returns the memory operation.
    #[must_use]
    pub const fn operation(&self) -> ZeroMemOperation {
        self.operation
    }

    /// Returns zero calls, zero input tokens, and zero output tokens.
    #[must_use]
    pub const fn llm_usage(&self) -> ModelUsage {
        self.llm_usage
    }

    /// Returns encoder and index counters for this operation.
    #[must_use]
    pub const fn encoder_index_usage(&self) -> EncoderIndexUsage {
        self.encoder_index_usage
    }
}

/// Zero-Mem accounting ledger for non-final-answer memory operations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ZeroMemAccounting {
    memory_records: Vec<ZeroMemMemoryRecord>,
}

impl ZeroMemAccounting {
    /// Records encoder/index work for a memory operation with zero LLM usage.
    pub fn record_memory_operation(
        &mut self,
        operation: ZeroMemOperation,
        encoder_index_usage: EncoderIndexUsage,
    ) {
        self.memory_records.push(ZeroMemMemoryRecord {
            operation,
            llm_usage: ModelUsage::ZERO,
            encoder_index_usage,
        });
    }

    /// Returns all memory-operation accounting records.
    #[must_use]
    pub fn memory_records(&self) -> &[ZeroMemMemoryRecord] {
        &self.memory_records
    }

    /// Returns separately aggregated encoder and index counters.
    #[must_use]
    pub fn total_encoder_index_usage(&self) -> EncoderIndexUsage {
        self.memory_records
            .iter()
            .fold(EncoderIndexUsage::default(), |total, record| {
                total.saturating_add(record.encoder_index_usage)
            })
    }
}

fn require_nonempty(field: &'static str, value: &str) -> Result<(), ModelConfigError> {
    if value.trim().is_empty() {
        Err(ModelConfigError::EmptyField { field })
    } else {
        Ok(())
    }
}

fn request_checksum(config: &ModelRunConfig) -> String {
    let mut hasher = Sha256::new();
    hash_text(&mut hasher, REQUEST_IDENTITY_SCHEMA);
    hash_text(&mut hasher, &config.provider);
    hash_text(&mut hasher, &config.model);
    hash_text(&mut hasher, &config.prompt);
    hash_text(&mut hasher, &config.tokenizer);
    hasher.update(config.seed.to_be_bytes());
    hasher.update(config.request_budget.max_input_tokens.to_be_bytes());
    hasher.update(config.request_budget.max_output_tokens.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_identity(request_checksum: &str, context: &ContextIdentity) -> String {
    let mut hasher = Sha256::new();
    hash_text(&mut hasher, CACHE_IDENTITY_SCHEMA);
    hash_text(&mut hasher, request_checksum);
    hash_text(&mut hasher, &context.case_id);
    hash_text(&mut hasher, context.variant.as_str());
    hash_text(&mut hasher, &context.checksum);
    format!("{:x}", hasher.finalize())
}

fn hash_text(hasher: &mut Sha256, value: &str) {
    let length = u64::try_from(value.len()).unwrap_or(u64::MAX);
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
