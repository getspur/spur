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
/// Returns `None` for unknown agents. ACP-wrapped variants alias to the
/// same descriptor as their direct-binary equivalents when delegation
/// semantics match.
pub fn builtin_descriptor(agent_name: &str) -> Option<DelegationDescriptor> {
    let key = match agent_name {
        "claude-code" => "claude-code-acp",
        "codex-acp" => "codex",
        "codex-bin" => "codex",
        "gemini-acp" => "gemini",
        other => other,
    };
    defaults().get(key).cloned()
}

/// Names of agents with built-in descriptors, for testing and
/// documentation generation.
pub fn known_agents() -> &'static [&'static str] {
    &[
        "claude-code-acp",
        "claude-code",
        "kiro",
        "codex",
        "codex-acp",
        "codex-bin",
        "gemini",
        "gemini-acp",
        "opencode",
        "kimi",
    ]
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
            if user.description.is_none() {
                user.description = default.description;
            }
            if user.tier.is_none() {
                user.tier = default.tier;
            }
            if user.good_for.is_empty() {
                user.good_for = default.good_for;
            }
            if user.avoid_for.is_empty() {
                user.avoid_for = default.avoid_for;
            }
            if user.strengths.is_empty() {
                user.strengths = default.strengths;
            }
            if user.limitations.is_empty() {
                user.limitations = default.limitations;
            }
            if user.input_expectations.is_none() {
                user.input_expectations = default.input_expectations;
            }
            if user.output_shape.is_none() {
                user.output_shape = default.output_shape;
            }
            tracing::debug!(agent = %cfg.name, "applied built-in delegation descriptor");
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
                cfg.delegation.description =
                    Some(format!("{} agent (no descriptor configured)", cfg.name));
                cfg.delegation.tier = Some(Tier::Generalist);
                tracing::debug!(agent = %cfg.name, "synthesized thin delegation descriptor");
            }
        }
    }
}

/// Keyword table for lint #4 (capability/descriptor cross-check).
///
/// MAINTENANCE NOTE: when a new token is added to `AgentConfig::capabilities`,
/// add the corresponding trigger keywords here so the lint flags
/// good_for entries that reference the capability without declaring it.
const CAPABILITY_KEYWORDS: &[(&str, &[&str])] = &[
    ("plan_mode", &["plan mode", "plan-mode", "planning"]),
    ("usage", &["usage tracking", "token counting"]),
    ("load_session", &["session resume", "load_session"]),
    ("list_sessions", &["list_sessions"]),
    ("session_resume", &["session_resume"]),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LintLevel {
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct LintMessage {
    pub level: LintLevel,
    pub agent: String,
    pub message: String,
}

/// Run all delegation-config lints over the given AgentConfigs.
/// Call AFTER `apply_builtin_defaults` so inherited values are visible.
/// All v1 lints emit `Warn` level; user sees them but startup continues.
pub fn validate_delegation_config(cfgs: &[AgentConfig]) -> Vec<LintMessage> {
    let mut msgs = Vec::new();
    for cfg in cfgs {
        lint_length(cfg, &mut msgs);
        lint_worker_without_description(cfg, &mut msgs);
        lint_worker_without_good_for(cfg, &mut msgs);
        lint_capability_mismatch(cfg, &mut msgs);
    }
    msgs
}

fn lint_length(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    for (i, entry) in cfg.delegation.good_for.iter().enumerate() {
        if entry.chars().count() > 80 {
            out.push(LintMessage {
                level: LintLevel::Warn,
                agent: cfg.name.clone(),
                message: format!(
                    "good_for[{}] exceeds 80 chars; use a short task pattern, not a sentence",
                    i
                ),
            });
        }
    }
    for (i, entry) in cfg.delegation.avoid_for.iter().enumerate() {
        if entry.chars().count() > 80 {
            out.push(LintMessage {
                level: LintLevel::Warn,
                agent: cfg.name.clone(),
                message: format!(
                    "avoid_for[{}] exceeds 80 chars; use a short task pattern, not a sentence",
                    i
                ),
            });
        }
    }
}

fn lint_worker_without_description(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    if cfg.role.is_worker_capable() && cfg.delegation.description.is_none() {
        out.push(LintMessage {
            level: LintLevel::Warn,
            agent: cfg.name.clone(),
            message: "worker-capable but has no delegation.description — routing will be weak"
                .into(),
        });
    }
}

fn lint_worker_without_good_for(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    if cfg.role.is_worker_capable() && cfg.delegation.good_for.is_empty() {
        out.push(LintMessage {
            level:   LintLevel::Warn,
            agent:   cfg.name.clone(),
            message: "worker-capable but no delegation.good_for entries — brain has no positive routing signal".into(),
        });
    }
}

fn lint_capability_mismatch(cfg: &AgentConfig, out: &mut Vec<LintMessage>) {
    let joined = cfg.delegation.good_for.join(" ").to_lowercase();
    for (token, keywords) in CAPABILITY_KEYWORDS {
        for kw in keywords.iter() {
            if joined.contains(&kw.to_lowercase()) && !cfg.capabilities.iter().any(|c| c == token) {
                out.push(LintMessage {
                    level: LintLevel::Warn,
                    agent: cfg.name.clone(),
                    message: format!(
                        "delegation.good_for references {} but capabilities does not declare {}",
                        kw, token
                    ),
                });
                break; // one message per token
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
        let toml = format!(
            r#"name = "{}"
command = "x"
transport = "acp""#,
            name
        );
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
    fn opencode_builtin_default_populates_good_for() {
        let mut cfg = minimal_agent("opencode");
        apply_builtin_defaults(&mut cfg);
        assert!(!cfg.delegation.good_for.is_empty());
    }

    #[test]
    fn kimi_builtin_default_populates_good_for() {
        let mut cfg = minimal_agent("kimi");
        apply_builtin_defaults(&mut cfg);
        assert!(!cfg.delegation.good_for.is_empty());
    }

    #[test]
    fn codex_bin_alias_populates_good_for() {
        let mut cfg = minimal_agent("codex-bin");
        apply_builtin_defaults(&mut cfg);
        assert!(!cfg.delegation.good_for.is_empty());
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
        assert!(cfg
            .delegation
            .description
            .as_ref()
            .unwrap()
            .contains("my-custom-agent"));
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

    #[test]
    fn codex_acp_aliases_to_codex() {
        let a = builtin_descriptor("codex-acp").unwrap();
        let b = builtin_descriptor("codex").unwrap();
        assert_eq!(a.description, b.description);
    }

    #[test]
    fn gemini_acp_aliases_to_gemini() {
        let a = builtin_descriptor("gemini-acp").unwrap();
        let b = builtin_descriptor("gemini").unwrap();
        assert_eq!(a.description, b.description);
    }

    #[test]
    fn lint_flags_oversized_good_for_entry() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec![
            "a".repeat(90), // over 80 chars
            "ok short entry".into(),
        ];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("exceeds 80")));
    }

    #[test]
    fn lint_flags_oversized_avoid_for_entry() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.avoid_for = vec!["a".repeat(81)];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("avoid_for")));
    }

    #[test]
    fn lint_flags_worker_without_description() {
        // my-agent has no built-in default; no user description; worker role
        let mut cfg = minimal_agent("my-agent");
        // Note: default role is `Both` which is worker-capable
        cfg.delegation.description = None;
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("description")));
    }

    #[test]
    fn lint_flags_worker_without_good_for() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.description = Some("something".into());
        cfg.delegation.good_for = vec![];
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.iter().any(|m| m.message.contains("good_for")));
    }

    #[test]
    fn lint_flags_capability_mismatch() {
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec!["plan mode refactors".into()];
        // capabilities stays empty by default
        let msgs = validate_delegation_config(&[cfg]);
        assert!(
            msgs.iter().any(|m| m.message.contains("plan_mode")),
            "expected plan_mode mismatch warning, got: {:?}",
            msgs.iter().map(|m| &m.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn lint_clean_config_produces_no_warnings() {
        let mut cfg = minimal_agent("claude-code-acp");
        apply_builtin_defaults(&mut cfg);
        let msgs = validate_delegation_config(&[cfg]);
        assert!(
            msgs.is_empty(),
            "expected no warnings, got: {:?}",
            msgs.iter().map(|m| &m.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn lint_counts_chars_not_bytes() {
        // Non-ASCII: each char is multi-byte but counts as 1 char.
        let mut cfg = minimal_agent("my-agent");
        cfg.delegation.good_for = vec!["日".repeat(50)]; // 50 chars, 150 bytes
        let msgs = validate_delegation_config(&[cfg]);
        // Should NOT flag — 50 chars is under 80.
        assert!(
            !msgs.iter().any(|m| m.message.contains("exceeds 80")),
            "should not flag 50-char entry even though it's 150 bytes"
        );
    }

    #[test]
    fn end_to_end_config_load_applies_defaults_and_lints() {
        use crate::agents::defaults::apply_builtin_defaults;
        let toml = r#"
            name = "claude-code-acp"
            command = "npx"
            args = ["--yes", "@agentclientprotocol/claude-agent-acp"]
            transport = "acp"
        "#;
        let mut cfg: AgentConfig = toml::from_str(toml).unwrap();
        apply_builtin_defaults(&mut cfg);
        // Descriptor filled from defaults.toml:
        assert!(cfg.delegation.description.is_some());
        assert!(!cfg.delegation.good_for.is_empty());
        // And the clean config should produce no lint warnings:
        let msgs = validate_delegation_config(&[cfg]);
        assert!(msgs.is_empty());
    }
}
