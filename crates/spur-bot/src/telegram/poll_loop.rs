pub fn advance_offset(current: i64, update_ids: &[i64], accepted: bool) -> i64 {
    if !accepted {
        return current;
    }
    update_ids
        .iter()
        .copied()
        .max()
        .map(|id| id + 1)
        .unwrap_or(current)
}

pub async fn run_poll_loop(
    client: &crate::telegram::client::TelegramClient,
    timeout_secs: u64,
    cancellation: tokio_util::sync::CancellationToken,
    mut on_batch: impl FnMut(Vec<frankenstein::updates::Update>) -> anyhow::Result<()> + Send,
) -> anyhow::Result<()> {
    client.delete_webhook().await?;
    let mut offset = 0_i64;
    let mut backoff = std::time::Duration::from_millis(250);

    loop {
        let poll_deadline = std::time::Duration::from_secs(timeout_secs.saturating_add(10));
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            result = tokio::time::timeout(poll_deadline, client.get_updates(offset, timeout_secs)) => {
                match result {
                    Ok(Ok(batch)) => {
                        let ids: Vec<i64> = batch.iter().map(|u| u.update_id as i64).collect();
                        let accepted = on_batch(batch).is_ok();
                        offset = advance_offset(offset, &ids, accepted);
                        backoff = std::time::Duration::from_millis(250);
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "telegram poll failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                    }
                    Err(_elapsed) => {
                        tracing::warn!(
                            secs = poll_deadline.as_secs(),
                            "long-poll exceeded outer deadline; rotating connection"
                        );
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                    }
                }
            }
        }
    }
}
