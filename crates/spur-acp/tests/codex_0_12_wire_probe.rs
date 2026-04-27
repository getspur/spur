//! End-to-end probe of v1 (`/model`, `/effort`) against codex-acp 0.12.0
//! captured wire output. This is NOT a mock — the JSON fixture is the verbatim
//! `session/new` result returned by `npx --yes @zed-industries/codex-acp@0.12.0`
//! on 2026-04-27 (see `scripts/probe-codex-acp.mjs`).
//!
//! Purpose: prove that the v1 path (`NewSessionResponse.config_options` →
//! `synthesize()` → `AdvertisedCommand`) actually wires up against the real
//! codex wire shape. If this test passes, the v1 RX-side is provably correct
//! against today's codex.

use agent_client_protocol::schema::NewSessionResponse;
use spur_acp::adapter::config_options::synthesize;

const FIXTURE: &str = include_str!("data/codex_acp_0_12_new_session_response.json");

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
