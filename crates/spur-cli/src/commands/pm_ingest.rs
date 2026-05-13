//! `spur pm ingest github <owner>/<repo>` — Phase 1 ingest entry point.
//!
//! Per spec §8 of `docs/architecture/spur-pm-github-ingest.md`:
//!
//! - Resolves `PmService::sync_target("github")`; on `None`, prints the §7.1
//!   remediation and exits 1 (`NeedsAuth`).
//! - Calls `sync.fetch_changes_since(opts.since)` then `pm.apply_remote_delta`.
//! - `--dry-run`: skips `apply_remote_delta`; reports fetched remote counts.
//! - `--json`: writes `IngestReport` as JSON and always exits 0 unless a
//!   hard `SyncError` fires.
//! - Default (human) output: progress + summary; non-zero exit iff
//!   `conflicts > 0` or any `SyncError`.
//!
//! Exit codes match the §6 failure model: 1 = NeedsAuth, 2 = Transient,
//! 3 = Malformed, 0 = success-or-conflicts-only (JSON) / success-no-conflicts
//! (human).

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use chrono::{DateTime, Utc};
use spur_acp::config::SpurConfig;
use spur_pm::ingest::{IngestOptions, IngestReport};
use spur_pm::sync::{ExternalPmSync, SyncError};
use spur_pm::PmService;

/// Parsed CLI args for the `ingest github` subcommand.
///
/// `repo` is the positional `<owner>/<repo>`; the caller resolves it from the
/// CLI or from `[pm.github].default_repo` before passing.
///
/// `page_size` is accepted per spec §8 but is honored only at GitHubSync
/// construction time. Phase 1 builds the sync with the default page size
/// (25) at PmService init; runtime overrides land with the Phase 2 sync
/// rebuild path, so the field is parsed-but-unused here.
#[derive(Debug, Clone)]
pub struct IngestGitHubArgs {
    pub repo: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub label_namespace: String,
    #[allow(dead_code)]
    pub page_size: u32,
    pub dry_run: bool,
    pub json: bool,
}

impl Default for IngestGitHubArgs {
    fn default() -> Self {
        Self {
            repo: None,
            since: None,
            label_namespace: "gh".to_string(),
            page_size: 25,
            dry_run: false,
            json: false,
        }
    }
}

/// Exit-code mapping from §6 of the spec.
pub fn exit_code_for(result: &Result<IngestReport, SyncError>, json_mode: bool) -> i32 {
    match result {
        Err(SyncError::NeedsAuth(_)) => 1,
        Err(SyncError::Transient(_)) => 2,
        Err(SyncError::Malformed(_)) => 3,
        Err(SyncError::Gone(_)) => 1,
        Err(SyncError::RateLimited { .. }) => 2,
        Err(SyncError::Other(_)) => 1,
        Ok(report) => {
            if json_mode {
                // Per spec: `--json` always exits 0 unless hard error.
                0
            } else if report.conflicts.is_empty() {
                0
            } else {
                // Non-zero on conflicts in human mode so scripts can detect.
                1
            }
        }
    }
}

/// Format an [`IngestReport`] for human (terminal) output.
pub fn format_human_report(report: &IngestReport) -> String {
    let mut out = String::new();
    let dry_run_suffix = if report.dry_run { " (dry-run)" } else { "" };
    out.push_str(&format!(
        "[spur] ingest {}@{} done{}\n",
        report.source_system, report.source_repo, dry_run_suffix
    ));
    if report.dry_run || report.fetched_remote_nodes > 0 {
        out.push_str(&format!("  fetched:    {}\n", report.fetched_remote_nodes));
    }
    out.push_str(&format!("  ingested:   {}\n", report.ingested));
    out.push_str(&format!("  updated:    {}\n", report.updated));
    out.push_str(&format!("  unchanged:  {}\n", report.unchanged));
    out.push_str(&format!("  conflicts:  {}\n", report.conflicts.len()));
    out.push_str(&format!("  deletions:  {}\n", report.deletions.len()));
    out.push_str(&format!("  dep-hints:  {}\n", report.dep_hints_added));
    out.push_str(&format!("  comments:   {}\n", report.comments_added));
    if !report.conflicts.is_empty() {
        out.push_str("\nConflicts:\n");
        for c in &report.conflicts {
            out.push_str(&format!(
                "  {} (remote {}): {:?}\n",
                c.beads_id, c.remote_id, c.reason
            ));
        }
    }
    out
}

/// Format a `SyncError` as a one-paragraph human remediation per §6.
pub fn format_error(err: &SyncError) -> String {
    match err {
        SyncError::NeedsAuth(_) => {
            "Authentication failed. Run `gh auth login` or set $SPUR_GITHUB_TOKEN.".to_string()
        }
        SyncError::Gone(s) => format!("GitHub repo not found or inaccessible: {s}"),
        SyncError::RateLimited { retry_after_s } => {
            format!("GitHub rate limit hit; retry in {retry_after_s}s.")
        }
        SyncError::Transient(s) => {
            format!("Network error: {s}. Partial progress saved; re-run to resume.")
        }
        SyncError::Malformed(s) => {
            format!("GitHub returned an unexpected response: {s}. Please file a bug.")
        }
        SyncError::Other(e) => format!("Error: {e:#}"),
    }
}

/// Pretty-print an [`IngestReport`] as indented JSON.
pub fn format_json_report(report: &IngestReport) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

/// Build an `IngestOptions` from CLI args + spec defaults.
pub fn ingest_options_from(args: &IngestGitHubArgs) -> IngestOptions {
    IngestOptions {
        since: args.since,
        label_namespace: args.label_namespace.clone(),
        auto_label: Some("spur-managed".to_string()),
        dry_run: args.dry_run,
        lock_timeout_ms: None,
    }
}

/// Run the full ingest flow against an explicit `&dyn ExternalPmSync`.
///
/// Factored out of [`run`] so the T-7 snapshot tests can drive the flow
/// with a `MockSync` from `spur-pm`'s `test-helpers` feature.
pub async fn run_with_sync(
    pm: &PmService,
    sync: &dyn ExternalPmSync,
    args: &IngestGitHubArgs,
    mut stdout: impl Write,
    mut stderr: impl Write,
) -> i32 {
    let opts = ingest_options_from(args);
    if !args.json {
        let _ = writeln!(
            stderr,
            "[spur] ingest {}@{}: fetching from GitHub… (page size {})",
            sync.source_system(),
            sync.source_repo(),
            args.page_size,
        );
    }
    // §8: --dry-run skips apply but still calls fetch_changes_since.
    let fetch_result = sync.fetch_changes_since(opts.since).await;

    let result: Result<IngestReport, SyncError> = match fetch_result {
        Ok(delta) => {
            if args.dry_run {
                let fetched_remote_nodes = delta.nodes.len();
                let dep_hints_added = delta.nodes.iter().map(|node| node.dep_hints.len()).sum();
                let comments_added = delta.nodes.iter().map(|node| node.comments.len()).sum();
                Ok(IngestReport {
                    source_system: sync.source_system().to_string(),
                    source_repo: sync.source_repo().to_string(),
                    fetched_remote_nodes,
                    dry_run: true,
                    dep_hints_added,
                    comments_added,
                    ..Default::default()
                })
            } else {
                pm.apply_remote_delta(sync, delta, &opts).await
            }
        }
        Err(e) => Err(e),
    };

    match &result {
        Ok(report) => {
            if args.json {
                match format_json_report(report) {
                    Ok(json) => {
                        let _ = writeln!(stdout, "{json}");
                    }
                    Err(e) => {
                        let _ = writeln!(stderr, "Error serializing report: {e}");
                        return 1;
                    }
                }
            } else {
                let _ = write!(stdout, "{}", format_human_report(report));
            }
        }
        Err(e) => {
            let _ = writeln!(stderr, "{}", format_error(e));
        }
    }
    exit_code_for(&result, args.json)
}

/// Load `.spur/config.toml` from `repo_root` (or fall back to the user
/// global config, or defaults). Matches the precedence used by
/// `spur-cli`'s `load_config_for_repo` so the ingest subcommand reads the
/// same `[pm.*]` blocks as the rest of the CLI.
fn load_config_for_repo(repo_root: &Path) -> Result<SpurConfig> {
    let project_config = repo_root.join(".spur").join("config.toml");
    let user_config = directories::BaseDirs::new()
        .map(|d| d.home_dir().join(".spur/config.toml"))
        .unwrap_or_default();
    if project_config.exists() {
        let content = std::fs::read_to_string(&project_config)?;
        Ok(toml::from_str(&content)?)
    } else if user_config.exists() {
        let content = std::fs::read_to_string(&user_config)?;
        Ok(toml::from_str(&content)?)
    } else {
        Ok(SpurConfig::default())
    }
}

/// Top-level CLI entry. Constructs `PmService`, resolves the sync target,
/// and dispatches to [`run_with_sync`]. Exits the process via the returned
/// `i32`.
pub async fn run(repo_root: &Path, args: IngestGitHubArgs) -> Result<i32> {
    let config = load_config_for_repo(repo_root)?;

    let configured_repo = args.repo.clone().or_else(|| {
        config
            .pm
            .github
            .as_ref()
            .and_then(|g| g.default_repo.clone().or_else(|| g.repo.clone()))
    });

    let pm_service = spur_pm::PmService::try_new(
        configured_repo,
        config.pm.beads.as_ref().is_none_or(|b| b.enabled),
        config.pm.github.as_ref().is_none_or(|g| g.enabled),
        repo_root,
        None,
    )
    .await?;

    let Some(pm) = pm_service else {
        eprintln!(
            "[spur] No PM backend available. Run `spur pm init` to initialize beads in this repo."
        );
        return Ok(1);
    };

    let Some(sync) = pm.sync_target("github") else {
        eprintln!(
            "[spur] GitHub ingest not configured.\n\
             \n\
             To enable, do one of:\n  \
                - export SPUR_GITHUB_TOKEN=<token>\n  \
                - run `gh auth login`\n\
             \n\
             And configure the target repo in `.spur/config.toml`:\n  \
                [pm.github]\n  \
                default_repo = \"owner/repo\""
        );
        return Ok(1);
    };

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let code = run_with_sync(&pm, sync.as_ref(), &args, stdout.lock(), stderr.lock()).await;
    Ok(code)
}
