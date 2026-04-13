use spur_acp::SessionId;
use spur_tui::mentions::MentionRegistry;

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
