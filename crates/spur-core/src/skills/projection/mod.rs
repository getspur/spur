#[cfg(test)]
mod test_support;

pub mod resolver;

/// Policy used to choose the effective runtime skill set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    /// Include every bundled and accepted active pool skill.
    AllActive,
}

/// Runtime entry point requesting a skill projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeRole {
    Brain,
    Worker,
    Init,
}

/// Inputs shared by projection resolution, generation, and reconciliation.
#[derive(Debug, Clone)]
pub struct ProjectionRequest<'a> {
    pub source_repo_root: &'a std::path::Path,
    pub launch_root: &'a std::path::Path,
    pub adapter: crate::skills::adapters::Adapter,
    pub role: RuntimeRole,
    pub policy: SelectionPolicy,
}
