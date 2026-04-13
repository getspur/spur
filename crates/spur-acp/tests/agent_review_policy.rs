use spur_acp::config::AgentConfig;
use spur_acp::TimeoutFallback;
use std::time::Duration;

#[test]
fn review_defaults_when_section_absent() {
    let toml_src = r#"
name = "codex"
command = "codex"
transport = "stdio"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert_eq!(cfg.review.review_required, false);
    assert_eq!(cfg.review.review_timeout, Duration::from_secs(30 * 60));
    assert_eq!(cfg.review.max_review_retries, 3);
    assert_eq!(
        cfg.review.review_timeout_default,
        TimeoutFallback::Reject {
            reason: "review timeout".into()
        }
    );
}

#[test]
fn review_reads_explicit_values() {
    let toml_src = r#"
name = "codex"
command = "codex"
transport = "stdio"

[review]
review_required = true
review_timeout_secs = 60
max_review_retries = 5
review_timeout_default = "Approve"
"#;
    let cfg: AgentConfig = toml::from_str(toml_src).expect("parse");
    assert!(cfg.review.review_required);
    assert_eq!(cfg.review.review_timeout, Duration::from_secs(60));
    assert_eq!(cfg.review.max_review_retries, 5);
    assert_eq!(cfg.review.review_timeout_default, TimeoutFallback::Approve);
}
