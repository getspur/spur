use super::*;

impl App {
    pub(super) fn process_theme_cmd(&mut self, arg: String) -> Option<Action> {
        self.handle_theme_command(arg);
        None
    }

    /// `/brain [<name>]` — Scope A hot-swap.
    ///
    /// Bare `/brain` opens the fuzzy picker **locally** from config (same
    /// pattern as `/theme`) so the UI does not wait on an orchestrator
    /// round-trip. Named `/brain <name>` still goes through the channel.
    pub(super) fn process_brain_cmd(&mut self, arg: String) -> Option<Action> {
        let name = {
            let trimmed = arg.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        };

        // Bare command: open picker immediately from local SpurConfig.
        if name.is_none() {
            let brains = self.local_brain_info_list();
            let active = self
                .brain_name
                .clone()
                .unwrap_or_else(|| self.config.brain.default.clone());
            if brains.is_empty() {
                self.flash_hint(
                    "no brain-capable agents in config — check agents.entries roles",
                    std::time::Duration::from_secs(6),
                );
            } else {
                self.open_brain_picker_or_flash_status(brains, &active);
            }
            self.dirty = true;
            return None;
        }

        let target = name.expect("named path");
        self.flash_hint_short(format!("switching brain to {target}…"));
        self.brain_status = BrainStatus::Connecting;
        self.sync_brain_status();

        let send_ok = match self.user_input_tx.as_ref() {
            Some(tx) => match tx.try_send(crate::UserInput::SwitchBrain { name: Some(target) }) {
                Ok(()) => true,
                Err(e) => {
                    tracing::error!(
                        err = ?e,
                        "process_brain_cmd: user_input tx send failed — brain switch not delivered"
                    );
                    false
                }
            },
            None => {
                tracing::error!("process_brain_cmd: user_input_tx is None — cannot switch brain");
                false
            }
        };

        if !send_ok {
            // Revert optimistic Connecting and surface the failure longer than
            // a 2s flash so a full channel is not silent.
            self.brain_status = if self.session_detail.is_some() {
                BrainStatus::Ready
            } else {
                BrainStatus::Idle
            };
            self.sync_brain_status();
            self.flash_hint(
                "brain command failed: orchestrator channel unavailable or full — retry",
                std::time::Duration::from_secs(6),
            );
        }
        self.dirty = true;
        None
    }

    pub(super) fn process_notebook_cmd(&mut self, arg: String) -> Option<Action> {
        if self.session_detail.is_none() {
            return Some(Action::FlashHint {
                message: "/notebook: no active brain session in focus".into(),
            });
        }
        let Some(socket_nonce) = self.notebook_socket_nonce.clone() else {
            return Some(Action::FlashHint {
                message: "/notebook: notebook socket not ready".into(),
            });
        };
        let socket_path = spur_core::notebook::control_socket_path(&socket_nonce);
        let label = notebook_command_label(&arg);
        let command_action = notebook_command_action(&arg);
        let tx = self.background_action_tx.clone();
        self.flash_hint_short(format!("{label}..."));
        tokio::spawn(async move {
            match crate::notebook_daemon::send_notebook_command(&arg, &socket_path).await {
                Ok(response) if response.ok => {
                    tracing::info!(
                        path = response.path.as_deref(),
                        socket = %socket_path.display(),
                        "notebook daemon command completed"
                    );
                    let message = match response.path.as_deref() {
                        Some(path) => format!("{label} done: {path}"),
                        None => format!("{label} done"),
                    };
                    let _ = tx.send(Action::FlashHint { message });
                }
                Ok(response) => {
                    let detail = notebook_control_failure_detail(response.error.as_ref());
                    if let Some(error) = response.error.as_ref() {
                        tracing::warn!(
                            code = %error.code,
                            message = %error.message,
                            "notebook daemon command failed"
                        );
                    } else {
                        tracing::warn!("notebook daemon command failed");
                    }
                    let _ = tx.send(Action::FlashHint {
                        message: format!(
                            "notebook daemon command failed ({command_action} failed): {detail}"
                        ),
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "notebook daemon command failed");
                    let _ = tx.send(Action::FlashHint {
                        message: notebook_daemon_failure_message(command_action, &error),
                    });
                }
            }
        });
        None
    }
}

pub(super) fn notebook_command_label(arg: &str) -> &'static str {
    let arg = arg.trim();
    if is_notebook_data_add(arg) {
        "Attaching datasource"
    } else {
        match arg {
            "" => "Reopening notebook",
            "new" => "Creating notebook",
            "close" => "Closing notebook",
            _ => "Opening notebook",
        }
    }
}

pub(super) fn notebook_command_action(arg: &str) -> &'static str {
    let arg = arg.trim();
    if is_notebook_data_add(arg) {
        "attach datasource"
    } else {
        match arg {
            "" => "reopen",
            "new" => "new",
            "close" => "close",
            _ => "open",
        }
    }
}

fn is_notebook_data_add(arg: &str) -> bool {
    arg == "data add" || arg.starts_with("data add ")
}

pub(super) fn notebook_control_failure_detail(
    error: Option<&crate::notebook_daemon::ControlError>,
) -> String {
    match error {
        Some(error) if error.message.is_empty() => error.code.clone(),
        Some(error) if error.code.is_empty() => error.message.clone(),
        Some(error) => format!("{}: {}", error.code, error.message),
        None => "unknown error".to_string(),
    }
}

pub(super) fn notebook_daemon_failure_message(action: &str, error: &anyhow::Error) -> String {
    if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
        let prefix = if matches!(
            io_error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::NotFound
                | std::io::ErrorKind::AddrNotAvailable
        ) {
            "notebook daemon unavailable"
        } else {
            "notebook daemon command failed"
        };
        return format!(
            "{prefix} ({action} failed): {}",
            notebook_io_error_class(io_error)
        );
    }

    format!("notebook daemon command failed ({action} failed): {error}")
}

pub(super) fn notebook_io_error_class(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
        std::io::ErrorKind::NotFound => "socket not found".to_string(),
        std::io::ErrorKind::AddrNotAvailable => "address not available".to_string(),
        std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        std::io::ErrorKind::TimedOut => "timed out".to_string(),
        std::io::ErrorKind::UnexpectedEof => "unexpected end of file".to_string(),
        std::io::ErrorKind::InvalidData => "invalid protocol data".to_string(),
        _ => error.to_string(),
    }
}
