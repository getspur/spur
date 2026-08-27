//! Reproducible code-intelligence benchmark contracts for SPUR.

mod contract;
mod materialize;
mod query;
mod sources;

pub use contract::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceIdentity, Suite,
};
pub use materialize::{
    compute_materialization_hash, MaterializeError, MaterializedRoot, Materializer,
};
pub use query::{
    retrieve, AnswerStatus, BackendCall, BackendResponse, EvidenceHit, EvidenceIssue,
    EvidenceIssueKind, GoldCallEdge, LeakageKind, LeakagePolicy, QueryBackend, QueryBackendFuture,
    QueryError, RetrievalRequest, RetrievalResult, SourceKind, SpurQueryBackend, Staleness,
};
pub use sources::{
    validate_bytes, LanguageCapability, SchemaEvidence, SourceError, SourceFormat, SourceManifest,
    SourceSpec, ValidatedSource, SOURCE_ADAPTER_CONTRACT_VERSION, SOURCE_MANIFEST_VERSION,
};

/// Version of the public code-evaluation contract.
pub const CONTRACT_VERSION: &str = "code-eval-v1";
