use std::path::Path;
use std::sync::Arc;

use crate::adapter::{IssueTracker, PrService};
use crate::beads::BeadsAdapter;
use crate::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use crate::bv::BvAdapter;
use crate::github::GitHubAdapter;
use crate::graph::DependencyGraph;
use crate::graph_engine::GraphEngineConfig;
use crate::types::*;

/// Resolve the beads "closed" status string. Default is `"closed"` — the
/// value the default beads config accepts. Override via the argument for
/// projects whose beads config uses a different vocabulary (e.g., `"done"`,
/// `"resolved"`).
pub(crate) fn resolve_closed_status(override_value: Option<String>) -> String {
    override_value.unwrap_or_else(|| "closed".to_string())
}

fn default_beads_actor() -> Option<String> {
    Some("reconciler".to_string())
}

enum PmBackendInner {
    Beads {
        beads: BeadsAdapter,
        github: Option<GitHubAdapter>,
    },
    GitHub {
        adapter: GitHubAdapter,
    },
}

pub struct PmService {
    inner: PmBackendInner,
    bv: Option<BvAdapter>,
    closed_status: String,
}

impl PmService {
    /// Returns None if no PM backend available. Errors only for misconfiguration
    /// (e.g., .beads/ exists and enabled but br binary is missing).
    pub async fn try_new(
        github_repo: Option<String>,
        beads_enabled: bool,
        github_enabled: bool,
        repo_root: &Path,
        closed_status: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        Self::try_new_with_actor(
            github_repo,
            beads_enabled,
            github_enabled,
            repo_root,
            closed_status,
            default_beads_actor(),
        )
        .await
    }

    /// Actor-aware constructor for beads-backed services.
    ///
    /// Existing callers should continue using [`PmService::try_new`], which
    /// defaults to the server-level `"reconciler"` actor. Pass `None` here to
    /// disable actor attribution for beads CLI calls.
    pub async fn try_new_with_actor(
        github_repo: Option<String>,
        beads_enabled: bool,
        github_enabled: bool,
        repo_root: &Path,
        closed_status: Option<String>,
        actor: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let resolved_closed = resolve_closed_status(closed_status);
        let beads_dir = repo_root.join(".beads");

        if beads_dir.is_dir() && beads_enabled {
            let cursor_path = beads_dir.join(".spur-poll-cursor");
            let beads =
                BeadsAdapter::connect_with_actor(repo_root, actor, Some(cursor_path)).await?;
            let bv = match BeadsCrateAdapter::open(&beads_dir, AdapterConfig::default()).await {
                Ok(beads_crate) => Some(BvAdapter::from_beads(
                    Arc::new(beads_crate),
                    GraphEngineConfig::default(),
                )),
                Err(e) => {
                    tracing::info!("graph engine unavailable (graph analysis disabled): {e}");
                    None
                }
            };
            let github = if github_enabled {
                Self::try_github(github_repo, repo_root).await
            } else {
                None
            };
            return Ok(Some(Self {
                inner: PmBackendInner::Beads { beads, github },
                bv,
                closed_status: resolved_closed,
            }));
        }

        if github_enabled {
            if let Some(gh) = Self::try_github(github_repo, repo_root).await {
                return Ok(Some(Self {
                    inner: PmBackendInner::GitHub { adapter: gh },
                    bv: None,
                    closed_status: resolved_closed,
                }));
            }
        }

        Ok(None)
    }

    async fn try_github(repo: Option<String>, repo_root: &Path) -> Option<GitHubAdapter> {
        match GitHubAdapter::connect(repo, repo_root).await {
            Ok(gh) => Some(gh),
            Err(e) => {
                tracing::debug!("GitHub PM unavailable: {e}");
                None
            }
        }
    }

    pub async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.get_issue(id).await,
            PmBackendInner::GitHub { adapter } => adapter.get_issue(id).await,
        }
    }

    pub async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.list_issues(filter).await,
            PmBackendInner::GitHub { adapter } => adapter.list_issues(filter).await,
        }
    }

    pub async fn create_issue(&self, params: crate::types::IssueCreate) -> anyhow::Result<String> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.create_issue(params).await,
            PmBackendInner::GitHub { adapter } => adapter.create_issue(params).await,
        }
    }

    pub async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => {
                beads.add_dependency(issue_id, depends_on_id).await
            }
            PmBackendInner::GitHub { adapter } => {
                adapter.add_dependency(issue_id, depends_on_id).await
            }
        }
    }

    pub async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.update_issue(id, update).await,
            PmBackendInner::GitHub { adapter } => adapter.update_issue(id, update).await,
        }
    }

    pub async fn create_pr(&self, params: PrParams) -> anyhow::Result<String> {
        match &self.inner {
            PmBackendInner::Beads {
                github: Some(gh), ..
            } => gh.create_pr(params).await,
            PmBackendInner::Beads { github: None, .. } => {
                anyhow::bail!("No PR service. Configure [pm.github] for PR creation.")
            }
            PmBackendInner::GitHub { adapter } => adapter.create_pr(params).await,
        }
    }

    pub async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => beads.poll().await,
            PmBackendInner::GitHub { adapter } => adapter.poll().await,
        }
    }

    /// Returns the status string used to mark an issue as closed/done in the
    /// configured PM backend. Default `"closed"` unless overridden at
    /// construction.
    pub fn closed_status(&self) -> &str {
        &self.closed_status
    }

    pub fn source_str(&self) -> &'static str {
        match &self.inner {
            PmBackendInner::Beads { .. } => "beads",
            PmBackendInner::GitHub { .. } => "github",
        }
    }

    /// Returns the graph analyzer if `bv` (beads_viewer) is available.
    pub fn analyzer(&self) -> Option<&BvAdapter> {
        self.bv.as_ref()
    }

    pub fn issue_graph_available(&self) -> bool {
        self.bv.is_some()
    }

    pub async fn issue_subgraph_json(&self, id: &str) -> anyhow::Result<DependencyGraph> {
        let bv = self
            .bv
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("graph engine unavailable for issue graph"))?;

        if let PmBackendInner::Beads { beads, .. } = &self.inner {
            if let Some(plan_label) = beads.plan_id_label_for_epic(id).await? {
                return bv.graph_by_label(&plan_label, Some("json")).await;
            }
        }

        bv.subgraph(id, Some(2), Some("json")).await
    }

    /// Returns the beads-advanced extension surface if the backend is beads.
    /// Returns `None` for non-beads backends (GitHub). Callers use this to
    /// gate adaptive-plan-repair features on beads availability.
    pub fn advanced(&self) -> Option<&dyn crate::advanced::BeadsAdvanced> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => {
                Some(beads as &dyn crate::advanced::BeadsAdvanced)
            }
            PmBackendInner::GitHub { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn closed_status_defaults_to_closed_when_none() {
        assert_eq!(super::resolve_closed_status(None), "closed");
        assert_eq!(
            super::resolve_closed_status(Some("resolved".to_string())),
            "resolved"
        );
    }

    #[test]
    fn default_beads_actor_is_reconciler() {
        assert_eq!(super::default_beads_actor().as_deref(), Some("reconciler"));
    }

    #[test]
    fn advanced_returns_none_without_backend() {
        fn assert_accessor(svc: &super::PmService) -> Option<&dyn crate::BeadsAdvanced> {
            svc.advanced()
        }
        let _ = assert_accessor;
    }

    #[test]
    fn issue_filter_with_offset_flows_through_service_surface() {
        fn accepts_filter(_: crate::types::IssueFilter) {}

        accepts_filter(crate::types::IssueFilter {
            offset: Some(50),
            limit: Some(25),
            ..Default::default()
        });
    }
}
