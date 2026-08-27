//! Reproducible code-intelligence benchmark contracts for SPUR.

mod contract;
mod sources;

pub use contract::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceIdentity, Suite,
};
pub use sources::{
    validate_bytes, LanguageCapability, SchemaEvidence, SourceError, SourceFormat, SourceManifest,
    SourceSpec, ValidatedSource, SOURCE_ADAPTER_CONTRACT_VERSION, SOURCE_MANIFEST_VERSION,
};

/// Version of the public code-evaluation contract.
pub const CONTRACT_VERSION: &str = "code-eval-v1";
