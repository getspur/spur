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
use spur_acp::capability_evidence::DispatchRoute;
use spur_acp::{
    ContentBlock, EmbeddedResource, EmbeddedResourceResource, ResourceLink, SpurAgentCaps,
    TextContent, TextResourceContents,
};

use crate::action::Action;
use crate::components::input_bar::{ImageAttachment, ProtectedRange, RangeKind};
use crate::components::query_source::RetrievalAccept;
use crate::mentions::code_graph::expansion::{expand, ExpandedMention, PER_PROMPT_CAP_BYTES};
use spur_graph::{CodeMentionKind, CodeMentionPayload};

use super::advertised::pinned_route_for_command;
use super::entry::{CommandSource, Dispatch};
use super::registry::CommandRegistry;

/// What the controller should do with an Enter-submitted InputBar.
#[derive(Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "transient UI action/payload enums; instances are short-lived and never stored in bulk, boxing would churn every construction site"
)]
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
        command_name: String,
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
    /// Grok-only reasoning effort selected from its proprietary catalog.
    SetSessionEffort {
        value: String,
    },
    /// ACP session mode selected from the active agent's advertised catalog.
    SetSessionMode {
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

    // /brain [<name>] → Scope A hot-swap; preserve trailing arg.
    if text == "/brain" {
        return SubmitDecision::Local {
            action: Action::BrainCommand { arg: String::new() },
        };
    }
    if let Some(rest) = text.strip_prefix("/brain ") {
        return SubmitDecision::Local {
            action: Action::BrainCommand {
                arg: rest.trim().to_string(),
            },
        };
    }
    if text == "/brains" {
        return SubmitDecision::Local {
            action: Action::ListBrains,
        };
    }

    if text == "/notebook" {
        return SubmitDecision::Local {
            action: Action::NotebookCommand { arg: String::new() },
        };
    }
    if let Some(rest) = text.strip_prefix("/notebook ") {
        return SubmitDecision::Local {
            action: Action::NotebookCommand {
                arg: rest.trim().to_string(),
            },
        };
    }

    if text == "/configure" {
        return SubmitDecision::Local {
            action: Action::NavigateTo(crate::action::ViewId::AgentConfigBrowser {
                preselect: None,
            }),
        };
    }
    if let Some(rest) = text.strip_prefix("/configure ") {
        let preselect = rest.trim();
        return SubmitDecision::Local {
            action: Action::NavigateTo(crate::action::ViewId::AgentConfigBrowser {
                preselect: (!preselect.is_empty()).then(|| preselect.to_string()),
            }),
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
            let command_name = entry.name;
            // Snapshot the immutable evidence epoch and selected route before
            // producing a decision. The returned decision is irreversible: a
            // later capability refresh can only affect a later invocation.
            let pinned_route = match &entry.source {
                CommandSource::Agent { .. } | CommandSource::Advertised { .. } => caps
                    .and_then(|caps| pinned_route_for_command(caps, &command_name))
                    .map(|pinned| (pinned.evidence_epoch, pinned.route)),
                CommandSource::Spur => None,
            };
            if let Some((_evidence_epoch, route)) = pinned_route {
                match route {
                    DispatchRoute::Hidden => return SubmitDecision::Empty,
                    DispatchRoute::PromptOnly => {
                        let normalized = match &entry.dispatch {
                            Dispatch::PromptText { normalized } => normalized.clone(),
                            _ => format!("/{command_name}"),
                        };
                        let rest = rest_after_first_token(text);
                        let normalized_full = if rest.is_empty() {
                            normalized
                        } else {
                            format!("{normalized} {rest}")
                        };
                        return SubmitDecision::Send {
                            blocks: vec![ContentBlock::Text(TextContent::new(normalized_full))],
                            interrupt,
                        };
                    }
                    DispatchRoute::NativePreferred
                        if matches!(&entry.dispatch, Dispatch::PromptText { .. }) =>
                    {
                        return SubmitDecision::Empty;
                    }
                    DispatchRoute::NativePreferred => {}
                }
            }
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
                    } else if pinned_route.is_none()
                        && config_id == "model"
                        && caps.is_some_and(|c| c.supports_set_model())
                    {
                        // Wave B.4 / spec §6.3: prefer the dedicated
                        // `session/set_model` dispatch. Fallback when
                        // `set_model` is unavailable stays via the
                        // existing SetSessionConfigOption path —
                        // NativeAcpConnection::set_session_model also
                        // applies its own state-gated fallback for
                        // calls that flow through it directly.
                        SubmitDecision::SetSessionModel { value }
                    } else {
                        SubmitDecision::SetSessionConfigOption {
                            command_name,
                            config_id,
                            value,
                        }
                    }
                }
                Dispatch::SetSessionModel => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() {
                        SubmitDecision::Empty
                    } else {
                        SubmitDecision::SetSessionModel { value }
                    }
                }
                Dispatch::SetSessionEffort => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() {
                        SubmitDecision::Empty
                    } else {
                        SubmitDecision::SetSessionEffort { value }
                    }
                }
                Dispatch::SetSessionMode => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() || !is_advertised_mode(caps, &value) {
                        SubmitDecision::Empty
                    } else {
                        SubmitDecision::SetSessionMode { value }
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

    let blocks =
        assemble_blocks_with_prompt_caps(text, ranges, images, PromptBlockCaps::from_agent(caps));
    SubmitDecision::Send { blocks, interrupt }
}

fn is_advertised_mode(caps: Option<&SpurAgentCaps>, value: &str) -> bool {
    caps.and_then(|caps| caps.modes.as_ref())
        .is_some_and(|modes| {
            modes
                .available_modes
                .iter()
                .any(|mode| mode.id.0.as_ref() == value)
        })
}

/// Prompt-type gates from the agent's advertised `promptCapabilities`.
/// Omitted / unknown caps are treated as unsupported (ACP initialize).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptBlockCaps {
    pub image: bool,
    pub embedded_context: bool,
}

impl PromptBlockCaps {
    #[must_use]
    pub fn from_agent(caps: Option<&SpurAgentCaps>) -> Self {
        let Some(caps) = caps else {
            return Self::default();
        };
        Self {
            image: caps.agent.prompt_capabilities.image,
            embedded_context: caps.agent.prompt_capabilities.embedded_context,
        }
    }
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
/// Uses protocol-safe defaults (no image, no embed) when caps are unknown.
pub fn assemble_blocks(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
) -> Vec<ContentBlock> {
    assemble_blocks_with_prompt_caps(text, ranges, images, PromptBlockCaps::default())
}

pub fn assemble_blocks_with_prompt_caps(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    caps: PromptBlockCaps,
) -> Vec<ContentBlock> {
    assemble_blocks_inner(text, ranges, images, None, caps)
}

const CODE_SYMBOL_TOPOLOGY_HINT: &str =
    "\ntopology_available_via_mcp_for_above_symbols: pass each MENTION's qualified_name OR path:line to code_callers / code_callees / code_subgraph(radius=1); use code_resolve for ambiguous names";

struct CodeExpansion {
    text: String,
    is_symbol_body: bool,
}

type CodeExpansionLookup<'a> = Option<&'a mut dyn FnMut(&str) -> Option<CodeExpansion>>;

pub fn assemble_blocks_with_code_mentions<'a>(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    worktree_root: &Path,
    lookup_code_payload: impl FnMut(&str) -> Option<&'a CodeMentionPayload>,
) -> Vec<ContentBlock> {
    assemble_blocks_with_code_mentions_and_caps(
        text,
        ranges,
        images,
        worktree_root,
        lookup_code_payload,
        PromptBlockCaps::default(),
    )
}

pub fn assemble_blocks_with_code_mentions_and_caps<'a>(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    worktree_root: &Path,
    mut lookup_code_payload: impl FnMut(&str) -> Option<&'a CodeMentionPayload>,
    caps: PromptBlockCaps,
) -> Vec<ContentBlock> {
    let mut lookup = |uri: &str| {
        lookup_code_payload(uri).map(|payload| {
            let is_symbol = matches!(payload.authoritative.kind, CodeMentionKind::Symbol);
            match expand(payload, worktree_root) {
                ExpandedMention::Body { text } => CodeExpansion {
                    text,
                    is_symbol_body: is_symbol,
                },
                ExpandedMention::Warning { text, .. } => CodeExpansion {
                    text,
                    is_symbol_body: false,
                },
            }
        })
    };
    assemble_blocks_inner(text, ranges, images, Some(&mut lookup), caps)
}

fn assemble_blocks_inner(
    text: &str,
    ranges: &[ProtectedRange],
    images: &[ImageAttachment],
    mut code_expansion_lookup: CodeExpansionLookup<'_>,
    caps: PromptBlockCaps,
) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    let mut code_expansion_bytes = 0usize;
    let mut expanded_symbol_body = false;
    for r in ranges {
        if r.start > cursor {
            out.push(ContentBlock::Text(TextContent::new(
                text[cursor..r.start].to_string(),
            )));
        }
        match &r.kind {
            RangeKind::ImageRef(id) => {
                if !caps.image {
                    out.push(ContentBlock::Text(TextContent::new(r.name.clone())));
                } else {
                    match images.iter().find(|att| att.id == *id) {
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
                    }
                }
            }
            _ => {
                if r.uri.starts_with("graph://") {
                    if let Some(expansion) = code_expansion_lookup
                        .as_deref_mut()
                        .and_then(|lookup| lookup(&r.uri))
                    {
                        if code_expansion_bytes + expansion.text.len() > PER_PROMPT_CAP_BYTES {
                            out.push(ContentBlock::Text(TextContent::new(format!(
                                "MENTION_OMITTED {} (per-prompt cap)\n",
                                r.uri
                            ))));
                        } else {
                            code_expansion_bytes += expansion.text.len();
                            expanded_symbol_body |= expansion.is_symbol_body;
                            out.push(ContentBlock::Text(TextContent::new(expansion.text)));
                        }
                    } else {
                        out.push(ContentBlock::Text(TextContent::new(format!(
                            "MENTION_WARNING {}\nintended_uri:   {}\nfailure_reason: payload_not_in_registry\nreplaced_with:  dropped\n",
                            r.name, r.uri
                        ))));
                    }
                } else {
                    out.push(mention_content_block(r, caps));
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
    if expanded_symbol_body {
        out.push(ContentBlock::Text(TextContent::new(
            CODE_SYMBOL_TOPOLOGY_HINT.to_string(),
        )));
    }
    out
}

fn mention_content_block(range: &ProtectedRange, caps: PromptBlockCaps) -> ContentBlock {
    if caps.embedded_context {
        if let Some(block) = try_embed_file_mention(range) {
            return block;
        }
    }
    ContentBlock::ResourceLink(ResourceLink::new(range.name.clone(), range.uri.clone()))
}

fn try_embed_file_mention(range: &ProtectedRange) -> Option<ContentBlock> {
    let path = range.uri.strip_prefix("file://")?;
    let path = Path::new(path);
    if !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > PER_PROMPT_CAP_BYTES {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(ContentBlock::Resource(EmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(
            TextResourceContents::new(text, range.uri.clone())
                .mime_type(Some(file_mention_mime(path).to_string())),
        ),
    )))
}

fn file_mention_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => "text/x-rust",
        Some("py") => "text/x-python",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("toml") => "text/x-toml",
        Some("ts" | "tsx" | "js" | "jsx") => "text/javascript",
        _ => "text/plain",
    }
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
    Ok(ContentBlock::Image(spur_acp::ImageContent::new(
        data,
        "image/png",
    )))
}

/// Agent-facing worker/datasource framing. Local echo and history restore
/// must not replay this as user-typed text — the mention already follows
/// as a `ResourceLink` / `Resource`.
const UI_HINT_PREFIX: &str = "[UI hint]";

/// Flatten one outbound prompt block into the user-visible composer/trace
/// form. Returns `None` for agent-only framing and for variants with no
/// mention display (image/audio).
pub fn flatten_prompt_block(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) if t.text.starts_with(UI_HINT_PREFIX) => None,
        ContentBlock::Text(t) => Some(t.text.clone()),
        ContentBlock::ResourceLink(r) => Some(format!("@{}", r.name)),
        ContentBlock::Resource(r) => Some(format!("@{}", resource_display_name(r))),
        _ => None,
    }
}

fn resource_display_name(resource: &spur_acp::EmbeddedResource) -> String {
    use spur_acp::EmbeddedResourceResource;
    let uri = match &resource.resource {
        EmbeddedResourceResource::TextResourceContents(t) => t.uri.as_str(),
        _ => return "resource".to_string(),
    };
    mention_name_from_uri(uri)
}

pub(crate) fn mention_name_from_uri(uri: &str) -> String {
    let path = uri
        .strip_prefix("file://")
        .or_else(|| uri.split_once("://").map(|(_, rest)| rest))
        .unwrap_or(uri);
    path.rsplit(|c| c == '/' || c == '\\')
        .find(|seg| !seg.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Flatten blocks into a human-readable string for the local trace echo.
///
/// `Text` blocks concatenate their text except `[UI hint]` framing;
/// `ResourceLink` / `Resource` blocks render as `@<name>`; other
/// variants are skipped.
pub fn blocks_preview(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        if let Some(piece) = flatten_prompt_block(b) {
            s.push_str(&piece);
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

        let blocks = assemble_blocks_with_prompt_caps(
            &text,
            &ranges,
            &images,
            PromptBlockCaps {
                image: true,
                embedded_context: false,
            },
        );

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
                assert!(
                    !blocks.iter().any(|b| matches!(b, ContentBlock::Image(_))),
                    "omitted image cap must not send Image, got {blocks:?}"
                );
                assert!(
                    blocks
                        .iter()
                        .any(|b| matches!(b, ContentBlock::Text(t) if t.text == label)),
                    "expected placeholder text for omitted image, got {blocks:?}"
                );
            }
            other => panic!("expected Send, got {other:?}"),
        }
    }

    fn spur_caps_with_prompt(image: bool, embedded_context: bool) -> SpurAgentCaps {
        let mut agent = spur_acp::AgentCapabilities::default();
        agent.prompt_capabilities.image = image;
        agent.prompt_capabilities.embedded_context = embedded_context;
        SpurAgentCaps {
            agent,
            modes: None,
            config_options: Vec::new(),
            agent_kind: spur_acp::AgentKind::Generic,
            grok_display: None,
            kiro_display: None,
            capability_evidence: None,
        }
    }

    #[test]
    fn route_with_caps_emits_image_when_agent_advertises_image() {
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
        let caps = spur_caps_with_prompt(true, false);

        let decision = route_with_caps(&text, &ranges, &images, &registry, false, Some(&caps));

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
    fn assemble_embeds_file_mention_when_embedded_context_advertised() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "fn hello() {}\n").unwrap();
        let uri = format!("file://{}", tmp.path().display());
        let atom = format!("@{}", tmp.path().file_name().unwrap().to_string_lossy());
        let text = format!("review {atom} now");
        let start = "review ".len();
        let ranges = vec![ProtectedRange {
            start,
            end: start + atom.len(),
            kind: RangeKind::Atom,
            uri: uri.clone(),
            name: tmp.path().file_name().unwrap().to_string_lossy().into(),
        }];

        let blocks = assemble_blocks_with_prompt_caps(
            &text,
            &ranges,
            &[],
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
    fn assemble_keeps_resource_link_when_embedded_context_off() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "fn hello() {}\n").unwrap();
        let uri = format!("file://{}", tmp.path().display());
        let atom = "@lib.rs";
        let text = format!("review {atom} now");
        let start = "review ".len();
        let ranges = vec![ProtectedRange {
            start,
            end: start + atom.len(),
            kind: RangeKind::Atom,
            uri: uri.clone(),
            name: "lib.rs".into(),
        }];

        let blocks =
            assemble_blocks_with_prompt_caps(&text, &ranges, &[], PromptBlockCaps::default());

        assert!(
            blocks.iter().any(|b| matches!(
                b,
                ContentBlock::ResourceLink(r) if r.uri == uri && r.name == "lib.rs"
            )),
            "expected ResourceLink fallback, got {blocks:?}"
        );
        assert!(
            !blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Resource(_))),
            "must not embed without embeddedContext, got {blocks:?}"
        );
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
mod brain_slash_tests {
    use super::*;
    use crate::commands::registry::CommandRegistry;

    #[test]
    fn slash_brain_bare_routes_to_brain_command_empty_arg() {
        let registry = CommandRegistry::from_configs(&[]);
        let decision = route("/brain", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::BrainCommand { arg },
            } if arg.is_empty() => {}
            other => panic!("expected BrainCommand empty arg, got {other:?}"),
        }
    }

    #[test]
    fn slash_brain_named_preserves_arg() {
        let registry = CommandRegistry::from_configs(&[]);
        let decision = route("/brain opencode", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::BrainCommand { arg },
            } if arg == "opencode" => {}
            other => panic!("expected BrainCommand opencode, got {other:?}"),
        }
    }

    #[test]
    fn slash_brains_routes_to_list_brains() {
        let registry = CommandRegistry::from_configs(&[]);
        let decision = route("/brains", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::ListBrains,
            } => {}
            other => panic!("expected ListBrains, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod sessions_slash_tests {
    use super::*;
    use crate::commands::registry::CommandRegistry;
    use spur_acp::capability_evidence::{
        CapabilityChoice, CapabilityKey, CapabilityKind, CliIdentity, EvidenceClaim, EvidenceEpoch,
        EvidenceEpochId, EvidenceProvenance, EvidenceRecord, EvidenceSessionScope, ObservationTime,
        RawEvidenceDigest,
    };
    use spur_acp::spur_agent_caps::CapabilityEvidenceSnapshot;

    fn build_registry_for_test() -> CommandRegistry {
        CommandRegistry::new()
    }

    fn model_route_caps(
        epoch_id: u64,
        claim: EvidenceClaim,
        provenance: EvidenceProvenance,
    ) -> SpurAgentCaps {
        use spur_acp::{AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion};

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
            claim,
            provenance,
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

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut caps = SpurAgentCaps::new(
            &init,
            &NewSessionResponse::new(spur_acp::AcpSessionId::new("sid")),
            AgentKind::Generic,
        );
        caps.capability_evidence = Some(
            serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"),
        );
        caps
    }

    fn prompt_only_command_caps(epoch_id: u64, command_name: &str) -> SpurAgentCaps {
        use spur_acp::{AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion};

        let identity = CliIdentity {
            resolved_executable: std::path::PathBuf::from("/usr/bin/kiro-cli"),
            upstream_version: Some("1.0.0".to_owned()),
            argv_fingerprint: "argv".to_owned(),
            environment_fingerprint: "env".to_owned(),
        };
        let record = EvidenceRecord {
            key: CapabilityKey {
                kind: CapabilityKind::Command,
                upstream_id: "commands".to_owned(),
            },
            claim: EvidenceClaim::CandidateObserved,
            provenance: EvidenceProvenance::PromptFallback,
            identity: identity.clone(),
            observed_at: ObservationTime(epoch_id),
            raw_digest: RawEvidenceDigest(format!("sha256:command:{epoch_id}")),
            session_scope: EvidenceSessionScope::Session("sid".to_owned()),
            choices: vec![CapabilityChoice {
                id: command_name.to_owned(),
                label: format!("/{command_name}"),
                description: None,
            }],
        };
        let epoch = EvidenceEpoch::new(EvidenceEpochId(epoch_id), identity.clone(), vec![record])
            .expect("test evidence must use one identity");
        let snapshot = CapabilityEvidenceSnapshot::from_epoch(epoch, &identity);
        let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
        wire["completeness"] = serde_json::json!("complete");

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut caps = SpurAgentCaps::new(
            &init,
            &NewSessionResponse::new(spur_acp::AcpSessionId::new("sid")),
            AgentKind::Kiro,
        );
        caps.capability_evidence = Some(
            serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"),
        );
        caps
    }

    fn registry_from_reduced_caps(caps: &SpurAgentCaps) -> CommandRegistry {
        let mut registry = CommandRegistry::new();
        registry.set_agent_commands(
            "agent",
            vec![crate::commands::entry::CommandEntry {
                name: "model".to_owned(),
                description: "Agent prompt model".to_owned(),
                hint: None,
                source: crate::commands::entry::CommandSource::Agent {
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
            crate::commands::advertised::AdvertisedSource::entries_from_caps("agent", caps),
        );
        registry
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
            SubmitDecision::SetSessionConfigOption {
                config_id, value, ..
            } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-5-codex");
            }
            other => panic!("expected SetSessionConfigOption, got {:?}", other),
        }
    }

    #[test]
    fn grok_effort_dispatch_routes_to_set_session_effort() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};

        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "grok",
            vec![CommandEntry {
                name: "effort".into(),
                description: "Switch effort".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "grok".into(),
                },
                dispatch: Dispatch::SetSessionEffort,
                arg_picker_spec: None,
            }],
        );

        let decision = route("/effort low", &[], &[], &registry, false);
        assert!(matches!(
            decision,
            SubmitDecision::SetSessionEffort { value } if value == "low"
        ));
    }

    fn registry_and_caps_with_agent_modes() -> (CommandRegistry, SpurAgentCaps) {
        use spur_acp::{
            AgentKind, InitializeResponse, NewSessionResponse, ProtocolVersion, SessionMode,
            SessionModeId, SessionModeState,
        };

        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.modes = Some(SessionModeState::new(
            SessionModeId::new("read-only"),
            vec![
                SessionMode::new(SessionModeId::new("read-only"), "Ask for approval"),
                SessionMode::new(SessionModeId::new("agent"), "Agent"),
            ],
        ));
        let mut caps = SpurAgentCaps::new(&init, &new, AgentKind::CodexAcp);
        let identity = CliIdentity {
            resolved_executable: std::path::PathBuf::from("/usr/bin/test-acp"),
            upstream_version: Some("1.0.0".to_owned()),
            argv_fingerprint: "argv".to_owned(),
            environment_fingerprint: "env".to_owned(),
        };
        let record = EvidenceRecord {
            key: CapabilityKey {
                kind: CapabilityKind::Mode,
                upstream_id: "mode".to_owned(),
            },
            claim: EvidenceClaim::NativeVerified,
            provenance: EvidenceProvenance::StandardAdvertisement,
            identity: identity.clone(),
            observed_at: ObservationTime(41),
            raw_digest: RawEvidenceDigest("sha256:mode:41".to_owned()),
            session_scope: EvidenceSessionScope::Session("sid".to_owned()),
            choices: vec![
                CapabilityChoice {
                    id: "read-only".to_owned(),
                    label: "Ask for approval".to_owned(),
                    description: None,
                },
                CapabilityChoice {
                    id: "agent".to_owned(),
                    label: "Agent".to_owned(),
                    description: None,
                },
            ],
        };
        let epoch = EvidenceEpoch::new(EvidenceEpochId(41), identity.clone(), vec![record])
            .expect("mode evidence must use one identity");
        let snapshot = CapabilityEvidenceSnapshot::from_epoch(epoch, &identity);
        let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
        wire["completeness"] = serde_json::json!("complete");
        caps.capability_evidence = Some(
            serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"),
        );
        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "codex",
            crate::commands::advertised::AdvertisedSource::entries_from_caps("codex", &caps),
        );
        (registry, caps)
    }

    #[test]
    fn slash_mode_routes_an_advertised_agent_mode() {
        let (registry, caps) = registry_and_caps_with_agent_modes();
        let decision = route_with_caps("/mode agent", &[], &[], &registry, false, Some(&caps));
        assert!(matches!(
            decision,
            SubmitDecision::SetSessionMode { value } if value == "agent"
        ));
    }

    #[test]
    fn slash_mode_rejects_an_unadvertised_mode() {
        let (registry, caps) = registry_and_caps_with_agent_modes();
        let decision = route_with_caps(
            "/mode bypassPermissions",
            &[],
            &[],
            &registry,
            false,
            Some(&caps),
        );
        assert!(matches!(decision, SubmitDecision::Empty));
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
                use spur_acp::ContentBlock;
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
        use spur_acp::{
            InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
            SessionConfigOption, SessionConfigSelectOption,
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

        // Build caps that advertise a non-empty model config option.
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
        new.config_options = Some(vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "gpt-5-codex",
            vec![SessionConfigSelectOption::new("gpt-5-codex", "GPT-5 Codex")],
        )]);
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
        use spur_acp::{
            InitializeResponse, NewSessionResponse, ProtocolVersion, SessionConfigId,
            SessionConfigKind, SessionConfigOption, SessionConfigSelect,
            SessionConfigSelectOptions, SessionConfigValueId,
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
        let mut new = NewSessionResponse::new(spur_acp::AcpSessionId::new("sid"));
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
            SubmitDecision::SetSessionConfigOption {
                config_id, value, ..
            } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-4o");
            }
            other => panic!("expected SetSessionConfigOption fallback, got {other:?}"),
        }
    }

    #[test]
    fn prompt_only_model_dispatches_prompt_route_once() {
        let caps = model_route_caps(
            21,
            EvidenceClaim::CandidateObserved,
            EvidenceProvenance::PromptFallback,
        );
        let mut registry = CommandRegistry::new();
        registry.set_advertised_commands(
            "agent",
            vec![crate::commands::entry::CommandEntry {
                name: "model".to_owned(),
                description: "Synthetic native model".to_owned(),
                hint: None,
                source: crate::commands::entry::CommandSource::Advertised {
                    handle: "agent".to_owned(),
                },
                dispatch: Dispatch::SetSessionModel,
                arg_picker_spec: None,
            }],
        );

        let decision =
            route_with_caps("/model test-model", &[], &[], &registry, false, Some(&caps));

        let SubmitDecision::Send { blocks, .. } = decision else {
            panic!("PromptOnly must dispatch one prompt route");
        };
        assert_eq!(blocks.len(), 1);
        assert!(matches!(
            &blocks[0],
            ContentBlock::Text(text) if text.text == "/model test-model"
        ));
    }

    #[test]
    fn review_prompt_only_agent_clear_does_not_hijack_spur_clear() {
        let caps = prompt_only_command_caps(22, "clear");
        let mut registry = CommandRegistry::new();
        registry.set_agent_commands(
            "kiro",
            vec![crate::commands::entry::CommandEntry {
                name: "clear".to_owned(),
                description: "Kiro prompt clear".to_owned(),
                hint: None,
                source: crate::commands::entry::CommandSource::Agent {
                    handle: "kiro".to_owned(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/clear".to_owned(),
                },
                arg_picker_spec: None,
            }],
        );
        registry.set_advertised_commands(
            "kiro",
            crate::commands::advertised::AdvertisedSource::entries_from_caps("kiro", &caps),
        );

        let resolved = registry
            .resolve("/clear")
            .expect("SPUR /clear must resolve");
        assert!(matches!(
            resolved.source,
            crate::commands::entry::CommandSource::Spur
        ));

        let decision = route_with_caps("/clear", &[], &[], &registry, false, Some(&caps));
        assert!(matches!(
            decision,
            SubmitDecision::Local {
                action: Action::ClearSession
            }
        ));
    }

    #[test]
    fn evidence_refresh_changes_only_the_next_model_action_route() {
        let native_caps = model_route_caps(
            31,
            EvidenceClaim::NativeVerified,
            EvidenceProvenance::AcceptedActiveProbe,
        );
        let native_registry = registry_from_reduced_caps(&native_caps);

        let in_flight = route_with_caps(
            "/model test-model",
            &[],
            &[],
            &native_registry,
            false,
            Some(&native_caps),
        );

        let prompt_caps = model_route_caps(
            32,
            EvidenceClaim::NativeFailed,
            EvidenceProvenance::NativeDispatch,
        );
        let mut prompt_caps = prompt_caps;
        let identity = prompt_caps
            .capability_evidence
            .as_ref()
            .expect("snapshot")
            .epoch()
            .identity()
            .clone();
        let mut records = prompt_caps
            .capability_evidence
            .as_ref()
            .expect("snapshot")
            .epoch()
            .records()
            .to_vec();
        records.push(EvidenceRecord {
            key: CapabilityKey {
                kind: CapabilityKind::Model,
                upstream_id: "model".to_owned(),
            },
            claim: EvidenceClaim::CandidateObserved,
            provenance: EvidenceProvenance::PromptFallback,
            identity: identity.clone(),
            observed_at: ObservationTime(32),
            raw_digest: RawEvidenceDigest("sha256:model:prompt".to_owned()),
            session_scope: EvidenceSessionScope::Session("sid".to_owned()),
            choices: vec![CapabilityChoice {
                id: "test-model".to_owned(),
                label: "Test Model".to_owned(),
                description: None,
            }],
        });
        let refreshed = EvidenceEpoch::new(EvidenceEpochId(32), identity.clone(), records)
            .expect("refresh evidence must use one identity");
        let snapshot = CapabilityEvidenceSnapshot::from_epoch(refreshed, &identity);
        let mut wire = serde_json::to_value(snapshot).expect("snapshot must serialize");
        wire["completeness"] = serde_json::json!("complete");
        prompt_caps.capability_evidence = Some(
            serde_json::from_value(wire).expect("complete evidence snapshot must deserialize"),
        );
        let prompt_registry = registry_from_reduced_caps(&prompt_caps);
        let subsequent = route_with_caps(
            "/model test-model",
            &[],
            &[],
            &prompt_registry,
            false,
            Some(&prompt_caps),
        );

        assert!(matches!(in_flight, SubmitDecision::SetSessionModel { .. }));
        assert!(matches!(subsequent, SubmitDecision::Send { .. }));
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

    #[test]
    fn slash_notebook_with_path_routes_to_notebook_command_with_arg() {
        let registry = build_registry_for_test();
        let decision = route("/notebook foo.ipynb", &[], &[], &registry, false);
        match decision {
            SubmitDecision::Local {
                action: Action::NotebookCommand { arg },
            } => assert_eq!(arg, "foo.ipynb"),
            other => panic!(
                "expected NotebookCommand {{ arg: \"foo.ipynb\" }}, got {:?}",
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
            SubmitDecision::SetSessionConfigOption {
                config_id, value, ..
            } => {
                assert_eq!(config_id, "model");
                assert_eq!(value, "gpt-4o");
            }
            other => {
                panic!("expected SetSessionConfigOption (None caps preserves shape), got {other:?}")
            }
        }
    }
}
