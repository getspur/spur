use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::process::Command;

use crate::adapter::IssueTracker;
use crate::poll_cursor::{PollCursor, POLL_FETCH_LIMIT};
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
struct BrListEnvelope {
    issues: Vec<BrIssueWithCounts>,
    #[serde(default)]
    has_more: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum BrListOutput {
    Envelope(BrListEnvelope),
    Bare(Vec<BrIssueWithCounts>),
}

struct BrListPage {
    issues: Vec<BrIssueWithCounts>,
    has_more: Option<bool>,
}

impl BrListPage {
    fn saturated(&self, limit: usize) -> bool {
        self.has_more.unwrap_or(self.issues.len() == limit)
    }
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

#[cfg(test)]
fn paginate_issue_summaries(issues: Vec<IssueSummary>, filter: &IssueFilter) -> Vec<IssueSummary> {
    let offset = filter.offset.unwrap_or(0);
    let limit = filter.limit.unwrap_or(50);
    issues.into_iter().skip(offset).take(limit).collect()
}

fn parse_br_list_output(output: &str, command: &str) -> anyhow::Result<BrListPage> {
    match serde_json::from_str::<BrListOutput>(output) {
        Ok(BrListOutput::Envelope(envelope)) => Ok(BrListPage {
            issues: envelope.issues,
            has_more: envelope.has_more,
        }),
        Ok(BrListOutput::Bare(issues)) => Ok(BrListPage {
            issues,
            has_more: None,
        }),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to parse `{command}` output: {e}\nRaw: {output}"
        )),
    }
}

// ─── BeadsAdapter ─────────────────────────────────────────────────────

pub struct BeadsAdapter {
    cwd: PathBuf,
    last_poll: Mutex<Option<PollCursor>>,
    default_actor: Option<String>,
    cursor_path: Option<PathBuf>, // used by Task 9; present now for forward compat
}

impl BeadsAdapter {
    /// Verify that the `br` binary is installed and that the `.beads/` database is readable.
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        Self::connect_with_actor(repo_root, None, None).await
    }

    pub async fn connect_with_actor(
        repo_root: &Path,
        default_actor: Option<String>,
        cursor_path: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
            last_poll: Mutex::new(None),
            default_actor,
            cursor_path,
        };

        // Hydrate last_poll from disk if cursor_path is set and file exists.
        if let Some(cursor) = adapter.load_cursor() {
            let mut guard = adapter
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            *guard = Some(cursor);
        }

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

    async fn issue_details(&self, id: &str) -> anyhow::Result<BrIssueDetails> {
        let output = self.run_br(vec!["show".into(), id.to_string()]).await?;
        let mut items: Vec<BrIssueDetails> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("Failed to parse `br show` output: {e}\nRaw: {output}"))?;
        items
            .pop()
            .ok_or_else(|| anyhow::anyhow!("`br show {id}` returned empty result"))
    }

    #[allow(dead_code)]
    pub(crate) async fn plan_id_label_for_epic(&self, id: &str) -> anyhow::Result<Option<String>> {
        let issue = self.issue_details(id).await?;
        if !issue.issue_type.eq_ignore_ascii_case("epic") {
            return Ok(None);
        }

        Ok(issue
            .labels
            .into_iter()
            .find(|label| label.starts_with("spur:plan-id:")))
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
    async fn run_br_inner(&self, args: Vec<String>) -> anyhow::Result<String> {
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

    /// Run `br` with an optional actor override prepended as `--actor <actor>`.
    async fn run_br_as(
        &self,
        args: Vec<String>,
        actor_override: Option<&str>,
    ) -> anyhow::Result<String> {
        let actor = actor_override.or(self.default_actor.as_deref());
        let mut full = args;
        if let Some(a) = actor {
            full.insert(0, "--actor".into());
            full.insert(1, a.to_string());
        }
        self.run_br_inner(full).await
    }

    /// Run `br` with the default actor (if any) attached.
    async fn run_br(&self, args: Vec<String>) -> anyhow::Result<String> {
        self.run_br_as(args, None).await
    }

    /// Load the poll cursor from disk (if `cursor_path` is set and file exists).
    ///
    /// Disk format is JSON-serialized `PollCursor`. For backward compatibility
    /// with v0a.1 cursor files that stored a bare RFC3339 string, we first attempt
    /// JSON deserialization; if that fails, we try parsing the raw content as an
    /// RFC3339 datetime and produce a `PollCursor` with an empty `ids_at_boundary`
    /// set. This ensures a clean upgrade without losing the cursor position.
    fn load_cursor(&self) -> Option<PollCursor> {
        let path = self.cursor_path.as_ref()?;
        // Distinguish "file does not exist yet" (fresh install) from
        // "file exists but unreadable" (permissions, IO error): the latter
        // is worth a warn so operators notice silent-replay risk.
        let contents = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                tracing::warn!(
                    ?path,
                    "cursor file exists but unreadable ({e}); starting without cursor — next poll may replay"
                );
                return None;
            }
        };
        let trimmed = contents.trim();

        // Try JSON first (new format).
        if let Ok(cursor) = serde_json::from_str::<PollCursor>(trimmed) {
            return Some(cursor);
        }

        // Backward-compat: v0a.1 stored a bare RFC3339 string.
        if let Ok(ts) = trimmed.parse::<DateTime<Utc>>() {
            return Some(PollCursor {
                ts,
                ids_at_boundary: HashSet::new(),
            });
        }

        tracing::warn!(
            ?path,
            "cursor file content unparseable as JSON or RFC3339; starting without cursor"
        );
        None
    }

    /// Poll for open-issue updates, fetching up to `limit` rows.
    ///
    /// Production always uses [`POLL_FETCH_LIMIT`] via [`IssueTracker::poll`].
    /// This inherent helper exists so integration tests can drive the
    /// saturation boundary (`items.len() == limit`) deterministically at a
    /// small N without creating hundreds of real issues.
    pub async fn poll_with_limit(&self, limit: usize) -> anyhow::Result<Vec<PmEvent>> {
        let output = self
            .run_br(vec![
                "list".into(),
                "-s".into(),
                "open".into(),
                "--limit".into(),
                limit.to_string(),
            ])
            .await?;

        let page = parse_br_list_output(&output, "br list poll")?;

        // Saturation sentinel: `br list --limit N` truncates. If the returned
        // batch is exactly `limit` rows, more qualifying rows may exist on the
        // backend with `updated_at <= max(fetched.ts)`. Advancing the cursor
        // past `max(fetched.ts)` would hide those rows forever.
        let saturated = page.saturated(limit);

        // Snapshot the cursor under the lock, then release immediately.
        let prior_cursor: Option<PollCursor> = {
            let guard = self
                .last_poll
                .lock()
                .map_err(|e| anyhow::anyhow!("last_poll mutex poisoned: {e}"))?;
            guard.clone()
        };

        // Apply the boundary-safe filter: emit only items that pass the cursor predicate.
        let had_prior = prior_cursor.is_some();
        let kept: Vec<BrIssueWithCounts> = page
            .issues
            .into_iter()
            .filter(|item| match &prior_cursor {
                None => true,
                Some(c) => c.allows(&item.id, item.updated_at),
            })
            .collect();

        // Advance the cursor based on the kept items.
        let new_cursor: Option<PollCursor> = if !kept.is_empty() {
            if saturated {
                // Data-loss guard: emit observed events but preserve the prior
                // cursor. On a first saturated poll we intentionally leave the
                // cursor unset instead of seeding it to `Utc::now()`: older
                // rows outside the truncated batch must remain eligible for
                // future polls once the head of the backlog drains. Callers
                // already dedup first-poll replays on `id`.
                tracing::warn!(
                    limit,
                    kept_count = kept.len(),
                    "poll() fetch saturated --limit; preserving cursor to avoid \
                     boundary-row data loss. Consider raising POLL_FETCH_LIMIT \
                     or investigating row-update velocity."
                );
                prior_cursor.clone()
            } else {
                let max_ts = kept.iter().map(|i| i.updated_at).max().unwrap(); // safe: kept non-empty
                let ids_at_max: HashSet<String> = kept
                    .iter()
                    .filter(|i| i.updated_at == max_ts)
                    .map(|i| i.id.clone())
                    .collect();
                Some(PollCursor {
                    ts: max_ts,
                    ids_at_boundary: ids_at_max,
                })
            }
        } else if let Some(existing) = prior_cursor {
            Some(existing)
        } else {
            Some(PollCursor {
                ts: Utc::now(),
                ids_at_boundary: HashSet::new(),
            })
        };

        let events: Vec<PmEvent> = kept
            .into_iter()
            .map(|item| {
                let summary = IssueSummary::from(item);
                if had_prior {
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
            *guard = new_cursor.clone();
        }
        if let Some(cursor) = new_cursor.as_ref() {
            self.save_cursor(cursor);
        }

        Ok(events)
    }

    /// Persist the poll cursor to disk (if `cursor_path` is set).
    /// Failures are logged as warnings and do not abort the poll (best-effort).
    fn save_cursor(&self, cursor: &PollCursor) {
        if let Some(path) = self.cursor_path.as_ref() {
            match serde_json::to_string(cursor) {
                Ok(s) => {
                    if let Err(e) = std::fs::write(path, s) {
                        tracing::warn!(?path, "failed to write cursor file: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to serialize cursor: {e}");
                }
            }
        }
    }
}

// ─── IssueTracker implementation ──────────────────────────────────────

#[async_trait]
impl IssueTracker for BeadsAdapter {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        Ok(Issue::from(self.issue_details(id).await?))
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

        if filter.include_closed {
            args.push("-a".into());
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

        let requested_limit = filter.limit.unwrap_or(50);
        let offset = filter.offset.unwrap_or(0);
        args.push("--limit".into());
        args.push(requested_limit.to_string());
        if offset > 0 {
            args.push("--offset".into());
            args.push(offset.to_string());
        }

        let output = self.run_br(args).await?;
        let page = parse_br_list_output(&output, "br list")?;

        Ok(page.issues.into_iter().map(IssueSummary::from).collect())
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

        // Step 3: add labels one at a time. The current br CLI rejects
        // repeated `-l` flags in a single invocation.
        for label in &update.add_labels {
            self.run_br(vec![
                "label".into(),
                "add".into(),
                id.to_string(),
                "-l".into(),
                label.clone(),
            ])
            .await?;
        }

        // Step 4: remove labels one at a time for the same CLI reason.
        for label in &update.remove_labels {
            self.run_br(vec![
                "label".into(),
                "remove".into(),
                id.to_string(),
                "-l".into(),
                label.clone(),
            ])
            .await?;
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
        self.poll_with_limit(POLL_FETCH_LIMIT).await
    }
}

// ─── BeadsAdvanced impl ───────────────────────────────────────────────

use crate::advanced::{BeadsAdvanced, Comment, CommentId, DependencyCycle, ReadyFilter};

#[derive(serde::Deserialize)]
struct BrReadyItem {
    id: String,
    title: String,
    status: String,
    priority: i32,
    issue_type: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    assignee: Option<String>,
}

impl From<BrReadyItem> for IssueSummary {
    fn from(r: BrReadyItem) -> Self {
        Self {
            id: r.id.clone(),
            source: PmSource::Beads,
            title: r.title,
            status: r.status,
            labels: r.labels,
            url: format!("beads://{}", r.id),
            priority: Some(r.priority),
            issue_type: Some(r.issue_type),
            assignee: r.assignee,
        }
    }
}

#[async_trait]
impl BeadsAdvanced for BeadsAdapter {
    async fn list_ready(&self, filter: ReadyFilter) -> anyhow::Result<Vec<IssueSummary>> {
        let mut args: Vec<String> = vec!["ready".into()];

        if let Some(ref a) = filter.assignee {
            args.push("--assignee".into());
            args.push(a.clone());
        }
        for l in &filter.labels_all {
            args.push("-l".into());
            args.push(l.clone());
        }
        for l in &filter.labels_any {
            args.push("--label-any".into());
            args.push(l.clone());
        }
        if let Some(ref t) = filter.issue_type {
            args.push("-t".into());
            args.push(t.clone());
        }
        // `br ready -p <n>` is a repeatable set-membership filter (0-4 or P0-P4),
        // NOT a range: `-p 0 -p 2` returns P0 ∪ P2. Emit one `-p` per element.
        for p in &filter.priorities {
            args.push("-p".into());
            args.push(p.to_string());
        }
        args.push("--limit".into());
        args.push(filter.limit.unwrap_or(20).to_string());

        let output = self.run_br(args).await?;
        let items: Vec<BrReadyItem> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parse `br ready`: {e}\nraw: {output}"))?;
        Ok(items.into_iter().map(IssueSummary::from).collect())
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<Comment>> {
        #[derive(serde::Deserialize)]
        struct BrComment {
            // br returns id as integer
            id: serde_json::Value,
            // br uses "text" for comment body
            #[serde(alias = "body")]
            text: String,
            // br uses "author" for the commenter
            #[serde(alias = "actor", default)]
            author: Option<String>,
            created_at: chrono::DateTime<chrono::Utc>,
        }
        let output = self
            .run_br(vec!["comments".into(), "list".into(), issue_id.into()])
            .await?;
        let items: Vec<BrComment> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parse `br comments list`: {e}\nraw: {output}"))?;
        Ok(items
            .into_iter()
            .map(|c| {
                let id_str = match &c.id {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                Comment {
                    id: id_str,
                    body: c.text,
                    actor: c.author.unwrap_or_default(),
                    created_at: c.created_at,
                }
            })
            .collect())
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<CommentId> {
        #[derive(serde::Deserialize)]
        struct BrCommentAdd {
            // br returns id as integer; accept both via serde_json::Value then convert.
            id: serde_json::Value,
        }
        let output = self
            .run_br(vec![
                "comments".into(),
                "add".into(),
                issue_id.into(),
                body.into(),
            ])
            .await?;
        // `br comments add --json` returns `{"id": <int|str>}` on success.
        let added: BrCommentAdd = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parse `br comments add`: {e}\nraw: {output}"))?;
        let id_str = match &added.id {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            other => anyhow::bail!("unexpected id type from br comments add: {other}"),
        };
        Ok(id_str)
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        self.run_br(vec![
            "dep".into(),
            "remove".into(),
            issue_id.into(),
            depends_on_id.into(),
        ])
        .await?;
        Ok(())
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<DependencyCycle>> {
        #[derive(serde::Deserialize)]
        struct BrCycle {
            #[serde(default)]
            issues: Vec<String>,
        }
        #[derive(serde::Deserialize)]
        struct BrCyclesOutput {
            #[serde(default)]
            cycles: Vec<BrCycle>,
        }
        let output = self.run_br(vec!["dep".into(), "cycles".into()]).await?;
        // `br dep cycles --json` returns either an array or {"cycles": [...]};
        // try the wrapped form first, then fall back to a bare array.
        if let Ok(wrapped) = serde_json::from_str::<BrCyclesOutput>(&output) {
            return Ok(wrapped
                .cycles
                .into_iter()
                .map(|c| DependencyCycle { issues: c.issues })
                .collect());
        }
        let bare: Vec<BrCycle> = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("parse `br dep cycles`: {e}\nraw: {output}"))?;
        Ok(bare
            .into_iter()
            .map(|c| DependencyCycle { issues: c.issues })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    const BR_LIST_ROW: &str = r#"{
        "id": "bd-1",
        "title": "Issue 1",
        "status": "open",
        "priority": 2,
        "issue_type": "task",
        "created_at": "2026-05-05T00:00:00Z",
        "updated_at": "2026-05-05T00:00:00Z"
    }"#;

    fn summary(id: &str) -> crate::types::IssueSummary {
        crate::types::IssueSummary {
            id: id.to_string(),
            source: crate::types::PmSource::Beads,
            title: format!("Issue {id}"),
            status: "open".into(),
            labels: Vec::new(),
            url: format!("beads://{id}"),
            priority: None,
            issue_type: Some("task".into()),
            assignee: None,
        }
    }

    #[test]
    fn parse_br_list_accepts_023_envelope() {
        let raw = format!(
            r#"{{
                "issues": [{BR_LIST_ROW}],
                "total": 1,
                "limit": 50,
                "offset": 0,
                "has_more": false
            }}"#
        );

        let page = super::parse_br_list_output(&raw, "br list").expect("parse envelope");

        assert_eq!(page.issues.len(), 1);
        assert_eq!(page.issues[0].id, "bd-1");
        assert!(!page.saturated(1));
    }

    #[test]
    fn parse_br_list_accepts_empty_023_envelope() {
        let raw = r#"{
            "issues": [],
            "total": 0,
            "limit": 1000,
            "offset": 0,
            "has_more": false
        }"#;

        let page = super::parse_br_list_output(raw, "br list").expect("parse empty envelope");

        assert!(page.issues.is_empty());
        assert!(!page.saturated(1_000));
    }

    #[test]
    fn parse_br_list_accepts_legacy_bare_array() {
        let raw = format!("[{BR_LIST_ROW}]");

        let page = super::parse_br_list_output(&raw, "br list").expect("parse bare array");

        assert_eq!(page.issues.len(), 1);
        assert_eq!(page.issues[0].id, "bd-1");
        assert!(page.saturated(1));
    }

    #[test]
    fn paginate_issue_summaries_without_offset_preserves_first_page() {
        let issues = vec![summary("bd-1"), summary("bd-2"), summary("bd-3")];
        let page = super::paginate_issue_summaries(
            issues,
            &crate::types::IssueFilter {
                limit: Some(2),
                ..Default::default()
            },
        );
        let ids: Vec<String> = page.into_iter().map(|issue| issue.id).collect();
        assert_eq!(ids, vec!["bd-1".to_string(), "bd-2".to_string()]);
    }

    #[test]
    fn paginate_issue_summaries_applies_offset() {
        let issues = vec![summary("bd-1"), summary("bd-2"), summary("bd-3")];
        let page = super::paginate_issue_summaries(
            issues,
            &crate::types::IssueFilter {
                offset: Some(1),
                limit: Some(5),
                ..Default::default()
            },
        );
        let ids: Vec<String> = page.into_iter().map(|issue| issue.id).collect();
        assert_eq!(ids, vec!["bd-2".to_string(), "bd-3".to_string()]);
    }

    #[test]
    fn paginate_issue_summaries_caps_length_after_offset() {
        let issues = vec![
            summary("bd-1"),
            summary("bd-2"),
            summary("bd-3"),
            summary("bd-4"),
        ];
        let page = super::paginate_issue_summaries(
            issues,
            &crate::types::IssueFilter {
                offset: Some(1),
                limit: Some(2),
                ..Default::default()
            },
        );
        let ids: Vec<String> = page.into_iter().map(|issue| issue.id).collect();
        assert_eq!(ids, vec!["bd-2".to_string(), "bd-3".to_string()]);
    }

    #[test]
    fn paginate_issue_summaries_keeps_exact_page_length() {
        let issues = vec![summary("bd-1"), summary("bd-2"), summary("bd-3")];
        let page = super::paginate_issue_summaries(
            issues,
            &crate::types::IssueFilter {
                limit: Some(3),
                ..Default::default()
            },
        );
        let ids: Vec<String> = page.into_iter().map(|issue| issue.id).collect();
        assert_eq!(
            ids,
            vec!["bd-1".to_string(), "bd-2".to_string(), "bd-3".to_string()]
        );
    }

    #[test]
    fn paginate_issue_summaries_handles_empty_input() {
        let page =
            super::paginate_issue_summaries(Vec::new(), &crate::types::IssueFilter::default());
        assert!(page.is_empty());
    }
}
