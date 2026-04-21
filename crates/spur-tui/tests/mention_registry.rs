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
    let mut reg = MentionRegistry::for_brain_session(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: Some("Refactors Rust".into()),
        tier: Some("specialist".into()),
    }]);
    let sid = SessionId::new();
    // Use an empty temp dir so file source returns nothing and worker entries
    // are always within the limit.
    let tmp = tempfile::tempdir().unwrap();
    let cwd = tmp.path();
    let hits = reg.query(&sid, cwd, "", 10);
    assert!(
        hits.iter()
            .any(|h| h.kind == MentionKind::Worker && h.display == "worker:claude-code"),
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
    use std::io::Write;
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
    // Seed competing files. Their display strings are short ("a.rs", "b.rs",
    // ...) so under the OLD length-sorted ranking they'd push workers
    // (display "worker:worker-N") off the top of the result.
    for ch in ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h'] {
        let mut f = std::fs::File::create(tmp.path().join(format!("{}.rs", ch))).unwrap();
        writeln!(f, "// stub").unwrap();
    }
    let hits = reg.query(&sid, tmp.path(), "", 20);
    let worker_count = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(
        worker_count,
        6,
        "expected first 6 rows to be workers, got {:?}",
        hits.iter()
            .take(6)
            .map(|h| (&h.kind, &h.display))
            .collect::<Vec<_>>()
    );
}

#[test]
fn empty_query_caps_workers_at_pin_cap() {
    use std::io::Write;
    // 10 workers in snapshot; only 6 (WORKER_PIN_CAP) should appear.
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
    // Need enough competing files to fill the remaining 14 slots in
    // a limit=20 query — and to make the cap visible.
    for i in 0..20 {
        let mut f = std::fs::File::create(tmp.path().join(format!("file_{:02}.rs", i))).unwrap();
        writeln!(f, "// stub").unwrap();
    }
    let hits = reg.query(&sid, tmp.path(), "", 20);
    let head_workers = hits
        .iter()
        .take(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(head_workers, 6, "first 6 rows should be workers");
    // Rows 7-20 should not be workers (the cap capped at 6).
    let tail_workers = hits
        .iter()
        .skip(6)
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    assert_eq!(tail_workers, 0, "no workers should appear after the cap");
    // And there should be at least one file present in the tail.
    assert!(
        hits.iter().skip(6).any(|h| h.kind != MentionKind::Worker),
        "expected files after the worker pin"
    );
}

#[test]
fn typed_query_boosts_worker_in_ambiguous_match() {
    let mut reg = MentionRegistry::for_brain_session(vec![WorkerMentionDescriptor {
        name: "claude-code".into(),
        description: None,
        tier: None,
    }]);
    let sid = SessionId::new();
    // Use a real workspace dir so FileMentionSource has files to compete.
    let cwd = std::env::current_dir().unwrap();
    let hits = reg.query(&sid, &cwd, "cla", 5);
    assert!(
        hits.first()
            .map(|h| h.kind == MentionKind::Worker)
            .unwrap_or(false),
        "expected worker:claude-code at row 0 for 'cla', got {:?}",
        hits.iter()
            .map(|h| (&h.kind, &h.display))
            .collect::<Vec<_>>()
    );
}
