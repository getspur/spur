//! Built-in delegation descriptors.
//!
//! Loaded once from bundled `defaults.toml`. See design spec section A
//! and `docs/spur/contributing-agent-defaults.md`.

use crate::config::{AgentConfig, DelegationDescriptor, Tier};
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

/// Merge built-in descriptor into an `AgentConfig`'s delegation field.
/// Per-field override semantics: user values win; missing fields and
/// empty vecs inherit from the default. When `inherit_defaults = false`,
/// user values are used verbatim (no merge).
///
/// Idempotent.
pub fn apply_builtin_defaults(cfg: &mut AgentConfig) {
    if !cfg.delegation.inherit_defaults {
        return;
    }
    match builtin_descriptor(&cfg.name) {
        Some(default) => {
            let user = &mut cfg.delegation;
            if user.description.is_none() { user.description = default.description; }
            if user.tier.is_none() { user.tier = default.tier; }
            if user.good_for.is_empty() { user.good_for = default.good_for; }
            if user.avoid_for.is_empty() { user.avoid_for = default.avoid_for; }
            if user.strengths.is_empty() { user.strengths = default.strengths; }
            if user.limitations.is_empty() { user.limitations = default.limitations; }
            if user.input_expectations.is_none() {
                user.input_expectations = default.input_expectations;
            }
            if user.output_shape.is_none() {
                user.output_shape = default.output_shape;
            }
            tracing::info!(agent = %cfg.name, "applied built-in delegation descriptor");
        }
        None => {
            // No built-in default. Thin-synthesize only if user config
            // is fully empty — otherwise leave user's partial config
            // alone.
            let user = &cfg.delegation;
            let is_empty = user.description.is_none()
                && user.tier.is_none()
                && user.good_for.is_empty()
                && user.avoid_for.is_empty();
            if is_empty {
                cfg.delegation.description = Some(
                    format!("{} agent (no descriptor configured)", cfg.name)
                );
                cfg.delegation.tier = Some(Tier::Generalist);
                tracing::info!(agent = %cfg.name, "synthesized thin delegation descriptor");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentConfig, Tier};

    fn minimal_agent(name: &str) -> AgentConfig {
        // Constructs a minimum-shape AgentConfig; relies on serde for
        // default values we don't care about here.
        let toml = format!(r#"name = "{}"
command = "x"
transport = "acp""#, name);
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn merge_fills_in_missing_from_default() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_some());
        assert!(!cfg.delegation.good_for.is_empty());
        assert!(cfg.delegation.tier.is_some());
    }

    #[test]
    fn merge_preserves_user_overrides() {
        let mut cfg = minimal_agent("claude-code-acp");
        cfg.delegation.description = Some("MY OVERRIDE".into());
        cfg.delegation.good_for = vec!["custom".into()];
        apply_builtin_defaults(&mut cfg);
        assert_eq!(cfg.delegation.description.as_deref(), Some("MY OVERRIDE"));
        assert_eq!(cfg.delegation.good_for, vec!["custom".to_string()]);
    }

    #[test]
    fn merge_empty_vec_treated_as_inherit() {
        let mut cfg = minimal_agent("claude-code-acp");
        // good_for starts empty by default; should get populated
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.good_for.len() >= 3);
    }

    #[test]
    fn merge_inherit_defaults_false_keeps_empty() {
        let mut cfg = minimal_agent("claude-code-acp");
        cfg.delegation.inherit_defaults = false;
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_none());
        assert!(cfg.delegation.good_for.is_empty());
    }

    #[test]
    fn merge_unknown_agent_synthesizes_thin() {
        let mut cfg = minimal_agent("my-custom-agent");
        apply_builtin_defaults(&mut cfg);
        assert!(cfg.delegation.description.is_some());
        assert!(cfg.delegation.description.as_ref().unwrap().contains("my-custom-agent"));
        assert!(cfg.delegation.good_for.is_empty());
        assert!(matches!(cfg.delegation.tier, Some(Tier::Generalist)));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        let after_first = cfg.delegation.clone();
        apply_builtin_defaults(&mut cfg);
        assert_eq!(after_first.description, cfg.delegation.description);
        assert_eq!(after_first.good_for, cfg.delegation.good_for);
    }

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
