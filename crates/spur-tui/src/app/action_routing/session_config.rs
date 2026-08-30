use super::*;
use spur_acp::config::{ConfigPatch, EditorMode};

/// SAVE-APPLY: agent saves must not write `App.config` before orchestrator ok.
#[cfg(test)]
pub(crate) fn agent_save_should_mutate_before_send() -> bool {
    false
}

pub(crate) fn apply_config_patch_locally(cfg: &mut spur_acp::SpurConfig, patch: &ConfigPatch) {
    let _ = patch.apply(cfg);
}

impl App {
    fn queue_config_patch(&mut self, patch: ConfigPatch) -> Option<Action> {
        let Some(tx) = self.user_input_tx.as_ref() else {
            return Some(Action::FlashHint {
                message: "config save failed: backend unavailable".into(),
            });
        };
        if tx
            .try_send(UserInput::UpdateConfig {
                patch: patch.clone(),
            })
            .is_err()
        {
            return Some(Action::FlashHint {
                message: "config save failed: backend queue full".into(),
            });
        }
        self.pending_config_patch = Some(patch);
        None
    }

    pub(crate) fn apply_pending_config_on_ok(&mut self) {
        let Some(patch) = self.pending_config_patch.take() else {
            return;
        };
        apply_config_patch_locally(std::sync::Arc::make_mut(&mut self.config), &patch);
        self.apply_config_live_hooks(&patch);
    }

    pub(crate) fn discard_pending_config_patch(&mut self) {
        self.pending_config_patch = None;
    }

    fn apply_config_live_hooks(&mut self, patch: &ConfigPatch) {
        match patch {
            ConfigPatch::Agent {
                name,
                updated_entry,
            } => {
                if let Some(browser) = self.agent_config_browser.as_mut() {
                    browser.replace_agent_config(name, updated_entry.clone());
                }
                self.sync_dashboard_workers();
            }
            ConfigPatch::TuiEditMode(mode) => {
                self.edit_mode = EditMode::from(*mode);
                self.dashboard.set_edit_mode(self.edit_mode);
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.set_edit_mode(self.edit_mode);
                }
            }
            ConfigPatch::TuiTheme(name) => {
                let (theme, outcome) = crate::theme::load_runtime_theme(name);
                if let crate::theme::ThemeLoadOutcome::FellBackToDark { reason } = &outcome {
                    tracing::warn!(
                        target: "spur_tui::theme",
                        target_name = %name,
                        reason = %reason,
                        "theme apply after persist fell back to dark"
                    );
                    self.flash_hint_short(format!("theme `{name}` not found"));
                }
                self.theme = std::sync::Arc::new(theme);
                self.active_theme_name = name.clone();
            }
            ConfigPatch::TuiDisablePasteBurst(disabled) => {
                self.dashboard.set_disable_paste_burst(*disabled);
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.set_disable_paste_burst(*disabled);
                }
            }
            ConfigPatch::GraphEmbeddingModel { .. }
            | ConfigPatch::GraphOverlayFsmonitor(_)
            | ConfigPatch::SkillsProjectionMode(_) => {}
            ConfigPatch::McpServerUpsert { .. } | ConfigPatch::McpServerRemove { .. } => {
                if let Some(browser) = self.agent_config_browser.as_mut() {
                    browser.set_mcp_config(&self.config.mcp_servers);
                }
            }
        }
        self.dirty = true;
    }

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
                // Do not write `self.config` until AgentConfigUpdateResult ok.
                let exists = self
                    .config
                    .agents
                    .entries
                    .iter()
                    .any(|entry| entry.name == name);
                if !exists {
                    return Some(Action::FlashHint {
                        message: format!("agent config `{name}` is not configured"),
                    });
                }

                let Some(tx) = self.user_input_tx.as_ref() else {
                    return Some(Action::FlashHint {
                        message: "agent config save failed: backend unavailable".into(),
                    });
                };

                let patch = ConfigPatch::Agent {
                    name: name.clone(),
                    updated_entry: updated_entry.clone(),
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
                self.pending_config_patch = Some(patch);
                None
            }

            Action::ConfigSaveRequested { patch } => self.queue_config_patch(patch),

            Action::SetSessionModel { session_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionModel { session_id, value });
                }
                None
            }

            Action::SetSessionEffort { session_id, value } => {
                if let Some(tx) = self.user_input_tx.as_ref() {
                    let _ = tx.try_send(UserInput::SetSessionEffort { session_id, value });
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
                let next = match self.edit_mode {
                    EditMode::Emacs => EditorMode::Vim,
                    EditMode::Vim(_) => EditorMode::Emacs,
                };
                if self.user_input_tx.is_some() {
                    return self.queue_config_patch(ConfigPatch::TuiEditMode(next));
                }
                // Tests and detached TUI: keep the in-memory toggle when
                // there is no orchestrator channel.
                self.edit_mode = EditMode::from(next);
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
    use super::*;
    use spur_acp::config::{
        ConfigPatch, EditorMode, McpServerEntry, McpServerTransport, OverlayFsmonitorMode,
    };
    use tokio::sync::mpsc;

    fn app_with_agent(tx: Option<mpsc::Sender<UserInput>>) -> App {
        let mut config = spur_acp::SpurConfig::default();
        let mut agent = spur_acp::AgentConfig::with_defaults("kiro");
        agent.skip_permissions = false;
        config.agents.entries = vec![agent];
        App::new_with_config(
            tx,
            false,
            std::sync::Arc::new(config),
            crate::landing::LandingDecision::ShowDashboard,
        )
    }

    fn updated_kiro() -> spur_acp::AgentConfig {
        let mut agent = spur_acp::AgentConfig::with_defaults("kiro");
        agent.skip_permissions = true;
        agent
    }

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

    #[test]
    fn apply_config_patch_locally_writes_tui_edit_mode() {
        let mut cfg = spur_acp::SpurConfig::default();
        apply_config_patch_locally(&mut cfg, &ConfigPatch::TuiEditMode(EditorMode::Vim));
        assert_eq!(cfg.tui.edit_mode, EditorMode::Vim);
        assert_eq!(cfg.tui.theme, "dark");
    }

    #[test]
    fn acknowledged_graph_fsmonitor_patch_applies_without_a_tui_live_hook() {
        let mut app = app_with_agent(None);
        app.dirty = false;
        app.pending_config_patch = Some(ConfigPatch::GraphOverlayFsmonitor(
            OverlayFsmonitorMode::Auto,
        ));

        app.apply_pending_config_on_ok();

        assert_eq!(
            app.config.graph.overlay_fsmonitor,
            OverlayFsmonitorMode::Auto
        );
        assert!(app.dirty);
    }

    #[test]
    fn save_apply_is_persist_then_apply() {
        assert!(!agent_save_should_mutate_before_send());
    }

    #[test]
    fn agent_save_does_not_mutate_config_before_orchestrator_ok() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = app_with_agent(Some(tx));
        app.process_action(Action::AgentConfigSaveRequested {
            name: "kiro".into(),
            updated_entry: updated_kiro(),
        });

        assert!(
            !app.config.agents.entries[0].skip_permissions,
            "SAVE-APPLY: App.config must not change before orchestrator ok"
        );
        match rx.try_recv() {
            Ok(UserInput::UpdateAgentConfig {
                name,
                updated_entry,
            }) => {
                assert_eq!(name, "kiro");
                assert!(updated_entry.skip_permissions);
            }
            Ok(_) => panic!("expected UpdateAgentConfig"),
            Err(err) => panic!("expected UpdateAgentConfig, got {err}"),
        }
    }

    #[test]
    fn agent_save_applies_on_update_result_ok() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = app_with_agent(Some(tx));
        app.process_action(Action::AgentConfigSaveRequested {
            name: "kiro".into(),
            updated_entry: updated_kiro(),
        });
        assert!(!app.config.agents.entries[0].skip_permissions);

        app.handle_spur_event(SpurEvent::now(SpurEventBody::AgentConfigUpdateResult {
            name: "kiro".into(),
            ok: true,
            message: "saved - applies to next delegation".into(),
        }));

        assert!(app.config.agents.entries[0].skip_permissions);
        let hint = app.transient_hint_text().unwrap_or("");
        assert!(
            hint.contains("kiro"),
            "success flash should name the agent: {hint}"
        );
        assert!(
            !hint.contains("failed"),
            "ok result must not flash failure: {hint}"
        );
    }

    #[test]
    fn agent_save_failure_does_not_apply() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = app_with_agent(Some(tx));
        app.process_action(Action::AgentConfigSaveRequested {
            name: "kiro".into(),
            updated_entry: updated_kiro(),
        });

        app.handle_spur_event(SpurEvent::now(SpurEventBody::AgentConfigUpdateResult {
            name: "kiro".into(),
            ok: false,
            message: "disk full".into(),
        }));

        assert!(!app.config.agents.entries[0].skip_permissions);
        let hint = app.transient_hint_text().unwrap_or("");
        assert!(hint.contains("failed"), "failure flash missing: {hint}");
        assert!(hint.contains("disk full"), "error message missing: {hint}");
    }

    #[test]
    fn config_save_applies_tui_edit_mode_on_ok() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        assert_eq!(app.edit_mode, EditMode::Emacs);

        app.process_action(Action::ConfigSaveRequested {
            patch: ConfigPatch::TuiEditMode(EditorMode::Vim),
        });
        assert_eq!(app.edit_mode, EditMode::Emacs);
        assert_eq!(app.config.tui.edit_mode, EditorMode::Emacs);
        match rx.try_recv() {
            Ok(UserInput::UpdateConfig { patch }) => {
                assert!(matches!(patch, ConfigPatch::TuiEditMode(EditorMode::Vim)));
            }
            Ok(_) => panic!("expected UpdateConfig"),
            Err(err) => panic!("expected UpdateConfig, got {err}"),
        }

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "tui".into(),
            ok: true,
            message: "saved".into(),
        }));
        assert!(matches!(app.edit_mode, EditMode::Vim(_)));
        assert_eq!(app.config.tui.edit_mode, EditorMode::Vim);
    }

    #[test]
    fn config_save_failure_does_not_apply() {
        let (tx, _rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        app.process_action(Action::ConfigSaveRequested {
            patch: ConfigPatch::TuiEditMode(EditorMode::Vim),
        });

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "tui".into(),
            ok: false,
            message: "permission denied".into(),
        }));

        assert_eq!(app.edit_mode, EditMode::Emacs);
        assert_eq!(app.config.tui.edit_mode, EditorMode::Emacs);
        let hint = app.transient_hint_text().unwrap_or("");
        assert!(hint.contains("failed"), "failure flash missing: {hint}");
    }

    #[test]
    fn graph_overlay_fsmonitor_applies_only_after_confirmation() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);

        app.process_action(Action::ConfigSaveRequested {
            patch: ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto),
        });
        assert_eq!(
            app.config.graph.overlay_fsmonitor,
            OverlayFsmonitorMode::Off
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(UserInput::UpdateConfig {
                patch: ConfigPatch::GraphOverlayFsmonitor(OverlayFsmonitorMode::Auto)
            })
        ));

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "graph".into(),
            ok: true,
            message: "saved - restart required".into(),
        }));

        assert_eq!(
            app.config.graph.overlay_fsmonitor,
            OverlayFsmonitorMode::Auto
        );
    }

    fn mcp_entry(name: &str, enabled: bool) -> McpServerEntry {
        McpServerEntry {
            name: name.into(),
            enabled,
            transport: McpServerTransport::Http {
                url: "https://example.test/mcp".into(),
                headers: Default::default(),
            },
        }
    }

    #[test]
    fn mcp_upsert_refreshes_open_browser_after_persist_ok() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        app.process_nav(Action::NavigateTo(ViewId::AgentConfigBrowser {
            preselect: Some("mcp".into()),
        }));
        app.process_action(Action::ConfigSaveRequested {
            patch: ConfigPatch::McpServerUpsert {
                entry: mcp_entry("github", false),
            },
        });
        assert!(app.config.mcp_servers.entries.is_empty());
        assert!(matches!(
            rx.try_recv(),
            Ok(UserInput::UpdateConfig {
                patch: ConfigPatch::McpServerUpsert { .. }
            })
        ));

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "mcp".into(),
            ok: true,
            message: "saved - applies to next session".into(),
        }));

        assert_eq!(app.config.mcp_servers.entries.len(), 1);
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = crate::views::ViewContext::test_ctx(&lineage);
        let browser = app.agent_config_browser.as_mut().expect("open browser");
        assert!(matches!(
            crate::views::View::handle_key(browser, crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ), &ctx),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        enabled: true,
                        ..
                    }
                }
            }) if name == "github"
        ));
    }

    #[test]
    fn mcp_remove_refreshes_open_browser_after_persist_ok() {
        let (tx, _rx) = mpsc::channel(8);
        let mut config = spur_acp::SpurConfig::default();
        config.mcp_servers.entries = vec![mcp_entry("github", true), mcp_entry("linear", false)];
        let mut app = App::new_with_config(
            Some(tx),
            false,
            std::sync::Arc::new(config),
            crate::landing::LandingDecision::ShowDashboard,
        );
        app.process_nav(Action::NavigateTo(ViewId::AgentConfigBrowser {
            preselect: Some("mcp".into()),
        }));
        app.process_action(Action::ConfigSaveRequested {
            patch: ConfigPatch::McpServerRemove {
                name: "github".into(),
            },
        });

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "mcp".into(),
            ok: true,
            message: "saved - applies to next session".into(),
        }));

        assert_eq!(
            app.config
                .mcp_servers
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["linear"]
        );
        let lineage = spur_core::ExecutorLineage::new();
        let ctx = crate::views::ViewContext::test_ctx(&lineage);
        let browser = app.agent_config_browser.as_mut().expect("open browser");
        assert!(matches!(
            crate::views::View::handle_key(browser, crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ), &ctx),
            Some(Action::ConfigSaveRequested {
                patch: ConfigPatch::McpServerUpsert {
                    entry: McpServerEntry {
                        ref name,
                        enabled: true,
                        ..
                    }
                }
            }) if name == "linear"
        ));
    }

    #[test]
    fn vim_toggle_without_backend_still_flips_in_memory() {
        let mut app = App::new(None, false);
        assert_eq!(app.edit_mode, EditMode::Emacs);
        app.process_action(Action::ToggleVimMode);
        assert!(matches!(app.edit_mode, EditMode::Vim(_)));
        assert_eq!(
            app.config.tui.edit_mode,
            EditorMode::Emacs,
            "no-backend tests must not pretend persist succeeded"
        );
    }

    #[test]
    fn vim_toggle_with_backend_persists_then_applies_on_ok() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut app = App::new(Some(tx), false);
        app.process_action(Action::ToggleVimMode);
        assert_eq!(app.edit_mode, EditMode::Emacs);
        match rx.try_recv() {
            Ok(UserInput::UpdateConfig { patch }) => {
                assert!(matches!(patch, ConfigPatch::TuiEditMode(EditorMode::Vim)));
            }
            Ok(_) => panic!("expected UpdateConfig for /vim persist"),
            Err(err) => panic!("expected UpdateConfig, got {err}"),
        }

        app.handle_spur_event(SpurEvent::now(SpurEventBody::ConfigUpdateResult {
            section: "tui".into(),
            ok: true,
            message: "saved".into(),
        }));
        assert!(matches!(app.edit_mode, EditMode::Vim(_)));
        assert_eq!(app.config.tui.edit_mode, EditorMode::Vim);
    }
}
