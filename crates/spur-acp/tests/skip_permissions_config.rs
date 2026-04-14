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
