use agent_client_protocol::schema::v1::ToolCall;
use serde_json::Value;

use super::{ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

/// Refine the base `ToolFamily` for Kiro agents.
///
/// Kiro uses `--trust-all-tools` so no mode-badge is needed.  The only
/// refinement performed here is promoting `mcp__*`-titled calls to `Mcp`;
/// all other calls keep their protocol-given base family.
pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    if title.starts_with("mcp__") {
        return ToolFamily::Mcp;
    }
    base
}

/// Kiro does not expose a known input shape at this scope — return `None`
/// and let the generic fallback handle it.
pub fn try_format_input(_raw: &Value) -> Option<ToolInputDisplay> {
    None
}

/// Kiro does not expose a known output shape at this scope — return `None`
/// and let the generic fallback handle it.
pub fn try_extract_observe(_raw: &Value) -> Option<ObservePayload> {
    None
}

/// Kiro uses `--trust-all-tools`; no mode-badge concept applies.
pub fn mode_badge(_mode_id: &str) -> Option<ModeBadge> {
    None
}

/// Kiro `_meta` extractor stub.
/// TODO(vendor-onboarding): replace with real extractor when kiro emits
/// recognizable `_meta.kiro.*` fields. See
/// docs/spur/acp-meta-conventions.md.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}
