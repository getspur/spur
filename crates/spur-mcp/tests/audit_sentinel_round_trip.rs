//! Integration test: [[spur-audit v1]] sentinel comments round-trip through
//! real `br comments add` + `br comments list --json`.

use std::path::Path;
use std::process::Command;

use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
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
        .expect("br invocation");
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(format!(
            "br {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v.get("id")
        .and_then(|x| x.as_str())
        .expect("id")
        .to_string()
}

#[test]
fn completion_state_and_dispatch_orphan_cleared_round_trip() {
    if !br_available() {
        eprintln!("skipping: `br` not on PATH");
        return;
    }
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let id = extract_id(&run_br(dir.path(), &["create", "t", "-t", "task"]).unwrap());

    let completion = AuditSentinelKind::Completion {
        delegation_id: "del-1".into(),
        completion_state: audit_sentinel::CompletionState::Superseded,
        superseded: true,
        worker_branch: Some("feat/x".into()),
        result_summary: Some("worker narrative: three refactors".into()),
    };
    let orphan = AuditSentinelKind::DispatchOrphanCleared {
        delegation_id: "del-1".into(),
        reason: "restart-orphan-cleared".into(),
    };

    for v in [&completion, &orphan] {
        let body = audit_sentinel::encode_comment(v);
        run_br(dir.path(), &["comments", "add", &id, &body]).unwrap();
    }

    let list_out = run_br(dir.path(), &["comments", "list", &id]).unwrap();
    let items: serde_json::Value = serde_json::from_str(&list_out).unwrap();
    let texts: Vec<String> = items
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();

    assert!(texts.iter().any(|t| {
        audit_sentinel::parse_comment(t)
            .and_then(|r| r.ok())
            .is_some_and(|k| k == completion)
    }));
    assert!(texts.iter().any(|t| {
        audit_sentinel::parse_comment(t)
            .and_then(|r| r.ok())
            .is_some_and(|k| k == orphan)
    }));
}
