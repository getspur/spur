use spur_acp::types::AgentKind;
use spur_acp::{AgentConfig, ProfileConfig, ProfileStrategy, SelectStrategy};

#[test]
fn defaults_encode_the_probe_matrix() {
    use AgentKind::*;

    let cases = [
        (
            ClaudeCodeAcp,
            SelectStrategy::ConfigOption { id: "agent".into() },
            true,
        ),
        (
            OpenCode,
            SelectStrategy::ConfigOption { id: "mode".into() },
            true,
        ),
        (
            Kiro,
            SelectStrategy::SpawnArg {
                flag: "--agent".into(),
            },
            true,
        ),
        (CodexAcp, SelectStrategy::None, true),
        (Kimi, SelectStrategy::None, false),
        (Gemini, SelectStrategy::None, false),
        (Generic, SelectStrategy::None, false),
        (ClaudeStreamJson, SelectStrategy::None, true),
    ];

    for (kind, select, materialize) in cases {
        let strategy = ProfileStrategy::for_kind(kind);
        assert_eq!(strategy.select, select, "select mismatch for {kind:?}");
        assert_eq!(
            strategy.materialize, materialize,
            "materialize mismatch for {kind:?}"
        );
    }
}

#[test]
fn config_block_overrides_kind_default() {
    let toml = r#"select = "config_option:agent""#;
    let cfg: ProfileConfig = toml::from_str(toml).unwrap();

    let strategy = ProfileStrategy::resolve(AgentKind::CodexAcp, Some(&cfg));

    assert_eq!(
        strategy.select,
        SelectStrategy::ConfigOption { id: "agent".into() }
    );
    assert!(strategy.materialize);
}

#[test]
fn config_block_supports_all_select_strings_and_materialize_override() {
    let cfg = ProfileConfig {
        select: Some("session_mode".into()),
        materialize: Some(false),
    };
    let strategy = ProfileStrategy::resolve(AgentKind::ClaudeCodeAcp, Some(&cfg));
    assert_eq!(strategy.select, SelectStrategy::SessionMode);
    assert!(!strategy.materialize);

    let cfg = ProfileConfig {
        select: Some("spawn_arg:--agent".into()),
        materialize: Some(true),
    };
    let strategy = ProfileStrategy::resolve(AgentKind::Generic, Some(&cfg));
    assert_eq!(
        strategy.select,
        SelectStrategy::SpawnArg {
            flag: "--agent".into()
        }
    );
    assert!(strategy.materialize);

    let cfg = ProfileConfig {
        select: Some("none".into()),
        materialize: Some(true),
    };
    let strategy = ProfileStrategy::resolve(AgentKind::Kimi, Some(&cfg));
    assert_eq!(strategy.select, SelectStrategy::None);
    assert!(strategy.materialize);
}

#[test]
fn agent_config_profile_block_round_trips_through_toml() {
    let toml_src = r#"
name = "codex"
command = "codex"
transport = "acp"
kind = "codex-acp"

[profile]
select = "config_option:agent"
materialize = false
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    let profile = cfg.profile.as_ref().expect("profile block");
    assert_eq!(profile.select.as_deref(), Some("config_option:agent"));
    assert_eq!(profile.materialize, Some(false));

    let encoded = toml::to_string(&cfg).expect("serialize");
    let decoded: AgentConfig = toml::from_str(&encoded).expect("deserialize");
    let profile = decoded.profile.as_ref().expect("profile block");
    assert_eq!(profile.select.as_deref(), Some("config_option:agent"));
    assert_eq!(profile.materialize, Some(false));
}
