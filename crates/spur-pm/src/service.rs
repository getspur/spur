use std::path::Path;

use crate::adapter::{IssueTracker, PrService};
use crate::beads::BeadsAdapter;
use crate::github::GitHubAdapter;
use crate::types::*;

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
}

impl PmService {
    /// Returns None if no PM backend available. Errors only for misconfiguration
    /// (e.g., .beads/ exists and enabled but br binary is missing).
    pub async fn try_new(
        github_repo: Option<String>,
        beads_enabled: bool,
        github_enabled: bool,
        repo_root: &Path,
    ) -> anyhow::Result<Option<Self>> {
        let beads_dir = repo_root.join(".beads");

        if beads_dir.is_dir() && beads_enabled {
            let beads = BeadsAdapter::connect(repo_root).await?;
            let github = if github_enabled {
                Self::try_github(github_repo, repo_root).await
            } else {
                None
            };
            return Ok(Some(Self {
                inner: PmBackendInner::Beads { beads, github },
            }));
        }

        if github_enabled {
            if let Some(gh) = Self::try_github(github_repo, repo_root).await {
                return Ok(Some(Self {
                    inner: PmBackendInner::GitHub { adapter: gh },
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

    pub fn source_str(&self) -> &'static str {
        match &self.inner {
            PmBackendInner::Beads { .. } => "beads",
            PmBackendInner::GitHub { .. } => "github",
        }
    }
}
