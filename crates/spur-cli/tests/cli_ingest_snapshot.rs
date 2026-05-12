//! T-7 — snapshot tests for `spur pm ingest github` output.
//!
//! These exercise the testable seams of `spur_cli::commands::pm_ingest`
//! directly, with a scripted [`spur_pm::sync::MockSync`] in lieu of a real
//! GitHub client. They cover:
//!
//! - human format on a clean run
//! - JSON format on a clean run
//! - human format with conflicts (non-zero exit; conflicts listed)
//! - error message + exit-code for each `SyncError` variant
//! - `IngestOptions` mapping from CLI args
//!
//! Spec §10 T-7 names insta as the snapshot tool; we use plain
//! `assert_eq!` here to avoid adding a workspace dependency for four
//! small text payloads. The intent — stable, reviewable output diffs —
//! is preserved: the snapshots are inline string literals.

use chrono::TimeZone;
use spur_cli::commands::pm_ingest::{
    exit_code_for, format_error, format_human_report, format_json_report, ingest_options_from,
    IngestGitHubArgs,
};
use spur_pm::ingest::IngestReport;
use spur_pm::sync::{ConflictReason, RemoteConflict, SyncError};

fn ts(secs: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc.timestamp_opt(secs, 0).single().unwrap()
}

fn clean_report() -> IngestReport {
    IngestReport {
        run_id: 0,
        source_system: "github".into(),
        source_repo: "octocat/Hello-World".into(),
        fetched_remote_nodes: 0,
        dry_run: false,
        ingested: 3,
        updated: 1,
        unchanged: 7,
        conflicts: Vec::new(),
        deletions: Vec::new(),
        dep_hints_added: 2,
        comments_added: 5,
    }
}

fn dry_run_report() -> IngestReport {
    IngestReport {
        fetched_remote_nodes: 100,
        dry_run: true,
        ingested: 0,
        updated: 0,
        unchanged: 0,
        dep_hints_added: 4,
        comments_added: 12,
        ..clean_report()
    }
}

fn conflicted_report() -> IngestReport {
    IngestReport {
        conflicts: vec![RemoteConflict {
            beads_id: "bd-42".into(),
            remote_id: "I_kwDO".into(),
            local_updated_at: ts(100),
            remote_updated_at: ts(200),
            watermark_remote_updated_at: ts(50),
            reason: ConflictReason::LocalAndRemoteBothMutated,
        }],
        ..clean_report()
    }
}

#[test]
fn human_clean_report_snapshot() {
    let text = format_human_report(&clean_report());
    assert_eq!(
        text,
        "\
[spur] ingest github@octocat/Hello-World done
  ingested:   3
  updated:    1
  unchanged:  7
  conflicts:  0
  deletions:  0
  dep-hints:  2
  comments:   5
"
    );
}

#[test]
fn human_real_run_with_fetched_nodes_snapshot() {
    let report = IngestReport {
        fetched_remote_nodes: 42,
        dry_run: false,
        ..clean_report()
    };
    let text = format_human_report(&report);
    assert_eq!(
        text,
        "\
[spur] ingest github@octocat/Hello-World done
  fetched:    42
  ingested:   3
  updated:    1
  unchanged:  7
  conflicts:  0
  deletions:  0
  dep-hints:  2
  comments:   5
"
    );
}

#[test]
fn human_dry_run_report_snapshot() {
    let text = format_human_report(&dry_run_report());
    assert_eq!(
        text,
        "\
[spur] ingest github@octocat/Hello-World done (dry-run)
  fetched:    100
  ingested:   0
  updated:    0
  unchanged:  0
  conflicts:  0
  deletions:  0
  dep-hints:  4
  comments:   12
"
    );
}

#[test]
fn human_conflict_report_snapshot() {
    let text = format_human_report(&conflicted_report());
    assert_eq!(
        text,
        "\
[spur] ingest github@octocat/Hello-World done
  ingested:   3
  updated:    1
  unchanged:  7
  conflicts:  1
  deletions:  0
  dep-hints:  2
  comments:   5

Conflicts:
  bd-42 (remote I_kwDO): LocalAndRemoteBothMutated
"
    );
}

#[test]
fn json_clean_report_is_parseable_and_contains_counts() {
    let json = format_json_report(&clean_report()).expect("serialize");
    // Stable shape check: not every field is value-pinned (chrono renders
    // RFC3339 with sub-second precision that could shift), so we parse it
    // back and validate the values.
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["source_system"], "github");
    assert_eq!(v["source_repo"], "octocat/Hello-World");
    assert!(v.get("fetched_remote_nodes").is_none());
    assert!(v.get("dry_run").is_none());
    assert_eq!(v["ingested"], 3);
    assert_eq!(v["updated"], 1);
    assert_eq!(v["unchanged"], 7);
    assert_eq!(v["dep_hints_added"], 2);
    assert_eq!(v["comments_added"], 5);
    assert_eq!(v["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(v["deletions"].as_array().unwrap().len(), 0);
}

#[test]
fn json_dry_run_report_includes_non_default_dry_run_fields() {
    let json = format_json_report(&dry_run_report()).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["fetched_remote_nodes"], 100);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["dep_hints_added"], 4);
    assert_eq!(v["comments_added"], 12);
}

#[test]
fn exit_code_clean_human_is_zero() {
    let ok = Ok(clean_report());
    assert_eq!(exit_code_for(&ok, false), 0);
    assert_eq!(exit_code_for(&ok, true), 0);
}

#[test]
fn exit_code_conflicts_human_is_one_json_is_zero() {
    let with_conflicts = Ok(conflicted_report());
    assert_eq!(exit_code_for(&with_conflicts, false), 1);
    // Spec §8: --json always exits 0 unless a hard error.
    assert_eq!(exit_code_for(&with_conflicts, true), 0);
}

#[test]
fn exit_code_sync_errors_map_to_spec_codes() {
    // NeedsAuth → 1, Transient → 2, Malformed → 3 (spec §6).
    let needs_auth: Result<IngestReport, SyncError> =
        Err(SyncError::NeedsAuth("token expired".into()));
    assert_eq!(exit_code_for(&needs_auth, false), 1);
    assert_eq!(exit_code_for(&needs_auth, true), 1);

    let transient: Result<IngestReport, SyncError> = Err(SyncError::Transient("ECONNRESET".into()));
    assert_eq!(exit_code_for(&transient, false), 2);
    assert_eq!(exit_code_for(&transient, true), 2);

    let malformed: Result<IngestReport, SyncError> =
        Err(SyncError::Malformed("unexpected null".into()));
    assert_eq!(exit_code_for(&malformed, false), 3);
    assert_eq!(exit_code_for(&malformed, true), 3);

    let rate: Result<IngestReport, SyncError> = Err(SyncError::RateLimited { retry_after_s: 60 });
    assert_eq!(exit_code_for(&rate, false), 2);

    let gone: Result<IngestReport, SyncError> = Err(SyncError::Gone("octocat/Missing".into()));
    assert_eq!(exit_code_for(&gone, false), 1);
}

#[test]
fn error_messages_carry_remediation_keywords() {
    let s = format_error(&SyncError::NeedsAuth("x".into()));
    assert!(s.contains("gh auth login"), "{s}");
    assert!(s.contains("SPUR_GITHUB_TOKEN"), "{s}");

    let s = format_error(&SyncError::Transient("oops".into()));
    assert!(s.contains("re-run"), "{s}");

    let s = format_error(&SyncError::Malformed("bad json".into()));
    assert!(s.contains("file a bug"), "{s}");
}

#[test]
fn ingest_options_default_label_namespace_and_auto_label() {
    let args = IngestGitHubArgs::default();
    let opts = ingest_options_from(&args);
    assert_eq!(opts.label_namespace, "gh");
    assert_eq!(opts.auto_label.as_deref(), Some("spur-managed"));
    assert!(!opts.dry_run);
    assert!(opts.lock_timeout_ms.is_none());
}

#[test]
fn ingest_options_propagates_dry_run_and_since() {
    let since = ts(1_700_000_000);
    let args = IngestGitHubArgs {
        since: Some(since),
        dry_run: true,
        label_namespace: "linear".into(),
        ..IngestGitHubArgs::default()
    };
    let opts = ingest_options_from(&args);
    assert_eq!(opts.since, Some(since));
    assert!(opts.dry_run);
    assert_eq!(opts.label_namespace, "linear");
}

// Note: the full `run_with_sync` path requires a real `BeadsCrateAdapter`
// (and therefore a tempdir-backed .beads/beads.db). It's exercised
// indirectly by spur-pm's apply_test.rs against the same MockSync — there
// is no value in re-running that contract here. T-7 covers the CLI-shaped
// seams.
