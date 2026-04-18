use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::process::Command;

use crate::adapter::IssueTracker;
use crate::types::{Issue, IssueCreate, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource};

// ─── Private error type ───────────────────────────────────────────────

enum BrCallError {
    Retryable(String),
    Fatal(anyhow::Error),
}

// ─── Private deserialization structs ─────────────────────────────────

#[derive(Deserialize)]
struct BrVersion {
    version: String,
}

#[derive(Deserialize)]
struct BrErrorEnvelope {
    error: BrErrorInner,
}

#[derive(Deserialize)]
struct BrErrorInner {
    #[allow(dead_code)]
    code: String,
    message: String,
    retryable: bool,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct BrIssueWithCounts {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct BrIssueDetails {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default)]
    due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    dependencies: Vec<BrDependency>,
}

#[derive(Deserialize)]
struct BrDependency {
    /// br v1 uses `id` (the depended-on issue ID).
    #[serde(alias = "depends_on_id")]
    id: String,
    #[serde(alias = "type", default)]
    dependency_type: String,
    // Extra fields from br show (ignored — we only need id + type).
    #[serde(default)]
    #[allow(dead_code)]
    title: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    status: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    priority: Option<i32>,
}

// ─── From conversions ─────────────────────────────────────────────────

const BLOCKING_TYPES: &[&str] = &["blocks", "parent-child", "conditional-blocks", "waits-for"];

impl From<BrIssueDetails> for Issue {
    fn from(br: BrIssueDetails) -> Self {
        Self {
            id: br.id.clone(),
            source: PmSource::Beads,
            title: br.title,
            body: br.description.unwrap_or_default(),
            status: br.status,
            labels: br.labels,
            assignee: br.assignee,
            url: format!("beads://{}", br.id),
            priority: Some(br.priority),
            issue_type: Some(br.issue_type),
            blocked_by: br
                .dependencies
                .iter()
                .filter(|d| BLOCKING_TYPES.contains(&d.dependency_type.as_str()))
                .map(|d| d.id.clone())
                .collect(),
            due_at: br.due_at,
            created_at: br.created_at,
            updated_at: br.updated_at,
        }
    }
}

impl From<BrIssueWithCounts> for IssueSummary {
    fn from(br: BrIssueWithCounts) -> Self {
        Self {
            id: br.id.clone(),
            source: PmSource::Beads,
            title: br.title,
            status: br.status,
            labels: br.labels,
            url: format!("beads://{}", br.id),
            priority: Some(br.priority),
            issue_type: Some(br.issue_type),
            assignee: br.assignee,
        }
    }
}

// ─── BeadsAdapter ─────────────────────────────────────────────────────

pub struct BeadsAdapter {
    cwd: PathBuf,
    last_poll: Mutex<Option<DateTime<Utc>>>,
}

impl BeadsAdapter {
    /// Verify that the `br` binary is installed and that the `.beads/` database is readable.
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
            last_poll: Mutex::new(None),
        };

        // Verify binary is installed by running `br version --json`
        let version_output = adapter
            .run_br(vec!["version".into()])
            .await
            .map_err(|e| {
                if e.to_string().contains("br binary not found") {
                    e
                } else {
                    anyhow::anyhow!(
                        "Failed to run `br version`: {e}\n\
                         Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
                    )
                }
            })?;

        let version: BrVersion = serde_json::from_str(&version_output).map_err(|e| {
            anyhow::anyhow!("Failed to parse `br version` output: {e}\nRaw: {version_output}")
        })?;

        tracing::info!(version = %version.version, "connected to beads_rust (br)");

        // Verify .beads/ database is readable by running `br stats --format json`
        adapter
            .run_br(vec!["stats".into()])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read .beads/ database (`br stats`): {e}"))?;

        Ok(adapter)
    }

    /// Run `br` once, returning stdout or a classified error.
    async fn run_br_once(&self, args: &[String]) -> Result<String, BrCallError> {
        let mut cmd = Command::new("br");
        cmd.args(args)
            .arg("--json")
            .current_dir(&self.cwd)
            .env("RUST_LOG", "error");

        let output = cmd.output().await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BrCallError::Fatal(anyhow::anyhow!(
                    "br binary not found. Install: cargo install --git \
                     https://github.com/Dicklesworthstone/beads_rust.git"
                ))
            } else {
                BrCallError::Fatal(anyhow::anyhow!("Failed to execute `br`: {e}"))
            }
        })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if output.status.success() {
            return Ok(stdout);
        }

        // Try to parse structured error from stderr
        if let Ok(envelope) = serde_json::from_str::<BrErrorEnvelope>(&stderr) {
            if envelope.error.retryable {
                return Err(BrCallError::Retryable(envelope.error.message));
            } else {
                return Err(BrCallError::Fatal(anyhow::anyhow!(
                    "br error: {}",
                    envelope.error.message
                )));
            }
        }

        // Also try stdout for error JSON (some CLIs write errors there)
        if let Ok(envelope) = serde_json::from_str::<BrErrorEnvelope>(&stdout) {
            if envelope.error.retryable {
                return Err(BrCallError::Retryable(envelope.error.message));
            } else {
                return Err(BrCallError::Fatal(anyhow::anyhow!(
                    "br error: {}",
                    envelope.error.message
                )));
            }
        }

        // Fall back to raw stderr/stdout
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        Err(BrCallError::Fatal(anyhow::anyhow!("br failed: {}", msg)))
    }

    /// Run `br` with bounded retry: max 2 attempts, 50ms wait on retryable error.
    async fn run_br(&self, args: Vec<String>) -> anyhow::Result<String> {
        tracing::debug!(?args, "running br CLI");

        match self.run_br_once(&args).await {
            Ok(out) => Ok(out),
            Err(BrCallError::Fatal(e)) => Err(e),
            Err(BrCallError::Retryable(msg)) => {
                tracing::debug!(reason = %msg, "br retryable error, retrying after 50ms");
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                match self.run_br_once(&args).await {
                    Ok(out) => Ok(out),
                    Err(BrCallError::Fatal(e)) => Err(e),
                    Err(BrCallError::Retryable(msg2)) => {
                        anyhow::bail!("br retryable error after 2 attempts: {}", msg2)
                    }
                }
            }
        }
    }
}

// ─── IssueTracker implementation ──────────────────────────────────────

#[async_trait]
impl IssueTracker for BeadsAdapter {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        let output = self.run_br(vec!["show".into(), id.to_string()]).await?;
        let mut items: Vec<BrIssueDetails> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse `br show` output: {e}\nRaw: {output}"))?;
        let details = items
            .pop()
            .ok_or_else(|| anyhow::anyhow!("`br show {id}` returned empty result"))?;
        Ok(Issue::from(details))
    }

    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let mut args: Vec<String> = vec!["list".into()];

        if let Some(ref status) = filter.status {
            args.push("-s".into());
            args.push(status.clone());
        }

        for label in &filter.labels {
            args.push("-l".into());
            args.push(label.clone());
        }

        if let Some(min) = filter.priority_min {
            args.push("--priority-min".into());
            args.push(min.to_string());
        }

        if let Some(max) = filter.priority_max {
            args.push("--priority-max".into());
            args.push(max.to_string());
        }

        if let Some(ref itype) = filter.issue_type {
            args.push("-t".into());
            args.push(itype.clone());
        }

        if let Some(ref assignee) = filter.assignee {
            args.push("--assignee".into());
            args.push(assignee.clone());
        }

        // Note: br list has no --since flag; since-based filtering is done
        // client-side in poll() via updated_at comparison.

        if let Some(ref text) = filter.text_search {
            args.push("--title-contains".into());
            args.push(text.clone());
        }

        let limit = filter.limit.unwrap_or(50);
        args.push("--limit".into());
        args.push(limit.to_string());

        let output = self.run_br(args).await?;
        let issues: Vec<BrIssueWithCounts> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse `br list` output: {e}\nRaw: {output}"))?;

        Ok(issues.into_iter().map(IssueSummary::from).collect())
    }

    async fn create_issue(&self, params: IssueCreate) -> anyhow::Result<String> {
        let mut args: Vec<String> = vec!["create".into(), "--silent".into()];

        args.push(params.title.clone());

        if let Some(ref desc) = params.description {
            args.push("-d".into());
            args.push(desc.clone());
        }
        if let Some(ref itype) = params.issue_type {
            args.push("-t".into());
            args.push(itype.clone());
        }
        if let Some(priority) = params.priority {
            args.push("-p".into());
            args.push(priority.to_string());
        }
        if !params.labels.is_empty() {
            args.push("-l".into());
            args.push(params.labels.join(","));
        }
        if let Some(ref parent) = params.parent {
            args.push("--parent".into());
            args.push(parent.clone());
        }
        if let Some(ref assignee) = params.assignee {
            args.push("-a".into());
            args.push(assignee.clone());
        }
        if let Some(est) = params.estimate_minutes {
            args.push("-e".into());
            args.push(est.to_string());
        }

        let output = self.run_br(args).await?;
        let issue_id = output.trim().to_string();

        if issue_id.is_empty() {
            anyhow::bail!("br create returned empty issue ID");
        }

        // Wire additional dependencies (parent-child is handled by --parent above).
        for dep_id in &params.depends_on {
            self.run_br(vec![
                "dep".into(),
                "add".into(),
                issue_id.clone(),
                dep_id.clone(),
            ])
            .await?;
        }

        tracing::info!(id = %issue_id, title = %params.title, "Created beads issue");
        Ok(issue_id)
    }

    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
        // Step 1: update fields (status, priority, assignee) if any are set
        let has_field_update =
            update.status.is_some() || update.priority.is_some() || update.assignee.is_some();

        if has_field_update {
            let mut args: Vec<String> = vec!["update".into(), id.to_string()];

            if let Some(ref status) = update.status {
                args.push("-s".into());
                args.push(status.clone());
            }

            if let Some(priority) = update.priority {
                args.push("-p".into());
                args.push(priority.to_string());
            }

            if let Some(ref assignee) = update.assignee {
                args.push("--assignee".into());
                args.push(assignee.clone());
            }

            self.run_br(args).await?;
        }

        // Step 2: add comment if set
        if let Some(ref comment) = update.comment {
            self.run_br(vec![
                "comments".into(),
                "add".into(),
                id.to_string(),
                comment.clone(),
            ])
            .await?;
        }

        // Step 3: add labels if non-empty (br label add <id> -l <label> ...)
        if !update.add_labels.is_empty() {
            let mut args = vec!["label".into(), "add".into(), id.to_string()];
            for label in &update.add_labels {
                args.push("-l".into());
                args.push(label.clone());
            }
            self.run_br(args).await?;
        }

        // Step 4: remove labels if non-empty (br label remove <id> -l <label> ...)
        if !update.remove_labels.is_empty() {
            let mut args = vec!["label".into(), "remove".into(), id.to_string()];
            for label in &update.remove_labels {
                args.push("-l".into());
                args.push(label.clone());
            }
            self.run_br(args).await?;
        }

        Ok(())
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        self.run_br(vec![
            "dep".into(),
            "add".into(),
            issue_id.to_string(),
            depends_on_id.to_string(),
        ])
        .await?;
        tracing::info!(issue = %issue_id, depends_on = %depends_on_id, "Added dependency");
        Ok(())
    }

    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
        let output = self
            .run_br(vec![
                "list".into(),
                "-s".into(),
                "open".into(),
                "--limit".into(),
                "20".into(),
            ])
            .await?;

        let issues: Vec<BrIssueWithCounts> = serde_json::from_str(&output).map_err(|e| {
            anyhow::anyhow!("Failed to parse `br list` poll output: {e}\nRaw: {output}")
        })?;

        let now = Utc::now();
        let last_poll = {
            let guard = self
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            *guard
        };

        let events: Vec<PmEvent> = issues
            .into_iter()
            .filter(|item| {
                if let Some(last) = last_poll {
                    item.updated_at >= last
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
