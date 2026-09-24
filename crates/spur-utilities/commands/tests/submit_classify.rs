//! Pure submit-classification tests for `spur_commands::submit` — the
//! neutral seam the TUI `submit_shell` delegates to (decoupling spec
//! 2026-09-23 §3.4). Everything here is frontend-agnostic: no TUI types,
//! no `spur-mentions` dependency, no `Action` — [`SubmitPlan::Local`]
//! carries `{name, arg}` for the frontend boundary to resolve.
//!
//! Written RED-first against the C3 API: `classify` + `MentionSpan`
//! assembly + `PromptBlockCaps` + the flatten/preview helpers.

use spur_acp::{
    AgentCapabilities, AgentKind, ContentBlock, EmbeddedResource, EmbeddedResourceResource,
    ResourceLink, SpurAgentCaps, TextContent, TextResourceContents,
};
use spur_commands::entry::{CommandEntry, CommandSource, Dispatch};
use spur_commands::registry::{CommandRegistry, LocalLayer};
use spur_commands::submit::{
    assemble_blocks, assemble_blocks_with_special, blocks_preview, blocks_to_text, classify,
    flatten_prompt_block, mention_name_from_uri, MentionSpan, PromptBlockCaps, SpecialSpan,
    SubmitPlan,
};

// ------------------------------------------------------------- helpers

fn span_at(start: usize, atom: &str, uri: &str, name: &str) -> MentionSpan {
    MentionSpan {
        start,
        end: start + atom.len(),
        name: name.to_owned(),
        uri: uri.to_owned(),
    }
}

fn registry_with(entries: Vec<CommandEntry>) -> CommandRegistry {
    let mut registry = CommandRegistry::new(LocalLayer::empty());
    registry.set_agent_commands("agent", entries);
    registry
}

fn local_entry(name: &str) -> CommandEntry {
    CommandEntry {
        name: name.into(),
        description: format!("{name} fixture"),
        hint: None,
        source: CommandSource::Spur,
        dispatch: Dispatch::Local { name: name.into() },
        arg_picker_spec: None,
    }
}

fn send_blocks(plan: SubmitPlan) -> Vec<ContentBlock> {
    match plan {
        SubmitPlan::Send { blocks, interrupt } => {
            assert!(!interrupt, "Send fixture must not carry interrupt");
            blocks
        }
        other => panic!("expected Send, got {other:?}"),
    }
}

fn text_of(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text(t) => t.text.as_str(),
        other => panic!("expected Text block, got {other:?}"),
    }
}

fn prompt_caps(image: bool, embedded_context: bool) -> SpurAgentCaps {
    let mut agent = AgentCapabilities::default();
    agent.prompt_capabilities.image = image;
    agent.prompt_capabilities.embedded_context = embedded_context;
    SpurAgentCaps {
        agent,
        modes: None,
        config_options: Vec::new(),
        agent_kind: AgentKind::Generic,
        grok_display: None,
        kiro_display: None,
        capability_evidence: None,
    }
}

/// Caps with a complete capability-evidence epoch pinning one route for
/// the synthetic `/model` command (same construction the TUI parity
/// tests use; pure spur-acp types only).
fn model_route_caps(epoch_id: u64, route_is_prompt_only: bool) -> SpurAgentCaps {
    use spur_acp::capability_evidence::{
        CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, EvidenceClaim, EvidenceEpoch,
        EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope, ObservationTime,
        RawEvidenceDigest,
    };
    use spur_acp::spur_agent_caps::CapabilityEvidenceSnapshot;

    let identity = CliIdentity {
        resolved_executable: std::path::PathBuf::from("/usr/bin/test-acp"),
        upstream_version: Some("1.0.0".to_owned()),
        argv_fingerprint: "argv".to_owned(),
        environment_fingerprint: "env".to_owned(),
    };
    let record = EvidenceRecord {
        key: CapabilityKey {
            kind: CapabilityKind::Model,
            upstream_id: "model".to_owned(),
        },
        claim: if route_is_prompt_only {
            EvidenceClaim::CandidateObserved
        } else {
            EvidenceClaim::NativeVerified
        },
        provenance: if route_is_prompt_only {
            EvidenceProvenance::PromptFallback
        } else {
            EvidenceProvenance::AcceptedActiveProbe
        },
        identity: identity.clone(),
        observed_at: ObservationTime(epoch_id),
        raw_digest: RawEvidenceDigest(format!("sha256:model:{epoch_id}")),
        session_scope: EvidenceSessionScope::Session("sid".to_owned()),
        choices: vec![CapabilityChoice {
            id: "test-model".to_owned(),
            label: "Test Model".to_owned(),
            description: None,
        }],
    };
    let epoch = EvidenceEpoch::new(EvidenceEpochId(epoch_id), identity.clone(), vec![record])
        .expect("test evidence must use one identity");
    let snapshot = CapabilityEvidenceSnapshot::from_epoch(epoch, &identity);
    let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
    wire["completeness"] = serde_json::json!("complete");

    let mut caps = prompt_caps(false, false);
    caps.capability_evidence =
        Some(serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"));
    caps
}

/// Registry shape matching the capability-route reduction fixture: an
/// agent prompt `/model` entry plus the advertised entries synthesized
/// from `caps`.
fn reduced_model_registry(caps: &SpurAgentCaps) -> CommandRegistry {
    let mut registry = CommandRegistry::new(LocalLayer::empty());
    registry.set_agent_commands(
        "agent",
        vec![CommandEntry {
            name: "model".to_owned(),
            description: "Agent prompt model".to_owned(),
            hint: None,
            source: CommandSource::Agent {
                handle: "agent".to_owned(),
            },
            dispatch: Dispatch::PromptText {
                normalized: "/model".to_owned(),
            },
            arg_picker_spec: None,
        }],
    );
    registry.set_advertised_commands(
        "agent",
        spur_commands::advertised::AdvertisedSource::entries_from_caps("agent", caps),
    );
    registry
}

// ------------------------------------------------- flatten / preview

#[test]
fn flatten_prompt_block_skips_ui_hint_text() {
    let block = ContentBlock::Text(TextContent::new(
        "[UI hint] User-suggested workers for delegation this turn: claude-code.",
    ));
    assert_eq!(flatten_prompt_block(&block), None);
}

#[test]
fn flatten_prompt_block_renders_resource_link_as_at_name() {
    let block = ContentBlock::ResourceLink(ResourceLink::new("lib.rs", "file:///p/lib.rs"));
    assert_eq!(flatten_prompt_block(&block), Some("@lib.rs".to_owned()));
}

#[test]
fn flatten_prompt_block_renders_embedded_resource_as_at_basename() {
    let block = ContentBlock::Resource(EmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(TextResourceContents::new(
            "fn main() {}",
            "file:///tmp/proj/src/lib.rs",
        )),
    ));
    assert_eq!(flatten_prompt_block(&block), Some("@lib.rs".to_owned()));
}

#[test]
fn flatten_prompt_block_drops_blocks_without_mention_display() {
    let image = ContentBlock::Image(spur_acp::ImageContent::new("AAAA", "image/png"));
    assert_eq!(flatten_prompt_block(&image), None);
}

#[test]
fn blocks_preview_concatenates_text_and_skips_hints() {
    let blocks = vec![
        ContentBlock::Text(TextContent::new(
            "[UI hint] delegation framing that must not replay",
        )),
        ContentBlock::Text(TextContent::new("review ")),
        ContentBlock::ResourceLink(ResourceLink::new("lib.rs", "file:///p/lib.rs")),
        ContentBlock::Text(TextContent::new(" now")),
    ];
    assert_eq!(blocks_preview(&blocks), "review @lib.rs now");
    assert_eq!(blocks_to_text(&blocks), "review @lib.rs now");
}

#[test]
fn mention_name_from_uri_takes_last_path_segment() {
    assert_eq!(
        mention_name_from_uri("file:///tmp/proj/src/lib.rs"),
        "lib.rs"
    );
    assert_eq!(mention_name_from_uri("worker://claude-code"), "claude-code");
    assert_eq!(mention_name_from_uri("src/main.rs"), "main.rs");
}

#[test]
fn mention_name_from_uri_handles_windows_and_trailing_separators() {
    assert_eq!(mention_name_from_uri(r"file://C:\proj\lib.rs"), "lib.rs");
    assert_eq!(mention_name_from_uri("file:///tmp/proj/"), "proj");
}

// -------------------------------------------------------- caps

#[test]
fn prompt_block_caps_default_when_caps_absent() {
    assert_eq!(
        PromptBlockCaps::from_agent(None),
        PromptBlockCaps {
            image: false,
            embedded_context: false
        }
    );
}

#[test]
fn prompt_block_caps_reads_advertised_prompt_capabilities() {
    let caps = prompt_caps(true, true);
    assert_eq!(
        PromptBlockCaps::from_agent(Some(&caps)),
        PromptBlockCaps {
            image: true,
            embedded_context: true
        }
    );
}

// ------------------------------------------------- classify: routing

#[test]
fn classify_empty_text_is_empty() {
    let registry = registry_with(vec![]);
    assert!(matches!(
        classify("", &[], &registry, false, None, None),
        SubmitPlan::Empty
    ));
}

#[test]
fn classify_plain_text_routes_to_single_text_block() {
    let registry = registry_with(vec![]);
    let plan = classify("hello world", &[], &registry, true, None, None);
    let blocks = match plan {
        SubmitPlan::Send { blocks, interrupt } => {
            assert!(interrupt);
            blocks
        }
        other => panic!("expected Send, got {other:?}"),
    };
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "hello world");
}

#[test]
fn classify_unknown_slash_falls_through_to_send_verbatim() {
    let registry = registry_with(vec![]);
    let blocks = send_blocks(classify(
        "/definitely-not-a-command arg",
        &[],
        &registry,
        false,
        None,
        None,
    ));
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "/definitely-not-a-command arg");
}

#[test]
fn classify_local_entry_carries_name_and_trimmed_arg() {
    let registry = registry_with(vec![local_entry("help")]);

    let bare = classify("/help", &[], &registry, false, None, None);
    match bare {
        SubmitPlan::Local { name, arg } => {
            assert_eq!(name, "help");
            assert_eq!(arg, None);
        }
        other => panic!("expected Local, got {other:?}"),
    }

    let named = classify("/help  tail text ", &[], &registry, false, None, None);
    match named {
        SubmitPlan::Local { name, arg } => {
            assert_eq!(name, "help");
            assert_eq!(arg.as_deref(), Some("tail text"));
        }
        other => panic!("expected Local, got {other:?}"),
    }
}

#[test]
fn classify_prompt_text_entry_appends_rest() {
    let registry = registry_with(vec![CommandEntry {
        name: "compact".into(),
        description: "compact history".into(),
        hint: None,
        source: CommandSource::Agent {
            handle: "claude".into(),
        },
        dispatch: Dispatch::PromptText {
            normalized: "/compact".into(),
        },
        arg_picker_spec: None,
    }]);
    let blocks = send_blocks(classify(
        "/compact please",
        &[],
        &registry,
        false,
        None,
        None,
    ));
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "/compact please");
}

#[test]
fn classify_vendor_exec_raw_rest_builds_params() {
    use spur_acp::ArgsTemplateKind;
    let registry = registry_with(vec![CommandEntry {
        name: "context".into(),
        description: "Show context".into(),
        hint: None,
        source: CommandSource::Agent {
            handle: "kiro".into(),
        },
        dispatch: Dispatch::VendorExec {
            method: "_kiro.dev/commands/execute".into(),
            command: "context".into(),
            args_template: ArgsTemplateKind::RawRest,
        },
        arg_picker_spec: None,
    }]);

    match classify("/context some rest", &[], &registry, false, None, None) {
        SubmitPlan::VendorExec { method, params } => {
            assert_eq!(method, "_kiro.dev/commands/execute");
            assert_eq!(
                params,
                serde_json::json!({ "command": "context", "args": { "raw": "some rest" } })
            );
        }
        other => panic!("expected VendorExec, got {other:?}"),
    }

    match classify("/context", &[], &registry, false, None, None) {
        SubmitPlan::VendorExec { params, .. } => {
            assert_eq!(params, serde_json::json!({ "command": "context" }));
        }
        other => panic!("expected VendorExec, got {other:?}"),
    }
}

#[test]
fn classify_config_option_requires_value_and_carries_command_name() {
    let registry = registry_with(vec![CommandEntry {
        name: "model".into(),
        description: "Switch model".into(),
        hint: None,
        source: CommandSource::Advertised {
            handle: "codex".into(),
        },
        dispatch: Dispatch::SetSessionConfigOption {
            config_id: "model".into(),
        },
        arg_picker_spec: None,
    }]);

    assert!(matches!(
        classify("/model ", &[], &registry, false, None, None),
        SubmitPlan::Empty
    ));

    match classify("/model gpt-5-codex", &[], &registry, false, None, None) {
        SubmitPlan::SetSessionConfigOption {
            command_name,
            config_id,
            value,
        } => {
            assert_eq!(command_name, "model");
            assert_eq!(config_id, "model");
            assert_eq!(value, "gpt-5-codex");
        }
        other => panic!("expected SetSessionConfigOption, got {other:?}"),
    }
}

#[test]
fn classify_model_config_promotes_to_set_session_model_for_legacy_caps() {
    use spur_acp::{
        InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
        SessionConfigOption, SessionConfigSelectOption,
    };
    let registry = registry_with(vec![CommandEntry {
        name: "model".into(),
        description: "Switch model".into(),
        hint: None,
        source: CommandSource::Advertised {
            handle: "codex".into(),
        },
        dispatch: Dispatch::SetSessionConfigOption {
            config_id: "model".into(),
        },
        arg_picker_spec: None,
    }]);

    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
    new.config_options = Some(vec![SessionConfigOption::select(
        SessionConfigId::new("model"),
        "Model",
        "gpt-5-codex",
        vec![SessionConfigSelectOption::new("gpt-5-codex", "GPT-5 Codex")],
    )]);
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

    match classify(
        "/model gpt-5-codex",
        &[],
        &registry,
        false,
        Some(&caps),
        None,
    ) {
        SubmitPlan::SetSessionModel { value } => assert_eq!(value, "gpt-5-codex"),
        other => panic!("expected SetSessionModel, got {other:?}"),
    }
}

#[test]
fn classify_mode_requires_advertised_mode_id() {
    use spur_acp::{
        InitializeResponse, NewSessionResponse, ProtocolVersion, SessionMode, SessionModeId,
        SessionModeState,
    };
    let mut registry = CommandRegistry::new(LocalLayer::empty());
    registry.set_advertised_commands(
        "codex",
        vec![CommandEntry {
            name: "mode".into(),
            description: "Switch mode".into(),
            hint: None,
            source: CommandSource::Advertised {
                handle: "codex".into(),
            },
            dispatch: Dispatch::SetSessionMode,
            arg_picker_spec: None,
        }],
    );

    let init = InitializeResponse::new(ProtocolVersion::LATEST);
    let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
    new.modes = Some(SessionModeState::new(
        SessionModeId::new("read-only"),
        vec![SessionMode::new(SessionModeId::new("read-only"), "Ask")],
    ));
    let caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);

    assert!(matches!(
        classify("/mode agent", &[], &registry, false, Some(&caps), None),
        SubmitPlan::Empty
    ));
    match classify("/mode read-only", &[], &registry, false, Some(&caps), None) {
        SubmitPlan::SetSessionMode { value } => assert_eq!(value, "read-only"),
        other => panic!("expected SetSessionMode, got {other:?}"),
    }
}

#[test]
fn classify_pinned_prompt_only_model_routes_as_prompt_send() {
    let caps = model_route_caps(21, true);
    let registry = reduced_model_registry(&caps);

    let blocks = send_blocks(classify(
        "/model test-model",
        &[],
        &registry,
        false,
        Some(&caps),
        None,
    ));
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "/model test-model");
}

#[test]
fn classify_pinned_native_model_routes_to_set_session_model() {
    let caps = model_route_caps(31, false);
    let registry = reduced_model_registry(&caps);

    match classify(
        "/model test-model",
        &[],
        &registry,
        false,
        Some(&caps),
        None,
    ) {
        SubmitPlan::SetSessionModel { value } => assert_eq!(value, "test-model"),
        other => panic!("expected SetSessionModel, got {other:?}"),
    }
}

// ------------------------------------------------- classify: assembly

#[test]
fn classify_send_assembles_blocks_from_spans() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "fn hello() {}\n").unwrap();
    let uri = format!("file://{}", path.display());
    let text = "review @lib.rs now";
    let spans = [span_at("review ".len(), "@lib.rs", &uri, "lib.rs")];

    let registry = registry_with(vec![]);
    let blocks = send_blocks(classify(text, &spans, &registry, false, None, None));
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert_eq!(text_of(&blocks[0]), "review ");
    assert!(matches!(
        &blocks[1],
        ContentBlock::ResourceLink(r) if r.uri == uri && r.name == "lib.rs"
    ));
    assert_eq!(text_of(&blocks[2]), " now");
}

// ------------------------------------------------- assemble_blocks

#[test]
fn assemble_blocks_embeds_file_when_advertised() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("lib.rs");
    std::fs::write(&path, "fn hello() {}\n").unwrap();
    let uri = format!("file://{}", path.display());
    let text = "review @lib.rs now";
    let spans = [span_at("review ".len(), "@lib.rs", &uri, "lib.rs")];

    let blocks = assemble_blocks(
        text,
        &spans,
        PromptBlockCaps {
            image: false,
            embedded_context: true,
        },
    );
    assert!(
        blocks.iter().any(|b| matches!(
            b,
            ContentBlock::Resource(r)
                if matches!(
                    &r.resource,
                    EmbeddedResourceResource::TextResourceContents(t)
                        if t.uri == uri && t.text.contains("fn hello")
                )
        )),
        "expected embedded Resource with file bytes, got {blocks:?}"
    );
    assert!(
        !blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ResourceLink(_))),
        "must not also send ResourceLink when embed succeeds, got {blocks:?}"
    );
}

#[test]
fn assemble_blocks_keeps_resource_link_when_embed_off_or_file_too_large() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("big.rs");
    std::fs::write(&path, "x".repeat(64 * 1024)).unwrap();
    let uri = format!("file://{}", path.display());
    let text = "review @big.rs";
    let spans = [span_at("review ".len(), "@big.rs", &uri, "big.rs")];

    // embed off → ResourceLink
    let blocks = assemble_blocks(text, &spans, PromptBlockCaps::default());
    assert!(matches!(&blocks[1], ContentBlock::ResourceLink(r) if r.uri == uri));

    // embed advertised but file over PER_PROMPT_CAP_BYTES → ResourceLink
    let blocks = assemble_blocks(
        text,
        &spans,
        PromptBlockCaps {
            image: false,
            embedded_context: true,
        },
    );
    assert!(matches!(&blocks[1], ContentBlock::ResourceLink(r) if r.uri == uri));
}

#[test]
fn assemble_blocks_no_spans_falls_back_to_whole_text() {
    let blocks = assemble_blocks("hello", &[], PromptBlockCaps::default());
    assert_eq!(blocks.len(), 1);
    assert_eq!(text_of(&blocks[0]), "hello");
}

#[test]
fn assemble_blocks_unresolved_graph_span_warns_instead_of_linking() {
    // A code mention nobody resolved — no special hook, no payload —
    // fails loudly with the MENTION_WARNING framing instead of
    // silently degrading to a ResourceLink (pre-split router parity).
    let spans = [span_at(0, "@sym", "graph://symbol/sym", "sym")];
    let blocks = assemble_blocks("@sym", &spans, PromptBlockCaps::default());
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    assert_eq!(
        text_of(&blocks[0]),
        "MENTION_WARNING sym\nintended_uri:   graph://symbol/sym\nfailure_reason: payload_not_in_registry\nreplaced_with:  dropped\n"
    );
}

// ------------------------------------------------- special spans

#[test]
fn assemble_blocks_special_expansion_respects_per_prompt_cap() {
    let text = "a @b";
    let spans = [
        span_at(0, "a", "graph://x", "a"),
        span_at(2, "@b", "graph://y", "b"),
    ];
    let big = "z".repeat(31 * 1024);
    let mut emitted = 0;
    let mut lookup = |span: &MentionSpan| {
        if emitted == 0 {
            emitted += 1;
            Some(SpecialSpan::Expansion {
                text: big.clone(),
                is_symbol_body: false,
            })
        } else if span.uri == "graph://y" {
            Some(SpecialSpan::Expansion {
                // 31 KiB + 2 KiB > 32 KiB budget → omitted
                text: "y".repeat(2 * 1024),
                is_symbol_body: false,
            })
        } else {
            None
        }
    };

    let blocks =
        assemble_blocks_with_special(text, &spans, PromptBlockCaps::default(), Some(&mut lookup));
    assert!(
        blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::Text(t) if t.text
                == "MENTION_OMITTED graph://y (per-prompt cap)\n")),
        "expected MENTION_OMITTED for the over-budget span, got {blocks:?}"
    );
}

#[test]
fn assemble_blocks_special_symbol_body_appends_topology_hint() {
    let spans = [span_at(0, "@sym", "graph://s", "sym")];
    let mut lookup = |_span: &MentionSpan| {
        Some(SpecialSpan::Expansion {
            text: "fn body() {}".to_owned(),
            is_symbol_body: true,
        })
    };
    let blocks = assemble_blocks_with_special(
        "@sym",
        &spans,
        PromptBlockCaps::default(),
        Some(&mut lookup),
    );
    let last = blocks.last().expect("hint block");
    assert!(
        text_of(last).contains("topology_available_via_mcp_for_above_symbols"),
        "expected topology hint appended, got {blocks:?}"
    );
}

#[test]
fn assemble_blocks_special_text_and_blocks_pass_through() {
    let spans = [
        span_at(0, "@warn", "graph://w", "warn"),
        span_at(6, "@img", "graph://i", "img"),
    ];
    let mut lookup = |span: &MentionSpan| {
        if span.uri == "graph://w" {
            Some(SpecialSpan::Text(
                "MENTION_WARNING warn\nintended_uri:   graph://w\nfailure_reason: payload_not_in_registry\nreplaced_with:  dropped\n".to_owned(),
            ))
        } else {
            Some(SpecialSpan::Blocks(vec![ContentBlock::Image(
                spur_acp::ImageContent::new("AAAA", "image/png"),
            )]))
        }
    };
    let blocks = assemble_blocks_with_special(
        "@warn @img",
        &spans,
        PromptBlockCaps::default(),
        Some(&mut lookup),
    );
    assert_eq!(blocks.len(), 3, "{blocks:?}");
    assert!(text_of(&blocks[0]).starts_with("MENTION_WARNING"));
    assert_eq!(text_of(&blocks[1]), " ");
    assert!(matches!(&blocks[2], ContentBlock::Image(_)));
}
