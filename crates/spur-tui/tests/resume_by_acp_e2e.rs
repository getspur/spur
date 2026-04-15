//! End-to-end: AgentSessionReady → metadata updated → re-load store →
//! last_active_acp() returns the ACP id that resume should send.

use spur_tui::session_metadata::SessionMetadataStore;

#[test]
fn agent_session_ready_persists_acp_mapping_across_reload() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Simulate what app.rs does on AgentSessionReady.
    {
        let mut store = SessionMetadataStore::load(&path);
        store.set_acp_mapping("spur-session-1", "acp-session-xyz", "claude-code-acp");
        store.save().unwrap();
    }

    // Simulate a fresh process reading the same file.
    let reloaded = SessionMetadataStore::load(&path);
    let (acp, brain) = reloaded
        .last_active_acp()
        .expect("top-level pointers populated");
    assert_eq!(acp, "acp-session-xyz");
    assert_eq!(brain, "claude-code-acp");
}
