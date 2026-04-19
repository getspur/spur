use spur_acp::SessionId;
use spur_tui::mentions::{MentionKind, MentionRegistry, WorkerMentionDescriptor};

#[test]
fn file_mentions_index_and_fuzzy_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/foo.rs"), "// foo").unwrap();
    std::fs::write(root.join("src/bar.rs"), "// bar").unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

    let mut reg = MentionRegistry::new();
    let sid = SessionId::new();
    let hits = reg.query(&sid, root, "foo", 10);
    assert!(
        hits.iter().any(|h| h.display.contains("foo.rs")),
        "{:?}",
        hits
    );

    let all = reg.query(&sid, root, "", 10);
    assert!(!all.is_empty());
}

#[test]
fn brain_session_includes_workers_in_empty_query() {
    let mut reg = MentionRegistry::for_brain_session(vec![
        WorkerMentionDescriptor {
            name: "claude-code".into(),
            description: Some("Refactors Rust".into()),
            tier: Some("specialist".into()),
        },
    ]);
    let sid = SessionId::new();
    // Use an empty temp dir so file source returns nothing and worker entries
    // are always within the limit.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let hits = reg.query(&sid, cwd, "", 10);
    assert!(
        hits.iter().any(|h| h.kind == MentionKind::Worker
            && h.display == "worker:claude-code"),
        "expected worker:claude-code in hits, got {:?}",
        hits.iter().map(|h| &h.display).collect::<Vec<_>>()
    );
}

#[test]
fn direct_session_excludes_workers() {
    let mut reg = MentionRegistry::for_direct_session();
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let hits = reg.query(&sid, tmp.path(), "", 50);
    assert!(
        !hits.iter().any(|h| h.kind == MentionKind::Worker),
        "direct session should not surface worker entries"
    );
}

#[test]
fn empty_query_pins_workers_first() {
    let workers: Vec<WorkerMentionDescriptor> = (0..6)
        .map(|i| WorkerMentionDescriptor {
            name: format!("worker-{}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let hits = reg.query(&sid, tmp.path(), "", 20);
    // First 6 must all be Worker kind.
    let worker_count = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(
        worker_count, 6,
        "expected first 6 rows to be workers, got {:?}",
        hits.iter().take(6).map(|h| (&h.kind, &h.display)).collect::<Vec<_>>()
    );
}

#[test]
fn empty_query_caps_workers_at_pin_cap() {
    // Provide 10 workers — only 6 (WORKER_PIN_CAP) should appear at the top.
    let workers: Vec<WorkerMentionDescriptor> = (0..10)
        .map(|i| WorkerMentionDescriptor {
            name: format!("w{:02}", i),
            description: None,
            tier: None,
        })
        .collect();
    let mut reg = MentionRegistry::for_brain_session(workers);
    let sid = SessionId::new();
    let tmp = tempfile::tempdir().unwrap();
    let hits = reg.query(&sid, tmp.path(), "", 20);
    let head_workers = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(head_workers, 6);
}

#[test]
fn typed_query_boosts_worker_in_ambiguous_match() {
    let mut reg = MentionRegistry::for_brain_session(vec![
        WorkerMentionDescriptor {
            name: "claude-code".into(),
            description: None,
            tier: None,
        },
    ]);
    let sid = SessionId::new();
    // Use a real workspace dir so FileMentionSource has files to compete.
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "cla", 5);
    assert!(
        hits.first().map(|h| h.kind == MentionKind::Worker).unwrap_or(false),
        "expected worker:claude-code at row 0 for 'cla', got {:?}",
        hits.iter().map(|h| (&h.kind, &h.display)).collect::<Vec<_>>()
    );
}
