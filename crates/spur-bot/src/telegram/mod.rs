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

    let poll_cancellation_for_loop = cancellation.clone();
    let poll_cancellation_for_main = cancellation.clone();
    let cfg_poll_timeout = cfg.poll_timeout_secs;
    tokio::spawn(async move {
        let result = poll_loop::run_poll_loop(
            &poll_client,
            cfg_poll_timeout,
            poll_cancellation_for_loop,
            |batch| {
                let update_tx = update_tx.clone();
                async move {
                    let mut inputs = Vec::new();
                    for update in batch {
                        if let Some(input) = router::normalize_update(&update, operator_user_id) {
                            inputs.push(input);
                        }
                    }
                    if !inputs.is_empty() {
                        update_tx
                            .send(inputs)
                            .await
                            .map_err(|_| anyhow::anyhow!("update channel closed"))?;
                    }
                    Ok(())
                }
            },
        )
        .await;
        match result {
            Ok(()) => tracing::info!("telegram poll loop terminated cleanly"),
            Err(err) => {
                tracing::error!(error = ?err, "telegram poll loop terminated unexpectedly")
            }
        }
        poll_cancellation_for_main.cancel();
    });

    loop {
        tokio::select! {
            maybe_update = update_rx.recv() => {
                let Some(inputs) = maybe_update else { break; };
                process_input_batch(&mut runtime, &handle, &client, &sender, inputs).await;
            }
            event = event_rx.recv() => {
                match event {
                    Ok(event) => {
                        if let Err(err) = process_spur_event(
                            &mut runtime,
                            &handle,
                            &client,
                            &sender,
                            event,
                        )
                        .await
                        {
                            tracing::error!(
                                error = ?err,
                                "transient error handling spur event"
                            );
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            skipped = n,
                            "telegram bot lagged on spur event broadcast"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            Some(request) = perm_rx.recv() => {
                if let Err(err) = process_permission(
                    &mut runtime,
                    &client,
                    &sender,
                    request,
                )
                .await
                {
                    tracing::error!(
                        error = ?err,
                        "transient error handling permission request"
                    );
                }
            }
            _ = cancellation.cancelled() => {
                tracing::info!("cancellation signaled; winding down telegram bot");
                break;
            }
        }
    }

    drain_remaining_inputs(&mut update_rx, &mut runtime, &handle, &client, &sender).await;
    host.shutdown().await
}

async fn process_input_batch(
    runtime: &mut crate::runtime::BotRuntime,
    handle: &spur_interactive::InteractiveFrontendHandle,
    client: &client::TelegramClient,
    sender: &sender::TelegramSender,
    inputs: Vec<router::TelegramInput>,
) {
    for input in inputs {
        if let Err(err) = process_input(runtime, handle, client, sender, input).await {
            tracing::error!(error = ?err, "transient error handling telegram input");
        }
    }
}

async fn drain_remaining_inputs(
    update_rx: &mut tokio::sync::mpsc::Receiver<Vec<router::TelegramInput>>,
    runtime: &mut crate::runtime::BotRuntime,
    handle: &spur_interactive::InteractiveFrontendHandle,
    client: &client::TelegramClient,
    sender: &sender::TelegramSender,
) {
    let batches = drain_ready_input_batches(update_rx);
    if !batches.is_empty() {
        let batch_count = batches.len();
        let input_count = batches.iter().map(Vec::len).sum::<usize>();
        tracing::info!(
            batches = batch_count,
            inputs = input_count,
            "draining buffered telegram updates before shutdown"
        );
    }
    for inputs in batches {
        process_input_batch(runtime, handle, client, sender, inputs).await;
    }
}

fn drain_ready_input_batches(
    update_rx: &mut tokio::sync::mpsc::Receiver<Vec<router::TelegramInput>>,
) -> Vec<Vec<router::TelegramInput>> {
    let mut batches = Vec::new();
    while let Ok(inputs) = update_rx.try_recv() {
        batches.push(inputs);
    }
    batches
}

async fn process_input(
    runtime: &mut crate::runtime::BotRuntime,
    handle: &spur_interactive::InteractiveFrontendHandle,
    client: &client::TelegramClient,
    sender: &sender::TelegramSender,
    input: router::TelegramInput,
) -> anyhow::Result<()> {
    match input {
        router::TelegramInput::Text {
            chat_id,
            message_thread_id,
            text,
            ..
        } => {
            let renders = runtime
                .handle_chat_text(handle, chat_id, message_thread_id, &text)
                .await?;
            let mut all_renders = renders;
            let key = crate::state::ThreadKey {
                chat_id,
                message_thread_id,
            };
            let pending = runtime.flush_pending(handle, &key).await?;
            all_renders.extend(pending);

            for render in &all_renders {
                if let crate::runtime::RuntimeRender::CreateTopic { topic_name } = render {
                    let topic = client.create_forum_topic(chat_id, topic_name.clone()).await?;
                    runtime
                        .ensure_topic_record(chat_id, topic.message_thread_id, topic_name.clone())
                        .await?;
                    client
                        .send_text_to_thread(
                            chat_id,
                            Some(topic.message_thread_id),
                            "Send your first message to start the session.".into(),
                        )
                        .await?;
                }
            }

            let display_renders: Vec<_> = all_renders
                .into_iter()
                .filter(|r| !matches!(r, crate::runtime::RuntimeRender::CreateTopic { .. }))
                .collect();
            render::render_batch_to_thread(
                client,
                sender,
                chat_id,
                message_thread_id,
                display_renders,
            )
            .await
        }
        router::TelegramInput::Callback {
            query_id,
            token,
            chat_id,
            message_thread_id,
            ..
        } => {
            let key = crate::state::ThreadKey {
                chat_id,
                message_thread_id,
            };
            let renders = runtime
                .handle_callback(handle, &key, &query_id, &token)
                .await?;
            render::render_batch_to_thread(
                client,
                sender,
                chat_id,
                message_thread_id,
                renders,
            )
            .await
        }
    }
}

async fn process_spur_event(
    runtime: &mut crate::runtime::BotRuntime,
    handle: &spur_interactive::InteractiveFrontendHandle,
    client: &client::TelegramClient,
    sender: &sender::TelegramSender,
    event: spur_acp::SpurEvent,
) -> anyhow::Result<()> {
    let (maybe_key, renders) = runtime.handle_spur_event(event).await?;
    let mut all_renders = renders;
    if let Some(ref key) = maybe_key {
        let pending = runtime.flush_pending(handle, key).await?;
        all_renders.extend(pending);
    }
    if let Some(key) = maybe_key {
        render::render_batch_to_thread(
            client,
            sender,
            key.chat_id,
            key.message_thread_id,
            all_renders,
        )
        .await?;
    }
    Ok(())
}

async fn process_permission(
    runtime: &mut crate::runtime::BotRuntime,
    client: &client::TelegramClient,
    sender: &sender::TelegramSender,
    request: spur_acp::types::PermissionRequest,
) -> anyhow::Result<()> {
    let (key, renders) = runtime.handle_permission_request(request)?;
    render::render_batch_to_thread(
        client,
        sender,
        key.chat_id,
        key.message_thread_id,
        renders,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::router::TelegramInput;

    #[test]
    fn drain_ready_input_batches_processes_buffered_updates_after_cancellation() {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let (tx, mut update_rx) = tokio::sync::mpsc::channel(64);
        let first = TelegramInput::Text {
            user_id: 1,
            chat_id: 42,
            message_thread_id: None,
            text: "first".into(),
        };
        let second = TelegramInput::Text {
            user_id: 1,
            chat_id: 42,
            message_thread_id: Some(77),
            text: "second".into(),
        };
        tx.try_send(vec![first.clone()]).unwrap();
        tx.try_send(vec![second.clone()]).unwrap();

        cancellation.cancel();
        assert!(cancellation.is_cancelled());

        let mut processed = Vec::new();
        for batch in super::drain_ready_input_batches(&mut update_rx) {
            processed.extend(batch);
        }

        assert_eq!(processed, vec![first, second]);
        assert!(matches!(
            update_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }
}
