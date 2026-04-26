//! Integration test placeholder for cross-process explicit session attach.
//!
//! The lock behavior is covered by `spur_acp::session_lock` unit tests. A full
//! `spur tui --session <id>` collision test needs a headless TUI event-dump mode
//! and a reusable session metadata fixture; neither exists in this crate yet.

#[test]
#[ignore = "requires headless TUI event-dump mode and session metadata fixture"]
fn second_concurrent_session_attach_emits_rejected() {}
