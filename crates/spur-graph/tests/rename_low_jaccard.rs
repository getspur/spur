use std::path::Path;
use std::process::Command;

use spur_graph::git_walk::{symbol_changes_for_commit, SymbolDiffCtx};
use spur_graph::ChangeKind;
use tempfile::TempDir;

fn run_git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout utf8")
}

fn init_repo(dir: &Path) {
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.email", "t@t"]);
    run_git(dir, &["config", "user.name", "T"]);
}

fn commit(dir: &Path, message: &str) -> String {
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-q", "-m", message]);
    run_git(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

#[test]
fn rename_low_jaccard_emits_ambiguous_rename_on_both_endpoints() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(
        dir.path().join("lib.rs"),
        br#"
pub fn process_chunk(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    for byte in input {
        if byte % 2 == 0 {
            output.push(byte / 2);
        } else {
            output.push(byte.saturating_mul(3));
        }
    }
    output
}
"#,
    )
    .unwrap();
    commit(dir.path(), "c1");
    std::fs::write(
        dir.path().join("lib.rs"),
        br#"
pub fn process_batch(records: &[String]) -> usize {
    let mut accepted = 0;
    let mut rejected = 0;
    for record in records {
        match record.split_once(':') {
            Some((tag, value)) if tag == "ok" && value.len() > 3 => accepted += 1,
            Some(_) => rejected += 1,
            None => rejected += 1,
        }
    }
    accepted.saturating_sub(rejected)
}
"#,
    )
    .unwrap();
    let sha = commit(dir.path(), "rewrite rename");

    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
    let added = changes
        .iter()
        .find(|change| change.snapshot.entity_name == "process_batch")
        .expect("process_batch snapshot");
    let deleted = changes
        .iter()
        .find(|change| change.snapshot.entity_name == "process_chunk")
        .expect("process_chunk snapshot");

    assert!(matches!(added.change_kind, ChangeKind::Added));
    assert!(matches!(deleted.change_kind, ChangeKind::Deleted));
    assert!(
        !changes
            .iter()
            .any(|change| matches!(change.change_kind, ChangeKind::RenamedFrom(_))),
        "low-Jaccard body rewrite must not emit RenamedFrom: {changes:#?}"
    );

    let added_id = &added.snapshot.key.stable_symbol_id;
    let deleted_id = &deleted.snapshot.key.stable_symbol_id;
    assert!(
        has_ambiguous_rename(ctx.diagnostics(), added_id, deleted_id),
        "missing added endpoint diagnostic for {added_id} -> {deleted_id}: {:#?}",
        ctx.diagnostics()
    );
    assert!(
        has_ambiguous_rename(ctx.diagnostics(), deleted_id, added_id),
        "missing deleted endpoint diagnostic for {deleted_id} -> {added_id}: {:#?}",
        ctx.diagnostics()
    );
}

fn has_ambiguous_rename(diagnostics: &[String], stable_id: &str, other_stable_id: &str) -> bool {
    diagnostics.iter().any(|diagnostic| {
        diagnostic.contains("ambiguous_rename")
            && diagnostic.contains(&format!("stable_symbol_id={stable_id}"))
            && diagnostic.contains(&format!("other_stable_symbol_id={other_stable_id}"))
    })
}
