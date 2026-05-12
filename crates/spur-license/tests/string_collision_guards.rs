//! These tests guard against accidental bulk-renaming of policy key strings
//! that ALSO appear in MCP/ACP wire payloads. If you see one of these tests
//! fail in PR review, it almost certainly means a sed/rg --replace migration
//! crossed protocol boundaries.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_workspace_file(relative_path: &str) -> String {
    let path = workspace_root().join(relative_path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn count_occurrences(contents: &str, needle: &str) -> usize {
    contents.matches(needle).count()
}

#[test]
fn brain_session_wire_field_literal_in_mcp_server_is_not_policy_key_renamed() {
    let server = [
        read_workspace_file("crates/spur-mcp/src/server/types.rs"),
        read_workspace_file("crates/spur-mcp/src/server/handlers/delegation_tests.rs"),
    ]
    .join("\n");

    assert_eq!(
        count_occurrences(&server, "\"core_core_brain_session\""),
        0,
        "MCP wire field must remain brain_session, not the policy key spelling"
    );
    assert!(
        count_occurrences(&server, "\"brain_session\"") >= 4,
        "MCP server should preserve the brain_session wire-field literals"
    );
}

#[test]
fn session_resume_agent_default_literal_is_not_policy_key_renamed() {
    let defaults = read_workspace_file("crates/spur-acp/src/agents/defaults.rs");

    assert_eq!(
        count_occurrences(&defaults, "\"core_core_session_resume\""),
        0,
        "ACP agent default must remain session_resume, not the policy key spelling"
    );
    assert!(
        count_occurrences(&defaults, "\"session_resume\"") >= 2,
        "ACP agent defaults should preserve the session_resume literals"
    );
}

#[test]
fn brain_session_id_wire_field_literals_are_not_policy_key_renamed() {
    let sources = [
        ("crates/spur-acp/src/domain/events.rs", 1),
        ("crates/spur-acp/src/domain/replay_compat.rs", 1),
        ("crates/spur-blob-store/src/fs_store.rs", 2),
        ("crates/spur-worktree/src/git_blob_store.rs", 3),
    ];

    for (relative_path, expected_count) in sources {
        let contents = read_workspace_file(relative_path);

        assert_eq!(
            count_occurrences(&contents, "\"core_core_brain_session_id\""),
            0,
            "{relative_path} must not use the policy key spelling for brain_session_id"
        );
        assert!(
            count_occurrences(&contents, "\"brain_session_id\"") >= expected_count,
            "{relative_path} should preserve the brain_session_id wire-field literals"
        );
    }
}
