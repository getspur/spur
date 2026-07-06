use super::*;

impl App {
    pub(super) fn process_session_config(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::VendorExec {
                session,
                method,
                params,
            } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::VendorExec {
                        session,
                        method,
                        params,
                    });
                }
                None
            }

            Action::SetSessionConfigOption { config_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionConfigOption { config_id, value });
                }
                None
            }

            Action::AgentConfigSaveRequested {
                name,
                updated_entry,
            } => {
                let mut updated_app_config = false;
                {
                    let config = std::sync::Arc::make_mut(&mut self.config);
                    if let Some(slot) = config
                        .agents
                        .entries
                        .iter_mut()
                        .find(|entry| entry.name == name)
                    {
                        *slot = updated_entry.clone();
                        updated_app_config = true;
                    }
                }

                if !updated_app_config {
                    return Some(Action::FlashHint {
                        message: format!("agent config `{name}` is not configured"),
                    });
                }

                if let Some(browser) = self.agent_config_browser.as_mut() {
                    browser.replace_agent_config(&name, updated_entry.clone());
                }
                self.sync_dashboard_workers();
                self.dirty = true;

                let Some(tx) = self.user_input_tx.as_ref() else {
                    return Some(Action::FlashHint {
                        message: "agent config save failed: backend unavailable".into(),
                    });
                };

                if tx
                    .try_send(UserInput::UpdateAgentConfig {
                        name,
                        updated_entry,
                    })
                    .is_err()
                {
                    return Some(Action::FlashHint {
                        message: "agent config save failed: backend queue full".into(),
                    });
                }

                None
            }

            Action::SetSessionModel { session_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionModel { session_id, value });
                }
                None
            }

            Action::TogglePlanMode => {
                // Cycle between "plan" and "default". If mode is unknown, assume
                // we're in "default" and jump to "plan".
                let current = self
                    .session_detail
                    .as_ref()
                    .and_then(|d| d.current_mode.as_deref());
                let next = match current {
                    Some("plan") => "default",
                    _ => "plan",
                };
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SetSessionMode {
                        mode_id: next.to_string(),
                    });
                }
                // Optimistic update so the status bar reflects the toggle
                // immediately; orchestrator will emit CurrentModeUpdate to
                // reconcile if the agent rejects the mode id.
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_current_mode(Some(next.to_string()));
                }
                None
            }

            Action::ToggleVimMode => {
                self.edit_mode = match self.edit_mode {
                    EditMode::Emacs => EditMode::Vim(crate::components::input_bar::VimMode::Normal),
                    EditMode::Vim(_) => EditMode::Emacs,
                };
                self.dashboard.set_edit_mode(self.edit_mode);
                if let Some(ref mut detail) = self.session_detail {
                    detail.set_edit_mode(self.edit_mode);

                    let configured = EditMode::from(self.config.tui.edit_mode);
                    if self.edit_mode != configured {
                        let label = match self.edit_mode {
                            EditMode::Emacs => "Emacs",
                            EditMode::Vim(_) => "Vim",
                        };
                        detail.push_persist_hint(label);
                    }
                }
                self.dirty = true;
                None
            }

            Action::ToggleVerbose => {
                // Verbose mode is tracked by the dashboard view internally.
                // We toggle it via a dedicated method or re-send the key.
                // For now, the dashboard already handles this in handle_key.
                None
            }

            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notebook_command_helpers_live_in_slash_commands_module() {
        assert_eq!(
            super::super::slash_commands::notebook_command_label(""),
            "Reopening notebook"
        );
        assert_eq!(
            super::super::slash_commands::notebook_command_action("new"),
            "new"
        );
        assert_eq!(
            super::super::slash_commands::notebook_io_error_class(&std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused"
            )),
            "connection refused"
        );
    }
}
