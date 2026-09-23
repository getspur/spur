//! TUI-dependent advertised-command tests relocated from
//! `spur-commands/src/advertised.rs` (spec 2026-09-23 §3.3/§5 C2): every
//! test here builds a `SessionDetailView` or calls the submit router's
//! `route_with_caps`, both of which stay TUI-owned.

use spur_acp::capability_evidence::{
    CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, EvidenceClaim, EvidenceEpoch,
    EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope, ObservationTime,
    RawEvidenceDigest,
};
use spur_acp::spur_agent_caps::CapabilityEvidenceSnapshot;
use spur_acp::{
    AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
    SessionConfigOption, SessionConfigSelectOption, SessionMode, SessionModeId, SessionModeState,
    SpurAgentCaps,
};
use spur_tui::commands::advertised::{pinned_route_for_command, AdvertisedSource};
use spur_tui::commands::submit_shell::route_with_caps;
use spur_tui::commands::submit_shell::SubmitDecision;
use spur_tui::commands::{CommandEntry, CommandSource, Dispatch};

fn evidence_identity() -> CliIdentity {
    CliIdentity {
        resolved_executable: std::path::PathBuf::from("/usr/bin/test-acp"),
        upstream_version: Some("1.0.0".to_owned()),
        argv_fingerprint: "argv".to_owned(),
        environment_fingerprint: "env".to_owned(),
    }
}

fn model_evidence(
    identity: &CliIdentity,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
) -> EvidenceRecord {
    capability_evidence(
        identity,
        CapabilityKind::Model,
        "model",
        claim,
        provenance,
        &[("test-model", "Test Model")],
    )
}

fn capability_evidence(
    identity: &CliIdentity,
    kind: CapabilityKind,
    upstream_id: &str,
    claim: EvidenceClaim,
    provenance: EvidenceProvenance,
    choices: &[(&str, &str)],
) -> EvidenceRecord {
    EvidenceRecord {
        key: CapabilityKey {
            kind,
            upstream_id: upstream_id.to_owned(),
        },
        claim,
        provenance,
        identity: identity.clone(),
        observed_at: ObservationTime(1),
        raw_digest: RawEvidenceDigest("sha256:model".to_owned()),
        session_scope: EvidenceSessionScope::Session("sid".to_owned()),
        choices: choices
            .iter()
            .map(|(id, label)| CapabilityChoice {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                description: None,
            })
            .collect(),
    }
}

fn with_complete_evidence(
    mut caps: SpurAgentCaps,
    epoch_id: u64,
    records: Vec<EvidenceRecord>,
) -> SpurAgentCaps {
    let identity = evidence_identity();
    let epoch = EvidenceEpoch::new(EvidenceEpochId(epoch_id), identity.clone(), records)
        .expect("test evidence must use one identity");
    let snapshot = CapabilityEvidenceSnapshot::from_epoch(epoch, &identity);
    let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
    wire["completeness"] = serde_json::json!("complete");
    caps.capability_evidence =
        Some(serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"));
    caps
}

fn with_incomplete_evidence(mut caps: SpurAgentCaps) -> SpurAgentCaps {
    let mut wire = serde_json::to_value(
        caps.capability_evidence
            .take()
            .expect("test caps must include capability evidence"),
    )
    .expect("snapshot must serialize");
    wire["completeness"] = serde_json::json!("incomplete");
    caps.capability_evidence =
        Some(serde_json::from_value(wire).expect("incomplete evidence snapshot must deserialize"));
    caps
}

fn caps_with_modes() -> SpurAgentCaps {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
    new.modes = Some(SessionModeState::new(
        SessionModeId::new("read-only"),
        vec![
            SessionMode::new(SessionModeId::new("read-only"), "Ask for approval"),
            SessionMode::new(SessionModeId::new("agent"), "Agent"),
            SessionMode::new(
                SessionModeId::new("agent-full-access"),
                "Agent (full access)",
            ),
        ],
    ));
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);
    let identity = evidence_identity();
    with_complete_evidence(
        caps,
        2,
        vec![capability_evidence(
            &identity,
            CapabilityKind::Mode,
            "mode",
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::StandardAdvertisement,
            &[
                ("read-only", "Ask for approval"),
                ("agent", "Agent"),
                ("agent-full-access", "Agent (full access)"),
            ],
        )],
    )
}
#[test]
fn incomplete_evidence_keeps_notified_skill_commands_visible() {
    let identity = evidence_identity();
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
    let caps = with_incomplete_evidence(with_complete_evidence(
        SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp),
        14,
        vec![capability_evidence(
            &identity,
            CapabilityKind::Command,
            "commands",
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::ObservedNotification,
            &[
                ("$spurpower-solve", "Solve"),
                ("custom-skill", "Custom skill"),
            ],
        )],
    ));
    let mut cfg = spur_acp::AgentConfig::with_defaults("codex");
    cfg.kind = AgentKind::CodexAcp;
    let commands = vec![
        spur_acp::AvailableCommand::new("$spurpower-solve", "Solve"),
        spur_acp::AvailableCommand::new("custom-skill", "Custom skill"),
    ];

    // Both startup orderings must preserve a complete command notification.
    for notification_first in [false, true] {
        let mut view = spur_tui::views::session_detail::SessionDetailView::new(
            spur_acp::SessionId("sid".to_owned()),
            "codex".to_owned(),
            "brain".to_owned(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(cfg.clone()),
            Vec::new(),
        );
        if notification_first {
            view.apply_available_commands(&commands);
        }
        view.apply_advertised_commands(Some(&caps), &[]);
        if !notification_first {
            view.apply_available_commands(&commands);
        }
        let registry = view.command_registry();
        for command in &commands {
            let visible = registry.available_commands_for_session(Some(&caps));
            let entries = visible
                .iter()
                .filter(|entry| entry.name == command.name)
                .collect::<Vec<_>>();
            assert_eq!(entries.len(), 1, "missing skill {}", command.name);
            assert!(matches!(entries[0].dispatch, Dispatch::PromptText { .. }));
            assert_eq!(pinned_route_for_command(&caps, &command.name), None);
            let input = format!("/{} inspect this", command.name);
            let SubmitDecision::Send { blocks, .. } =
                route_with_caps(&input, &[], &[], registry, false, Some(&caps))
            else {
                panic!("skill must dispatch as a prompt");
            };
            assert!(
                matches!(blocks.as_slice(), [spur_acp::ContentBlock::Text(text)]
                    if text.text == input)
            );
        }
        view.apply_available_commands(&[]);
        assert!(view
            .command_registry()
            .resolve("/$spurpower-solve")
            .is_none());
    }
}

#[test]
fn incomplete_evidence_keeps_standard_config_commands_on_protocol_dispatch() {
    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
    new.config_options = Some(vec![
        SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "test-model",
            vec![SessionConfigSelectOption::new("test-model", "Test Model")],
        ),
        SessionConfigOption::select(
            SessionConfigId::new("reasoning_effort"),
            "Reasoning effort",
            "high",
            vec![SessionConfigSelectOption::new("high", "High")],
        ),
    ]);
    let identity = evidence_identity();
    let caps = with_incomplete_evidence(with_complete_evidence(
        SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp),
        14,
        vec![
            model_evidence(
                &identity,
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
            ),
            capability_evidence(
                &identity,
                CapabilityKind::Effort,
                "reasoning_effort",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::StandardAdvertisement,
                &[("high", "High")],
            ),
        ],
    ));

    let entries = AdvertisedSource::entries_from_caps("codex", &caps);
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["model", "effort"]
    );
    assert!(entries
        .iter()
        .all(|entry| matches!(entry.dispatch, Dispatch::SetSessionConfigOption { .. })));
    assert_eq!(pinned_route_for_command(&caps, "model"), None);
    assert_eq!(pinned_route_for_command(&caps, "effort"), None);

    let mut registry = spur_tui::commands::CommandRegistry::new();
    registry.set_agent_commands(
        "codex",
        vec![
            CommandEntry {
                name: "model".to_owned(),
                description: "Agent prompt model".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "codex".to_owned(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/model".to_owned(),
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "effort".to_owned(),
                description: "Unverified vendor effort".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "codex".to_owned(),
                },
                dispatch: Dispatch::VendorExec {
                    method: "vendor/set_effort".to_owned(),
                    command: "effort".to_owned(),
                    args_template: spur_acp::ArgsTemplateKind::RawRest,
                },
                arg_picker_spec: None,
            },
        ],
    );
    registry.set_advertised_commands("codex", entries);
    assert!(matches!(
        route_with_caps(
            "/model test-model",
            &[],
            &[],
            &registry,
            false,
            Some(&caps),
        ),
        SubmitDecision::SetSessionConfigOption { ref config_id, ref value, .. }
            if config_id == "model" && value == "test-model"
    ));
    assert!(matches!(
        route_with_caps(
            "/effort high",
            &[],
            &[],
            &registry,
            false,
            Some(&caps),
        ),
        SubmitDecision::SetSessionConfigOption { ref config_id, ref value, .. }
            if config_id == "reasoning_effort" && value == "high"
    ));
}

#[test]
fn incomplete_evidence_does_not_enable_vendor_native_commands() {
    let mut caps = grok_caps();
    caps.config_options = vec![SessionConfigOption::select(
        SessionConfigId::new("model"),
        "Model",
        "test-model",
        vec![SessionConfigSelectOption::new("test-model", "Test Model")],
    )];
    let caps = with_incomplete_evidence(caps);

    let entries = AdvertisedSource::entries_from_caps("grok", &caps);

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "model");
    assert!(matches!(
        entries[0].dispatch,
        Dispatch::SetSessionConfigOption { .. }
    ));
    assert_eq!(pinned_route_for_command(&caps, "model"), None);
    assert_eq!(pinned_route_for_command(&caps, "effort"), None);

    let mut registry = spur_tui::commands::CommandRegistry::new();
    registry.set_advertised_commands("grok", entries);
    assert!(matches!(
        route_with_caps(
            "/model test-model",
            &[],
            &[],
            &registry,
            false,
            Some(&caps),
        ),
        SubmitDecision::SetSessionConfigOption { ref config_id, ref value, .. }
            if config_id == "model" && value == "test-model"
    ));
}

#[test]
fn incomplete_evidence_suppresses_vendor_only_command_collisions() {
    let caps = with_incomplete_evidence(grok_caps());
    let entries = AdvertisedSource::entries_from_caps("grok", &caps);
    assert!(entries.is_empty());

    let mut registry = spur_tui::commands::CommandRegistry::new();
    registry.set_agent_commands(
        "grok",
        vec![
            CommandEntry {
                name: "model".to_owned(),
                description: "Unverified vendor model".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "grok".to_owned(),
                },
                dispatch: Dispatch::VendorExec {
                    method: "vendor/set_model".to_owned(),
                    command: "model".to_owned(),
                    args_template: spur_acp::ArgsTemplateKind::RawRest,
                },
                arg_picker_spec: None,
            },
            CommandEntry {
                name: "effort".to_owned(),
                description: "Unverified vendor effort".to_owned(),
                hint: None,
                source: CommandSource::Agent {
                    handle: "grok".to_owned(),
                },
                dispatch: Dispatch::VendorExec {
                    method: "vendor/set_effort".to_owned(),
                    command: "effort".to_owned(),
                    args_template: spur_acp::ArgsTemplateKind::RawRest,
                },
                arg_picker_spec: None,
            },
        ],
    );
    registry.set_advertised_commands("grok", entries);

    let visible = registry.available_commands_for_session(Some(&caps));
    assert!(visible
        .iter()
        .all(|entry| entry.name != "model" && entry.name != "effort"));
    assert!(matches!(
        route_with_caps("/model grok-4.6", &[], &[], &registry, false, Some(&caps)),
        SubmitDecision::Send { .. }
    ));
    assert!(matches!(
        route_with_caps("/effort high", &[], &[], &registry, false, Some(&caps)),
        SubmitDecision::Send { .. }
    ));
}

#[test]
fn incomplete_evidence_keeps_standard_mode_on_protocol_dispatch() {
    let caps = with_incomplete_evidence(caps_with_modes());
    let entries = AdvertisedSource::entries_from_caps("codex", &caps);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "mode");
    assert!(matches!(entries[0].dispatch, Dispatch::SetSessionMode));
    assert_eq!(pinned_route_for_command(&caps, "mode"), None);

    let mut registry = spur_tui::commands::CommandRegistry::new();
    registry.set_agent_commands(
        "codex",
        vec![CommandEntry {
            name: "mode".to_owned(),
            description: "Unverified vendor mode".to_owned(),
            hint: None,
            source: CommandSource::Agent {
                handle: "codex".to_owned(),
            },
            dispatch: Dispatch::VendorExec {
                method: "vendor/set_mode".to_owned(),
                command: "mode".to_owned(),
                args_template: spur_acp::ArgsTemplateKind::RawRest,
            },
            arg_picker_spec: None,
        }],
    );
    registry.set_advertised_commands("codex", entries);

    assert!(matches!(
        route_with_caps("/mode agent", &[], &[], &registry, false, Some(&caps)),
        SubmitDecision::SetSessionMode { ref value } if value == "agent"
    ));
}

fn grok_caps() -> SpurAgentCaps {
    let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
    init.meta = Some(
        serde_json::json!({
            "modelState": {
                "currentModelId": "grok-4.6",
                "availableModels": [
                    {
                        "modelId": "grok-4.6",
                        "name": "Grok 4.6",
                        "_meta": {
                            "reasoningEffort": "high",
                            "reasoningEfforts": [
                                {"id": "xhigh", "label": "Extra High Effort"},
                                {"id": "high", "label": "High Effort"},
                                {"id": "medium", "label": "Medium Effort"},
                                {"id": "low", "label": "Low Effort"}
                            ]
                        }
                    },
                    {
                        "modelId": "grok-composer-2.5-fast",
                        "name": "Grok Composer 2.5 Fast",
                        "_meta": {"reasoningEfforts": []}
                    }
                ]
            }
        })
        .as_object()
        .expect("meta fixture must be an object")
        .clone(),
    );
    let caps = SpurAgentCaps::new(
        &init,
        &NewSessionResponse::new(spur_acp::AcpSessionId::new("sid")),
        AgentKind::Grok,
    );
    let identity = evidence_identity();
    with_complete_evidence(
        caps,
        4,
        vec![
            capability_evidence(
                &identity,
                CapabilityKind::Model,
                "model",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
                &[
                    ("grok-4.6", "Grok 4.6"),
                    ("grok-composer-2.5-fast", "Grok Composer 2.5 Fast"),
                ],
            ),
            capability_evidence(
                &identity,
                CapabilityKind::Effort,
                "reasoning_effort",
                EvidenceClaim::NativeVerified,
                EvidenceProvenance::AcceptedActiveProbe,
                &[
                    ("xhigh", "Extra High Effort"),
                    ("high", "High Effort"),
                    ("medium", "Medium Effort"),
                    ("low", "Low Effort"),
                ],
            ),
        ],
    )
}
