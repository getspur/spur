//! Integration test: every constructor in `spur_mcp::plan::labels` produces a
//! label that round-trips cleanly through real `br label add` + `br list --json`.
//!
//! Regression guard for B5 — `br 0.1.14` enforces label grammar
//! `[A-Za-z0-9_:-]+`, rejecting `.`, `=`, `/`, whitespace. Any constructor
//! emitting an illegal label would silently ship as a runtime bug because no
//! in-process unit test calls the real `br` binary. This test closes that gap.

use std::path::Path;

use spur_mcp::plan::labels;
use tempfile::TempDir;

mod common;
fn br_available() -> bool {
    common::beads::br_available()
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("br create json");
    v.get("id")
        .and_then(|x| x.as_str())
        .expect("br create response missing `id`")
        .to_string()
}

#[ignore = "requires br on PATH; run with --ignored"]
#[test]
fn every_label_constructor_is_accepted_by_br() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let create_out = run_br(dir.path(), &["create", "t", "-t", "task"]).unwrap();
    let id = extract_id(&create_out);

    // Every constructor in labels.rs must emit a br-legal label.
    let constructed = vec![
        labels::plan_id("plan-xyz"),
        labels::plan_task_id("task-a"),
        labels::agent("claude-code-acp"),
        labels::source_issue("bd-42"),
        labels::delegation_id("del-abc-123"),
        labels::signal_kind("scope-drift"),
        labels::signal_kind_bucket("scope-drift", "high"),
        labels::mutation_id_label(&uuid::Uuid::nil()),
        labels::signal_processed_label(&uuid::Uuid::nil()),
        labels::superseded_by_labels(&["bd-1".into()])
            .pop()
            .unwrap(),
        labels::SIGNAL_LATE_ARRIVAL.to_string(),
        labels::READY_FOR_REVIEW.to_string(),
        labels::REVIEW_REJECTED.to_string(),
    ];

    for label in &constructed {
        run_br(dir.path(), &["label", "add", &id, "-l", label])
            .unwrap_or_else(|e| panic!("br rejected constructor label {label:?}: {e}"));
    }

    // Round-trip assertion: every constructed label appears in `br list --json`.
    let list_out = run_br(dir.path(), &["list"]).unwrap();
    let items: serde_json::Value = serde_json::from_str(&list_out).unwrap();
    let labels_in_db: Vec<String> = items["issues"][0]["labels"]
        .as_array()
        .expect("list response missing labels")
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    for label in &constructed {
        assert!(
            labels_in_db.contains(label),
            "label {label:?} missing from `br list` result: {labels_in_db:?}"
        );
    }
}

#[ignore = "requires br on PATH; run with --ignored"]
#[test]
fn parsers_round_trip_through_real_br_labels() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let id = extract_id(&run_br(dir.path(), &["create", "t", "-t", "task"]).unwrap());

    let plan_label = labels::plan_id("plan-xyz");
    let task_label = labels::plan_task_id("task-a");
    let agent_label = labels::agent("claude-code-acp");
    let src_label = labels::source_issue("bd-42");

    for lbl in [&plan_label, &task_label, &agent_label, &src_label] {
        run_br(dir.path(), &["label", "add", &id, "-l", lbl]).unwrap();
    }

    let list_out = run_br(dir.path(), &["list"]).unwrap();
    let items: serde_json::Value = serde_json::from_str(&list_out).unwrap();
    let returned: Vec<String> = items["issues"][0]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();

    // The parsers should recover the original component values.
    let mut seen_plan = false;
    let mut seen_task = false;
    let mut seen_agent = false;
    let mut seen_src = false;
    for label in &returned {
        if let Some(v) = labels::parse_plan_id(label) {
            assert_eq!(v, "plan-xyz");
            seen_plan = true;
        }
        if let Some(v) = labels::parse_plan_task_id(label) {
            assert_eq!(v, "task-a");
            seen_task = true;
        }
        if let Some(v) = labels::parse_agent(label) {
            assert_eq!(v, "claude-code-acp");
            seen_agent = true;
        }
        if let Some(v) = labels::parse_source_issue(label) {
            assert_eq!(v, "bd-42");
            seen_src = true;
        }
    }
    assert!(seen_plan && seen_task && seen_agent && seen_src);
}

/// Regression guard for the asymmetric label-length cap in `br 0.1.14`.
///
/// `br create --label <label>` rejects labels longer than 50 characters
/// (`Validation failed: label: exceeds 50 characters`), while `br label add`
/// accepts arbitrary lengths. Constructors in `plan::labels` that feed into
/// `IssueCreate.labels` MUST stay under 50 characters — enforced by using
/// the compact (hyphen-free) UUID form for `mutation_id_label` and
/// `signal_processed_label`.
#[ignore = "requires br on PATH; run with --ignored"]
#[test]
fn br_create_enforces_50_char_cap_but_label_add_does_not() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();

    // `br create --label` enforces the cap.
    let create_49 = "x".repeat(49);
    run_br(dir.path(), &["create", "t", "-t", "task", "-l", &create_49])
        .expect("br create must accept 49-char label");
    let create_51 = "x".repeat(51);
    assert!(
        run_br(dir.path(), &["create", "t", "-t", "task", "-l", &create_51]).is_err(),
        "br create must reject 51-char label (regression: 50-char cap may have moved)"
    );

    // `br label add` does not enforce the cap.
    let id = extract_id(&run_br(dir.path(), &["create", "t", "-t", "task"]).unwrap());
    for len in [51usize, 64, 128, 256] {
        let label = "x".repeat(len);
        run_br(dir.path(), &["label", "add", &id, "-l", &label])
            .unwrap_or_else(|e| panic!("br label add unexpectedly rejected {len}-char label: {e}"));
    }
}

/// Regression guard: constructors whose output feeds `IssueCreate.labels`
/// (the `br create --label` path) must fit under the 50-character cap.
/// Currently only `mutation_id_label` reaches that path
/// (see `mutation_executor::apply_mutation`). `signal_processed_label` is
/// used only via `IssueUpdate.add_labels` (the `br label add` path), so it
/// is not constrained here — the module docstring documents this asymmetry.
#[ignore = "requires br on PATH; run with --ignored"]
#[test]
fn mutation_id_label_fits_under_br_create_cap() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();

    let label = labels::mutation_id_label(&uuid::Uuid::new_v4());
    assert!(
        label.len() <= 50,
        "mutation_id_label must fit under br create cap: {label:?} ({} chars)",
        label.len()
    );
    run_br(dir.path(), &["create", "t", "-t", "task", "-l", &label])
        .unwrap_or_else(|e| panic!("br create rejected mutation_id_label {label:?}: {e}"));
}
