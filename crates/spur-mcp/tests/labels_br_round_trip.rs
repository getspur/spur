//! Integration test: every constructor in `spur_mcp::plan::labels` produces a
//! label that round-trips cleanly through real `br label add` + `br list --json`.
//!
//! Regression guard for B5 — `br 0.1.14` enforces label grammar
//! `[A-Za-z0-9_:-]+`, rejecting `.`, `=`, `/`, whitespace. Any constructor
//! emitting an illegal label would silently ship as a runtime bug because no
//! in-process unit test calls the real `br` binary. This test closes that gap.

use std::path::Path;
use std::process::Command;

use spur_mcp::plan::labels;
use tempfile::TempDir;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("br")
        .args(args)
        .arg("--json")
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        Err(format!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        ))
    }
}

fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).expect("br create json");
    v.get("id")
        .and_then(|x| x.as_str())
        .expect("br create response missing `id`")
        .to_string()
}

#[test]
fn every_label_constructor_is_accepted_by_br() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
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
        labels::mutation_id(&uuid::Uuid::nil()),
        labels::SIGNAL_LATE_ARRIVAL.to_string(),
        labels::READY_FOR_REVIEW.to_string(),
    ];

    for label in &constructed {
        run_br(dir.path(), &["label", "add", &id, "-l", label])
            .unwrap_or_else(|e| panic!("br rejected constructor label {label:?}: {e}"));
    }

    // Round-trip assertion: every constructed label appears in `br list --json`.
    let list_out = run_br(dir.path(), &["list"]).unwrap();
    let items: serde_json::Value = serde_json::from_str(&list_out).unwrap();
    let labels_in_db: Vec<String> = items[0]["labels"]
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

#[test]
fn parsers_round_trip_through_real_br_labels() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
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
    let returned: Vec<String> = items[0]["labels"]
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
