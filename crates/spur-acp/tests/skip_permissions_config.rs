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
    assert_eq!(
        cfg.skip_permissions_args,
        vec!["--trust-all-tools".to_string()]
    );
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
        additional_directories: vec![],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        profile: None,
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: Some("bypassPermissions".into()),
        delegation: Default::default(),
    };
    let encoded = toml::to_string(&original).expect("serialize");
    let decoded: AgentConfig = toml::from_str(&encoded).expect("deserialize");
    assert!(decoded.skip_permissions);
    assert_eq!(
        decoded.skip_permissions_args,
        vec!["--trust-all-tools".to_string()]
    );
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
        additional_directories: vec![],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        profile: None,
        skip_permissions: false,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
        delegation: Default::default(),
    };
    assert_eq!(cfg.effective_args(), vec!["acp".to_string()]);
}

#[test]
fn effective_args_appends_skip_args_when_enabled() {
    let cfg = AgentConfig {
        name: "kiro".into(),
        command: "kiro-cli".into(),
        args: vec!["acp".into()],
        additional_directories: vec![],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        profile: None,
        skip_permissions: true,
        skip_permissions_args: vec!["--trust-all-tools".into()],
        skip_permissions_session_mode: None,
        delegation: Default::default(),
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
        args: vec![
            "--yes".into(),
            "@agentclientprotocol/claude-agent-acp@0.26.0".into(),
        ],
        additional_directories: vec![],
        transport: spur_acp::types::TransportKind::Acp,
        kind: spur_acp::types::AgentKind::Generic,
        role: spur_acp::types::AgentRole::Both,
        capabilities: vec![],
        cost_tier: spur_acp::types::CostTier::Medium,
        rate_limit_window: None,
        review: Default::default(),
        display: Default::default(),
        commands: Default::default(),
        permissions: Default::default(),
        profile: None,
        skip_permissions: true,
        skip_permissions_args: vec![],
        skip_permissions_session_mode: Some("bypassPermissions".into()),
        delegation: Default::default(),
    };
    assert_eq!(
        cfg.effective_args(),
        vec![
            "--yes".to_string(),
            "@agentclientprotocol/claude-agent-acp@0.26.0".to_string()
        ]
    );
}

#[test]
fn flat_and_nested_permissions_yield_equivalent_effective() {
    let flat_toml = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
skip_permissions = true
skip_permissions_args = ["--trust-all-tools"]
skip_permissions_session_mode = "bypassPermissions"
"#;
    let nested_toml = r#"
name = "kiro"
command = "kiro-cli"
args = ["acp"]
transport = "acp"

[permissions]
skip = true
args = ["--trust-all-tools"]
session_mode = "bypassPermissions"
"#;
    let flat: AgentConfig = toml::from_str(flat_toml).expect("flat");
    let nested: AgentConfig = toml::from_str(nested_toml).expect("nested");

    let flat_eff = flat.effective_permissions();
    let nested_eff = nested.effective_permissions();

    assert_eq!(flat_eff.skip, nested_eff.skip);
    assert_eq!(flat_eff.args, nested_eff.args);
    assert_eq!(flat_eff.session_mode, nested_eff.session_mode);

    // effective_args must behave the same too.
    assert_eq!(flat.effective_args(), nested.effective_args());
}

#[test]
fn nested_permissions_wins_when_both_present() {
    // Top-level flat fields are legacy; if a user has written a nested
    // [permissions] block, that is the source of truth.
    let toml_src = r#"
name = "mixed"
command = "x"
transport = "acp"
skip_permissions = false
skip_permissions_args = ["--ignored-flat"]

[permissions]
skip = true
args = ["--wins"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    let eff = cfg.effective_permissions();
    assert!(eff.skip);
    assert_eq!(eff.args, vec!["--wins".to_string()]);
}

#[test]
fn flat_only_config_without_nested_block_promotes_correctly() {
    // The legacy user path: existing .spur/config.toml with flat
    // skip_permissions_* fields, no [permissions] block. Must promote
    // to nested via effective_permissions without requiring a config
    // rewrite.
    let toml_src = r#"
name = "kiro-legacy"
command = "kiro-cli"
args = ["acp"]
transport = "acp"
skip_permissions = true
skip_permissions_args = ["--trust-all-tools"]
skip_permissions_session_mode = "bypassPermissions"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");

    // Nested block is at its default (absent in TOML).
    assert!(!cfg.permissions.skip);
    assert!(cfg.permissions.args.is_empty());
    assert!(cfg.permissions.session_mode.is_none());

    // But effective_permissions promotes the flat fields.
    let eff = cfg.effective_permissions();
    assert!(eff.skip);
    assert_eq!(eff.args, vec!["--trust-all-tools".to_string()]);
    assert_eq!(eff.session_mode.as_deref(), Some("bypassPermissions"));

    // effective_args appends the bypass args.
    assert_eq!(
        cfg.effective_args(),
        vec!["acp".to_string(), "--trust-all-tools".to_string()]
    );
}
