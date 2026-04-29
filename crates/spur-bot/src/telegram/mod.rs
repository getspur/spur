pub mod client;
pub mod config;
pub mod format;
pub mod poll_loop;
pub mod render;
pub mod router;
pub mod sender;

use anyhow::Context;
use std::time::Duration;

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
    let mut runtime = crate::runtime::BotRuntime::new(crate::state::BotStateStore::new(state_path))
        .context("initializing bot runtime")?;
    let client = client::TelegramClient::new(
        cfg.bot_token.as_deref().expect("validated"),
        Duration::from_secs(cfg.request_timeout_secs.unwrap_or(30)),
    )?;
    let poll_client = client::TelegramClient::new(
        cfg.bot_token.as_deref().expect("validated"),
        Duration::from_secs(cfg.poll_timeout_secs.saturating_add(10)),
    )?;
    let sender =
        crate::telegram::sender::TelegramSender::new(client.clone(), cfg.max_requests_per_second);
    let cancellation = tokio_util::sync::CancellationToken::new();

    // Startup capability gate.
    let me = client.get_me().await?;
    if !me.has_topics_enabled.unwrap_or(false) {
        anyhow::bail!("telegram bot does not have topics enabled; enable private topics in BotFather before using thread sessions");
    }

    let poll_cancellation = cancellation.clone();
    let cfg_poll_timeout = cfg.poll_timeout_secs;
    tokio::spawn(async move {
        let _ =
            poll_loop::run_poll_loop(&poll_client, cfg_poll_timeout, poll_cancellation, |batch| {
                let mut inputs = Vec::new();
                for update in batch {
                    if let Some(input) = router::normalize_update(&update, operator_user_id) {
                        inputs.push(input);
                    }
                }
                if !inputs.is_empty() {
                    update_tx.try_send(inputs)?;
                }
                Ok(())
            })
            .await;
    });

    loop {
        tokio::select! {
            maybe_update = update_rx.recv() => {
                let Some(inputs) = maybe_update else { break; };
                for input in inputs {
                    match input {
                        router::TelegramInput::Text {
                            chat_id,
                            message_thread_id,
                            text,
                            ..
                        } => {
                            let renders = runtime
                                .handle_chat_text(&handle, chat_id, message_thread_id, &text)
                                .await?;
                            let mut all_renders = renders;
                            let key = crate::state::ThreadKey { chat_id, message_thread_id };
                            let pending = runtime.flush_pending(&handle, &key).await?;
                            all_renders.extend(pending);

                            // Handle topic creation inline.
                            for render in &all_renders {
                                if let crate::runtime::RuntimeRender::CreateTopic { topic_name } = render {
                                    let topic = client.create_forum_topic(chat_id, topic_name.clone()).await?;
                                    runtime.ensure_topic_record(chat_id, topic.message_thread_id, topic_name.clone()).await?;
                                    client.send_text_to_thread(
                                        chat_id,
                                        Some(topic.message_thread_id),
                                        "Send your first message to start the session.".into(),
                                    ).await?;
                                }
                            }

                            let display_renders: Vec<_> = all_renders
                                .into_iter()
                                .filter(|r| !matches!(r, crate::runtime::RuntimeRender::CreateTopic { .. }))
                                .collect();
                            render::render_batch_to_thread(
                                &client,
                                &sender,
                                chat_id,
                                message_thread_id,
                                display_renders,
                            )
                            .await?;
                        }
                        router::TelegramInput::Callback {
                            query_id,
                            token,
                            chat_id,
                            message_thread_id,
                            ..
                        } => {
                            let key = crate::state::ThreadKey { chat_id, message_thread_id };
                            let renders = runtime.handle_callback(&handle, &key, &query_id, &token).await?;
                            render::render_batch_to_thread(
                                &client,
                                &sender,
                                chat_id,
                                message_thread_id,
                                renders,
                            )
                            .await?;
                        }
                    }
                }
            }
            Ok(event) = event_rx.recv() => {
                let (maybe_key, renders) = runtime.handle_spur_event(event).await?;
                let mut all_renders = renders;
                if let Some(ref key) = maybe_key {
                    let pending = runtime.flush_pending(&handle, key).await?;
                    all_renders.extend(pending);
                }
                if let Some(key) = maybe_key {
                    render::render_batch_to_thread(
                        &client,
                        &sender,
                        key.chat_id,
                        key.message_thread_id,
                        all_renders,
                    )
                    .await?;
                }
            }
            Some(request) = perm_rx.recv() => {
                let (key, renders) = runtime.handle_permission_request(request)?;
                render::render_batch_to_thread(
                    &client,
                    &sender,
                    key.chat_id,
                    key.message_thread_id,
                    renders,
                )
                .await?;
            }
        }
    }

    host.shutdown().await
}
