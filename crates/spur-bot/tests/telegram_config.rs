use spur_acp::config::SpurConfig;

#[test]
fn parse_single_operator_telegram_config() {
    let cfg: SpurConfig = toml::from_str(
        r#"
[bot.telegram]
enabled = true
bot_token = "123:ABC"
operator_user_id = 424242
poll_timeout_secs = 30
draft_streaming = true
"#,
    )
    .unwrap();

    assert!(cfg.bot.telegram.enabled);
    assert_eq!(cfg.bot.telegram.operator_user_id, Some(424242));
    assert_eq!(cfg.bot.telegram.poll_timeout_secs, 30);
    assert!(cfg.bot.telegram.draft_streaming);
}

#[test]
fn enabled_bot_requires_token_and_operator() {
    let cfg: SpurConfig = toml::from_str(
        r#"
[bot.telegram]
enabled = true
"#,
    )
    .unwrap();

    let err = spur_bot::telegram::config::validate(&cfg.bot.telegram).unwrap_err();
    assert!(err.to_string().contains("bot_token"));
}
