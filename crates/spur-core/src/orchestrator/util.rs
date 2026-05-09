use spur_acp::config::SpurConfig;
use spur_acp::{SessionId, SpurEventBody};

use super::types::ReconnectError;

pub(super) fn format_error_chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

pub(super) fn reconnect_failure_event(
    session: SessionId,
    brain_name: String,
    error: ReconnectError,
) -> SpurEventBody {
    match error {
        ReconnectError::AlreadyAttached { acp_id, holder } => {
            SpurEventBody::SessionAttachRejected {
                acp_session_id: acp_id,
                holder,
                fs_unsafe: false,
            }
        }
        ReconnectError::Other(e) => SpurEventBody::BrainReconnectFailed {
            session,
            brain_name,
            reason: format_error_chain(&e),
        },
    }
}

// ─── Agent name normalization ─────────────────────────────────────────

/// Normalize an agent name for equality comparison.
/// - Lowercases
/// - Trims surrounding whitespace
/// - Strips `-acp`, `_acp`, `-cli`, `_cli` suffixes
///
/// Used to compare `DelegationPlan.chosen` (possibly a short name
/// the brain chose) against the dispatched `agent` (possibly a
/// fully-qualified registered name like `claude-code-acp`).
pub fn normalize_agent_name(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    for suffix in ["-acp", "_acp", "-cli", "_cli"].iter() {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    lower
}

/// Detect whether an error from an `AgentConnection` RPC indicates the
/// underlying subprocess has died (pipe closed, ACP thread exited, etc.),
/// versus a normal request-level error (auth needed, invalid session, etc.).
///
/// Pragmatic string-match against the two known "subprocess is gone"
/// patterns emitted by `NativeAcpConnection` and the ACP SDK. A more
/// structured signal would require a new trait method on `AgentConnection`;
/// revisit if the set of transports grows.
pub(crate) fn is_connection_death(err: &anyhow::Error) -> bool {
    let msg = err.to_string();
    msg.contains("ACP thread died") || msg.contains("server shut down unexpectedly")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BeadsStartupWarning {
    BrNotInstalled,
    BackendUnavailable,
}

pub(super) fn startup_beads_warning(
    config: &SpurConfig,
    feature_gate: Option<&spur_license::FeatureGate>,
    has_beads_dir: bool,
    pm_service_available: bool,
    br_binary_available: bool,
) -> Option<BeadsStartupWarning> {
    if !(has_beads_dir
        && !pm_service_available
        && config.pm.beads.as_ref().is_none_or(|beads| beads.enabled)
        && feature_gate.is_some_and(|gate| gate.has(spur_license::FeatureKey::PM_CORE_BEADS_BASIC)))
    {
        return None;
    }

    Some(if br_binary_available {
        BeadsStartupWarning::BackendUnavailable
    } else {
        BeadsStartupWarning::BrNotInstalled
    })
}

pub(super) fn render_beads_startup_warning(warning: BeadsStartupWarning) -> &'static str {
    match warning {
        BeadsStartupWarning::BrNotInstalled => {
            "br (beads) not installed — issue tracking disabled. Install: cargo install --git https://github.com/Dicklesworthstone/beads_rust.git"
        }
        BeadsStartupWarning::BackendUnavailable => {
            "beads PM backend failed to initialize — issue tracking disabled. `br` appears installed; check logs for the underlying startup error."
        }
    }
}

pub(super) fn binary_on_path(binary: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };

    #[cfg(windows)]
    let path_exts: Vec<String> = std::env::var_os("PATHEXT")
        .map(|exts| {
            exts.to_string_lossy()
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| ext.to_string())
                .collect()
        })
        .unwrap_or_else(|| vec![".EXE".into(), ".CMD".into(), ".BAT".into(), ".COM".into()]);

    std::env::split_paths(&path_var).any(|dir| {
        if dir.join(binary).is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            path_exts
                .iter()
                .any(|ext| dir.join(format!("{binary}{ext}")).is_file())
        }

        #[cfg(not(windows))]
        {
            false
        }
    })
}

// ─── Free function: log-cap enforcer ──────────────────────────────────────────

pub(super) fn enforce_log_cap(dir: &std::path::Path, cap: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.path(), m.modified().ok()?, m.len()))
        })
        .collect();
    let total: u64 = files.iter().map(|(_, _, s)| s).sum();
    if total <= cap {
        return;
    }
    files.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
    let mut to_free = total - cap;
    for (path, _, size) in files {
        if to_free == 0 {
            break;
        }
        let _ = std::fs::remove_file(&path);
        to_free = to_free.saturating_sub(size);
    }
}

/// Map a transport kind to its `CancelMode`. Single source of truth used
/// by `AgentSessionReady` emitters so the TUI can render transport-aware
/// cancel feedback without re-inspecting `AgentConfig`.
pub(crate) fn cancel_mode_for(transport: spur_acp::types::TransportKind) -> spur_acp::CancelMode {
    use spur_acp::types::TransportKind;
    match transport {
        TransportKind::Acp => spur_acp::CancelMode::AcpSoft,
        TransportKind::Stdio | TransportKind::CliWrap | TransportKind::StreamJson => {
            spur_acp::CancelMode::ProcessKill
        }
    }
}

/// Arm the 5-second force-end deadline used by the streaming `select!`.
/// Factored out so both the `Message { interrupt: true }` arm and the
/// new `CancelStream` arm set the deadline identically and so it is
/// directly unit-testable without a full mock orchestrator.
pub(crate) fn arm_cancel_deadline(deadline: &mut Option<tokio::time::Instant>) {
    *deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
}

/// Expand ~ to home directory.
pub(super) fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs_home() {
            return format!("{}/{}", home, rest);
        }
    }
    path.to_string()
}

fn dirs_home() -> Option<String> {
    directories::BaseDirs::new().map(|d| d.home_dir().to_string_lossy().to_string())
}

#[cfg(test)]
mod cancel_mode_helper_tests {
    use super::cancel_mode_for;
    use spur_acp::{types::TransportKind, CancelMode};

    #[test]
    fn acp_transport_is_acp_soft() {
        assert_eq!(cancel_mode_for(TransportKind::Acp), CancelMode::AcpSoft);
    }

    #[test]
    fn subprocess_transports_are_process_kill() {
        assert_eq!(
            cancel_mode_for(TransportKind::Stdio),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::CliWrap),
            CancelMode::ProcessKill
        );
        assert_eq!(
            cancel_mode_for(TransportKind::StreamJson),
            CancelMode::ProcessKill
        );
    }
}

#[cfg(test)]
mod cancel_deadline_arm_tests {
    use super::arm_cancel_deadline;

    #[tokio::test]
    async fn arm_cancel_deadline_sets_5s_from_now() {
        let mut deadline = None;
        let before = tokio::time::Instant::now();
        arm_cancel_deadline(&mut deadline);
        let set = deadline.expect("arm_cancel_deadline must populate Some(deadline)");
        let delta = set.saturating_duration_since(before);
        assert!(
            delta >= std::time::Duration::from_millis(4_900)
                && delta <= std::time::Duration::from_millis(5_100),
            "expected ~5s deadline, got {delta:?}"
        );
    }

    #[tokio::test]
    async fn arm_cancel_deadline_overwrites_existing() {
        let old = tokio::time::Instant::now() - std::time::Duration::from_secs(60);
        let mut deadline = Some(old);
        arm_cancel_deadline(&mut deadline);
        assert!(deadline.unwrap() > old + std::time::Duration::from_secs(1));
    }
}

#[cfg(test)]
mod is_connection_death_tests {
    use super::*;

    #[test]
    fn is_connection_death_detects_known_patterns() {
        let e1 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died during ext_method");
        assert!(is_connection_death(&e1));

        let e2 = anyhow::anyhow!("NativeAcpConnection 'kiro': ACP thread died");
        assert!(is_connection_death(&e2));

        let e3 = anyhow::anyhow!("Internal error: \"server shut down unexpectedly\"");
        assert!(is_connection_death(&e3));

        let e4 = anyhow::anyhow!("prompt rejected: invalid session id");
        assert!(!is_connection_death(&e4));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_agent_name;

    #[test]
    fn strips_acp_suffix() {
        assert_eq!(normalize_agent_name("claude-code-acp"), "claude-code");
        assert_eq!(normalize_agent_name("kiro-acp"), "kiro");
    }

    #[test]
    fn strips_cli_suffix() {
        assert_eq!(normalize_agent_name("gemini-cli"), "gemini");
    }

    #[test]
    fn lowercases() {
        assert_eq!(normalize_agent_name("CLAUDE"), "claude");
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(normalize_agent_name("  kiro  "), "kiro");
    }

    #[test]
    fn same_agent_matches_across_variants() {
        assert_eq!(
            normalize_agent_name("Claude-Code-ACP"),
            normalize_agent_name("claude-code")
        );
    }

    #[test]
    fn distinct_agents_do_not_collide() {
        assert_ne!(
            normalize_agent_name("our-claude"),
            normalize_agent_name("claude"),
        );
    }

    #[test]
    fn mismatch_detection_chosen_vs_dispatched_strings() {
        let dispatched = "kiro";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        // claude-code-acp normalizes to "claude-code", so "claude" != "claude-code"
        assert!(!matched);

        let dispatched = "claude-code-acp";
        let chosen = "claude-code-acp";
        let matched = normalize_agent_name(chosen) == normalize_agent_name(dispatched);
        assert!(matched);
    }
}

#[cfg(test)]
mod beads_startup_warning_tests {
    use super::{render_beads_startup_warning, startup_beads_warning, BeadsStartupWarning};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use spur_acp::config::{BeadsPmConfig, SpurConfig};
    use spur_license::policy::PolicyResolver;
    use spur_license::{EntitlementSnapshot, FeatureGate, LicenseState, Plan};

    fn community_gate() -> Arc<FeatureGate> {
        Arc::new(FeatureGate::new(PolicyResolver::embedded()))
    }

    fn gate_without_beads_basic() -> Arc<FeatureGate> {
        // Pro/Team/Enterprise inherit Community via the policy's
        // `@inherit:community` directive, so feeding an empty JWT no
        // longer strips pm_core_beads_basic. Inject a hand-crafted
        // empty snapshot to genuinely simulate the missing entitlement.
        let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
        gate.set_snapshot_for_test(EntitlementSnapshot::default());
        gate
    }

    fn beads_basic_gate() -> Arc<FeatureGate> {
        let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
        let mut features = BTreeSet::new();
        features.insert("pm_core_beads_basic".to_string());
        gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
        gate
    }

    #[test]
    fn beads_startup_warning_free_tier_with_missing_br_emits_install_hint() {
        let config = SpurConfig::default();
        let gate = community_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            Some(BeadsStartupWarning::BrNotInstalled)
        );
    }

    #[test]
    fn beads_startup_warning_missing_beads_basic_entitlement_suppresses_warning() {
        let config = SpurConfig::default();
        let gate = gate_without_beads_basic();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_entitled_tier_with_missing_br_emits_install_hint() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            Some(BeadsStartupWarning::BrNotInstalled)
        );
        assert!(
            render_beads_startup_warning(BeadsStartupWarning::BrNotInstalled)
                .contains("br (beads) not installed"),
        );
    }

    #[test]
    fn beads_startup_warning_entitled_tier_with_present_br_uses_generic_backend_copy() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, true),
            Some(BeadsStartupWarning::BackendUnavailable)
        );
        let warning = render_beads_startup_warning(BeadsStartupWarning::BackendUnavailable);
        assert!(
            !warning.contains("not installed"),
            "generic warning must not claim br is missing: {warning}",
        );
        assert!(warning.contains("failed to initialize"), "got: {warning}");
    }

    #[test]
    fn beads_startup_warning_disabled_beads_config_suppresses_warning() {
        let mut config = SpurConfig::default();
        config.pm.beads = Some(BeadsPmConfig {
            enabled: false,
            auto_sync: false,
        });
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_missing_feature_gate_suppresses_warning() {
        let config = SpurConfig::default();

        assert_eq!(
            startup_beads_warning(&config, None, true, false, false),
            None
        );
    }

    #[test]
    fn beads_startup_warning_existing_pm_service_suppresses_warning() {
        let config = SpurConfig::default();
        let gate = beads_basic_gate();

        assert_eq!(
            startup_beads_warning(&config, Some(gate.as_ref()), true, true, false),
            None
        );
    }
}
