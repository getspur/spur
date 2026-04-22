pub mod client;
pub mod config;
pub mod format;
pub mod poll_loop;
pub mod render;
pub mod router;
pub mod sender;

pub async fn run_telegram_bot(
    cfg: &spur_acp::config::TelegramBotConfig,
    mut host: spur_interactive::InteractiveFrontendHost,
    state_path: std::path::PathBuf,
) -> anyhow::Result<()> {
    let operator_user_id = cfg.operator_user_id.expect("validated");
    let handle = host.handle();
    let mut event_rx = host.take_event_stream().expect("event stream");
    let mut perm_rx = host.take_permission_stream().expect("permission stream");
    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(64);
    let mut runtime =
        crate::runtime::BotRuntime::new(crate::state::BotStateStore::new(state_path));
    let client = client::TelegramClient::new(cfg.bot_token.as_deref().expect("validated"));
    let sender = crate::telegram::sender::TelegramSender::new(
        client.clone(),
        cfg.max_requests_per_second,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();

    let poll_cancellation = cancellation.clone();
    let cfg_poll_timeout = cfg.poll_timeout_secs;
    let poll_client = client.clone();
    tokio::spawn(async move {
        let _ = poll_loop::run_poll_loop(
            &poll_client,
            cfg_poll_timeout,
            poll_cancellation,
            |batch| {
                for update in batch {
                    if let Some(input) = router::normalize_update(&update, operator_user_id) {
                        update_tx.try_send(input)?;
                    }
                }
                Ok(())
            },
        )
        .await;
    });

    loop {
        tokio::select! {
            maybe_update = update_rx.recv() => {
                let Some(input) = maybe_update else { break; };
                let renders = match input {
                    router::TelegramInput::Text { chat_id, text, .. } => {
                        runtime.handle_chat_text(&handle, chat_id, &text).await?
                    }
                    router::TelegramInput::Callback { query_id, token, .. } => {
                        runtime.handle_callback(&handle, &query_id, &token).await?
                    }
                };
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
            Ok(event) = event_rx.recv() => {
                let renders = runtime.handle_spur_event(event)?;
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
            Some(request) = perm_rx.recv() => {
                let renders = runtime.handle_permission_request(request)?;
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
        }
    }

    host.shutdown().await
}
