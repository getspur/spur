//! SubmitRouter — decide what to do with an Enter-submitted InputBar.
//!
//! On Enter, the `InputBar` captures `(text, ranges, interrupt)`. The
//! router takes that triple, pending image attachments, plus the
//! `CommandRegistry` and returns a
//! `SubmitDecision`:
//!
//! * `Empty`         — nothing to do.
//! * `Send`          — forward `Vec<ContentBlock>` to the agent.
//! * `Local`         — fire an `Action` without sending a message.
//! * `VendorExec`    — invoke an agent-specific vendor extension RPC.
//!
//! Non-slash text routes to `Send`, assembling blocks by interleaving
//! `Text` with `ResourceLink`/`Image` blocks from `ranges`.

use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use spur_acp::{ContentBlock, ResourceLink, SpurAgentCaps, TextContent};

use crate::action::Action;
use crate::components::input_bar::{ImageAttachment, ProtectedRange, RangeKind};
use crate::components::query_source::RetrievalAccept;
use crate::mentions::code_graph::expansion::{expand, ExpandedMention, PER_PROMPT_CAP_BYTES};
use crate::mentions::code_graph::CodeMentionPayload;

use super::entry::Dispatch;
use super::registry::CommandRegistry;

/// What the controller should do with an Enter-submitted InputBar.
#[derive(Debug)]
pub enum SubmitDecision {
    Send {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    Local {
        action: Action,
    },
    /// Generic vendor-extension dispatch. Carries the full wire method and
    /// the rendered params payload — the consumer (app.rs → orchestrator)
    /// calls `connection.call_ext(method, params)`.
    VendorExec {
        method: String,
        params: Value,
    },
    /// v1 codex `/model` and `/effort` slash pickers — typed wire dispatch
    /// to ACP `session/set_config_option`. The consumer (app.rs → orchestrator)
    /// maps this to `InteractiveInput::SetSessionConfigOption`.
    SetSessionConfigOption {
        config_id: String,
        value: String,
    },
    /// Wave B.4: emitted when caps advertise the dedicated ACP
    /// `session/set_model` method (`SpurAgentCaps::supports_set_model()`).
    /// The consumer routes through `NativeAcpConnection::set_session_model`
    /// instead of `set_session_config_option` (spec §6.3).
    SetSessionModel {
        value: String,
    },
    Empty,
}

/// Route a submitted input to a `SubmitDecision`. Caps-unaware overload —
/// equivalent to `route_with_caps(.., None)`. Preserved for sites that
/// don't yet thread `SpurAgentCaps` through (pre-M9 callers).
pub fn route(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    registry: &CommandRegistry,
    interrupt: bool,
) -> SubmitDecision {
    route_with_caps(text, ranges, images, registry, interrupt, None)
}

/// Caps-aware route. When `caps` advertise the dedicated
/// `session/set_model` method, `/model <value>` is rewritten from
/// `SubmitDecision::SetSessionConfigOption` to
/// `SubmitDecision::SetSessionModel` so the orchestrator can pick the
/// dedicated dispatch. Spec §6.3 / Wave B.4.
pub fn route_with_caps(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    registry: &CommandRegistry,
    interrupt: bool,
    caps: Option<&SpurAgentCaps>,
) -> SubmitDecision {
    if text.is_empty() {
        return SubmitDecision::Empty;
    }

    // /work <id> → issue WorkOn action
    if let Some(rest) = text.strip_prefix("/work ") {
        let id = rest.trim().to_string();
        if !id.is_empty() {
            return SubmitDecision::Local {
                action: Action::Issue(crate::action::IssueAction::WorkOn { id }),
            };
        }
    }

    // /theme [<name>|reload] → carry the raw arg into Action::ThemeCommand.
    // Intercepted ahead of the registry so the trailing arg is preserved
    // (the registry-resolved variant in `spur_local` carries an empty arg
    // and only services the bare `/theme` form).
    if text == "/theme" {
        return SubmitDecision::Local {
            action: Action::ThemeCommand { arg: String::new() },
        };
    }
    if let Some(rest) = text.strip_prefix("/theme ") {
        return SubmitDecision::Local {
            action: Action::ThemeCommand {
                arg: rest.trim().to_string(),
            },
        };
    }

    // /issue show <id> → issue ViewDetail action
    if let Some(rest) = text.strip_prefix("/issue show ") {
        let id = rest.trim().to_string();
        if !id.is_empty() {
            return SubmitDecision::Local {
                action: Action::Issue(crate::action::IssueAction::ViewDetail { id }),
            };
        }
    }

    if text.starts_with('/') {
        if let Some(entry) = registry.resolve(text) {
            return match entry.dispatch {
                Dispatch::SpurLocal(action) => SubmitDecision::Local { action },
                Dispatch::PromptText { normalized } => {
                    let rest = rest_after_first_token(text);
                    let normalized_full = if rest.is_empty() {
                        normalized
                    } else {
                        format!("{} {}", normalized, rest)
                    };
                    SubmitDecision::Send {
                        blocks: vec![ContentBlock::Text(TextContent::new(normalized_full))],
                        interrupt,
                    }
                }
                Dispatch::SetSessionConfigOption { config_id } => {
                    // Parse the arg from text (whatever follows `/<cmd> `).
                    let value = rest_after_first_token(text);
                    let value = value.trim().to_string();
                    if value.is_empty() {
                        // No arg yet — picker should still be open. Treat as no-op.
                        SubmitDecision::Empty
                    } else if config_id == "model" && caps.is_some_and(|c| c.supports_set_model()) {
                        // Wave B.4 / spec §6.3: prefer the dedicated
                        // `session/set_model` dispatch. Fallback when
                        // `set_model` is unavailable stays via the
                        // existing SetSessionConfigOption path —
                        // NativeAcpConnection::set_session_model also
                        // applies its own state-gated fallback for
                        // calls that flow through it directly.
                        SubmitDecision::SetSessionModel { value }
                    } else {
                        SubmitDecision::SetSessionConfigOption { config_id, value }
                    }
                }
                Dispatch::VendorExec {
                    method,
                    command,
                    args_template,
                } => {
                    let rest = rest_after_first_token(text);
                    let params = match args_template {
                        spur_acp::ArgsTemplateKind::RawRest => {
                            if rest.is_empty() {
                                serde_json::json!({ "command": command })
                            } else {
                                serde_json::json!({
                                    "command": command,
                                    "args": { "raw": rest },
                                })
                            }
                        }
                    };
                    SubmitDecision::VendorExec { method, params }
                }
            };
        }
        // Unknown /command — fall through to Send as plain text so the
        // agent receives it (agents often render unknown slash commands
        // verbatim as prompts).
    }

    let blocks = assemble_blocks(text, ranges, images);
    SubmitDecision::Send { blocks, interrupt }
}

pub(crate) fn local_action_from_picker_accept(
    accept: RetrievalAccept,
    registry: &CommandRegistry,
    caps: Option<&SpurAgentCaps>,
) -> Option<Action> {
    let RetrievalAccept::SubmitText { text } = accept else {
        return None;
    };

    match route_with_caps(&text, &[], &[], registry, false, caps) {
        SubmitDecision::Local { action } => Some(action),
        _ => None,
    }
}

/// Everything after the first whitespace-delimited token of `text`.
fn rest_after_first_token(text: &str) -> String {
    match text.split_once(char::is_whitespace) {
        Some((_, rest)) => rest.trim_start().to_string(),
        None => String::new(),
    }
}

/// Walk `text` + sorted `ranges` interleaved → `[Text, ResourceLink/Image, Text, …]`.
pub fn assemble_blocks(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
) -> Vec<ContentBlock> {
    assemble_blocks_inner(text, ranges, images, None)
}

type CodeExpansionLookup<'a> = Option<&'a mut dyn FnMut(&str) -> Option<String>>;

pub fn assemble_blocks_with_code_mentions<'a>(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    worktree_root: &Path,
    mut lookup_code_payload: impl FnMut(&str) -> Option<&'a CodeMentionPayload>,
) -> Vec<ContentBlock> {
    let mut lookup = |uri: &str| {
        lookup_code_payload(uri).map(|payload| match expand(payload, worktree_root) {
            ExpandedMention::Body { text } | ExpandedMention::Warning { text, .. } => text,
        })
    };
    assemble_blocks_inner(text, ranges, images, Some(&mut lookup))
}

fn assemble_blocks_inner(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    mut code_expansion_lookup: CodeExpansionLookup<'_>,
) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    let mut code_expansion_bytes = 0usize;
    for r in ranges {
        if r.start > cursor {
            out.push(ContentBlock::Text(TextContent::new(
                text[cursor..r.start].to_string(),
            )));
        }
        match &r.kind {
            RangeKind::ImageRef(id) => match images.iter().find(|att| att.id == *id) {
                Some(att) => match encode_image_attachment(att) {
                    Ok(block) => out.push(block),
                    Err(err) => {
                        tracing::error!("image encode failed for id={}: {err}", id);
                        out.push(ContentBlock::Text(TextContent::new(format!(
                            "[image encode error: {err}]"
                        ))));
                    }
                },
                None => {
                    tracing::warn!("ImageRef(id={}) not found in images list", id);
                }
            },
            _ => {
                if r.uri.starts_with("graph://") {
                    if let Some(expansion) = code_expansion_lookup
                        .as_deref_mut()
                        .and_then(|lookup| lookup(&r.uri))
                    {
                        if code_expansion_bytes + expansion.len() > PER_PROMPT_CAP_BYTES {
                            out.push(ContentBlock::Text(TextContent::new(format!(
                                "MENTION_OMITTED {} (per-prompt cap)\n",
                                r.uri
                            ))));
                        } else {
                            code_expansion_bytes += expansion.len();
                            out.push(ContentBlock::Text(TextContent::new(expansion)));
                        }
                    } else {
                        out.push(ContentBlock::ResourceLink(ResourceLink::new(
                            r.name.clone(),
                            r.uri.clone(),
                        )));
                    }
                } else {
                    out.push(ContentBlock::ResourceLink(ResourceLink::new(
                        r.name.clone(),
                        r.uri.clone(),
                    )));
                }
            }
        }
        cursor = r.end;
    }
    if cursor < text.len() {
        out.push(ContentBlock::Text(TextContent::new(
            text[cursor..].to_string(),
        )));
    }
    if out.is_empty() && ranges.is_empty() {
        out.push(ContentBlock::Text(TextContent::new(text.to_string())));
    }
    out
}

fn encode_image_attachment(att: &ImageAttachment) -> anyhow::Result<ContentBlock> {
    use image::imageops::FilterType;

    let bytes = std::fs::read(&att.source_path)?;
    let mut image = image::load_from_memory(&bytes)?;

    const MAX_DIM: u32 = 2048;
    if image.width() > MAX_DIM || image.height() > MAX_DIM {
        image = image.resize(MAX_DIM, MAX_DIM, FilterType::Lanczos3);
    }

    let mut cursor = std::io::Cursor::new(Vec::new());
    image.write_to(&mut cursor, image::ImageFormat::Png)?;
    let png_bytes = cursor.into_inner();

    const MAX_B64_BYTES: usize = 10 * 1024 * 1024;
    let encoded_len = base64::encoded_len(png_bytes.len(), true)
        .ok_or_else(|| anyhow::anyhow!("image too large to base64-encode"))?;
    if encoded_len > MAX_B64_BYTES {
        anyhow::bail!("image too large ({encoded_len} bytes base64); max 10 MB");
    }

    let data = STANDARD.encode(&png_bytes);
    Ok(ContentBlock::Image(
        agent_client_protocol::schema::ImageContent::new(data, "image/png"),
    ))
}

/// Flatten blocks into a human-readable string for the local trace echo.
///
/// `Text` blocks concatenate their text; `ResourceLink` blocks render as
/// `@<name>`; unknown variants are skipped.
pub fn blocks_preview(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text(t) => s.push_str(&t.text),
            ContentBlock::ResourceLink(r) => {
                s.push('@');
                s.push_str(&r.name);
            }
            _ => {}
        }
    }
    s
}

/// Flatten blocks into a plain text string (e.g. for CLI that forwards text).
/// Currently identical to `blocks_preview` — kept as a distinct entry point
/// so future divergence (e.g. CLI-specific serialization) is cheap.
pub fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks_preview(blocks)
}

#[cfg(test)]
mod image_block_tests {
    use super::*;
    use crate::components::input_bar::{ImageAttachment, ProtectedRange, RangeKind};

    fn make_png_file() -> (tempfile::NamedTempFile, (u32, u32)) {
        let tmp = tempfile::NamedTempFile::with_suffix(".png").unwrap();
        let img = image::RgbaImage::from_raw(2, 2, vec![128u8; 16]).unwrap();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        let mut cursor = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        std::fs::write(tmp.path(), cursor.into_inner()).unwrap();
        (tmp, (2, 2))
    }

    #[test]
    fn assemble_blocks_emits_image_block_for_image_ref() {
        let (tmp, dims) = make_png_file();
        let label = "[Image #1 - 2x2]";
        let attachment = ImageAttachment {
            id: 0,
            source_path: tmp.path().to_path_buf(),
            mime_type: "image/png".to_string(),
            dimensions: dims,
            byte_size: 0,
            owned_temp: None,
        };
        let images = vec![attachment];
        let text = format!("before {} after", label);
        let ranges = vec![ProtectedRange {
            start: "before ".len(),
            end: "before ".len() + label.len(),
            kind: RangeKind::ImageRef(0),
            uri: String::new(),
            name: label.to_string(),
        }];

        let blocks = assemble_blocks(&text, &ranges, &images);

        assert_eq!(blocks.len(), 3, "expected Text + Image + Text");
        assert!(matches!(&blocks[0], ContentBlock::Text(_)));
        match &blocks[1] {
            ContentBlock::Image(image) => {
                assert_eq!(image.mime_type, "image/png");
                assert!(!image.data.is_empty());
            }
            other => panic!("expected Image block, got {other:?}"),
        }
        assert!(matches!(&blocks[2], ContentBlock::Text(_)));
    }

    #[test]
    fn route_with_caps_emits_image_block_for_image_ref() {
        let (tmp, dims) = make_png_file();
        let label = "[Image #1 - 2x2]";
        let attachment = ImageAttachment {
            id: 0,
            source_path: tmp.path().to_path_buf(),
            mime_type: "image/png".to_string(),
            dimensions: dims,
            byte_size: 0,
            owned_temp: None,
        };
        let images = vec![attachment];
        let text = format!("before {} after", label);
        let ranges = vec![ProtectedRange {
            start: "before ".len(),
            end: "before ".len() + label.len(),
            kind: RangeKind::ImageRef(0),
            uri: String::new(),
            name: label.to_string(),
        }];
        let registry = CommandRegistry::new();

        let decision = route_with_caps(&text, &ranges, &images, &registry, false, None);

        match decision {
            SubmitDecision::Send { blocks, interrupt } => {
                assert!(!interrupt);
                assert_eq!(blocks.len(), 3, "expected Text + Image + Text");
                assert!(matches!(&blocks[0], ContentBlock::Text(_)));
                assert!(matches!(&blocks[1], ContentBlock::Image(_)));
                assert!(matches!(&blocks[2], ContentBlock::Text(_)));
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    #[test]
    fn assemble_blocks_missing_image_id_no_panic() {
        let label = "[Image #99 - 2x2]";
        let text = format!("before {} after", label);
        let ranges = vec![ProtectedRange {
            start: "before ".len(),
            end: "before ".len() + label.len(),
            kind: RangeKind::ImageRef(99),
            uri: String::new(),
            name: label.to_string(),
        }];
        let images = vec![];

        let blocks = assemble_blocks(&text, &ranges, &images);

        assert!(blocks
            .iter()
            .all(|block| !matches!(block, ContentBlock::Image(_))));
    }
}

#[cfg(test)]
mod sessions_slash_tests {
    use super::*;
    use crate::commands::registry::CommandRegistry;

    fn build_registry_for_test() -> CommandRegistry {
        CommandRegistry::new()
    }

    #[test]
    fn slash_sessions_routes_to_request_sessions() {
        let registry = build_registry_for_test();

        let decision = route("/sessions", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::RequestSessions,
            } => {}
            other => panic!(
                "expected Local {{ action: RequestSessions }}, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn vendor_exec_raw_rest_produces_params_with_command_and_args() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("context", "Show context"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/context some rest", &[], &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { method, params } => {
                assert_eq!(method, "_kiro.dev/commands/execute");
                assert_eq!(
                    params,
                    serde_json::json!({
                        "command": "context",
                        "args": { "raw": "some rest" },
                    })
                );
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }

    #[test]
    fn vendor_exec_raw_rest_empty_args_still_includes_command() {
        use spur_acp::{ArgsTemplateKind, AvailableCommand, CommandsConfig, DispatchKind};

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::VendorExec,
            exec_method: Some("_kiro.dev/commands/execute".into()),
            args_template: ArgsTemplateKind::RawRest,
            ..Default::default()
        };
        let entry = crate::agents::build_entry(
            "kiro",
            &cfg,
            &AvailableCommand::new("compact", "compact context"),
        );
        registry.set_agent_commands("kiro", vec![entry]);

        let decision = route("/kiro:compact", &[], &[], &registry, false);
        match decision {
            SubmitDecision::VendorExec { params, .. } => {
                assert_eq!(params, serde_json::json!({ "command": "compact" }));
            }
            other => panic!("expected VendorExec, got {:?}", other),
        }
    }

    /// Task 2.15: a /model entry with Dispatch::SetSessionConfigOption and
    /// a typed value routes to SubmitDecision::SetSessionConfigOption.
    #[test]
    fn slash_model_with_value_routes_to_set_session_config_option() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

        let mut registry = CommandRegistry::new();
        let entry = CommandEntry {
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
        };
        registry.set_advertised_commands("codex", vec![entry]);

        let decision = route("/model gpt-5-codex", &[], &[], &registry, false);
        match decision {
            SubmitDecision::SetSessionConfigOption { config_id, value } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-5-codex");
            }
            other => panic!("expected SetSessionConfigOption, got {:?}", other),
        }
    }

    /// Wave B.8: an agent-advertised command with Unstructured input (e.g.
    /// codex's /review-branch) routes free-text submits as PromptText so the
    /// agent receives the full canonical "/<cmd> <arg>" string.
    #[test]
    fn slash_review_branch_with_arg_routes_as_prompt_text() {
        use spur_acp::{
            AvailableCommand, AvailableCommandInput, CommandsConfig, DispatchKind,
            UnstructuredCommandInput,
        };

        let mut registry = CommandRegistry::new();
        let cfg = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            ..Default::default()
        };
        let cmd = AvailableCommand::new("review-branch", "Review against branch").input(
            AvailableCommandInput::Unstructured(UnstructuredCommandInput::new("branch name")),
        );
        let entry = crate::agents::build_entry("codex", &cfg, &cmd);
        // Sanity-check the auto-derived spec from Wave B.4 wired through.
        assert!(entry.arg_picker_spec.is_some());
        registry.set_agent_commands("codex", vec![entry]);

        let decision = route("/review-branch main", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Send { blocks, interrupt } => {
                assert!(!interrupt);
                assert_eq!(blocks.len(), 1);
                use agent_client_protocol::schema::ContentBlock;
                let text = match &blocks[0] {
                    ContentBlock::Text(t) => &t.text,
                    other => panic!("expected Text, got {other:?}"),
                };
                assert_eq!(text, "/review-branch main");
            }
            other => panic!("expected Send, got {:?}", other),
        }
    }

    /// Task 2.15: same entry but no value yet (`/model `) returns Empty so
    /// the picker stays open waiting for selection.
    #[test]
    fn slash_model_without_value_returns_empty() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

        let mut registry = CommandRegistry::new();
        let entry = CommandEntry {
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
        };
        registry.set_advertised_commands("codex", vec![entry]);

        let decision = route("/model ", &[], &[], &registry, false);
        assert!(matches!(decision, SubmitDecision::Empty));
    }

    /// Wave B.4: caps that advertise the dedicated `set_model` method
    /// route `/model <value>` to `SubmitDecision::SetSessionModel` so
    /// the orchestrator can dispatch through `set_session_model` instead
    /// of unconditionally through `set_session_config_option`.
    #[test]
    fn slash_model_with_caps_supporting_set_model_routes_to_set_session_model() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use agent_client_protocol::schema::{
            InitializeResponse, ModelId, ModelInfo, NewSessionResponse, ProtocolVersion, SessionId,
            SessionModelState,
        };

        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "codex",
            vec![CommandEntry {
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
            }],
        );

        // Build caps that advertise non-empty available_models — supports_set_model() = true.
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let new = NewSessionResponse::new(SessionId::new("sid")).models(SessionModelState::new(
            ModelId::new("gpt-5-codex"),
            vec![ModelInfo::new(ModelId::new("gpt-5-codex"), "GPT-5 Codex")],
        ));
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::CodexAcp);

        let decision = route_with_caps(
            "/model gpt-5-codex",
            &[],
            &[],
            &registry,
            false,
            Some(&caps),
        );
        match decision {
            SubmitDecision::SetSessionModel { value } => assert_eq!(value, "gpt-5-codex"),
            other => panic!("expected SetSessionModel, got {other:?}"),
        }
    }

    /// Wave B.4: caps WITHOUT the dedicated method (only set_config_option)
    /// preserve the existing SetSessionConfigOption decision.
    #[test]
    fn slash_model_with_caps_only_supporting_config_option_routes_to_existing() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use agent_client_protocol::schema::{
            InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
            SessionConfigKind, SessionConfigOption, SessionConfigSelect,
            SessionConfigSelectOptions, SessionConfigValueId, SessionId,
        };

        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "codex",
            vec![CommandEntry {
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
            }],
        );

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(SessionId::new("sid"));
        new.config_options = Some(vec![SessionConfigOption::new(
            SessionConfigId::new("model"),
            "Model",
            SessionConfigKind::Select(SessionConfigSelect::new(
                SessionConfigValueId::new("default"),
                SessionConfigSelectOptions::Ungrouped(vec![]),
            )),
        )]);
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::CodexAcp);
        assert!(!caps.supports_set_model());
        assert!(caps.supports_set_config_option());

        let decision = route_with_caps("/model gpt-4o", &[], &[], &registry, false, Some(&caps));
        match decision {
            SubmitDecision::SetSessionConfigOption { config_id, value } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-4o");
            }
            other => panic!("expected SetSessionConfigOption fallback, got {other:?}"),
        }
    }

    /// Bare `/theme` routes to `Action::ThemeCommand` with an empty arg
    /// — the App handler interprets that as "list available themes".
    #[test]
    fn slash_theme_bare_routes_to_theme_command_with_empty_arg() {
        let registry = build_registry_for_test();
        let decision = route("/theme", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ThemeCommand { arg },
            } => assert_eq!(arg, ""),
            other => panic!("expected ThemeCommand {{ arg: \"\" }}, got {:?}", other),
        }
    }

    /// `/theme <name>` carries the trimmed name into the action arg.
    #[test]
    fn slash_theme_with_name_routes_to_theme_command_with_arg() {
        let registry = build_registry_for_test();
        let decision = route("/theme light", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ThemeCommand { arg },
            } => assert_eq!(arg, "light"),
            other => panic!(
                "expected ThemeCommand {{ arg: \"light\" }}, got {:?}",
                other
            ),
        }
    }

    /// `/theme reload` is a sentinel arg, not a separate action variant.
    /// The App handler matches on `arg == "reload"` after the route.
    #[test]
    fn slash_theme_reload_carries_reload_arg() {
        let registry = build_registry_for_test();
        let decision = route("/theme reload", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ThemeCommand { arg },
            } => assert_eq!(arg, "reload"),
            other => panic!(
                "expected ThemeCommand {{ arg: \"reload\" }}, got {:?}",
                other
            ),
        }
    }

    /// Double-space between `/theme` and the arg collapses through the
    /// `strip_prefix("/theme ") + trim` pipeline. Prevents a regression
    /// where the trailing space leaks into the arg.
    #[test]
    fn slash_theme_double_space_trims_to_single_arg() {
        let registry = build_registry_for_test();
        let decision = route("/theme  reload", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ThemeCommand { arg },
            } => assert_eq!(arg, "reload"),
            other => panic!(
                "expected ThemeCommand {{ arg: \"reload\" }}, got {:?}",
                other
            ),
        }
    }

    /// `/theme reload extra-arg` keeps the full whitespace-trimmed tail
    /// as the arg. Documented behavior: tail content past `reload` is
    /// treated as a theme name (and will fail the `reload` sentinel
    /// match in the App handler, falling through to "unknown theme").
    /// Pinned here so a future refactor doesn't accidentally split on
    /// whitespace and pick up a multi-word arg.
    #[test]
    fn slash_theme_reload_with_extra_arg_keeps_full_tail() {
        let registry = build_registry_for_test();
        let decision = route("/theme reload extra-arg", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ThemeCommand { arg },
            } => assert_eq!(arg, "reload extra-arg"),
            other => panic!(
                "expected ThemeCommand {{ arg: \"reload extra-arg\" }}, got {:?}",
                other
            ),
        }
    }

    /// Wave B.4: caps = None (resumed sessions before M9 wires
    /// LoadSessionResponse) preserve the existing decision shape.
    #[test]
    fn slash_model_with_no_caps_routes_through_existing_path() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "codex",
            vec![CommandEntry {
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
            }],
        );

        let decision = route_with_caps("/model gpt-4o", &[], &[], &registry, false, None);
        match decision {
            SubmitDecision::SetSessionConfigOption { config_id, value } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-4o");
            }
            other => {
                panic!("expected SetSessionConfigOption (None caps preserves shape), got {other:?}")
            }
        }
    }
}
