use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Result status of a delegation to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DelegationStatus {
    Success,
    Failed { error: String },
    Conflict { files: Vec<PathBuf> },
    Timeout,
}

/// Result returned from a completed delegation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationResult {
    pub status: DelegationStatus,
    pub diff: Option<String>,
    pub summary: Option<String>,
    pub estimated_cost_usd: f64,
}
