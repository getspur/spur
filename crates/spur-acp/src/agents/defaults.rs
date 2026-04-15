//! Built-in delegation descriptors.
//!
//! Loaded once from bundled `defaults.toml`. See design spec section A
//! and `docs/spur/contributing-agent-defaults.md`.

use crate::config::DelegationDescriptor;
use std::collections::HashMap;
use std::sync::OnceLock;

const DEFAULTS_TOML: &str = include_str!("defaults.toml");

static DEFAULTS: OnceLock<HashMap<String, DelegationDescriptor>> = OnceLock::new();

fn defaults() -> &'static HashMap<String, DelegationDescriptor> {
    DEFAULTS.get_or_init(|| {
        toml::from_str::<HashMap<String, DelegationDescriptor>>(DEFAULTS_TOML)
            .expect("bundled defaults.toml must parse")
    })
}

/// Look up the built-in descriptor for a known agent name.
/// Returns `None` for unknown agents. `claude-code` aliases to
/// `claude-code-acp` because the stream-json variant has the same
/// semantics for delegation.
pub fn builtin_descriptor(agent_name: &str) -> Option<DelegationDescriptor> {
    let key = match agent_name {
        "claude-code" => "claude-code-acp",
        other => other,
    };
    defaults().get(key).cloned()
}

/// Names of agents with built-in descriptors, for testing and
/// documentation generation.
pub fn known_agents() -> &'static [&'static str] {
    &["claude-code-acp", "claude-code", "kiro", "codex", "gemini"]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_defaults_toml_parses() {
        // Force initialization; panic if TOML is malformed.
        let _ = defaults();
    }

    #[test]
    fn every_known_agent_resolves_to_a_descriptor() {
        for name in known_agents() {
            let d = builtin_descriptor(name);
            assert!(d.is_some(), "no descriptor for known agent: {}", name);
            let d = d.unwrap();
            assert!(d.description.is_some(), "{}: missing description", name);
            assert!(d.tier.is_some(), "{}: missing tier", name);
            assert!(!d.good_for.is_empty(), "{}: empty good_for", name);
            assert!(d.output_shape.is_some(), "{}: missing output_shape", name);
        }
    }

    #[test]
    fn unknown_agent_returns_none() {
        assert!(builtin_descriptor("not-a-real-agent").is_none());
    }

    #[test]
    fn claude_code_aliases_to_claude_code_acp() {
        let a = builtin_descriptor("claude-code").unwrap();
        let b = builtin_descriptor("claude-code-acp").unwrap();
        assert_eq!(a.description, b.description);
    }
}
