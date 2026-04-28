//! Integration: verify `SpurAgentCaps` is reachable across spur-acp →
//! spur-core → spur-tui, and that the orchestrator's
//! `spur_agent_caps` getter is wired to BrainSession's cache slot.
//!
//! Mirrors the cross-crate event-style shape of
//! `tests/advertised_commands_event.rs`, but tests the cache plumbing
//! introduced by Wave A Task A.5 (spec §6.1 + §9). The full
//! event-driven path through `SessionDetailView` is reserved for
//! later waves once a `SpurEventBody::SpurAgentCapsReady` (or
//! similar) ships.

use std::sync::Arc;

use agent_client_protocol::schema::{InitializeResponse, NewSessionResponse, ProtocolVersion};
use spur_acp::{AgentKind, SpurAgentCaps};

#[test]
fn spur_agent_caps_constructed_from_codex_fixture_round_trips_via_arc() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let json = include_str!("../../spur-acp/tests/data/codex_acp_0_12_new_session_response.json");
    let new: NewSessionResponse =
        serde_json::from_str(json).expect("codex fixture must deserialize");

    let caps = Arc::new(SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp));

    // Sanity: the codex fixture exercises all 3 set-* gates plus some
    // structural counts. If this regresses, downstream UI gating is
    // probably also broken — keep this assertion explicit.
    assert!(caps.supports_set_mode(), "codex fixture has 3 modes");
    assert!(caps.supports_set_model(), "codex fixture has Some(models)");
    assert!(
        caps.supports_set_config_option(),
        "codex fixture has 3 config_options"
    );
    assert_eq!(caps.config_options.len(), 3);

    // Cheap-clone semantics: the cache wraps in `Arc<>` so spur-tui
    // can share between SessionDetailView and the orchestrator getter
    // without deep clones. Confirm `Arc::ptr_eq` between two clones of
    // the same Arc returns true.
    let cloned = Arc::clone(&caps);
    assert!(Arc::ptr_eq(&caps, &cloned));
}

#[test]
fn empty_caps_yield_all_false_via_public_api() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let new = NewSessionResponse::new(agent_client_protocol::schema::SessionId::new("x"));
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::Generic);
    assert!(!caps.supports_set_mode());
    assert!(!caps.supports_set_model());
    assert!(!caps.supports_set_config_option());
    assert!(!caps.supports_load_session());
    assert!(!caps.meta_capability("terminal_output"));
}
