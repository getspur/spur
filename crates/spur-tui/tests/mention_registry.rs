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
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "", 50);
    assert!(
        !hits.iter().any(|h| h.kind == MentionKind::Worker),
        "direct session should not surface worker entries"
    );
}
