//! End-to-end smoke for the /model and /effort picker wire path.
//!
//! # Harness option (per plan Task 2.17)
//!
//! This test follows **Option C** (layer-tier composition) from the plan.
//! Per-task tests already cover orchestrator caching + event emission
//! (`spur-core/src/orchestrator.rs::replace_session_config_options_updates_cache_and_emits_event`),
//! the `synthesize` allow-list (`spur-acp/src/adapter/config_options.rs`),
//! `AdvertisedSource::entries`, and `submit_router::route` on the
//! `SetSessionConfigOption` dispatch. This smoke composes those public
//! surfaces into one happy path so a regression in any of them — wiring a
//! cached config_options snapshot through to a typed wire dispatch —
//! surfaces here.
//!
//! Options A (bash fixture exercising `NativeAcpConnection`) and B
//! (in-process `MockCodexConnection`) were considered but rejected: they
//! would each require >100 lines of harness code (JSON-RPC fixture or full
//! `AgentConnection` impl) for marginal additional coverage versus the
//! per-task tests already in place.

use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};
use spur_tui::commands::advertised::AdvertisedSource;
use spur_tui::commands::submit_router::{route, SubmitDecision};
use spur_tui::commands::CommandRegistry;

const HANDLE: &str = "codex";

fn select(config_id: &str, current: &str, choices: &[(&str, &str)]) -> SessionConfigOption {
    let opts: Vec<SessionConfigSelectOption> = choices
        .iter()
        .map(|(value, name)| {
            SessionConfigSelectOption::new((*value).to_string(), (*name).to_string())
        })
        .collect();
    SessionConfigOption::select(
        SessionConfigId::new(config_id.to_string()),
        format!("{config_id} label"),
        current.to_string(),
        opts,
    )
}

fn snapshot_with_three_choices_each() -> Vec<SessionConfigOption> {
    vec![
        select(
            "model",
            "gpt-5",
            &[
                ("gpt-5", "GPT-5"),
                ("gpt-5-codex", "GPT-5 Codex"),
                ("o3", "O3"),
            ],
        ),
        select(
            "reasoning_effort",
            "medium",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        ),
    ]
}

#[test]
fn codex_model_picker_end_to_end() {
    // 1. Cached snapshot from a hypothetical NewSessionResponse.config_options.
    let options = snapshot_with_three_choices_each();

    // 2. AdvertisedSource::entries → /model and /effort, both with picker spec.
    let entries = AdvertisedSource::entries(HANDLE, &options);
    assert_eq!(entries.len(), 2, "expected /model and /effort");

    let model = entries.iter().find(|e| e.name == "model").expect("/model");
    let effort = entries
        .iter()
        .find(|e| e.name == "effort")
        .expect("/effort");
    assert!(model.arg_picker_spec.is_some(), "/model needs picker spec");
    assert!(
        effort.arg_picker_spec.is_some(),
        "/effort needs picker spec"
    );

    // 3. Register the synthesized entries on the registry.
    let mut registry = CommandRegistry::new();
    registry.set_advertised_commands(HANDLE, entries.clone());

    // Sanity: the registry exposes the picker spec for /model.
    assert!(
        registry.arg_picker_spec("model").is_some(),
        "registry should expose ArgPickerSpec for /model"
    );

    // 4. Submit "/model gpt-5-codex" — typed wire dispatch.
    match route("/model gpt-5-codex", &[], &[], &registry, false) {
        SubmitDecision::SetSessionConfigOption { config_id, value } => {
            assert_eq!(config_id, "model");
            assert_eq!(value, "gpt-5-codex");
        }
        other => panic!("expected SetSessionConfigOption for /model, got {other:?}"),
    }

    // 5. Submit "/effort high" — note slash name "effort" but
    //    config_id "reasoning_effort" (the renaming happens in the
    //    synthesizer's allow-list).
    match route("/effort high", &[], &[], &registry, false) {
        SubmitDecision::SetSessionConfigOption { config_id, value } => {
            assert_eq!(
                config_id, "reasoning_effort",
                "/effort must dispatch the ACP config_id, not the slash name"
            );
            assert_eq!(value, "high");
        }
        other => panic!("expected SetSessionConfigOption for /effort, got {other:?}"),
    }

    // 6. Simulate the post-roundtrip cache refresh. The server echoes back
    //    a fresh config_options snapshot with the updated current_value
    //    (mirrors what `replace_session_config_options` would do after
    //    `set_session_config_option` returns; covered separately in
    //    `orchestrator.rs::replace_session_config_options_updates_cache_and_emits_event`).
    let refreshed = vec![
        select(
            "model",
            "gpt-5-codex",
            &[
                ("gpt-5", "GPT-5"),
                ("gpt-5-codex", "GPT-5 Codex"),
                ("o3", "O3"),
            ],
        ),
        select(
            "reasoning_effort",
            "high",
            &[("low", "Low"), ("medium", "Medium"), ("high", "High")],
        ),
    ];
    let refreshed_entries = AdvertisedSource::entries(HANDLE, &refreshed);
    registry.set_advertised_commands(HANDLE, refreshed_entries);

    // After the refresh the typed wire dispatch must still resolve, and
    // the entry hint should reflect the new current value (proves the
    // synthesizer re-ran against the updated snapshot).
    let refreshed_model_hint = AdvertisedSource::entries(HANDLE, &refreshed)
        .into_iter()
        .find(|e| e.name == "model")
        .and_then(|e| e.hint);
    assert_eq!(
        refreshed_model_hint.as_deref(),
        Some("current: gpt-5-codex"),
        "refreshed snapshot must surface the new current value as hint"
    );

    match route("/model o3", &[], &[], &registry, false) {
        SubmitDecision::SetSessionConfigOption { config_id, value } => {
            assert_eq!(config_id, "model");
            assert_eq!(value, "o3");
        }
        other => panic!("expected SetSessionConfigOption after refresh, got {other:?}"),
    }
}
