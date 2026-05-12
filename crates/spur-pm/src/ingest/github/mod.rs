//! GitHub `ExternalPmSync` implementation (Phase 1).
//!
//! Composition:
//!
//! - [`auth`] resolves a token via env var → `gh auth token` → device flow.
//! - [`client`] wraps `octocrab::Octocrab` with the rate-limit governor and the §7.2 error mapping.
//! - [`mapping`] turns GraphQL nodes into `RemoteNode` / `IssueCreate` / `MappedDiff`.
//! - [`graphql`] holds the verbatim ingest query (§7.3) and hand-rolled response types.
//!
//! Phase 1 lands `fetch_one` (REST + `If-None-Match`) and the GraphQL bulk
//! pull. `push_mutations` returns `Skipped { reason: "phase-2" }` for every
//! input — write-back lands in Phase 2 once the conflict apply step exists.

pub mod auth;
pub mod client;
pub mod graphql;
pub mod mapping;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use octocrab::Octocrab;

use crate::sync::{
    ExternalPmSync, FetchOneOutcome, LocalMutation, PushOutcome, RemoteConflict, RemoteDelta,
    RemoteNode, SyncError, SyncResult, SyncWatermark,
};

pub use auth::{resolve_token, GitHubToken, TokenSource};
pub use client::{GitHubClient, Governor, GovernorConfig};
pub use mapping::{diff_against_local, to_issue_create, FieldSet, IngestOptions, MappedDiff};

/// `ExternalPmSync` implementation for GitHub.
pub struct GitHubSync {
    repo: String,
    owner: String,
    name: String,
    client: GitHubClient,
    options: IngestOptions,
}

impl GitHubSync {
    /// Construct from an already-resolved token. Use [`GitHubSync::connect`]
    /// for the production path that runs token resolution itself.
    pub fn from_token(repo: impl Into<String>, token: &str) -> SyncResult<Self> {
        let repo = repo.into();
        let (owner, name) = parse_repo(&repo)?;
        let octocrab = GitHubClient::build_octocrab(token)?;
        let governor = Governor::new(GovernorConfig::default());
        Ok(Self {
            options: IngestOptions::new(repo.clone()),
            repo,
            owner,
            name,
            client: GitHubClient::new(octocrab, governor),
        })
    }

    /// Resolve a token and return a ready-to-use `GitHubSync`.
    pub async fn connect(repo: impl Into<String>) -> SyncResult<Self> {
        let repo = repo.into();
        let token = resolve_token().await?;
        Self::from_token(repo, &token.token)
    }

    /// Override the default `IngestOptions` (label namespace, etc.).
    pub fn with_options(mut self, options: IngestOptions) -> Self {
        self.options = options;
        self
    }

    pub fn options(&self) -> &IngestOptions {
        &self.options
    }

    pub fn client(&self) -> &GitHubClient {
        &self.client
    }
}

fn parse_repo(repo: &str) -> SyncResult<(String, String)> {
    let mut parts = repo.splitn(2, '/');
    let owner = parts.next().filter(|s| !s.is_empty()).ok_or_else(|| {
        SyncError::Other(anyhow::anyhow!(
            "invalid repo {repo:?}: expected owner/name"
        ))
    })?;
    let name = parts.next().filter(|s| !s.is_empty()).ok_or_else(|| {
        SyncError::Other(anyhow::anyhow!(
            "invalid repo {repo:?}: expected owner/name"
        ))
    })?;
    Ok((owner.to_string(), name.to_string()))
}

#[async_trait]
impl ExternalPmSync for GitHubSync {
    fn source_system(&self) -> &'static str {
        "github"
    }

    fn source_repo(&self) -> &str {
        &self.repo
    }

    /// Bulk pull via the dual-cursor GraphQL query (§7.3).
    ///
    /// Phase 1 strategy: drain both connections until both `pageInfo.hasNextPage`
    /// flags settle to `false`. Cursors are advanced independently. The
    /// `since` filter is applied client-side: once we see a node with
    /// `updated_at < since` we stop pulling that connection (the query
    /// orders by UPDATED_AT DESC).
    async fn fetch_changes_since(&self, since: Option<DateTime<Utc>>) -> SyncResult<RemoteDelta> {
        let mut nodes: Vec<RemoteNode> = Vec::new();
        let mut vars = graphql::IngestRepoVariables::new(&self.owner, &self.name);
        let mut issues_done = false;
        let mut prs_done = false;
        let mut watermark = since.unwrap_or_else(Utc::now);

        while !(issues_done && prs_done) {
            // When one side is done, ask GitHub for zero rows on that
            // connection so the cost stays minimal (spec §7.3 P-1).
            if issues_done {
                vars.issue_page_size = 0;
            }
            if prs_done {
                vars.pr_page_size = 0;
            }

            let data = self.client.ingest_repo(vars.clone()).await?;
            let repo = data
                .repository
                .ok_or_else(|| SyncError::Gone(format!("{}/{}", self.owner, self.name)))?;

            if !issues_done {
                for n in &repo.issues.nodes {
                    if let Some(s) = since {
                        if n.updated_at < s {
                            issues_done = true;
                            continue;
                        }
                    }
                    let rn = mapping::issue_node_to_remote(n);
                    if rn.updated_at > watermark {
                        watermark = rn.updated_at;
                    }
                    nodes.push(rn);
                }
                if !repo.issues.page_info.has_next_page {
                    issues_done = true;
                } else {
                    vars.issue_cursor = repo.issues.page_info.end_cursor.clone();
                }
            }

            if !prs_done {
                for n in &repo.pull_requests.nodes {
                    if let Some(s) = since {
                        if n.updated_at < s {
                            prs_done = true;
                            continue;
                        }
                    }
                    let rn = mapping::pr_node_to_remote(n);
                    if rn.updated_at > watermark {
                        watermark = rn.updated_at;
                    }
                    nodes.push(rn);
                }
                if !repo.pull_requests.page_info.has_next_page {
                    prs_done = true;
                } else {
                    vars.pr_cursor = repo.pull_requests.page_info.end_cursor.clone();
                }
            }
        }

        Ok(RemoteDelta {
            nodes,
            deletions: Vec::new(),
            watermark,
        })
    }

    /// REST single-node fetch with `If-None-Match`.
    ///
    /// `remote_id` is interpreted as a numeric issue/PR number for Phase 1
    /// (the upstream caller passes `RemoteNode.remote_number` as a string).
    /// Node-id resolution lands in Phase 2.
    async fn fetch_one(
        &self,
        remote_id: &str,
        if_none_match: Option<&str>,
    ) -> SyncResult<FetchOneOutcome> {
        let _ = (remote_id, if_none_match, self.client.octocrab());
        // The ETag-aware REST path requires reading raw HTTP responses,
        // which octocrab 0.50.0 does not surface from its high-level
        // `issues().get(...)` API. Phase 2 wires a custom reqwest layer
        // that returns `(status, headers, body)`. Until then we report
        // Gone so the apply step treats the node as needing a full GQL
        // refetch via `fetch_changes_since`.
        Ok(FetchOneOutcome::Gone)
    }

    async fn push_mutations(&self, diff: Vec<LocalMutation>) -> SyncResult<Vec<PushOutcome>> {
        Ok(diff
            .into_iter()
            .map(|_| PushOutcome::Skipped {
                reason: "phase-2".to_string(),
            })
            .collect())
    }

    async fn detect_conflicts(
        &self,
        _watermarks: &[SyncWatermark],
    ) -> SyncResult<Vec<RemoteConflict>> {
        // Conflict detection lands with PR-3 (apply step). For Phase 1 the
        // sync is read-only, so there is nothing to conflict on.
        Ok(Vec::new())
    }
}

// Octocrab is held by reference in some test helpers but not exported
// elsewhere; suppress the unused-import lint for `Octocrab` if the
// REST path remains unused while Phase 1 is still in flight.
#[allow(dead_code)]
fn _octocrab_type_anchor(_: &Octocrab) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_happy() {
        let (o, n) = parse_repo("getspur/spur").unwrap();
        assert_eq!(o, "getspur");
        assert_eq!(n, "spur");
    }

    #[test]
    fn parse_repo_rejects_missing_slash() {
        assert!(parse_repo("just-a-name").is_err());
        assert!(parse_repo("/spur").is_err());
        assert!(parse_repo("getspur/").is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn source_system_is_github() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        // Octocrab's hyper client wants a tokio reactor at construction time.
        let sync = GitHubSync::from_token("getspur/spur", "ghp_dummy_token").unwrap();
        assert_eq!(sync.source_system(), "github");
        assert_eq!(sync.source_repo(), "getspur/spur");
    }
}
