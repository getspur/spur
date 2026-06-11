//! Tauri invoke commands for sidebar chat.

use std::{path::Path, sync::Arc};

use agent_client_protocol::schema::{SessionId, SessionInfo};
use tauri::ipc::Channel;
use tokio::sync::mpsc;

use crate::{chat_state::get_or_init_sidebar_chat, state::State, Error};
use spur_notebook::sidebar_chat::{scope::resolve_app_scope, types::ChatEvent};

/// Run one sidebar chat turn and stream `ChatEvent`s to trusted React.
#[tauri::command]
pub async fn chat_turn(
    notebook_path: &str,
    prompt: &str,
    on_event: Channel<ChatEvent>,
    state: tauri::State<'_, Arc<State>>,
) -> Result<(), Error> {
    let scope = resolve_app_scope(Path::new(notebook_path)).map_err(command_error)?;
    let prompt = prompt.to_owned();
    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    let cancel = state.sidebar_chat.cancellation_root().child_token();
    let session_id = chat
        .ensure_session(&scope)
        .await
        .map_err(command_error)?
        .0
        .as_ref()
        .to_owned();
    state
        .sidebar_chat
        .register_turn_cancel(session_id.clone(), cancel.clone())
        .await;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<ChatEvent>();

    let turn_chat = Arc::clone(&chat);
    let turn_tx = event_tx.clone();
    let turn_cancel = cancel.clone();
    let mut turn =
        tokio::spawn(async move { turn_chat.turn(&scope, &prompt, turn_tx, turn_cancel).await });
    let mut permission_rx = state.sidebar_chat.permission_receiver().await;
    let mut event_tx = Some(event_tx);

    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                if on_event.send(event).is_err() {
                    cancel.cancel();
                    break;
                }
            }
            maybe_request = permission_rx.recv(), if event_tx.is_some() => {
                let Some(request) = maybe_request else {
                    continue;
                };
                let tx = event_tx.as_ref().expect("guarded by is_some");
                if let Err(error) = chat.handle_permission_request(request, tx).await {
                    let _ = tx.send(ChatEvent::Error {
                        message: error.to_string(),
                    });
                }
            }
            turn_result = &mut turn, if event_tx.is_some() => {
                state.sidebar_chat.unregister_turn_cancel(&session_id).await;
                if let Some(tx) = event_tx.take() {
                    match turn_result {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            let _ = tx.send(ChatEvent::Error {
                                message: error.to_string(),
                            });
                            let _ = tx.send(ChatEvent::Done);
                        }
                        Err(error) => {
                            let _ = tx.send(ChatEvent::Error {
                                message: error.to_string(),
                            });
                            let _ = tx.send(ChatEvent::Done);
                        }
                    }
                }
            }
        }
    }

    state.sidebar_chat.unregister_turn_cancel(&session_id).await;
    Ok(())
}

/// List agent sessions available for the current notebook or Spur App scope.
#[tauri::command]
pub async fn chat_sessions_list(
    notebook_path: &str,
    state: tauri::State<'_, Arc<State>>,
) -> Result<Vec<SessionInfo>, Error> {
    let scope = resolve_app_scope(Path::new(notebook_path)).map_err(command_error)?;
    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    chat.list_sessions(&scope).await.map_err(command_error)
}

/// Switch the current notebook or Spur App scope to an existing agent session.
#[tauri::command]
pub async fn chat_switch_session(
    notebook_path: &str,
    session_id: &str,
    state: tauri::State<'_, Arc<State>>,
) -> Result<(), Error> {
    let scope = resolve_app_scope(Path::new(notebook_path)).map_err(command_error)?;
    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    let _stream = chat
        .load_session(&scope, SessionId::new(session_id.to_owned()))
        .await
        .map_err(command_error)?;
    Ok(())
}

/// Ensure a chat session exists for the current notebook or Spur App scope.
#[tauri::command]
pub async fn chat_new_session(
    notebook_path: &str,
    state: tauri::State<'_, Arc<State>>,
) -> Result<String, Error> {
    let scope = resolve_app_scope(Path::new(notebook_path)).map_err(command_error)?;
    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    let session_id = chat.ensure_session(&scope).await.map_err(command_error)?;
    Ok(session_id.0.as_ref().to_owned())
}

/// Cancel an active agent session by ACP session id.
#[tauri::command]
pub async fn chat_cancel(
    session_id: &str,
    state: tauri::State<'_, Arc<State>>,
) -> Result<(), Error> {
    if let Some(cancel) = state.sidebar_chat.take_turn_cancel(session_id).await {
        cancel.cancel();
    }

    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    chat.cancel(&SessionId::new(session_id.to_owned()))
        .await
        .map_err(command_error)
}

/// Respond to a pending ACP permission request.
#[tauri::command]
pub async fn chat_permission_respond(
    request_id: &str,
    option_id: Option<String>,
    state: tauri::State<'_, Arc<State>>,
) -> Result<(), Error> {
    let chat = get_or_init_sidebar_chat(&state)
        .await
        .map_err(command_error)?;
    chat.respond_permission(request_id, option_id)
        .await
        .map_err(command_error)
}

fn command_error(error: impl std::fmt::Display) -> Error {
    Error::NotebookDaemon(error.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn chat_turn_has_channel_signature() {
        let _command = super::chat_turn;
    }
}
