pub fn advance_offset(current: i64, update_ids: &[i64]) -> i64 {
    update_ids
        .iter()
        .copied()
        .max()
        .map(|id| id + 1)
        .unwrap_or(current)
}

pub async fn run_poll_loop<F, Fut>(
    client: &crate::telegram::client::TelegramClient,
    timeout_secs: u64,
    cancellation: tokio_util::sync::CancellationToken,
    mut on_batch: F,
) -> anyhow::Result<()>
where
    F: FnMut(Vec<frankenstein::updates::Update>) -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
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
                        on_batch(batch).await?;
                        offset = advance_offset(offset, &ids);
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

#[cfg(test)]
mod tests {
    use super::advance_offset;

    #[test]
    fn advance_offset_keeps_current_when_batch_empty() {
        assert_eq!(advance_offset(42, &[]), 42);
    }

    #[test]
    fn advance_offset_moves_past_highest_update_id() {
        assert_eq!(advance_offset(0, &[5, 2, 9, 1]), 10);
    }

    #[test]
    fn advance_offset_ignores_current_when_batch_present() {
        // A stale `current` below the batch's ids must not suppress the
        // advance — the max of the *batch* always wins when non-empty.
        assert_eq!(advance_offset(100, &[3]), 4);
    }
}
