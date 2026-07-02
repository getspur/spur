//! End-to-end probe of v1 (`/model`, `/effort`) against codex-acp 0.12.0
//! captured wire output. This is NOT a mock — the JSON fixture is the verbatim
//! `session/new` result returned by `npx --yes @zed-industries/codex-acp@0.12.0`
//! on 2026-04-27 (see `scripts/probe-codex-acp.mjs`).
//!
//! Purpose: prove that the v1 path (`NewSessionResponse.config_options` →
//! `synthesize()` → `AdvertisedCommand`) actually wires up against the real
//! codex wire shape. If this test passes, the v1 RX-side is provably correct
//! against today's codex.

use agent_client_protocol::schema::v1::NewSessionResponse;
use spur_acp::adapter::config_options::synthesize;

const FIXTURE: &str = include_str!("data/codex_acp_0_12_new_session_response.json");
const SESSION_INFO_UPDATE_FIXTURE: &str =
    include_str!("data/codex_session_info_update_sample.json");

#[test]
fn deserializes_codex_0_12_new_session_response() {
    let resp: NewSessionResponse = serde_json::from_str(FIXTURE)
        .expect("codex 0.12 NewSessionResponse must deserialize against SDK 0.11.1 schema");
    let opts = resp
        .config_options
        .as_ref()
        .expect("codex 0.12 must populate configOptions in session/new response");
    let ids: Vec<&str> = opts.iter().map(|o| o.id.0.as_ref()).collect();
    assert!(
        ids.contains(&"model"),
        "expected `model` config option from codex; got {ids:?}",
    );
    assert!(
        ids.contains(&"reasoning_effort"),
        "expected `reasoning_effort` config option from codex; got {ids:?}",
    );
    // codex 0.12 also advertises `mode` (Approval Preset). Spur intentionally
    // filters this out via the allow-list — verified in the synthesize test
    // below.
    assert!(ids.contains(&"mode"));
}

#[test]
fn synthesize_produces_model_and_effort_for_codex_0_12() {
    let resp: NewSessionResponse = serde_json::from_str(FIXTURE).unwrap();
    let opts = resp.config_options.unwrap_or_default();
    let advertised = synthesize(&opts);

    // Expect exactly /model and /effort (mode is allowlisted out by design).
    let names: Vec<&str> = advertised.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["model", "effort"],
        "spur's allow-list should produce /model and /effort against codex 0.12",
    );

    let model = &advertised[0];
    assert_eq!(model.config_id, "model");
    assert!(
        !model.choices.is_empty(),
        "model picker must have at least one choice"
    );
    assert!(
        model.current_value.is_some(),
        "codex 0.12 sets currentValue=gpt-5.5; spur must capture it"
    );

    let effort = &advertised[1];
    assert_eq!(effort.name, "effort");
    assert_eq!(effort.config_id, "reasoning_effort");
    assert_eq!(effort.choices.len(), 4); // low / medium / high / xhigh
    let effort_values: Vec<&str> = effort.choices.iter().map(|c| c.value.as_str()).collect();
    assert_eq!(effort_values, vec!["low", "medium", "high", "xhigh"]);
}

/// M8 Wave 0.3 — `SessionInfoUpdate` shape probe.
///
/// `scripts/probe-codex-acp.mjs --prompts` captured a 2-turn session against
/// codex-acp 0.12.0 and recorded zero `session_info_update` notifications;
/// only `agent_message_chunk`, `available_commands_update`, `tool_call`,
/// `tool_call_update`, and `usage_update` were emitted. The fixture is therefore
/// a *synthetic* SDK-shaped sample used by Wave E.1 to drive a failing test for
/// the `apply_session_update` arm — the arm exists to remove the silent drop
/// and to opportunistically cache fields as other agents (or future codex
/// versions) start emitting them.
#[test]
fn session_info_update_fixture_deserializes_into_sdk_session_update_arm() {
    use agent_client_protocol::schema::v1::SessionUpdate;
    let parsed: SessionUpdate = serde_json::from_str(SESSION_INFO_UPDATE_FIXTURE).expect(
        "synthetic SessionInfoUpdate fixture must deserialize against SDK 0.11.x SessionUpdate",
    );
    match parsed {
        SessionUpdate::SessionInfoUpdate(info) => {
            assert!(
                info.title.value().is_some_and(|t| !t.is_empty()),
                "fixture must populate `title` so Wave E.1 has a non-empty cache target",
            );
            assert!(
                info.updated_at.is_value(),
                "fixture must populate `updatedAt` so Wave E.1 has a timestamp to log",
            );
        }
        other => {
            panic!("fixture must dispatch to SessionUpdate::SessionInfoUpdate; got {other:?}",)
        }
    }
}
