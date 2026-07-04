//! Startup validation for AgentConfig. Strongly-typed deserialize already
//! rejects unknown enum variants — this validator handles rules that
//! require cross-field knowledge.
//!
//! Rules (Spec 1):
//!   R1 (FATAL):  dispatch = "vendor_exec" requires exec_method to be set.
//!   R3 (WARN):   permissions.skip = true with no explicit mechanism
//!                (args empty AND session_mode absent) will rely solely
//!                on L2 auto-approve; flag so users notice.
//!
//! R2 (hook-ID registry lookup) is covered by serde enum parsing and is
//! intentionally out of scope for Spec 1 — see the roadmap for when/if
//! to add it.

use super::hooks::DispatchKind;
use super::AgentConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    VendorExecMissingMethod { agent: String },
    SkipPermissionsNoExplicitMechanism { agent: String, note: String },
}

impl ConfigError {
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::VendorExecMissingMethod { .. })
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VendorExecMissingMethod { agent } => write!(
                f,
                "{agent}: dispatch = \"vendor_exec\" requires [agents.entries.commands] exec_method"
            ),
            Self::SkipPermissionsNoExplicitMechanism { agent, note } => {
                write!(
                    f,
                    "{agent}: permissions.skip = true with no explicit mechanism — {note}"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Validate a single AgentConfig. Returns `Ok(())` on success, or a
/// Vec<ConfigError> containing all problems found (may mix fatal + warn).
/// Callers inspect `ConfigError::is_fatal` to decide whether to refuse
/// to start the agent.
pub fn validate_agent_config(cfg: &AgentConfig) -> Result<(), Vec<ConfigError>> {
    let mut errors = Vec::new();

    // R1: vendor_exec dispatch requires exec_method.
    if matches!(cfg.commands.dispatch, DispatchKind::VendorExec)
        && cfg.commands.exec_method.is_none()
    {
        errors.push(ConfigError::VendorExecMissingMethod {
            agent: cfg.name.clone(),
        });
    }

    // R3: skip_permissions with no mechanism → WARN.
    let perms = cfg.effective_permissions();
    if perms.skip && perms.args.is_empty() && perms.session_mode.is_none() {
        errors.push(ConfigError::SkipPermissionsNoExplicitMechanism {
            agent: cfg.name.clone(),
            note: "relying on L2 auto-approve only; consider setting permissions.args or permissions.session_mode".into(),
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::entries::PermissionsConfig;

    fn base_cfg(name: &str) -> AgentConfig {
        AgentConfig {
            name: name.into(),
            command: "x".into(),
            args: vec![],
            additional_directories: vec![],
            transport: crate::types::TransportKind::Acp,
            kind: crate::types::AgentKind::Generic,
            role: crate::types::AgentRole::Both,
            capabilities: vec![],
            cost_tier: crate::types::CostTier::Medium,
            rate_limit_window: None,
            review: Default::default(),
            display: Default::default(),
            commands: Default::default(),
            permissions: Default::default(),
            profile: None,
            skip_permissions: false,
            skip_permissions_args: vec![],
            skip_permissions_session_mode: None,
            delegation: Default::default(),
        }
    }

    #[test]
    fn r1_vendor_exec_without_exec_method_is_fatal() {
        let mut cfg = base_cfg("kiro");
        cfg.commands.dispatch = DispatchKind::VendorExec;
        cfg.commands.exec_method = None;
        let err = validate_agent_config(&cfg).expect_err("should error");
        assert_eq!(err.len(), 1);
        assert!(err[0].is_fatal());
        assert!(matches!(
            err[0],
            ConfigError::VendorExecMissingMethod { .. }
        ));
    }

    #[test]
    fn r1_vendor_exec_with_exec_method_passes() {
        let mut cfg = base_cfg("kiro");
        cfg.commands.dispatch = DispatchKind::VendorExec;
        cfg.commands.exec_method = Some("_kiro.dev/commands/execute".into());
        validate_agent_config(&cfg).expect("should pass");
    }

    #[test]
    fn r3_skip_without_mechanism_is_warning() {
        let mut cfg = base_cfg("bogus");
        cfg.permissions = PermissionsConfig {
            skip: true,
            args: vec![],
            session_mode: None,
        };
        let err = validate_agent_config(&cfg).expect_err("should warn");
        assert_eq!(err.len(), 1);
        assert!(!err[0].is_fatal(), "R3 must be warning, not fatal");
        assert!(matches!(
            err[0],
            ConfigError::SkipPermissionsNoExplicitMechanism { .. }
        ));
    }

    #[test]
    fn r3_skip_with_session_mode_passes() {
        let mut cfg = base_cfg("claude");
        cfg.permissions = PermissionsConfig {
            skip: true,
            args: vec![],
            session_mode: Some("bypassPermissions".into()),
        };
        validate_agent_config(&cfg).expect("should pass");
    }

    #[test]
    fn r3_skip_via_legacy_flat_fields_also_counts_as_mechanism() {
        // effective_permissions merges flat into nested; if user has flat
        // skip_permissions_session_mode set, R3 should not warn.
        let mut cfg = base_cfg("claude-legacy");
        cfg.skip_permissions = true;
        cfg.skip_permissions_session_mode = Some("bypassPermissions".into());
        validate_agent_config(&cfg).expect("should pass via legacy flat");
    }
}
