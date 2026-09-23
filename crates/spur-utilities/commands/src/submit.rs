//! Pure submit classification and prompt-block assembly shared by every
//! spur frontend (decoupling spec 2026-09-23 §3.4).
//!
//! [`classify`] reduces a submitted input line, the merged command
//! registry, and the agent's capability snapshot to a neutral
//! [`SubmitPlan`] — the same decision core the TUI submit router made,
//! minus the frontend-owned pieces (local action resolution, image
//! attachments, code-mention payload expansion), which each frontend
//! supplies through [`SpecialSpan`] resolution at its shell boundary.
//!
//! Mentions are described by the neutral [`MentionSpan`]; the TUI
//! converts its `ProtectedRange`s at the shell boundary. This crate must
//! not depend on `spur-mentions` (spec §2 rule `¬c_m`): everything
//! mention-shaped arrives as plain spans or frontend-resolved
//! [`SpecialSpan`]s.

use serde_json::Value;
use spur_acp::capability_evidence::DispatchRoute;
use spur_acp::{
    ContentBlock, EmbeddedResource, EmbeddedResourceResource, ResourceLink, SpurAgentCaps,
    TextContent, TextResourceContents,
};
use std::path::Path;

use crate::advertised::pinned_route_for_command;
use crate::entry::{CommandSource, Dispatch};
use crate::registry::CommandRegistry;

/// Agent-facing worker/datasource framing. Local echo and history restore
/// must not replay this as user-typed text — the mention already follows
/// as a `ResourceLink` / `Resource`.
const UI_HINT_PREFIX: &str = "[UI hint]";

/// Per-prompt byte budget for frontend-resolved code-mention expansions
/// and embedded file mentions.
///
/// Mirrors `spur_mentions::code::expansion::PER_PROMPT_CAP_BYTES`;
/// `spur-commands` must not depend on `spur-mentions` (spec §2 `¬c_m`),
/// so the constant is restated here and the two must be changed
/// together.
pub const PER_PROMPT_CAP_BYTES: usize = 32 * 1024;

/// Appended once per prompt when any code-mention symbol body was
/// expanded, pointing the agent at the MCP topology tools.
const CODE_SYMBOL_TOPOLOGY_HINT: &str =
    "\ntopology_available_via_mcp_for_above_symbols: pass each MENTION's qualified_name OR path:line to code_callers / code_callees / code_subgraph(radius=1); use code_resolve for ambiguous names";

/// Neutral mention span: byte range + display name + URI for one mention
/// inside the submitted text. Frontends convert their input-bar range
/// types into these at the shell boundary (the TUI's
/// `ProtectedRange → MentionSpan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionSpan {
    /// Byte offset of the first character of the mention atom.
    pub start: usize,
    /// Byte offset one past the last character of the mention atom.
    pub end: usize,
    /// Display name (what the user typed / sees, e.g. `lib.rs`).
    pub name: String,
    /// Mention URI (e.g. `file:///…`, `worker://…`, `graph://…`).
    pub uri: String,
}

impl MentionSpan {
    #[must_use]
    pub fn new(start: usize, end: usize, name: impl Into<String>, uri: impl Into<String>) -> Self {
        Self {
            start,
            end,
            name: name.into(),
            uri: uri.into(),
        }
    }
}

/// Frontend-resolved replacement content for one [`MentionSpan`] the
/// neutral core cannot produce itself (encoded images, code-mention
/// expansions, registry-miss warnings). Returned by the lookup hooks
/// passed to [`classify`] / [`assemble_blocks_with_special`].
#[derive(Debug, Clone)]
pub enum SpecialSpan {
    /// Code-mention expansion subject to the per-prompt byte budget
    /// ([`PER_PROMPT_CAP_BYTES`]); spans rejected by the budget render
    /// as `MENTION_OMITTED`. `is_symbol_body` marks symbol-body
    /// expansions, which append the MCP topology hint once per prompt.
    Expansion { text: String, is_symbol_body: bool },
    /// Uncapped literal replacement text (warnings, image-cap
    /// placeholders).
    Text(String),
    /// Fully built replacement blocks (e.g. an encoded image). Empty to
    /// drop the span entirely.
    Blocks(Vec<ContentBlock>),
}

/// Optional per-span [`SpecialSpan`] resolver. `None` → every span takes
/// the default treatment (file embed / `ResourceLink`).
pub type SpecialSpanLookup<'a> = Option<&'a mut dyn FnMut(&MentionSpan) -> Option<SpecialSpan>>;

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

/// Neutral routing decision for a submitted input line. Mirrors the TUI
/// `SubmitDecision` with [`SubmitPlan::Local`] carrying the neutral
/// `{name, arg}` pair — the frontend resolves the pair to its own action
/// type at the boundary (the TUI via `commands::spur_local::
/// local_dispatch`).
#[derive(Debug)]
pub enum SubmitPlan {
    Send {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    /// A frontend-owned meta command. `arg` is the whitespace-trimmed
    /// text after the command name (`None` when absent/empty).
    Local {
        name: String,
        arg: Option<String>,
    },
    /// Generic vendor-extension dispatch. Carries the full wire method
    /// and the rendered params payload.
    VendorExec {
        method: String,
        params: Value,
    },
    /// v1 codex `/model` and `/effort` slash pickers — typed wire
    /// dispatch to ACP `session/set_config_option`.
    SetSessionConfigOption {
        command_name: String,
        config_id: String,
        value: String,
    },
    /// Emitted when caps advertise the dedicated ACP `session/set_model`
    /// method (`SpurAgentCaps::supports_set_model()`).
    SetSessionModel {
        value: String,
    },
    /// Grok-only reasoning effort selected from its proprietary catalog.
    SetSessionEffort {
        value: String,
    },
    /// ACP session mode selected from the active agent's advertised
    /// catalog.
    SetSessionMode {
        value: String,
    },
    Empty,
}

/// Classify a submitted input line against the merged command registry
/// and agent caps (spec §3.4 decision core).
///
/// * empty text → [`SubmitPlan::Empty`]
/// * resolvable `/command` → the entry's dispatch decomposition
///   (`Dispatch::Local` → [`SubmitPlan::Local`] with the trimmed rest)
/// * unknown `/command` or plain text → [`SubmitPlan::Send`] with
///   blocks assembled from `text` + `spans` under
///   [`PromptBlockCaps::from_agent`]
///
/// `special` resolves spans the neutral core cannot build itself (see
/// [`SpecialSpan`]). Frontend-owned pre-registry interceptions (the
/// TUI's `/work`, `/theme <name>` … meta-command table) happen at the
/// shell boundary *before* calling this, so the frontend can fall
/// through on unresolved names.
pub fn classify(
    text: &str,
    spans: &[MentionSpan],
    registry: &CommandRegistry,
    interrupt: bool,
    caps: Option<&SpurAgentCaps>,
    special: SpecialSpanLookup<'_>,
) -> SubmitPlan {
    if text.is_empty() {
        return SubmitPlan::Empty;
    }

    if text.starts_with('/') {
        if let Some(entry) = registry.resolve(text) {
            let command_name = entry.name;
            // Snapshot the immutable evidence epoch and selected route
            // before producing a decision. The returned decision is
            // irreversible: a later capability refresh can only affect a
            // later invocation.
            let pinned_route = match &entry.source {
                CommandSource::Agent { .. } | CommandSource::Advertised { .. } => caps
                    .and_then(|caps| pinned_route_for_command(caps, &command_name))
                    .map(|pinned| (pinned.evidence_epoch, pinned.route)),
                CommandSource::Spur => None,
            };
            if let Some((_evidence_epoch, route)) = pinned_route {
                match route {
                    DispatchRoute::Hidden => return SubmitPlan::Empty,
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
                        return SubmitPlan::Send {
                            blocks: vec![ContentBlock::Text(TextContent::new(normalized_full))],
                            interrupt,
                        };
                    }
                    DispatchRoute::NativePreferred
                        if matches!(&entry.dispatch, Dispatch::PromptText { .. }) =>
                    {
                        return SubmitPlan::Empty;
                    }
                    DispatchRoute::NativePreferred => {}
                }
            }
            return match entry.dispatch {
                Dispatch::Local { name } => SubmitPlan::Local {
                    name,
                    arg: trimmed_arg(&rest_after_first_token(text)),
                },
                Dispatch::PromptText { normalized } => {
                    let rest = rest_after_first_token(text);
                    let normalized_full = if rest.is_empty() {
                        normalized
                    } else {
                        format!("{} {}", normalized, rest)
                    };
                    SubmitPlan::Send {
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
                        SubmitPlan::Empty
                    } else if pinned_route.is_none()
                        && config_id == "model"
                        && caps.is_some_and(|c| {
                            c.capability_evidence.is_none() && c.supports_set_model()
                        })
                    {
                        // Wave B.4 / spec §6.3: prefer the dedicated
                        // semantic model dispatch for legacy snapshots that
                        // predate capability evidence. Complete evidence uses
                        // the pinned route above; incomplete evidence keeps
                        // the standard SetSessionConfigOption entry
                        // fail-closed.
                        SubmitPlan::SetSessionModel { value }
                    } else {
                        SubmitPlan::SetSessionConfigOption {
                            command_name,
                            config_id,
                            value,
                        }
                    }
                }
                Dispatch::SetSessionModel => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() {
                        SubmitPlan::Empty
                    } else {
                        SubmitPlan::SetSessionModel { value }
                    }
                }
                Dispatch::SetSessionEffort => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() || !is_advertised_effort(caps, &value) {
                        SubmitPlan::Empty
                    } else {
                        SubmitPlan::SetSessionEffort { value }
                    }
                }
                Dispatch::SetSessionMode => {
                    let value = rest_after_first_token(text).trim().to_string();
                    if value.is_empty() || !is_advertised_mode(caps, &value) {
                        SubmitPlan::Empty
                    } else {
                        SubmitPlan::SetSessionMode { value }
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
                    SubmitPlan::VendorExec { method, params }
                }
            };
        }
        // Unknown /command — fall through to Send as plain text so the
        // agent receives it (agents often render unknown slash commands
        // verbatim as prompts).
    }

    let blocks =
        assemble_blocks_with_special(text, spans, PromptBlockCaps::from_agent(caps), special);
    SubmitPlan::Send { blocks, interrupt }
}

/// Walk `text` + sorted `spans` interleaved →
/// `[Text, ResourceLink/Resource, Text, …]` under protocol-safe default
/// caps (no image, no embed).
#[must_use]
pub fn assemble_blocks(
    text: &str,
    spans: &[MentionSpan],
    caps: PromptBlockCaps,
) -> Vec<ContentBlock> {
    assemble_blocks_with_special(text, spans, caps, None)
}

/// Block-assembly core (spec §3.4): text segmentation around mention
/// positions, file-mention embedding, and `ResourceLink` construction —
/// with frontend-resolved [`SpecialSpan`]s substituted where the hook
/// returns them.
#[must_use]
pub fn assemble_blocks_with_special(
    text: &str,
    spans: &[MentionSpan],
    caps: PromptBlockCaps,
    mut special: SpecialSpanLookup<'_>,
) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = Vec::new();
    let mut cursor = 0usize;
    let mut code_expansion_bytes = 0usize;
    let mut expanded_symbol_body = false;
    for span in spans {
        if span.start > cursor {
            out.push(ContentBlock::Text(TextContent::new(
                text[cursor..span.start].to_string(),
            )));
        }
        if let Some(resolved) = special.as_deref_mut().and_then(|lookup| lookup(span)) {
            match resolved {
                SpecialSpan::Expansion {
                    text: expansion_text,
                    is_symbol_body,
                } => {
                    if code_expansion_bytes + expansion_text.len() > PER_PROMPT_CAP_BYTES {
                        out.push(ContentBlock::Text(TextContent::new(format!(
                            "MENTION_OMITTED {} (per-prompt cap)\n",
                            span.uri
                        ))));
                    } else {
                        code_expansion_bytes += expansion_text.len();
                        expanded_symbol_body |= is_symbol_body;
                        out.push(ContentBlock::Text(TextContent::new(expansion_text)));
                    }
                }
                SpecialSpan::Text(replacement) => {
                    out.push(ContentBlock::Text(TextContent::new(replacement)));
                }
                SpecialSpan::Blocks(blocks) => out.extend(blocks),
            }
        } else if span.uri.starts_with("graph://") {
            // Code mention the frontend could not resolve (payload not in
            // the registry, or no code layer at all): fail loudly with the
            // protocol warning framing rather than silently degrading to
            // a link.
            out.push(ContentBlock::Text(TextContent::new(format!(
                "MENTION_WARNING {}\nintended_uri:   {}\nfailure_reason: payload_not_in_registry\nreplaced_with:  dropped\n",
                span.name, span.uri
            ))));
        } else {
            out.push(mention_content_block(span, caps));
        }
        cursor = span.end;
    }
    if cursor < text.len() {
        out.push(ContentBlock::Text(TextContent::new(
            text[cursor..].to_string(),
        )));
    }
    if out.is_empty() && spans.is_empty() {
        out.push(ContentBlock::Text(TextContent::new(text.to_string())));
    }
    if expanded_symbol_body {
        out.push(ContentBlock::Text(TextContent::new(
            CODE_SYMBOL_TOPOLOGY_HINT.to_string(),
        )));
    }
    out
}

fn mention_content_block(span: &MentionSpan, caps: PromptBlockCaps) -> ContentBlock {
    if caps.embedded_context {
        if let Some(block) = try_embed_file_mention(span) {
            return block;
        }
    }
    ContentBlock::ResourceLink(ResourceLink::new(span.name.clone(), span.uri.clone()))
}

fn try_embed_file_mention(span: &MentionSpan) -> Option<ContentBlock> {
    let path = span.uri.strip_prefix("file://")?;
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
            TextResourceContents::new(text, span.uri.clone())
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

fn is_advertised_effort(caps: Option<&SpurAgentCaps>, value: &str) -> bool {
    caps.and_then(|caps| caps.grok_display.as_ref())
        .and_then(|display| {
            display
                .model_id
                .as_deref()
                .map(|model_id| display.efforts_for_model(model_id))
        })
        .is_some_and(|efforts| efforts.iter().any(|effort| effort.id == value))
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

/// Everything after the first whitespace-delimited token of `text`
/// (leading whitespace trimmed only).
fn rest_after_first_token(text: &str) -> String {
    match text.split_once(char::is_whitespace) {
        Some((_, rest)) => rest.trim_start().to_string(),
        None => String::new(),
    }
}

/// `local_dispatch` arg semantics: whitespace-trimmed rest, `None` when
/// empty.
fn trimmed_arg(rest: &str) -> Option<String> {
    let trimmed = rest.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Flatten one outbound prompt block into the user-visible composer/trace
/// form. Returns `None` for agent-only framing and for variants with no
/// mention display (image/audio).
#[must_use]
pub fn flatten_prompt_block(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(t) if t.text.starts_with(UI_HINT_PREFIX) => None,
        ContentBlock::Text(t) => Some(t.text.clone()),
        ContentBlock::ResourceLink(r) => Some(format!("@{}", r.name)),
        ContentBlock::Resource(r) => Some(format!("@{}", resource_display_name(r))),
        _ => None,
    }
}

fn resource_display_name(resource: &EmbeddedResource) -> String {
    let uri = match &resource.resource {
        EmbeddedResourceResource::TextResourceContents(t) => t.uri.as_str(),
        _ => return "resource".to_string(),
    };
    mention_name_from_uri(uri)
}

/// Last path segment of a mention URI (display name for `@name`
/// flattening).
#[must_use]
pub fn mention_name_from_uri(uri: &str) -> String {
    let path = uri
        .strip_prefix("file://")
        .or_else(|| uri.split_once("://").map(|(_, rest)| rest))
        .unwrap_or(uri);
    path.rsplit(['/', '\\'])
        .find(|seg| !seg.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Flatten blocks into a human-readable string for the local trace echo.
///
/// `Text` blocks concatenate their text except `[UI hint]` framing;
/// `ResourceLink` / `Resource` blocks render as `@<name>`; other
/// variants are skipped.
#[must_use]
pub fn blocks_preview(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        if let Some(piece) = flatten_prompt_block(b) {
            s.push_str(&piece);
        }
    }
    s
}

/// Flatten blocks into a plain text string (e.g. for CLI that forwards
/// text). Currently identical to [`blocks_preview`] — kept as a distinct
/// entry point so future divergence (e.g. CLI-specific serialization) is
/// cheap.
#[must_use]
pub fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks_preview(blocks)
}
