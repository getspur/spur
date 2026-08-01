#[cfg(test)]
mod test_support;

mod generation;
mod manifest;
mod reconcile;
pub mod resolver;

/// Policy used to choose the effective runtime skill set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionPolicy {
    /// Include every bundled and accepted active pool skill.
    AllActive,
    /// Include only bundled foundation skills (skills-catalog + core SPUR skills).
    /// Remaining skills are discovered via skill_navigate / skill_read.
    CatalogOnly,
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

/// Structured reconciliation outcome shared by CLI and launch logs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectionSummary {
    pub adapter: String,
    pub generation: String,
    pub linked: Vec<std::path::PathBuf>,
    pub copied: Vec<std::path::PathBuf>,
    pub unchanged: Vec<std::path::PathBuf>,
    pub removed: Vec<std::path::PathBuf>,
    pub migrated: Vec<std::path::PathBuf>,
    pub skipped: Vec<ProjectionSkip>,
    pub selected: Vec<SelectedSource>,
}

impl std::fmt::Display for ProjectionSummary {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            formatter,
            "skill projection adapter={} generation={} linked={} copied={} unchanged={} removed={} migrated={} skipped={}",
            self.adapter,
            self.generation,
            self.linked.len(),
            self.copied.len(),
            self.unchanged.len(),
            self.removed.len(),
            self.migrated.len(),
            self.skipped.len()
        )?;
        for skipped in &self.skipped {
            writeln!(formatter, "  warning: {skipped}")?;
        }
        Ok(())
    }
}

/// Safe reason one target was preserved instead of projected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionSkipReason {
    UserOwned,
    UserEdited,
    OwnershipLost,
}

impl std::fmt::Display for ProjectionSkipReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserOwned => formatter.write_str("user-owned"),
            Self::UserEdited => formatter.write_str("user-edited"),
            Self::OwnershipLost => formatter.write_str("ownership-lost"),
        }
    }
}

/// One projection target preserved for user safety.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSkip {
    pub skill_id: String,
    pub path: std::path::PathBuf,
    pub reason: ProjectionSkipReason,
}

impl std::fmt::Display for ProjectionSkip {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "skill {} at {} ({})",
            self.skill_id,
            self.path.display(),
            self.reason
        )
    }
}

/// Source selected for one canonical skill ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedSource {
    pub skill_id: String,
    pub kind: resolver::ResolvedSourceKind,
    pub content_sha256: String,
}

impl std::fmt::Display for SelectedSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} ({:?}, {})",
            self.skill_id, self.kind, self.content_sha256
        )
    }
}

/// Reconciliation stage in which a fatal projection error occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionPhase {
    Resolve,
    Generate,
    Manifest,
    Recover,
    Reconcile,
    Excludes,
    GarbageCollect,
}

/// Fatal projection failure with launch and adapter context.
#[derive(Debug, thiserror::Error)]
#[error("skill projection {phase:?} failed for {adapter} at {launch_root}: {source}")]
pub struct ProjectionError {
    pub phase: ProjectionPhase,
    pub launch_root: std::path::PathBuf,
    pub adapter: String,
    pub skill_id: Option<String>,
    #[source]
    pub source: anyhow::Error,
}

/// Resolve, publish, and reconcile one adapter projection.
pub async fn reconcile(
    request: ProjectionRequest<'_>,
) -> Result<ProjectionSummary, ProjectionError> {
    let worktrees =
        spur_worktree::manager::WorktreeManager::new(request.source_repo_root.to_path_buf());
    reconcile_with_worktrees(&worktrees, request).await
}

/// Reconcile with an existing worktree manager for local Git excludes.
pub async fn reconcile_with_worktrees(
    worktrees: &spur_worktree::manager::WorktreeManager,
    request: ProjectionRequest<'_>,
) -> Result<ProjectionSummary, ProjectionError> {
    reconcile::run(worktrees, request).await
}

fn selection_policy_for_role(
    source_repo_root: &std::path::Path,
    role: RuntimeRole,
) -> anyhow::Result<SelectionPolicy> {
    // Init also uses the configured mode so `spur init` does not re-materialize
    // the full historical skill set when foundation/catalog projection is active.
    let _ = role;
    let mode = spur_acp::config::load_layered(source_repo_root)?
        .skills
        .projection_mode;
    Ok(match mode {
        spur_acp::config::SkillsProjectionMode::AllActive => SelectionPolicy::AllActive,
        spur_acp::config::SkillsProjectionMode::CatalogOnly => SelectionPolicy::CatalogOnly,
    })
}

/// Reconcile the configured projection for one supported runtime agent.
///
/// Unsupported agent kinds return `Ok(None)` without touching the filesystem.
pub async fn reconcile_for_agent_kind(
    worktrees: &spur_worktree::manager::WorktreeManager,
    source_repo_root: &std::path::Path,
    launch_root: &std::path::Path,
    kind: spur_acp::types::AgentKind,
    role: RuntimeRole,
) -> Result<Option<ProjectionSummary>, ProjectionError> {
    let Some(adapter) = crate::skills::adapters::Adapter::for_agent_kind(kind) else {
        return Ok(None);
    };
    let policy =
        selection_policy_for_role(source_repo_root, role).map_err(|source| ProjectionError {
            phase: ProjectionPhase::Resolve,
            launch_root: launch_root.to_path_buf(),
            adapter: adapter.key().to_owned(),
            skill_id: None,
            source,
        })?;
    reconcile_with_worktrees(
        worktrees,
        ProjectionRequest {
            source_repo_root,
            launch_root,
            adapter,
            role,
            policy,
        },
    )
    .await
    .map(Some)
}

/// Reconcile adapters in caller-supplied order for manual initialization.
pub async fn reconcile_many(
    source_repo_root: &std::path::Path,
    launch_root: &std::path::Path,
    adapters: &[crate::skills::adapters::Adapter],
) -> Result<Vec<ProjectionSummary>, ProjectionError> {
    let worktrees = spur_worktree::manager::WorktreeManager::new(source_repo_root.to_path_buf());
    let mut summaries = Vec::with_capacity(adapters.len());
    let legacy_hints =
        reconcile::snapshot_legacy_materialization_hints(source_repo_root, launch_root);
    let policy =
        selection_policy_for_role(source_repo_root, RuntimeRole::Init).map_err(|source| {
            ProjectionError {
                phase: ProjectionPhase::Resolve,
                launch_root: launch_root.to_path_buf(),
                adapter: adapters
                    .first()
                    .map(|adapter| adapter.key().to_owned())
                    .unwrap_or_else(|| "unknown".to_owned()),
                skill_id: None,
                source,
            }
        })?;
    for adapter in adapters {
        let outcome = reconcile::run_deferred(
            &worktrees,
            ProjectionRequest {
                source_repo_root,
                launch_root,
                adapter: *adapter,
                role: RuntimeRole::Init,
                policy,
            },
            &legacy_hints,
        )
        .await?;
        summaries.push(outcome.summary);
    }
    if let Some(adapter) = adapters.first().copied() {
        reconcile::retire_examined_legacy_materializations(
            ProjectionRequest {
                source_repo_root,
                launch_root,
                adapter,
                role: RuntimeRole::Init,
                policy,
            },
            adapters,
            &legacy_hints,
        )?;
    }
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::{selection_policy_for_role, RuntimeRole, SelectionPolicy};

    #[test]
    fn runtime_roles_select_catalog_only_from_layered_repository_config() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        std::fs::write(
            repo.path().join(".spur/config.toml"),
            "[skills]\nprojection_mode = \"catalog_only\"\n",
        )
        .unwrap();

        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Brain).unwrap(),
            SelectionPolicy::CatalogOnly
        );
        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Worker).unwrap(),
            SelectionPolicy::CatalogOnly
        );
    }

    #[test]
    fn init_follows_layered_config_and_explicit_all_active_rollbacks() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".spur")).unwrap();
        let config_path = repo.path().join(".spur/config.toml");
        std::fs::write(
            &config_path,
            "[skills]\nprojection_mode = \"catalog_only\"\n",
        )
        .unwrap();

        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Init).unwrap(),
            SelectionPolicy::CatalogOnly
        );

        std::fs::write(config_path, "[skills]\nprojection_mode = \"all_active\"\n").unwrap();
        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Brain).unwrap(),
            SelectionPolicy::AllActive
        );
        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Worker).unwrap(),
            SelectionPolicy::AllActive
        );
        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Init).unwrap(),
            SelectionPolicy::AllActive
        );
    }

    #[test]
    fn default_layered_config_selects_foundation_catalog_only() {
        let repo = tempfile::tempdir().unwrap();
        assert_eq!(
            selection_policy_for_role(repo.path(), RuntimeRole::Brain).unwrap(),
            SelectionPolicy::CatalogOnly
        );
    }
}
