//! Reproducible code-intelligence benchmark contracts for SPUR.

mod artifacts;
mod contract;
pub mod crosscodeeval;
pub mod jcg;
mod materialize;
pub mod metrics;
#[expect(
    clippy::missing_errors_doc,
    reason = "the exported Task 11 backend trait predates the Task 12 public module wiring"
)]
pub mod model;
mod query;
pub mod repoqa;
pub mod report;
mod sources;

pub use artifacts::{
    content_sha256, ArtifactError, ArtifactKind, ArtifactRecord, ArtifactStore, RunManifest,
    RunPhase,
};
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
