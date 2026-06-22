use super::build_worker_info;
use spur_acp::config::AgentConfig;

fn minimal_agent(name: &str) -> AgentConfig {
    let toml = format!(
        r#"name = "{}"
command = "x"
transport = "acp""#,
        name
    );
    toml::from_str(&toml).unwrap()
}

#[test]
fn build_worker_info_populates_all_fields() {
    let mut cfg = minimal_agent("claude-code-acp");
    spur_acp::agents::defaults::apply_builtin_defaults(&mut cfg);
    let info = build_worker_info(&cfg);
    assert_eq!(info.name, "claude-code-acp");
    assert!(info.description.is_some());
    assert!(info.tier.is_some());
    assert!(!info.good_for.is_empty());
    assert!(info.output_shape.is_some());
}

#[test]
fn build_worker_info_handles_empty_descriptor() {
    let cfg = minimal_agent("unknown-agent");
    // without apply_builtin_defaults, all fields stay empty
    let info = build_worker_info(&cfg);
    assert_eq!(info.name, "unknown-agent");
    assert!(info.description.is_none());
    assert!(info.good_for.is_empty());
}
