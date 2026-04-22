pub fn validate(cfg: &spur_acp::config::TelegramBotConfig) -> anyhow::Result<()> {
    if !cfg.enabled {
        return Ok(());
    }

    anyhow::ensure!(
        cfg.bot_token
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty()),
        "bot.telegram.bot_token is required when bot.telegram.enabled = true"
    );
    anyhow::ensure!(
        cfg.operator_user_id.is_some(),
        "bot.telegram.operator_user_id is required when bot.telegram.enabled = true"
    );
    anyhow::ensure!(
        cfg.poll_timeout_secs > 0,
        "bot.telegram.poll_timeout_secs must be greater than 0"
    );
    anyhow::ensure!(
        cfg.max_requests_per_second > 0,
        "bot.telegram.max_requests_per_second must be greater than 0"
    );
    Ok(())
}
