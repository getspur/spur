use super::*;

impl App {
    pub fn flash_hint(&mut self, msg: impl Into<String>, duration: Duration) {
        self.transient_hint = Some(TransientHint {
            text: msg.into(),
            expires_at: Instant::now() + duration,
        });
        self.dirty = true;
    }

    pub fn flash_hint_short(&mut self, msg: impl Into<String>) {
        self.flash_hint(msg, Duration::from_secs(2));
    }

    /// Service `/theme` slash command: list / switch / reload. On a successful
    /// switch or reload the `Arc<Theme>` on `self` is replaced atomically —
    /// every render surface reads the new theme on its next frame. If the app
    /// was booted from a discoverable project config, the chosen theme is also
    /// persisted back to `.spur/config.toml` so it survives the next TUI start.
    pub(super) fn handle_theme_command(&mut self, arg: String) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.open_theme_picker_or_flash_status();
            return;
        }

        let target = if arg == "reload" {
            self.active_theme_name.clone()
        } else {
            arg.to_string()
        };

        let (theme, outcome) = crate::theme::load_runtime_theme(&target);
        if let crate::theme::ThemeLoadOutcome::FellBackToDark { reason } = &outcome {
            tracing::warn!(target: "spur_tui::theme", target_name = %target, reason = %reason, "theme switch failed");
            self.flash_hint_short(format!("theme `{target}` not found"));
            return;
        }

        tracing::info!(
            target: "spur_tui::theme",
            theme = %theme.name,
            requested = %target,
            outcome = ?outcome,
            "theme switched at runtime"
        );
        self.theme = std::sync::Arc::new(theme);
        self.active_theme_name = target.clone();
        self.dirty = true;

        let mut persisted = false;
        if arg != "reload" {
            if let Some(ref path) = self.config_path {
                if path.exists() {
                    if let Err(e) = spur_acp::config::update_config(path, |c| {
                        c.tui.theme = target.clone();
                    }) {
                        tracing::warn!(target: "spur_tui::theme", error = %e, "failed to persist theme to config");
                        self.flash_hint_short(format!(
                            "theme: {target} (config write failed: {})",
                            e
                        ));
                        return;
                    }
                    persisted = true;
                }
            }
        }

        if arg == "reload" {
            self.flash_hint_short(format!("theme reloaded: {target}"));
        } else if persisted {
            self.flash_hint_short(format!(
                "theme: {target} (saved to .spur/config.toml). Global: spur config set tui.theme {target} --global"
            ));
        } else {
            self.flash_hint_short(format!(
                "theme: {target}. Persist with `spur config set tui.theme {target} --global`"
            ));
        }
    }

    pub(super) fn tick_transient_hint(&mut self, now: Instant) {
        if self
            .transient_hint
            .as_ref()
            .is_some_and(|hint| now >= hint.expires_at)
        {
            self.transient_hint = None;
            self.dirty = true;
        }
    }

    fn open_theme_picker_or_flash_status(&mut self) {
        match &self.current_view {
            ViewId::Dashboard => {
                self.dashboard.open_theme_picker(&self.active_theme_name);
                self.dirty = true;
            }
            ViewId::SessionDetail(_) => {
                if let Some(ref mut detail) = self.session_detail {
                    detail.open_theme_picker(&self.active_theme_name);
                    self.dirty = true;
                } else {
                    self.flash_theme_status();
                }
            }
            _ => self.flash_theme_status(),
        }
    }

    fn flash_theme_status(&mut self) {
        let available = crate::theme::list_available_themes();
        let active = &self.active_theme_name;
        // Show every theme entry the cascade can resolve, tagged by
        // origin so users can see when a project/user file shadows a
        // built-in. The active marker `*` always attaches to the
        // active name regardless of source — load_runtime_theme
        // determines which file actually loaded (project > user >
        // built-in cascade).
        let mark = |name: &str| {
            if name == active {
                format!("* {name}")
            } else {
                name.to_string()
            }
        };
        let mut parts: Vec<String> = available.built_in.iter().map(|n| mark(n)).collect();
        for name in &available.project {
            parts.push(format!("{} [project]", mark(name)));
        }
        for name in &available.user {
            parts.push(format!("{} [user]", mark(name)));
        }
        self.flash_hint(
            format!("themes: {} (active *)", parts.join(", ")),
            Duration::from_secs(4),
        );
    }

    pub(super) fn open_palette(&mut self) {
        if self.help_visible
            || self.quit_confirm_visible
            || self.collision_modal.is_some()
            || self.upgrade_modal.is_some()
        {
            return; // palette won't open while a higher-priority overlay is up
        }
        tracing::debug!(target: "palette", "open_palette: start");
        self.palette_state.reset();

        // Load sources: Views, Commands, Sessions, Workers. (Trace deferred — see U3c.)
        // CommandRegistry is not Clone; borrow from the active session_detail
        // or fall back to a fresh empty one (SpurLocal commands are still
        // included unconditionally via registry's ensure_cache).
        //
        // IMPORTANT — DO NOT "SIMPLIFY":
        // `owned_fallback` is declared on its own line BEFORE the match so its
        // storage outlives the `&owned_fallback` reference produced inside the
        // `None` arm. Rewriting this as `match ... { None => &CommandRegistry::new() }`
        // will NOT compile: the temporary returned by `CommandRegistry::new()`
        // would be dropped at the end of the arm, leaving a dangling reference.
        // This idiom intentionally trades two extra lines for a stable borrow.
        let owned_fallback;
        let cmd_registry: &crate::commands::registry::CommandRegistry =
            match self.session_detail.as_ref() {
                Some(view) => &view.command_registry,
                None => {
                    owned_fallback = crate::commands::registry::CommandRegistry::new();
                    &owned_fallback
                }
            };
        let view_src = ViewSource;
        let cmd_src = CommandSource::new(cmd_registry);
        let sess_src = SessionSource::from_metadata(self.metadata_store.metadata());
        let worker_src = WorkerSource::from_lineage(&self.lineage);

        let view_batch = view_src.collect();
        let cmd_batch = cmd_src.collect();
        let sess_batch = sess_src.collect();
        let worker_batch = worker_src.collect();
        // Trace source is unconditionally omitted (U3c) — log the deferral
        // state, not session presence, so telemetry stays honest.
        let trace_dispatch_deferred = true;
        tracing::debug!(
            target: "palette",
            commands = cmd_batch.len(),
            sessions = sess_batch.len(),
            workers = worker_batch.len(),
            trace_dispatch_deferred,
            "open_palette: sources collected"
        );
        let batches = vec![view_batch, cmd_batch, sess_batch, worker_batch];
        // Trace source is intentionally skipped until trace-dispatch lands;
        // see docs/superpowers/specs/2026-04-20-palette-end-to-end-integration-design.md (U3c).
        // TODO(palette-trace-dispatch): re-add a TraceSource batch here when
        // Action::ScrollToTraceEntry lands with a stable-id design for TraceEntry.
        self.palette_state.extend_raw(batches);

        self.palette_visible = true;
        self.dirty = true;
    }

    /// Current ACP session id, if a `session_detail` is active.
    /// Used by the palette's `Command` accept path to construct
    /// `Action::SendMessage` / `Action::VendorExec` without a round-trip
    /// through the session-detail view.
    fn current_acp_session_id(&self) -> Option<spur_acp::SessionId> {
        self.session_detail.as_ref().map(|v| v.session_id().clone())
    }

    pub(super) fn result_to_action(
        &self,
        result: crate::components::palette::PaletteResult,
    ) -> Option<crate::action::Action> {
        use crate::action::{Action, ViewId};
        use crate::commands::registry::CommandRegistry;
        use crate::commands::submit_router::{route, SubmitDecision};
        use crate::components::palette::PalettePayload;
        match result.payload {
            PalettePayload::View { action } => Some(action),
            PalettePayload::Session { session_id } => Some(Action::ResumeSession { session_id }),
            PalettePayload::Worker { session_id } => {
                Some(Action::NavigateTo(ViewId::SessionDetail(session_id)))
            }
            PalettePayload::Command { name } => {
                // IMPORTANT — DO NOT "SIMPLIFY":
                // `owned_fallback` is declared on its own line BEFORE the match so its
                // storage outlives the `&owned_fallback` reference returned from the
                // `None` arm. Rewriting this as `match ... { None => &CommandRegistry::new() }`
                // will NOT compile: the temporary returned by `CommandRegistry::new()`
                // would be dropped at the end of the arm, leaving a dangling reference.
                // This idiom is intentionally identical to the one in `open_palette`
                // (CommandRegistry is not Clone, so we can't sidestep with .clone()).
                let owned_fallback;
                let registry: &CommandRegistry = match self.session_detail.as_ref() {
                    Some(view) => &view.command_registry,
                    None => {
                        owned_fallback = CommandRegistry::new();
                        &owned_fallback
                    }
                };
                match route(&format!("/{name}"), &[], &[], registry, false) {
                    SubmitDecision::Local { action } => Some(action),
                    SubmitDecision::Send { blocks, interrupt } => {
                        let session = self.current_acp_session_id()?;
                        Some(Action::SendMessage {
                            session,
                            blocks,
                            interrupt,
                        })
                    }
                    SubmitDecision::VendorExec { method, params } => {
                        let session = self.current_acp_session_id()?;
                        Some(Action::VendorExec {
                            session,
                            method,
                            params,
                        })
                    }
                    SubmitDecision::SetSessionConfigOption { config_id, value } => {
                        Some(Action::SetSessionConfigOption { config_id, value })
                    }
                    SubmitDecision::SetSessionModel { value } => {
                        let session_id = self.current_acp_session_id()?;
                        Some(Action::SetSessionModel { session_id, value })
                    }
                    SubmitDecision::Empty => None,
                }
            }
            PalettePayload::Trace { entry_idx: _ } => {
                // TODO(palette-trace-dispatch): wire when stable-id design lands.
                // Unreachable in practice because TraceSource is omitted from
                // extend_raw (see open_palette). Kept as a type-exhaustiveness
                // anchor and a forward-compat hook.
                None
            }
        }
    }

    pub(super) fn show_user_warning(&mut self, message: String) {
        self.user_warning = Some(message);
        self.dirty = true;
    }

    pub(super) fn dismiss_user_warning(&mut self) {
        self.user_warning = None;
        self.dirty = true;
    }

    /// Render-gate predicate for the upgrade modal. The upgrade modal is
    /// suppressed whenever a higher-precedence modal (quit_confirm or
    /// collision) is up so on-screen visibility matches input precedence
    /// (quit_confirm > collision > upgrade).
    pub(super) fn should_render_upgrade_modal(&self) -> bool {
        !self.quit_confirm_visible && self.collision_modal.is_none()
    }
}

pub(super) fn render_user_warning(frame: &mut Frame, area: ratatui::layout::Rect, message: &str) {
    use ratatui::{
        style::{Color, Modifier, Style},
        text::Line,
        widgets::{Clear, Paragraph},
    };

    if area.width == 0 || area.height == 0 {
        return;
    }

    let text = Line::styled(
        ellipsize_for_width(message, area.width),
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().bg(Color::Yellow)),
        area,
    );
}

fn ellipsize_for_width(message: &str, width: u16) -> String {
    let width = usize::from(width);
    let char_count = message.chars().count();
    if char_count <= width {
        return message.to_string();
    }
    if width <= 3 {
        return ".".repeat(width);
    }

    let mut text = message.chars().take(width - 3).collect::<String>();
    text.push_str("...");
    text
}
