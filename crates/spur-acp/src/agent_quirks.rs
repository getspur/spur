//! Per-agent quirk allow/deny tables.
//!
//! Keep transport-aware policy here instead of scattering agent-name checks
//! through protocol or TUI modules.

use crate::types::AgentKind;

/// Whether this agent kind is expected to emit `SessionUpdate::UsageUpdate`.
#[must_use]
pub fn usage_emit_default(kind: AgentKind) -> bool {
    !matches!(kind, AgentKind::ClaudeCodeAcp)
}

#[cfg(test)]
mod tests {
    use super::usage_emit_default;
    use crate::types::AgentKind;

    #[test]
    fn usage_emit_default_table() {
        let cases = [
            (AgentKind::ClaudeStreamJson, true),
            (AgentKind::ClaudeCodeAcp, false),
            (AgentKind::CodexAcp, true),
            (AgentKind::Kiro, true),
            (AgentKind::Kimi, true),
            (AgentKind::Generic, true),
        ];

        for (kind, expected) in cases {
            assert_eq!(
                usage_emit_default(kind),
                expected,
                "usage_emit_default({kind:?})"
            );
        }
    }
}
