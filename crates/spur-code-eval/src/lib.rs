//! Reproducible code-intelligence benchmark contracts for SPUR.

mod contract;

pub use contract::{
    CaseStatus, CodeEvalCase, ContentPin, ContractError, GoldEvidence, Language, QueryPolicy,
    RepositoryPin, SourceIdentity, Suite,
};

/// Version of the public code-evaluation contract.
pub const CONTRACT_VERSION: &str = "code-eval-v1";
