use agent_client_protocol::schema::v1::ToolCall;
use serde_json::Value;

use super::{BadgeColor, ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

/// Refine the base `ToolFamily` for Kiro agents.
///
/// Promotes `mcp__*`-titled calls to `Mcp`; all other calls keep their
/// protocol-given base family.
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

/// Map Kiro ACP mode IDs (agents) to short badges.
///
/// Live modes from `kiro-cli acp` probe (2.12.1): `kiro_default`,
/// `kiro_planner`, `kiro_guide`. Custom agent mode ids return `None`.
///
/// | mode_id          | badge | color   |
/// |------------------|-------|---------|
/// | `"kiro_default"` | DEF   | Neutral |
/// | `"kiro_planner"` | PLAN  | Amber   |
/// | `"kiro_guide"`   | GUIDE | Green   |
/// | anything else    | —     | (none)  |
pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
    match mode_id {
        "kiro_default" => Some(ModeBadge {
            short: "DEF",
            color: BadgeColor::Neutral,
        }),
        "kiro_planner" => Some(ModeBadge {
            short: "PLAN",
            color: BadgeColor::Amber,
        }),
        "kiro_guide" => Some(ModeBadge {
            short: "GUIDE",
            color: BadgeColor::Green,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod mode_badge_tests {
    use super::*;

    #[test]
    fn maps_live_kiro_modes() {
        let cases = [
            ("kiro_default", "DEF", BadgeColor::Neutral),
            ("kiro_planner", "PLAN", BadgeColor::Amber),
            ("kiro_guide", "GUIDE", BadgeColor::Green),
        ];
        for (id, short, color) in cases {
            let badge = mode_badge(id).unwrap_or_else(|| panic!("expected badge for {id}"));
            assert_eq!(badge.short, short, "mode_id={id}");
            assert_eq!(badge.color, color, "mode_id={id}");
        }
    }

    #[test]
    fn unknown_custom_agent_mode_returns_none() {
        assert!(mode_badge("custom-agent").is_none());
        assert!(mode_badge("spur-probe").is_none());
        assert!(mode_badge("").is_none());
    }
}

/// Kiro `_meta` extractor stub.
/// TODO(vendor-onboarding): replace with real extractor when kiro emits
/// recognizable `_meta.kiro.*` fields. See
/// docs/spur/acp-meta-conventions.md.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}
