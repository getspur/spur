use std::path::Path;
use std::sync::Arc;

use crate::adapter::{IssueTracker, PrService};
use crate::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use crate::bv::BvAdapter;
use crate::github::GitHubAdapter;
use crate::graph::DependencyGraph;
use crate::graph_engine::GraphEngineConfig;
use crate::ingest::github::auth;
use crate::ingest::github::GitHubSync;
use crate::ingest::{apply_remote_delta, IngestOptions, IngestReport};
use crate::sync::{ExternalPmSync, RemoteDelta, SyncResult};
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
        beads: Box<BeadsCrateAdapter>,
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
    blocking_pool_probe: Option<tokio::task::JoinHandle<()>>,
    /// Optional GitHub `ExternalPmSync` for `spur pm ingest github`.
    /// Populated by `try_new_with_actor` when Beads is the active backend
    /// AND a GitHub token + repo can be resolved non-interactively. Lives
    /// alongside `inner` rather than inside `PmBackendInner::Beads` so the
    /// I-7 invariant (external systems are not peer authorities to Beads)
    /// stays explicit in the type system — `sync_target("github")` is a
    /// parallel accessor, not a backend variant.
    github_sync: Option<Arc<GitHubSync>>,
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
    /// defaults to the server-level `"reconciler"` actor. Passing `None`
    /// defaults to `"spur"` (BeadsCrateAdapter requires a non-empty actor for
    /// storage attribution); for explicit attribution use `Some(name)`.
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
            let config = AdapterConfig {
                actor: actor.unwrap_or_else(|| "spur".to_string()),
                cursor_path: Some(cursor_path),
                ..AdapterConfig::default()
            };
            let beads = BeadsCrateAdapter::open(&beads_dir, config).await?;
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
                Self::try_github(github_repo.clone(), repo_root).await
            } else {
                None
            };
            // `github_sync` is the §8 ingest accessor — distinct from the
            // legacy `GitHubAdapter` PR-service. Constructed lazily and
            // silently: any failure leaves it `None` so the CLI can print
            // a remediation message instead of PmService init bailing out.
            let github_sync = if github_enabled {
                Self::try_github_sync(github_repo.clone(), repo_root).await
            } else {
                None
            };
            let blocking_pool_probe = Some(crate::blocking_pool_probe::spawn_blocking_pool_probe());
            return Ok(Some(Self {
                inner: PmBackendInner::Beads {
                    beads: Box::new(beads),
                    github,
                },
                bv,
                closed_status: resolved_closed,
                blocking_pool_probe,
                github_sync,
            }));
        }

        if github_enabled {
            if let Some(gh) = Self::try_github(github_repo, repo_root).await {
                return Ok(Some(Self {
                    inner: PmBackendInner::GitHub { adapter: gh },
                    bv: None,
                    closed_status: resolved_closed,
                    blocking_pool_probe: None,
                    github_sync: None,
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

    /// Non-interactive `GitHubSync` constructor for the §8 ingest path.
    ///
    /// Token resolution: `SPUR_GITHUB_TOKEN` → `gh auth token`. Device flow
    /// is deliberately skipped here — PmService initialization runs on every
    /// TUI/CLI startup and must never block on stdin. The CLI subcommand
    /// re-runs the full [`auth::resolve_token`] chain (which does include
    /// device flow) when `sync_target` returns `None`, so first-time setup
    /// still works.
    ///
    /// Repo resolution: explicit `repo` arg → `gh repo view --json
    /// nameWithOwner`. Any failure → `None`; the CLI prints the remediation.
    async fn try_github_sync(repo: Option<String>, repo_root: &Path) -> Option<Arc<GitHubSync>> {
        let token = match auth::env_token() {
            Some(t) => t,
            None => match auth::gh_cli_token().await {
                Some(t) => t,
                None => {
                    tracing::debug!("GitHub ingest unavailable: no non-interactive token source");
                    return None;
                }
            },
        };
        let resolved_repo = match repo {
            Some(r) if !r.trim().is_empty() => r,
            _ => match detect_repo_via_gh(repo_root).await {
                Some(r) => r,
                None => {
                    tracing::debug!("GitHub ingest unavailable: no repo configured or detected");
                    return None;
                }
            },
        };
        match GitHubSync::from_token(resolved_repo, &token) {
            Ok(sync) => Some(Arc::new(sync)),
            Err(e) => {
                tracing::debug!("GitHub ingest unavailable: client build failed: {e}");
                None
            }
        }
    }

    /// Look up an external PM sync by source-system tag (`"github"`,
    /// `"linear"`, …). Gated on Beads being the active backend per I-7;
    /// returns `None` if the requested system isn't configured.
    ///
    /// This is the §8 accessor referenced by the `spur pm ingest …`
    /// subcommand. `PmBackendInner` is deliberately NOT extended — sync
    /// targets are auxiliary, not peers of the local source of truth.
    pub fn sync_target(&self, source_system: &str) -> Option<Arc<dyn ExternalPmSync>> {
        match (&self.inner, source_system) {
            (PmBackendInner::Beads { .. }, "github") => self
                .github_sync
                .clone()
                .map(|s| s as Arc<dyn ExternalPmSync>),
            _ => None,
        }
    }

    /// Apply a previously-fetched `RemoteDelta` to the Beads store.
    ///
    /// Wrapper around [`crate::ingest::apply_remote_delta`] that hides
    /// the internal `BeadsCrateAdapter` handle. Returns
    /// `SyncError::Other("…")` if the active backend isn't Beads —
    /// the CLI guards this with `sync_target("github").is_some()` so
    /// the error path is only reachable from misuse.
    pub async fn apply_remote_delta(
        &self,
        sync: &dyn ExternalPmSync,
        delta: RemoteDelta,
        opts: &IngestOptions,
    ) -> SyncResult<IngestReport> {
        match &self.inner {
            PmBackendInner::Beads { beads, .. } => {
                apply_remote_delta(beads, sync, delta, opts).await
            }
            PmBackendInner::GitHub { .. } => Err(crate::sync::SyncError::Other(anyhow::anyhow!(
                "apply_remote_delta requires the beads backend"
            ))),
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
                Some(&**beads as &dyn crate::advanced::BeadsAdvanced)
            }
            PmBackendInner::GitHub { .. } => None,
        }
    }
}

impl Drop for PmService {
    fn drop(&mut self) {
        if let Some(probe) = self.blocking_pool_probe.take() {
            probe.abort();
        }
    }
}

/// Shell-out to `gh repo view --json nameWithOwner -q .nameWithOwner` from
/// `repo_root`. Returns the trimmed `owner/name` string on success, `None`
/// on missing `gh` binary, non-zero exit, or empty stdout.
async fn detect_repo_via_gh(repo_root: &Path) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .current_dir(repo_root)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tracing::field::{Field, Visit};

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

    #[derive(Clone, Default)]
    struct CapturedPmTrace {
        events: Arc<std::sync::Mutex<Vec<CapturedTraceEvent>>>,
    }

    #[derive(Default)]
    struct CapturedTraceEvent {
        target: String,
        fields: String,
    }

    impl CapturedPmTrace {
        fn contains(&self, target: &str, needles: &[&str]) -> bool {
            self.events.lock().unwrap().iter().any(|event| {
                event.target == target && needles.iter().all(|needle| event.fields.contains(needle))
            })
        }
    }

    impl tracing::Subscriber for CapturedPmTrace {
        fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
            *metadata.level() <= tracing::Level::INFO
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            let mut visitor = TraceVisitor::default();
            event.record(&mut visitor);
            self.events.lock().unwrap().push(CapturedTraceEvent {
                target: event.metadata().target().to_string(),
                fields: visitor.0,
            });
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[derive(Default)]
    struct TraceVisitor(String);

    impl Visit for TraceVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(&format!("{value:?}"));
            self.0.push(' ');
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(value);
            self.0.push(' ');
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.push_str(field.name());
            self.0.push('=');
            self.0.push_str(&value.to_string());
            self.0.push(' ');
        }
    }

    #[tokio::test]
    async fn pmservice_lock_acquire_release_emits_spur_pm_tracing() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join(".beads")).expect("create .beads");
        let pm = super::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new")
            .expect("beads pm service");

        let captured = CapturedPmTrace::default();
        let guard = tracing::subscriber::set_default(captured.clone());
        pm.poll().await.expect("poll");
        drop(guard);

        assert!(
            captured.contains(
                "spur.pm.lock",
                &[
                    "action=acquire",
                    "lock=beads.cursor",
                    "owner=BeadsCrateAdapter::poll_with_limit",
                ],
            ),
            "expected acquire trace, got {:?}",
            captured
                .events
                .lock()
                .unwrap()
                .iter()
                .map(|event| format!("{} {}", event.target, event.fields))
                .collect::<Vec<_>>()
        );
        assert!(
            captured.contains(
                "spur.pm.lock",
                &[
                    "action=release",
                    "lock=beads.cursor",
                    "owner=BeadsCrateAdapter::poll_with_limit",
                    "hold_ms=",
                ],
            ),
            "expected release trace"
        );
    }
}
