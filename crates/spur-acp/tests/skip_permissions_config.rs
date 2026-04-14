//! Serde round-trip tests for the three `skip_permissions*` fields on
//! `AgentConfig`. Guards against silent regressions in default values and
//! field names.

use spur_acp::config::AgentConfig;

#[test]
fn skip_permissions_defaults_when_absent() {
    let toml_src = r#"
name = "kiro"
command = "kiro-cli"
transport = "acp"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(!cfg.skip_permissions);
    assert!(cfg.skip_permissions_args.is_empty());
    assert!(cfg.skip_permissions_session_mode.is_none());
}

#[test]
fn skip_permissions_reads_explicit_values() {
    let toml_src = r#"
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
skip_permissions = true
skip_permissions_args = ["--trust-all-tools"]
skip_permissions_session_mode = "bypassPermissions"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.skip_permissions);
    assert_eq!(cfg.skip_permissions_args, vec!["--trust-all-tools".to_string()]);
    assert_eq!(
        cfg.skip_permissions_session_mode.as_deref(),
        Some("bypassPermissions")
    );
}

#[test]
fn skip_permissions_round_trips_through_toml() {
    let original = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: Some("bypassPermissions".into()),
    };
    let encoded = toml::to_string(&original).expect("serialize");
    let decoded: AgentConfig = toml::from_str(&encoded).expect("deserialize");
    assert!(decoded.skip_permissions);
    assert_eq!(decoded.skip_permissions_args, vec!["--trust-all-tools".to_string()]);
    assert_eq!(
        decoded.skip_permissions_session_mode.as_deref(),
        Some("bypassPermissions")
    );
}

#[test]
fn effective_args_returns_plain_args_when_disabled() {
    let cfg = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        skip_permissions: false,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
    };
    assert_eq!(cfg.effective_args(), vec!["acp".to_string()]);
}

#[test]
fn effective_args_appends_skip_args_when_enabled() {
    let cfg = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
    };
    assert_eq!(
        cfg.effective_args(),
        vec!["acp".to_string(), "--trust-all-tools".to_string()]
    );
}

#[test]
fn effective_args_returns_plain_args_when_enabled_but_no_skip_args() {
    // claude-code-acp case: skip_permissions = true, bypass via session
    // mode not spawn args. effective_args should be unchanged.
    let cfg = AgentConfig {
        name: "claude-code-acp".into(),
        command: "npx".into(),
        args: vec!["--yes".into(), "@agentclientprotocol/claude-agent-acp@0.26.0".into()],
        transport: spur_acp::types::TransportKind::Acp,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        skip_permissions: true,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: Some("bypassPermissions".into()),
    };
    assert_eq!(
        cfg.effective_args(),
        vec![
            "--yes".to_string(),
            "@agentclientprotocol/claude-agent-acp@0.26.0".to_string()
        ]
    );
}
