use crate::adapter::PmAdapter;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use crate::types::{Issue, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource, PrParams};
use std::sync::Mutex;
use tokio::process::Command;

/// GitHub adapter using the `gh` CLI for auth and API calls.
pub struct GitHubAdapter {
    pub repo: Option<String>,
    pub auto_label: String,
    last_poll: Mutex<Option<DateTime<Utc>>>,
}

impl GitHubAdapter {
    pub fn new(repo: Option<String>) -> Self {
        Self {
            repo,
            auto_label: String::from("spur-managed"),
            last_poll: Mutex::new(None),
        }
    }

    /// Run the `gh` CLI with the given arguments. Returns stdout on success,
    /// or an error containing stderr on non-zero exit.
    async fn run_gh(&self, args: &[&str]) -> anyhow::Result<String> {
        tracing::debug!(args = ?args, "running gh CLI");

        let output = Command::new("gh")
            .args(args)
            .output()
            .await
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "GitHub CLI (`gh`) not found. Install it from https://cli.github.com/ \
                         and run `gh auth login` to authenticate."
                    )
                } else {
                    anyhow::anyhow!("Failed to execute `gh`: {e}")
                }
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            tracing::debug!(stderr = %stderr, "gh CLI failed");
            anyhow::bail!("gh {}: {}", args.first().unwrap_or(&""), stderr.trim());
        }

        tracing::debug!(stdout_len = stdout.len(), "gh CLI succeeded");
        Ok(stdout)
    }

    /// Returns the repo slug, or an error if not configured.
    fn repo(&self) -> anyhow::Result<&str> {
        self.repo
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("No repository configured. Call `connect` first."))
    }
}

// ─── JSON helper structs for deserializing `gh` output ────────────────

#[derive(serde::Deserialize)]
struct GhLabel {
    name: String,
}

#[derive(serde::Deserialize)]
struct GhAssignee {
    login: String,
}

/// Full issue view returned by `gh issue view --json ...`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssueView {
    number: u64,
    title: String,
    body: Option<String>,
    labels: Vec<GhLabel>,
    state: String,
    assignees: Vec<GhAssignee>,
    url: String,
}

/// Issue list item returned by `gh issue list --json ...`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssueListItem {
    number: u64,
    title: String,
    labels: Vec<GhLabel>,
    state: String,
    url: String,
    #[serde(default)]
    updated_at: Option<String>,
}

/// Repo metadata returned by `gh repo view --json nameWithOwner`.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhRepoView {
    name_with_owner: String,
}

// ─── Conversions ──────────────────────────────────────────────────────

impl From<GhIssueView> for Issue {
    fn from(gh: GhIssueView) -> Self {
        Issue {
            id: gh.number.to_string(),
            source: PmSource::GitHub,
            title: gh.title,
            body: gh.body.unwrap_or_default(),
            labels: gh.labels.into_iter().map(|l| l.name).collect(),
            priority: None,
            assignee: gh.assignees.first().map(|a| a.login.clone()),
            status: gh.state.to_lowercase(),
            linked_prs: Vec::new(),
            url: gh.url,
        }
    }
}

impl From<GhIssueListItem> for IssueSummary {
    fn from(gh: GhIssueListItem) -> Self {
        IssueSummary {
            id: gh.number.to_string(),
            source: PmSource::GitHub,
            title: gh.title,
            labels: gh.labels.into_iter().map(|l| l.name).collect(),
            status: gh.state.to_lowercase(),
            url: gh.url,
        }
    }
}

// ─── PmAdapter implementation ─────────────────────────────────────────

#[async_trait]
impl PmAdapter for GitHubAdapter {
    async fn connect(&mut self) -> anyhow::Result<()> {
        // 1. Verify authentication
        self.run_gh(&["auth", "status"]).await.map_err(|e| {
            anyhow::anyhow!(
                "GitHub CLI is not authenticated. Run `gh auth login` first.\nDetails: {e}"
            )
        })?;

        // 2. Auto-detect repo if not set
        if self.repo.is_none() {
            let output = self
                .run_gh(&["repo", "view", "--json", "nameWithOwner"])
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "No --repo specified and could not detect repository. \
                         Either pass a repo or run from inside a git repository.\nDetails: {e}"
                    )
                })?;

            let view: GhRepoView = serde_json::from_str(&output).map_err(|e| {
                anyhow::anyhow!("Failed to parse repo metadata from `gh repo view`: {e}")
            })?;

            tracing::debug!(repo = %view.name_with_owner, "auto-detected repository");
            self.repo = Some(view.name_with_owner);
        }

        tracing::debug!(repo = ?self.repo, "connected to GitHub");
        Ok(())
    }

    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        let repo = self.repo()?;
        let output = self
            .run_gh(&[
                "issue",
                "view",
                id,
                "--repo",
                repo,
                "--json",
                "number,title,body,labels,state,assignees,url",
            ])
            .await?;

        let gh_issue: GhIssueView = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse issue JSON: {e}"))?;

        Ok(Issue::from(gh_issue))
    }

    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let repo = self.repo()?;

        let mut args: Vec<String> = vec![
            "issue".into(),
            "list".into(),
            "--repo".into(),
            repo.to_string(),
            "--json".into(),
            "number,title,labels,state,url,updatedAt".into(),
            "--limit".into(),
            "50".into(),
        ];

        // Apply label filters
        for label in &filter.labels {
            args.push("--label".into());
            args.push(label.clone());
        }

        // Apply state filter
        if let Some(ref status) = filter.status {
            let state = match status.to_lowercase().as_str() {
                "open" => "open",
                "closed" => "closed",
                _ => "all",
            };
            args.push("--state".into());
            args.push(state.to_string());
        }

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.run_gh(&arg_refs).await?;

        let items: Vec<GhIssueListItem> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse issue list JSON: {e}"))?;

        // Apply `since` filter in Rust (gh issue list doesn't support --since)
        let summaries: Vec<IssueSummary> = items
            .into_iter()
            .filter(|item| {
                if let Some(since) = &filter.since {
                    if let Some(ref updated_str) = item.updated_at {
                        if let Ok(updated) = DateTime::parse_from_rfc3339(updated_str) {
                            return updated.with_timezone(&Utc) >= *since;
                        }
                    }
                    // If we can't parse the date, include the item
                }
                true
            })
            .map(IssueSummary::from)
            .collect();

        Ok(summaries)
    }

    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
        let repo = self.repo()?;

        // Post comment if provided
        if let Some(ref comment) = update.comment {
            self.run_gh(&["issue", "comment", id, "--repo", repo, "--body", comment])
                .await?;
        }

        // Add labels
        if !update.add_labels.is_empty() {
            let labels = update.add_labels.join(",");
            self.run_gh(&["issue", "edit", id, "--repo", repo, "--add-label", &labels])
                .await?;
        }

        // Remove labels
        if !update.remove_labels.is_empty() {
            let labels = update.remove_labels.join(",");
            self.run_gh(&[
                "issue",
                "edit",
                id,
                "--repo",
                repo,
                "--remove-label",
                &labels,
            ])
            .await?;
        }

        // Status change via label (GitHub doesn't have native status — uses labels)
        if let Some(ref status) = update.status {
            self.run_gh(&[
                "issue",
                "edit",
                id,
                "--repo",
                repo,
                "--add-label",
                status,
            ])
            .await?;
        }

        Ok(())
    }

    async fn create_pr(&self, params: PrParams) -> anyhow::Result<String> {
        let repo = params
            .repo
            .as_deref()
            .or(self.repo.as_deref())
            .ok_or_else(|| anyhow::anyhow!("No repository configured for PR creation."))?;

        let mut args: Vec<String> = vec![
            "pr".into(),
            "create".into(),
            "--repo".into(),
            repo.to_string(),
            "--title".into(),
            params.title.clone(),
            "--body".into(),
            params.body.clone(),
            "--head".into(),
            params.head_branch.clone(),
        ];

        if let Some(ref base) = params.base_branch {
            args.push("--base".into());
            args.push(base.clone());
        }

        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = self.run_gh(&arg_refs).await?;

        // `gh pr create` prints the PR URL on stdout
        let url = output.trim().to_string();
        if url.is_empty() {
            anyhow::bail!("gh pr create returned empty output — PR may not have been created.");
        }

        Ok(url)
    }

    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
        let repo = self.repo()?;

        let output = self
            .run_gh(&[
                "issue",
                "list",
                "--repo",
                repo,
                "--json",
                "number,title,labels,state,url,updatedAt",
                "--state",
                "open",
                "--limit",
                "20",
            ])
            .await?;

        let items: Vec<GhIssueListItem> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse poll issue list JSON: {e}"))?;

        let now = Utc::now();
        let last_poll = {
            let guard = self
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            *guard
        };

        let events: Vec<PmEvent> = items
            .into_iter()
            .filter(|item| {
                // On first poll (last_poll is None), return all as IssueCreated.
                // On subsequent polls, only return issues updated since last_poll.
                if let Some(last) = last_poll {
                    if let Some(ref updated_str) = item.updated_at {
                        if let Ok(updated) = DateTime::parse_from_rfc3339(updated_str) {
                            return updated.with_timezone(&Utc) >= last;
                        }
                    }
                    // Can't determine update time — skip
                    false
                } else {
                    true
                }
            })
            .map(|item| {
                let summary = IssueSummary::from(item);
                if last_poll.is_some() {
                    PmEvent::IssueUpdated(summary)
                } else {
                    PmEvent::IssueCreated(summary)
                }
            })
            .collect();

        // Update last_poll timestamp
        {
            let mut guard = self
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            *guard = Some(now);
        }

        Ok(events)
    }
}
