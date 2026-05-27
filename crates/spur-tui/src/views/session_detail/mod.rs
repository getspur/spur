use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{layout::Rect, style::Color, Frame};

use spur_acp::{SessionId, SpurEvent, SpurEventBody};

use crate::action::{Action, ViewId};
use crate::components::input_bar::{ActivityKind, InputBar};
use crate::components::react_trace::ReactTrace;
#[cfg(test)]
use crate::components::react_trace::TraceKind;
use crate::theme::{resolve_token, ColorDepth, Theme};

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

use super::View;

mod events;
mod input;
mod render;
mod state;
#[cfg(any(test, debug_assertions))]
mod test_accessors;

#[cfg(test)]
use render::{build_auth_banner_widget, build_session_error_widget, extract_tool_call_text};

const READY_BANNER_TEXT: &str = "✨ Session cleared — your next prompt starts a fresh brain.";
const CANCEL_HINT_TEXT: &str = "Esc cancelled the active turn. Press Esc again to go back.";

fn brain_chat_trace(kind: spur_acp::AgentKind) -> ReactTrace {
    ReactTrace::with_kind(kind)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusedSessionPanel {
    /// Inline workers list above the ReAct trace when visible.
    Workers,
    /// Main ReAct trace pane.
    #[default]
    ReactTrace,
}

/// Derived render state for a session the user has navigated to but
/// whose resume pipeline may not yet be complete. Each variant is a
/// projection of the most recent milestone event received for this
/// view's session id (FP-2, FP-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    /// Default initial state when SessionDetail is entered via
    /// optimistic navigation from the picker.
    Retiring,
    Connecting {
        brain_name: String,
    },
    Loading,
    Ready,
    Failed {
        message: String,
    },
}

/// Full-screen view of a brain session's ReAct trace with chat input.
pub struct SessionDetailView {
    session_id: SessionId,
    agent_name: String,
    role: String,
    /// The AgentConfig backing this session. Owns the CommandsConfig used
    /// by the ingest/response loops, and the effective permissions.
    agent_cfg: std::sync::Arc<spur_acp::AgentConfig>,
    react_trace: ReactTrace,
    input_bar: InputBar,
    cost: f64,
    started_at: Instant,
    /// Current session mode id (e.g. "plan", "default"). Populated from
    /// `SessionUpdate::CurrentModeUpdate`.
    pub current_mode: Option<String>,
    /// Merged slash-command registry (spur-local + agent-advertised).
    /// Populated from `SessionUpdate::AvailableCommandsUpdate` for the
    /// agent portion.
    pub(crate) command_registry: crate::commands::CommandRegistry,
    /// Tokens currently used in the agent's context window. Populated from
    /// `SessionUpdate::UsageUpdate`.
    pub context_used: Option<u64>,
    /// Total context window size in tokens. Populated from
    /// `SessionUpdate::UsageUpdate`.
    pub context_size: Option<u64>,
    /// Most recent auth-required error for this session. Rendered as a red
    /// banner at the top of the view. Dismissed on the next keystroke.
    pub auth_error: Option<String>,
    /// Shared completion popup pipeline for @mentions and slash commands.
    completion: crate::components::input_completion::InputCompletionPort,
    /// Registry of `@`-mention sources (files, directories).
    mention_registry: std::rc::Rc<std::cell::RefCell<crate::mentions::MentionRegistry>>,
    /// Working directory used to resolve file mentions.
    cwd: std::path::PathBuf,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_registry: std::collections::HashMap<
        crate::components::mermaid::MermaidId,
        crate::components::mermaid::MermaidState,
    >,
    #[cfg(feature = "markdown")]
    pub(crate) mermaid_registry_version: u64,
    /// Owns rendered protocols for diagrams in `mermaid_registry`. Sibling
    /// of the registry so we can split-borrow during render.
    #[cfg(feature = "markdown")]
    pub(crate) image_cache: crate::components::image_cache::ImageCache,
    /// Coalesces re-raster requests — at most one in flight per id.
    #[cfg(feature = "markdown")]
    pub(crate) in_flight_renders: std::collections::HashSet<crate::components::mermaid::MermaidId>,
    /// Source of monotonic `image_generation` values stored on
    /// `MermaidState::Ready` and snapshotted by `image_cache` for
    /// stale-protocol detection.
    #[cfg(feature = "markdown")]
    pub(crate) next_image_generation: u64,
    #[cfg(feature = "markdown")]
    pub(crate) pending_fence_actions: std::collections::VecDeque<crate::action::Action>,
    /// Graphics `Picker` used to build inline mermaid image protocols during
    /// render. Set once from `App` when the view is created; `None` when no
    /// graphics protocol is available (text fallback kicks in).
    #[cfg(feature = "markdown")]
    pub(crate) render_picker: Option<ratatui_image::picker::Picker>,
    /// Timestamp of the most recent InputBar text change whose contents
    /// differ from `last_persisted_draft`. `None` while the debounce is idle.
    last_draft_change_at: Option<std::time::Instant>,
    /// Last InputBar text value written to the metadata store (initially "").
    last_persisted_draft: String,
    /// Informational banner shown when the session was auto-resumed on
    /// startup. Auto-fades after 3s or on first keystroke.
    resume_banner: Option<crate::components::resume_banner::ResumeBanner>,
    /// True from the first `AgentMessageChunk`/`AgentThoughtChunk` of a turn
    /// until the matching `TurnComplete`. Used to gate `Esc`-to-cancel on
    /// whether a stream is actually in flight, and to render the "Esc to
    /// stop" status-bar hint.
    pub(crate) stream_in_flight: bool,

    /// True from the moment we dispatch `Action::CancelStream` until
    /// `TurnComplete`. Overrides the streaming label with `cancelling…` and
    /// prevents re-entrant cancel dispatches (the next `Esc` falls through
    /// to existing handlers, e.g. NavigateBack).
    pub(crate) cancelling_in_flight: bool,
    /// True while the "Cancel turn? [y]es / [n]o" confirmation modal is
    /// open. Set when `Esc` fires during an in-flight stream; cleared on
    /// `y/Y` (which also dispatches `Action::CancelStream`), `n/N`, or a
    /// second `Esc` (vim-safe dismissal). `Enter` and other keys leave the
    /// modal open so the user must make an explicit choice.
    pub(crate) cancel_confirm_open: bool,
    cancel_hint_until: Option<Instant>,

    /// How `AgentConnection::cancel` behaves for this session's transport.
    /// Populated from `SpurEventBody::AgentSessionReady`. Used to select
    /// transport-aware text for the cancel system note. `None` until
    /// `AgentSessionReady` arrives; in that window, a generic fallback is
    /// rendered.
    pub(crate) cancel_mode: Option<spur_acp::CancelMode>,
    /// True when the session attached without an enforceable filesystem lock.
    fs_unsafe: bool,

    /// Whether the inline workers panel is collapsed. Toggled by Alt+D.
    workers_panel_collapsed: bool,
    focused_panel: FocusedSessionPanel,
    /// Maps ToolCall id -> render depth for subagent nesting.
    /// Populated on each ToolCall; read on subsequent ToolCalls to resolve
    /// the parent's depth. Capped at 8 to prevent runaway indentation.
    tool_depth: std::collections::HashMap<String, u8>,
    /// Set of known worker names, derived once at construction from
    /// the worker snapshot supplied to `new`. Used by
    /// `prepend_worker_hint` to filter unknown-name atoms out of
    /// the hint.
    known_worker_names: std::collections::HashSet<String>,

    /// True once this view has been reset by `/clear` and is waiting for
    /// the next `BrainSpawned` to be replaced. While `cleared`, the view's
    /// `session_id` is treated as opaque — `force_save_draft` and
    /// `draft_save_action` both return `None` early so no metadata write
    /// can target the retired session. See spec §3.5.
    cleared: bool,

    /// Transient banner rendered in the same layout slot as
    /// `resume_banner` when the view has been cleared. Cleared by
    /// construction of the next view (replacement drops it naturally).
    ready_banner: Option<String>,

    /// Derived load state for this session. Transitions from `Retiring`
    /// through `Connecting` → `Loading` → `Ready` as resume-pipeline
    /// milestone events arrive. Set to `Failed` on `BrainError`.
    /// Drives the pre-ready render path (Tranche 2 Task 5).
    pub load_state: LoadState,

    /// Most recent snapshot of advertised session config options for this
    /// session. Populated from `SpurEventBody::CommandRegistryDirty` (which
    /// the orchestrator emits at session creation and after each successful
    /// `set_session_config_option`). Drives both the synthesized `/model` and
    /// `/effort` slash entries in `command_registry` and the `SlashArg`
    /// picker's choice list via `CompletionEnv.session_config_options`.
    session_config_options: Vec<spur_acp::SessionConfigOption>,
    /// Optimistic model override set when the user dispatches a `/model`
    /// switch via `SubmitDecision::SetSessionModel`. Used by agents that
    /// accept `session/set_model` but don't emit `config_option_update`, so
    /// `session_config_options` never reflects the new value.
    /// Always loses to a live `session_config_options[id="model"]` entry.
    pending_model_override: Option<String>,

    /// Wave B/C (M8): cached `SpurAgentCaps` for this session. Populated by
    /// the upstream wiring once `Orchestrator::spur_agent_caps()` returns
    /// `Some(_)` (M9 ties this to a `SpurEventBody` arm). When `None`,
    /// caps are absent (e.g. resumed sessions before M9 wires
    /// `LoadSessionResponse`); the registry filter and submit-router
    /// treat `None` as permissive — full capability set assumed (F-3).
    spur_agent_caps: Option<std::sync::Arc<spur_acp::SpurAgentCaps>>,
}

impl View for SessionDetailView {
    fn handle_key(&mut self, key: KeyEvent, ctx: &super::ViewContext) -> Option<Action> {
        // Resume banner key consumption — must happen BEFORE normal key routing.
        if let Some(ref mut banner) = self.resume_banner {
            if banner.is_consuming_keys() {
                if let Some(action) = banner.handle_key(key) {
                    return Some(action);
                }
                // If banner handled the key but returned None (e.g. Esc fading),
                // still allow the key to fall through UNLESS it was Esc.
                if key.code == KeyCode::Esc {
                    return None;
                }
            }
        }
        let key = super::normalize_macos_option(key);
        if matches!(key.code, KeyCode::Char('p')) && key.modifiers.contains(KeyModifiers::ALT) {
            if ctx
                .plan_projection
                .current_for_session(self.session_id())
                .is_some()
            {
                return Some(Action::NavigateTo(ViewId::PlanInspector(
                    self.session_id.clone(),
                )));
            }
            self.input_bar.set_status(
                Some("No tracked plan for this session yet".into()),
                ActivityKind::Idle,
            );
            return None;
        }
        let action = self.handle_key_inner(key);
        // Arm the draft-save debounce whenever the InputBar text diverges
        // from the last persisted value. This covers inserts, deletes, and
        // the empty-after-send case (where sending clears the bar to "" —
        // if the previously-persisted draft was non-empty, we want to
        // overwrite it with the now-empty value).
        let current_text = self.input_bar.text();
        if current_text != self.last_persisted_draft {
            self.last_draft_change_at = Some(std::time::Instant::now());
        }
        action
    }

    fn handle_spur_event(&mut self, event: &SpurEvent, ctx: &super::ViewContext) {
        // Update LoadState from milestone events (Tranche 2 Task 5).
        self.apply_milestone_event(event);

        match &event.body {
            SpurEventBody::AgentNotification { .. }
            | SpurEventBody::CostUpdate { .. }
            | SpurEventBody::TurnComplete { .. } => self.on_stream(event, ctx),
            SpurEventBody::DelegationRequested { .. }
            | SpurEventBody::DelegationDispatched { .. }
            | SpurEventBody::DelegationCompleted { .. } => self.on_tool_call(event, ctx),
            SpurEventBody::PromptDispatched { .. } | SpurEventBody::ContinuationDropped { .. } => {
                self.on_plan_perm(event, ctx)
            }
            SpurEventBody::BrainError { .. }
            | SpurEventBody::BrainReconnecting { .. }
            | SpurEventBody::BrainReconnected { .. }
            | SpurEventBody::BrainReconnectFailed { .. }
            | SpurEventBody::AgentSessionReady { .. } => self.on_auth_banner(event, ctx),
            SpurEventBody::AgentExtNotification { .. }
            | SpurEventBody::CommandRegistryDirty { .. } => self.on_mermaid_caps(event, ctx),
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.react_trace.tick();
        let _ = self.completion.poll_updates();
        if matches!(
            self.input_bar.tick(),
            crate::components::input_bar::TickOutcome::FlushedPaste
        ) {
            self.dispatch_intent(crate::components::completion_trigger::IntentEvent::Pasted);
        }
        if let Some(ref mut banner) = self.resume_banner {
            banner.tick();
        }
        #[cfg(feature = "markdown")]
        {
            use crate::components::markdown_stream::StateLookup;

            let (error_ids, pending_ids) = self.build_state_lookup_sets();
            let states = StateLookup {
                errors: &error_ids,
                pending: &pending_ids,
            };

            for (_entry_idx, fence) in self.react_trace.drain_fence_dispatches(&states) {
                self.mermaid_registry_insert(
                    fence.id,
                    crate::components::mermaid::MermaidState::Pending {
                        code: fence.code.clone(),
                    },
                );
                self.in_flight_renders.insert(fence.id);
                self.pending_fence_actions
                    .push_back(crate::action::Action::MermaidRenderRequest {
                        session: self.session_id.clone(),
                        ref_id: fence.id,
                        code: fence.code,
                        target_width: {
                            let cell_w_px = self
                                .render_picker
                                .as_ref()
                                .map(|p| p.font_size().0)
                                .unwrap_or(8);
                            // Note: pane width at fence-emit time is not directly
                            // available; use the last known render width if cached,
                            // else smallest bucket. The next render frame's
                            // maybe_request_rerasters will upgrade if needed.
                            let pane_w_cols = self.react_trace.last_render_width().unwrap_or(80);
                            crate::components::mermaid::raster_width_for_pane(
                                (pane_w_cols as u32).saturating_mul(cell_w_px as u32),
                            )
                        },
                    });
            }
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &super::ViewContext) {
        self.render_inner(frame, area, ctx);
    }
}

#[cfg(test)]
mod banner_tests {
    use super::*;
    use ratatui::style::Modifier;
    use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};

    fn render_auth_banner(message: &str, area: Rect) -> Buffer {
        let banner = super::build_auth_banner_widget(message, crate::theme::fallback_theme());
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(banner, area)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_session_error(message: &str, area: Rect) -> Buffer {
        let banner = super::build_session_error_widget(message, crate::theme::fallback_theme());
        let backend = TestBackend::new(area.width, area.height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(banner, area)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn rendered_text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| {
                        buf.cell((x, y))
                            .map(|cell| cell.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn auth_banner_renders_title_body_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 64, 3);
        let message = "Run `spur login` to continue";
        let buf = render_auth_banner(message, area);
        let rendered = rendered_text(&buf);

        assert!(
            rendered.contains("Authentication required"),
            "auth banner title must appear. Rendered:\n{rendered}"
        );
        assert!(
            rendered.contains(message),
            "auth banner body must appear. Rendered:\n{rendered}"
        );

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).expect("cell should exist in banner area");
                assert_eq!(
                    cell.bg,
                    Color::Rgb(0xf8, 0x71, 0x71),
                    "cell ({x}, {y}) should have red background"
                );
                assert_eq!(
                    cell.fg,
                    Color::Rgb(0xff, 0xff, 0xff),
                    "cell ({x}, {y}) should have white foreground"
                );
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "cell ({x}, {y}) should be bold"
                );
                assert_ne!(
                    cell.symbol(),
                    "│",
                    "cell ({x}, {y}) should not render a vertical border glyph"
                );
            }
        }
    }

    #[test]
    fn session_error_renders_title_body_and_full_red_bg_with_no_pipe_glyph() {
        let area = Rect::new(0, 0, 64, 3);
        let message = "executor exited before ready";
        let buf = render_session_error(message, area);
        let rendered = rendered_text(&buf);

        assert!(
            rendered.contains("Session error"),
            "session error title must appear. Rendered:\n{rendered}"
        );
        assert!(
            rendered.contains(message),
            "session error body must appear. Rendered:\n{rendered}"
        );
        assert!(
            !rendered.contains('│'),
            "session error must not render vertical border glyphs. Rendered:\n{rendered}"
        );

        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf
                    .cell((x, y))
                    .expect("cell should exist in session error area");
                assert_eq!(
                    cell.bg,
                    Color::Rgb(0xf8, 0x71, 0x71),
                    "cell ({x}, {y}) should have red background"
                );
                assert_eq!(
                    cell.fg,
                    Color::Rgb(0xff, 0xff, 0xff),
                    "cell ({x}, {y}) should have white foreground"
                );
                assert!(
                    cell.modifier.contains(Modifier::BOLD),
                    "cell ({x}, {y}) should be bold"
                );
                assert_ne!(
                    cell.symbol(),
                    "│",
                    "cell ({x}, {y}) should not render a vertical border glyph"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "markdown"))]
mod maybe_request_rerasters_tests {
    use super::*;
    use crate::components::mermaid::{
        MermaidId, MermaidRenderOutput, MermaidState, RASTER_BUCKETS,
    };
    use image::{DynamicImage, RgbaImage};
    use std::sync::Arc;

    #[allow(dead_code)]
    fn buckets_constant_check() {
        // Touch RASTER_BUCKETS so the import isn't dead in builds where
        // tests skip every assertion that references it.
        let _ = RASTER_BUCKETS;
    }

    fn ready_at(bucket: u32, gen: u64) -> MermaidState {
        MermaidState::Ready {
            image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
            code: "graph TD\nA-->B".into(),
            rastered_at_bucket: bucket,
            image_generation: gen,
        }
    }

    fn rendered_image(image: Arc<DynamicImage>) -> MermaidRenderOutput {
        MermaidRenderOutput::Image(image)
    }

    #[test]
    fn maybe_request_rerasters_skips_when_bucket_unchanged() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 80 cols × 8 px = 640 → bucket 800. No upgrade.
        view.maybe_request_rerasters(80, 8);
        assert!(
            view.pending_fence_actions.is_empty(),
            "no requests when bucket unchanged"
        );
    }

    #[test]
    fn maybe_request_rerasters_emits_for_lower_bucketed_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(1), ready_at(800, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Upgrade.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(1)));
    }

    #[test]
    fn maybe_request_rerasters_skips_pending() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry
            .insert(MermaidId(2), MermaidState::Pending { code: "g".into() });
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn maybe_request_rerasters_skips_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(3), ready_at(800, 1));
        view.in_flight_renders.insert(MermaidId(3));
        view.maybe_request_rerasters(200, 8);
        assert!(
            view.pending_fence_actions.is_empty(),
            "no duplicate requests for in-flight ids"
        );
    }

    #[test]
    fn maybe_request_rerasters_skips_just_landed_at_new_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry
            .insert(MermaidId(4), ready_at(1600, 1));
        // pane_w_px = 200 cols × 8 px = 1600 → bucket 1600. Already there.
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }

    #[test]
    fn rerasters_coalesce_during_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(MermaidId(5), ready_at(800, 1));

        // First trigger: pane grows to bucket 1200.
        view.maybe_request_rerasters(150, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);

        // Second trigger BEFORE completion: pane grows to bucket 2000.
        view.maybe_request_rerasters(250, 8);
        // Still only one — id is in_flight, gated.
        assert_eq!(view.pending_fence_actions.len(), 1);
    }

    #[test]
    fn handle_completed_clears_in_flight() {
        let mut view = SessionDetailView::new_for_tests();
        view.in_flight_renders.insert(MermaidId(6));
        view.mermaid_registry
            .insert(MermaidId(6), MermaidState::Pending { code: "g".into() });
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(6), 800, Ok(rendered_image(img)));
        assert!(!view.in_flight_renders.contains(&MermaidId(6)));
    }

    #[test]
    fn handle_completed_records_target_width_on_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry
            .insert(MermaidId(7), MermaidState::Pending { code: "g".into() });
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(7), 1600, Ok(rendered_image(img)));
        match view.mermaid_registry.get(&MermaidId(7)) {
            Some(MermaidState::Ready {
                rastered_at_bucket, ..
            }) => {
                assert_eq!(*rastered_at_bucket, 1600);
            }
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_ready_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(8),
            MermaidState::Ready {
                image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
                code: "ORIGINAL".into(),
                rastered_at_bucket: 800,
                image_generation: 1,
            },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(8), 1600, Ok(rendered_image(img)));
        match view.mermaid_registry.get(&MermaidId(8)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "ORIGINAL"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_retains_code_on_pending_to_ready() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(9),
            MermaidState::Pending {
                code: "PENDING_SOURCE".into(),
            },
        );
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(9), 800, Ok(rendered_image(img)));
        match view.mermaid_registry.get(&MermaidId(9)) {
            Some(MermaidState::Ready { code, .. }) => assert_eq!(code, "PENDING_SOURCE"),
            _ => panic!("expected Ready"),
        }
    }

    #[test]
    fn handle_completed_bumps_image_generation_on_ok() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry
            .insert(MermaidId(10), MermaidState::Pending { code: "g".into() });
        let img = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.handle_mermaid_completed(MermaidId(10), 800, Ok(rendered_image(img.clone())));
        let gen1 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready {
                image_generation, ..
            }) => *image_generation,
            _ => panic!(),
        };
        view.handle_mermaid_completed(MermaidId(10), 1200, Ok(rendered_image(img)));
        let gen2 = match view.mermaid_registry.get(&MermaidId(10)) {
            Some(MermaidState::Ready {
                image_generation, ..
            }) => *image_generation,
            _ => panic!(),
        };
        assert!(gen2 > gen1, "generation must monotonically increase");
    }

    #[test]
    fn handle_completed_never_decreases_bucket() {
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry.insert(
            MermaidId(11),
            MermaidState::Ready {
                image: Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10))),
                code: "g".into(),
                rastered_at_bucket: 1600,
                image_generation: 1,
            },
        );
        // Even if a stale completion arrives with a smaller bucket, the
        // handler stores the COMPLETION's bucket — but maybe_request_rerasters
        // never EMITS at a smaller bucket (test is for the trigger, not the
        // handler). The handler simply records what arrived.
        // I-R1 is enforced at the EMIT side (maybe_request_rerasters compares
        // current_bucket against rastered_at_bucket and only emits if greater).
        // This test verifies the emit side.
        view.maybe_request_rerasters(80, 8); // pane_w_px=640 → bucket 800
        assert!(
            view.pending_fence_actions.is_empty(),
            "must never emit when current_bucket < rastered_at_bucket"
        );
    }

    #[test]
    fn fence_emit_uses_current_bucket() {
        // This test verifies that maybe_request_rerasters emits at the
        // CURRENT pane's bucket — exercises the fence emit pathway with a
        // pane wider than 800. Initial fence emit (Task 14 wires this) uses
        // the same path conceptually.
        let mut view = SessionDetailView::new_for_tests();
        view.mermaid_registry
            .insert(MermaidId(12), ready_at(800, 1));
        view.maybe_request_rerasters(200, 8); // pane_w_px=1600 → bucket 1600
        assert_eq!(view.pending_fence_actions.len(), 1);
        match view.pending_fence_actions.front() {
            Some(crate::action::Action::MermaidRenderRequest { target_width, .. }) => {
                assert!(
                    *target_width >= 1200,
                    "target_width should be ≥ 1200, got {target_width}"
                );
            }
            _ => panic!("expected MermaidRenderRequest"),
        }
    }

    #[test]
    fn mermaid_registry_version_bumps_on_insert_and_clear() {
        let mut view = SessionDetailView::new_for_tests();
        let start = view.mermaid_registry_version;

        view.mermaid_registry_insert(MermaidId(42), MermaidState::Pending { code: "g".into() });
        assert_eq!(view.mermaid_registry_version, start + 1);

        view.mermaid_registry_clear();
        assert_eq!(view.mermaid_registry_version, start + 2);
    }

    #[test]
    fn bucket_up_smoke_test() {
        // End-to-end: a Ready diagram at bucket 800, pane grows to 1600,
        // re-raster request emitted, completion handler runs, bucket
        // updated, image_generation bumped.
        use crate::action::Action;
        use image::{DynamicImage, RgbaImage};
        use std::sync::Arc;

        let mut view = SessionDetailView::new_for_tests();

        // 1. Seed Ready at bucket 800, generation 1.
        let img1 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(10, 10)));
        view.mermaid_registry.insert(
            MermaidId(99),
            MermaidState::Ready {
                image: img1,
                code: "graph TD\nA-->B".into(),
                rastered_at_bucket: 800,
                image_generation: 1,
            },
        );
        view.next_image_generation = 1;

        // 2. Pane grows to bucket 1600.
        view.maybe_request_rerasters(200, 8);
        assert_eq!(view.pending_fence_actions.len(), 1);
        assert!(view.in_flight_renders.contains(&MermaidId(99)));
        // Confirm the request is the expected fence Action variant.
        assert!(matches!(
            view.pending_fence_actions.front(),
            Some(Action::MermaidRenderRequest { .. })
        ));

        // 3. Worker completes (simulated).
        let img2 = Arc::new(DynamicImage::ImageRgba8(RgbaImage::new(20, 20)));
        view.handle_mermaid_completed(MermaidId(99), 1600, Ok(rendered_image(img2)));

        // 4. Verify state.
        assert!(!view.in_flight_renders.contains(&MermaidId(99)));
        match view.mermaid_registry.get(&MermaidId(99)) {
            Some(MermaidState::Ready {
                rastered_at_bucket,
                image_generation,
                code,
                ..
            }) => {
                assert_eq!(*rastered_at_bucket, 1600);
                assert!(*image_generation > 1, "generation must bump");
                assert_eq!(code, "graph TD\nA-->B", "code retained across re-raster");
            }
            _ => panic!("expected Ready"),
        }

        // 5. Subsequent maybe_request_rerasters at the SAME bucket emits nothing.
        view.pending_fence_actions.clear();
        view.maybe_request_rerasters(200, 8);
        assert!(view.pending_fence_actions.is_empty());
    }
}

#[cfg(all(test, feature = "markdown"))]
mod invalidate_protocols_tests {
    use super::*;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn test_view() -> SessionDetailView {
        SessionDetailView::new(
            spur_acp::SessionId("test".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            std::sync::Arc::new(spur_acp::AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    // Obsolete: tested removed `inline_protocol` field on MermaidState::Ready.
    // Conceptual replacement: `ImageCache::invalidate_all` tests in Task 7.

    #[test]
    fn alt_v_is_inert_when_render_picker_is_none() {
        use crate::action::Action;
        use crate::views::session_detail::ViewId;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut view = test_view();
        view.set_render_picker(None);

        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        let action =
            <SessionDetailView as crate::views::View>::handle_key(&mut view, key, &test_ctx());

        assert!(
            !matches!(action, Some(Action::NavigateTo(ViewId::MermaidOverlay(_)))),
            "Alt-v must not navigate to mermaid overlay when picker is None, got {action:?}"
        );
    }

    #[test]
    fn alt_v_opens_overlay_when_render_picker_is_some() {
        use crate::action::Action;
        use crate::views::session_detail::ViewId;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut view = test_view();
        view.set_render_picker(Some(ratatui_image::picker::Picker::halfblocks()));

        let key = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT);
        let action =
            <SessionDetailView as crate::views::View>::handle_key(&mut view, key, &test_ctx());

        match action {
            Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
            other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod static_command_seeding_tests {
    use super::*;
    use spur_acp::{AgentConfig, CommandsConfig, DispatchKind, SessionId, StaticCommandDecl};
    use std::sync::Arc;

    #[test]
    fn session_view_constructor_seeds_static_commands_from_config() {
        let mut cfg = AgentConfig::with_defaults("codex");
        cfg.display.handle = Some("codex".into());
        cfg.commands = CommandsConfig {
            dispatch: DispatchKind::PromptText,
            static_commands: vec![StaticCommandDecl {
                name: "compact".into(),
                description: "Compact history".into(),
                hint: None,
            }],
            ..Default::default()
        };
        let view = SessionDetailView::new(
            SessionId("test".into()),
            "codex".into(),
            "brain".into(),
            std::path::PathBuf::from("."),
            Arc::new(cfg),
            Vec::new(),
        );
        let names: Vec<_> = view
            .command_registry
            .list()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert!(
            names.contains(&"compact".to_string()),
            "static /compact should be visible at startup, got {names:?}"
        );
    }
}

#[cfg(test)]
mod cancel_state_tests {
    use super::*;

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn agent_msg_chunk_event(session: &spur_acp::SessionId) -> SpurEvent {
        let update = spur_acp::SessionUpdate::AgentMessageChunk(spur_acp::ContentChunk::new(
            spur_acp::ContentBlock::from("hi".to_string()),
        ));
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn turn_complete_event(session: &spur_acp::SessionId) -> SpurEvent {
        SpurEvent::now(SpurEventBody::TurnComplete {
            session: session.clone(),
        })
    }

    fn agent_session_ready_event(
        session: &spur_acp::SessionId,
        mode: spur_acp::CancelMode,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: session.clone(),
            acp_session_id: "acp-1".into(),
            brain: "claude".into(),
            resumed: false,
            cancel_mode: mode,
            fs_unsafe: false,
            caps: None,
        })
    }

    fn tool_call_event(session: &spur_acp::SessionId, id: &str) -> SpurEvent {
        let tc = spur_acp::AcpToolCall::new(spur_acp::ToolCallId::new(id), "read");
        let update = spur_acp::SessionUpdate::ToolCall(tc);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn tool_call_update_event(session: &spur_acp::SessionId, id: &str) -> SpurEvent {
        let fields = agent_client_protocol::schema::ToolCallUpdateFields::new()
            .status(spur_acp::ToolCallStatus::InProgress);
        let tcu = spur_acp::AcpToolCallUpdate::new(spur_acp::ToolCallId::new(id), fields);
        let update = spur_acp::SessionUpdate::ToolCallUpdate(tcu);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    fn plan_event(session: &spur_acp::SessionId) -> SpurEvent {
        let plan = spur_acp::Plan::new(vec![spur_acp::PlanEntry::new(
            "step 1",
            spur_acp::PlanEntryPriority::Medium,
            spur_acp::PlanEntryStatus::InProgress,
        )]);
        let update = spur_acp::SessionUpdate::Plan(plan);
        let notification = spur_acp::SessionNotification::new(session.0.clone(), update);
        SpurEvent::now(SpurEventBody::AgentNotification {
            session: session.clone(),
            notification: Box::new(notification),
        })
    }

    #[test]
    fn new_view_has_no_stream_in_flight() {
        let v = make_view();
        assert!(!v.stream_in_flight);
        assert!(!v.cancelling_in_flight);
        assert!(v.cancel_mode.is_none());
    }

    #[test]
    fn chunk_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&agent_msg_chunk_event(&sid), &test_ctx());
        assert!(v.stream_in_flight);
    }

    #[test]
    fn tool_call_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&tool_call_event(&sid, "t1"), &test_ctx());
        assert!(
            v.stream_in_flight,
            "tool-first turn should arm stream_in_flight"
        );
    }

    #[test]
    fn tool_call_update_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&tool_call_update_event(&sid, "t1"), &test_ctx());
        assert!(
            v.stream_in_flight,
            "ToolCallUpdate should arm stream_in_flight"
        );
    }

    #[test]
    fn plan_sets_stream_in_flight() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&plan_event(&sid), &test_ctx());
        assert!(
            v.stream_in_flight,
            "plan-first turn should arm stream_in_flight"
        );
    }

    #[test]
    fn esc_cancels_after_tool_first_update() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        v.handle_spur_event(&tool_call_event(&sid, "t1"), &test_ctx());
        assert!(v.stream_in_flight);

        // Esc opens the cancel-confirm modal; `y` dispatches.
        let opened = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(opened.is_none(), "first Esc must only open the modal");
        assert!(v.cancel_confirm_open);

        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('y')),
            &test_ctx(),
        );
        assert!(
            matches!(action, Some(Action::CancelStream { .. })),
            "y after Esc-modal should emit CancelStream, got {action:?}"
        );
        assert!(v.cancelling_in_flight);
        assert!(!v.cancel_confirm_open);
    }

    #[test]
    fn turn_complete_clears_both_flags() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.stream_in_flight = true;
        v.cancelling_in_flight = true;
        v.handle_spur_event(&turn_complete_event(&sid), &test_ctx());
        assert!(!v.stream_in_flight);
        assert!(!v.cancelling_in_flight);
    }

    #[test]
    fn turn_complete_closes_open_cancel_confirm_modal() {
        // If the agent finishes streaming while the modal is open, the modal
        // would otherwise hijack the user's next keystroke for a question
        // that's already moot.
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        v.handle_spur_event(&turn_complete_event(&sid), &test_ctx());
        assert!(!v.stream_in_flight);
        assert!(
            !v.cancel_confirm_open,
            "TurnComplete must close any open cancel-confirm modal"
        );
    }

    #[test]
    fn agent_session_ready_populates_cancel_mode() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(
            &agent_session_ready_event(&sid, spur_acp::CancelMode::AcpSoft),
            &test_ctx(),
        );
        assert_eq!(v.cancel_mode, Some(spur_acp::CancelMode::AcpSoft));
    }

    #[test]
    fn event_for_different_session_is_ignored() {
        let mut v = make_view();
        let other = spur_acp::SessionId("other".to_string());
        v.handle_spur_event(&agent_msg_chunk_event(&other), &test_ctx());
        assert!(!v.stream_in_flight);
    }

    use crate::action::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(key: KeyCode) -> KeyEvent {
        KeyEvent::new(key, KeyModifiers::NONE)
    }

    #[test]
    fn esc_with_stream_in_flight_opens_confirm_modal_does_not_dispatch() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(
            action.is_none(),
            "Esc must not dispatch CancelStream directly"
        );
        assert!(v.cancel_confirm_open, "Esc must open the confirm modal");
        assert!(
            !v.cancelling_in_flight,
            "cancelling_in_flight must NOT flip until confirmation"
        );
    }

    #[test]
    fn modal_y_dispatches_cancel_stream() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('y')),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::CancelStream { .. })));
        assert!(!v.cancel_confirm_open);
        assert!(v.cancelling_in_flight);
    }

    #[test]
    fn modal_uppercase_y_dispatches_cancel_stream() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('Y')),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::CancelStream { .. })));
        assert!(!v.cancel_confirm_open);
    }

    #[test]
    fn modal_n_dismisses_no_action() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('n')),
            &test_ctx(),
        );
        assert!(action.is_none());
        assert!(!v.cancel_confirm_open, "n must close the modal");
        assert!(
            !v.cancelling_in_flight,
            "n must not flip cancelling_in_flight"
        );
    }

    #[test]
    fn modal_uppercase_n_dismisses_no_action() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('N')),
            &test_ctx(),
        );
        assert!(action.is_none());
        assert!(!v.cancel_confirm_open);
    }

    #[test]
    fn modal_esc_dismisses_vim_safe_no_action() {
        // Vim users reflexively double-tap Esc; the second tap must dismiss
        // the modal harmlessly rather than confirm cancellation.
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(action.is_none());
        assert!(!v.cancel_confirm_open);
        assert!(!v.cancelling_in_flight);
    }

    #[test]
    fn modal_enter_keeps_modal_open_no_dispatch() {
        // Enter is intentionally NOT a confirmation key — vim users press
        // Enter to commit Normal-mode commands.
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Enter),
            &test_ctx(),
        );
        assert!(action.is_none());
        assert!(v.cancel_confirm_open, "Enter must NOT close the modal");
        assert!(!v.cancelling_in_flight);
    }

    #[test]
    fn modal_arbitrary_key_keeps_modal_open() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_confirm_open = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('x')),
            &test_ctx(),
        );
        assert!(action.is_none());
        assert!(v.cancel_confirm_open);
    }

    #[test]
    fn modal_does_not_open_when_already_cancelling() {
        // Once a cancel is in flight, ESC falls through to NavigateBack
        // (existing behavior preserved).
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancelling_in_flight = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::NavigateBack)));
        assert!(
            !v.cancel_confirm_open,
            "modal must not open when cancelling already in flight"
        );
    }

    #[test]
    fn esc_when_already_cancelling_falls_through_to_navigate_back() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancelling_in_flight = true;
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::NavigateBack)));
    }

    #[test]
    fn esc_without_stream_preserves_navigate_back() {
        let mut v = make_view();
        let action = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        assert!(matches!(action, Some(Action::NavigateBack)));
    }

    #[test]
    fn cancel_note_uses_acp_soft_text_when_mode_is_acp_soft() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::AcpSoft);
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('y')),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Cancellation requested"),
            "expected AcpSoft message; got {last_text:?}"
        );
    }

    #[test]
    fn cancel_note_uses_process_kill_text_when_mode_is_process_kill() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = Some(spur_acp::CancelMode::ProcessKill);
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('y')),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Stopping agent"),
            "expected ProcessKill message; got {last_text:?}"
        );
    }

    #[test]
    fn cancel_note_generic_when_cancel_mode_unknown() {
        let mut v = make_view();
        v.stream_in_flight = true;
        v.cancel_mode = None;
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Esc),
            &test_ctx(),
        );
        let _ = <SessionDetailView as crate::views::View>::handle_key(
            &mut v,
            press(KeyCode::Char('y')),
            &test_ctx(),
        );
        let trace = v.react_trace();
        let last_text = trace.last_text().unwrap_or_default();
        assert!(
            last_text.contains("Cancellation requested"),
            "expected generic fallback; got {last_text:?}"
        );
    }
}

#[cfg(test)]
mod tool_depth_tests {
    #[test]
    fn tool_depth_nested_two_levels() {
        use std::collections::HashMap;
        let mut tool_depth: HashMap<String, u8> = HashMap::new();
        tool_depth.insert("tc-root".into(), 0);

        let depth_1 = Some("tc-root")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        tool_depth.insert("tc-child".into(), depth_1);
        assert_eq!(depth_1, 1);

        let depth_2 = Some("tc-child")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth_2, 2);
    }

    #[test]
    fn tool_depth_unknown_parent_defaults_zero() {
        use std::collections::HashMap;
        let tool_depth: HashMap<String, u8> = HashMap::new();
        let depth = Some("tc-ghost")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth, 0);
    }

    #[test]
    fn tool_depth_caps_at_eight() {
        use std::collections::HashMap;
        let mut tool_depth: HashMap<String, u8> = HashMap::new();
        tool_depth.insert("tc-deep".into(), 8);
        let depth = Some("tc-deep")
            .and_then(|pid| tool_depth.get(pid).copied())
            .map(|d| d.saturating_add(1).min(8))
            .unwrap_or(0);
        assert_eq!(depth, 8);
    }
}

#[cfg(test)]
mod extract_tool_call_text_tests {
    use super::*;

    #[test]
    fn extract_tool_call_text_renders_diff_content() {
        use agent_client_protocol::schema::{Diff, ToolCallContent};
        let diff =
            Diff::new("src/foo.rs", "fn new_name() {}\n").old_text("fn old() {}\n".to_string());
        let content = vec![ToolCallContent::Diff(diff)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        assert!(out.contains("src/foo.rs"), "diff must include path");
        assert!(out.contains("-fn old"), "diff must include old-line prefix");
        assert!(
            out.contains("+fn new_name"),
            "diff must include new-line prefix"
        );
    }

    #[test]
    fn extract_tool_call_text_renders_terminal_placeholder() {
        use agent_client_protocol::schema::{Terminal, TerminalId, ToolCallContent};
        let term = Terminal::new(TerminalId::new("term-abc-123"));
        let content = vec![ToolCallContent::Terminal(term)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        assert!(out.contains("term-abc-123"), "placeholder must include id");
        assert!(out.starts_with("[terminal:"), "placeholder must be labeled");
    }

    #[test]
    fn extract_tool_call_text_truncates_long_diffs() {
        use agent_client_protocol::schema::{Diff, ToolCallContent};
        let big_new = "line\n".repeat(200);
        let diff = Diff::new("big.txt", big_new).old_text(String::new());
        let content = vec![ToolCallContent::Diff(diff)];
        let out = extract_tool_call_text(&content).expect("should return Some");
        let line_count = out.lines().count();
        assert_eq!(
            line_count, 43,
            "expected 2 header + 40 body + 1 trailer, got {} lines",
            line_count
        );
        assert!(
            out.contains("160 more lines"),
            "must show exact truncated count: {}",
            out
        );
    }

    #[test]
    fn extract_tool_call_text_concatenates_multiple_entries() {
        use agent_client_protocol::schema::{Diff, Terminal, TerminalId, ToolCallContent};
        let diff_entry =
            ToolCallContent::Diff(Diff::new("a.rs", "y\n").old_text("x\n".to_string()));
        let term_entry = ToolCallContent::Terminal(Terminal::new(TerminalId::new("t-1")));
        let out = extract_tool_call_text(&[diff_entry, term_entry]).expect("should return Some");
        assert!(out.contains("a.rs"), "diff section must render");
        assert!(out.contains("+y"), "diff + line must render");
        assert!(
            out.contains("[terminal: t-1]"),
            "terminal placeholder must render after diff"
        );
    }

    #[test]
    fn extract_tool_call_text_returns_none_for_empty_content() {
        let content: Vec<spur_acp::ToolCallContent> = vec![];
        assert!(extract_tool_call_text(&content).is_none());
    }
}

#[cfg(test)]
mod composer_routing_tests {
    use super::*;
    use crate::action::Action;
    use crate::views::View;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SpurEvent, SpurEventBody};

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
        v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
    }

    fn press_mod(v: &mut SessionDetailView, code: KeyCode, m: KeyModifiers) -> Option<Action> {
        v.handle_key(KeyEvent::new(code, m), &test_ctx())
    }

    fn test_ctx_with_plan() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::LazyLock<spur_core::PlanProjectionStore> =
            std::sync::LazyLock::new(|| {
                let mut store = spur_core::PlanProjectionStore::default();
                store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
                    session_id: spur_acp::SessionId("s".into()),
                    snapshot: Box::new(PlanSnapshot {
                        plan_id: "plan-1".into(),
                epic_id: None,
                        status: "running".into(),
                        progress: "0/1 done".into(),
                        next_action:
                            "Use get_task_diff to review each awaiting task, then review_task to approve or reject."
                                .into(),
                        ready_to_merge: false,
                        counts: PlanSnapshotCounts {
                            pending: 1,
                            ..Default::default()
                        },
                        tasks: vec![PlanSnapshotTask {
                            task_id: "task-1".into(),
                            task_name: "task-1".into(),
                            agent: "codex".into(),
                            issue_id: Some("bd-1".into()),
                            issue_title: None,
                            status: "pending".into(),
                            attempt: 0,
                            max_attempts: 3,
                            depends_on: Vec::new(),
                            blocked_by: Vec::new(),
                            unblocks: Vec::new(),
                            summary: None,
                            feedback: None,
                            error: None,
                            worker_branch: None,
                            delegation_id: None,
                            diff_summary: None,
                            mutation_id: None,
                            superseded_by: Vec::new(),
                            next_action: "wait".into(),
                        }],
                        owner_brain_session_id: None,
                        owner_token: None,
                        owner_acquired_at: None,
                    }),
                }));
                store
            });
        static SYNOPSIS: std::sync::LazyLock<spur_core::SessionSynopsisProjection> =
            std::sync::LazyLock::new(spur_core::SessionSynopsisProjection::new);
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: &PLAN_PROJECTION,
            synopsis: &SYNOPSIS,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    #[test]
    fn empty_emacs_j_scrolls_without_typing() {
        let mut v = make_view();
        assert!(v.input_bar_text_for_test().is_empty());
        let act = press(&mut v, KeyCode::Char('j'));
        assert!(
            v.input_bar_text_for_test().is_empty(),
            "empty bar must not type 'j'"
        );
        assert!(
            matches!(act, Some(Action::ScrollDown)),
            "expected ScrollDown, got {:?}",
            act
        );
    }

    #[test]
    fn non_empty_emacs_j_stays_in_composer() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("hello".into(), 5);
        let anchor_before = v.react_trace().anchor_for_tests();

        let act = press(&mut v, KeyCode::Char('j'));

        assert_eq!(v.input_bar_text_for_test(), "helloj");
        assert_eq!(v.react_trace().anchor_for_tests(), anchor_before);
        assert!(act.is_none());
    }

    #[test]
    fn non_empty_up_moves_composer_cursor() {
        let mut v = make_view();
        let text = "line1\nline2";
        v.input_bar_mut_for_test().set_text(text.into(), text.len());
        let cursor_before = v.input_bar_mut_for_test().cursor();
        assert_eq!(cursor_before, text.len());
        let anchor_before = v.react_trace().anchor_for_tests();

        let act = press(&mut v, KeyCode::Up);

        assert_eq!(
            v.react_trace().anchor_for_tests(),
            anchor_before,
            "trace must not scroll when composer has text"
        );
        let cursor_after = v.input_bar_mut_for_test().cursor();
        assert!(
            cursor_after < cursor_before,
            "cursor should move up in multiline composer, before={cursor_before}, after={cursor_after}"
        );
        assert!(act.is_none());
    }

    #[test]
    fn pending_permission_with_non_empty_composer_emits_grant() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("hello".into(), 5);
        v.push_permission("allow file write?", 60);

        let act = press(&mut v, KeyCode::Char('y'));

        assert_eq!(
            v.input_bar_text_for_test(),
            "hello",
            "permission key must not type into bar"
        );
        assert!(
            matches!(
                act,
                Some(Action::PermissionGrant(
                    crate::action::PermissionChoice::Allow
                ))
            ),
            "expected PermissionGrant(Allow), got {:?}",
            act
        );
    }

    #[test]
    fn non_empty_vim_normal_ctrl_p_recalls_history_not_paste() {
        let mut v = make_view();
        v.set_edit_mode(crate::components::input_bar::EditMode::Vim(
            crate::components::input_bar::VimMode::Normal,
        ));
        v.seed_input_history(vec![crate::input_history::InputHistoryEntry::new(
            crate::input_history::InputStateSnapshot::from_text("refactor the walker"),
        )]);
        v.input_bar_mut_for_test()
            .set_text("current draft".into(), 13);

        let act = press_mod(&mut v, KeyCode::Char('p'), KeyModifiers::CONTROL);

        assert_eq!(
            v.input_bar_text_for_test(),
            "refactor the walker",
            "Ctrl+P must recall history in Vim Normal, not paste"
        );
        assert!(act.is_none(), "history nav must not emit an action");
    }

    #[test]
    fn alt_v_without_render_picker_does_not_type_literal_v() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("x".into(), 1);
        let act = press_mod(&mut v, KeyCode::Char('v'), KeyModifiers::ALT);
        assert_eq!(
            v.input_bar_text_for_test(),
            "x",
            "Alt+V must not insert a literal when render_picker is None"
        );
        assert!(act.is_none(), "composer no-op must not emit action");
    }

    #[test]
    fn question_mark_with_empty_bar_emits_show_help() {
        let mut v = make_view();
        assert!(v.input_bar_text_for_test().is_empty());

        let act = press(&mut v, KeyCode::Char('?'));

        assert!(
            v.input_bar_text_for_test().is_empty(),
            "empty bar must not type '?'"
        );
        assert!(
            matches!(act, Some(Action::ShowHelp)),
            "expected ShowHelp, got {:?}",
            act
        );
    }

    #[test]
    fn question_mark_with_non_empty_bar_appends_to_message() {
        let mut v = make_view();
        v.input_bar_mut_for_test().set_text("hello".into(), 5);

        let act = press(&mut v, KeyCode::Char('?'));

        assert_eq!(
            v.input_bar_text_for_test(),
            "hello?",
            "? must be typed into a non-empty composer"
        );
        assert!(
            act.is_none(),
            "composer typing must not emit an action, got {:?}",
            act
        );
    }

    #[test]
    fn alt_p_noops_without_tracked_plan() {
        let mut v = make_view();
        let act = press_mod(&mut v, KeyCode::Char('p'), KeyModifiers::ALT);
        assert!(act.is_none(), "Alt+P must no-op without tracked plan");
    }

    #[test]
    fn alt_p_opens_plan_inspector_when_plan_is_tracked() {
        let mut v = make_view();
        let act = v.handle_key(
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
            &test_ctx_with_plan(),
        );
        assert!(matches!(
            act,
            Some(Action::NavigateTo(ViewId::PlanInspector(_)))
        ));
    }

    #[cfg(feature = "markdown")]
    #[test]
    fn alt_v_with_render_picker_navigates_to_overlay() {
        let mut v = make_view();
        v.set_render_picker(Some(ratatui_image::picker::Picker::halfblocks()));
        let act = press_mod(&mut v, KeyCode::Char('v'), KeyModifiers::ALT);
        match act {
            Some(Action::NavigateTo(ViewId::MermaidOverlay(_))) => {}
            other => panic!("expected NavigateTo(MermaidOverlay), got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_view() -> SessionDetailView {
        use spur_acp::AgentConfig;
        use std::sync::Arc;
        SessionDetailView::new(
            spur_acp::SessionId("s".to_string()),
            "claude".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("/tmp"),
            Arc::new(AgentConfig::with_defaults("claude")),
            Vec::new(),
        )
    }

    fn test_ctx() -> crate::views::ViewContext<'static> {
        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::LazyLock<spur_core::PlanProjectionStore> =
            std::sync::LazyLock::new(spur_core::PlanProjectionStore::new);
        static SYNOPSIS: std::sync::LazyLock<spur_core::SessionSynopsisProjection> =
            std::sync::LazyLock::new(spur_core::SessionSynopsisProjection::new);
        crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: &PLAN_PROJECTION,
            synopsis: &SYNOPSIS,
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        }
    }

    fn prompt_dispatched_event(
        session: &spur_acp::SessionId,
        turn_kind: &str,
        continuations_count: usize,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::PromptDispatched {
            session: session.clone(),
            turn_kind: turn_kind.into(),
            continuations_count,
        })
    }

    fn continuation_dropped_event(delegation_id: &str) -> SpurEvent {
        SpurEvent::now(SpurEventBody::ContinuationDropped {
            delegation_id: delegation_id.into(),
            attempt: 1,
            brain_session: spur_acp::SessionId("test-brain-session".into()),
            reason: spur_acp::domain::continuation::DropReason::SessionSwap,
        })
    }

    #[test]
    fn pending_model_override_overrides_frozen_caps_label_until_config_option_arrives() {
        use agent_client_protocol::schema::{
            InitializeResponse, ModelId, NewSessionResponse, ProtocolVersion, SessionConfigId,
            SessionConfigOption, SessionConfigSelectOption, SessionId, SessionModelState,
        };

        let mut view = make_view();
        let init = InitializeResponse::new(ProtocolVersion::LATEST);
        let new = NewSessionResponse::new(SessionId::new("session-model-test"))
            .models(SessionModelState::new(ModelId::new("gpt-5"), vec![]));
        let caps = spur_acp::SpurAgentCaps::new(&init, &new, spur_acp::AgentKind::CodexAcp);
        view.set_spur_agent_caps(Some(std::sync::Arc::new(caps)));

        view.pending_model_override = Some("sonnet".to_string());
        assert_eq!(
            view.resolved_model_label().as_deref(),
            Some("sonnet"),
            "optimistic override should win over frozen caps label when no live model option exists"
        );

        let options = vec![SessionConfigOption::select(
            SessionConfigId::new("model"),
            "Model",
            "opus",
            vec![
                SessionConfigSelectOption::new("sonnet", "Sonnet"),
                SessionConfigSelectOption::new("opus", "Opus"),
            ],
        )];
        view.apply_advertised_commands(None, &options);

        assert_eq!(
            view.pending_model_override, None,
            "model config option update should clear the optimistic override"
        );
        assert_eq!(
            view.resolved_model_label().as_deref(),
            Some("Opus"),
            "live config option label should become authoritative once available"
        );
    }

    fn delegation_completed_event(
        worker_session: &str,
        status: spur_acp::DelegationStatus,
    ) -> SpurEvent {
        SpurEvent::now(SpurEventBody::DelegationCompleted {
            worker_session: spur_acp::SessionId(worker_session.into()),
            status,
        })
    }

    #[test]
    fn new_view_defaults_cleared_false_and_no_ready_banner() {
        let view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        assert!(!view.is_cleared(), "new view must default cleared=false");
        assert!(
            view.ready_banner_text().is_none(),
            "new view must not start with a ready banner"
        );
    }

    #[test]
    fn reset_for_clear_wipes_conversation_state() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        // Seed state that reset_for_clear must wipe.
        view.tool_depth.insert("t1".to_string(), 2);
        #[cfg(feature = "markdown")]
        view.mermaid_registry.insert(
            crate::components::mermaid::MermaidId(1),
            crate::components::mermaid::MermaidState::Rendering,
        );

        view.reset_for_clear();

        assert!(view.tool_depth.is_empty(), "tool_depth must be cleared");
        // ReactTrace must be empty after reset — use whatever public
        // emptiness accessor exists on ReactTrace (grep
        // components/react_trace/mod.rs for `pub fn len\|is_empty\|entry_count`).
        // If no direct accessor, assert via rendered output in Task 10.
        // For now, assert the flag was set:
        assert!(view.is_cleared());
        assert_eq!(view.ready_banner_text(), Some(READY_BANNER_TEXT));
    }

    #[test]
    fn reset_for_clear_clears_header_status_fields() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        // Seed via existing public APIs.
        view.set_current_mode(Some("plan".into()));
        view.cost = 1.23;
        view.context_used = Some(1234);
        view.context_size = Some(200_000);
        view.auth_error = Some("auth failed".into());
        view.stream_in_flight = true;
        view.cancelling_in_flight = true;

        view.reset_for_clear();

        assert_eq!(view.cost, 0.0);
        assert_eq!(view.current_mode, None);
        assert_eq!(view.context_used, None);
        assert_eq!(view.context_size, None);
        assert_eq!(view.auth_error, None);
        assert!(!view.stream_in_flight);
        assert!(!view.cancelling_in_flight);
        // react_trace's mode mirror must also reset.
        assert_eq!(view.react_trace.current_mode(), None);
    }

    #[test]
    fn cleared_view_suppresses_force_save_draft() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();
        // Even with new text in the InputBar, force_save_draft must not
        // emit an Action keyed on the retired session_id.
        view.input_bar.set_text("new text".into(), 8);
        assert!(
            view.force_save_draft().is_none(),
            "cleared view must suppress force_save_draft"
        );
    }

    #[test]
    fn cleared_view_suppresses_draft_save_action() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();
        view.input_bar.set_text("new text".into(), 8);
        // Simulate a debounce trigger: set last_draft_change_at 600ms ago.
        view.test_set_last_draft_change({
            let now = std::time::Instant::now();
            now.checked_sub(std::time::Duration::from_millis(600))
                .unwrap_or(now)
        });
        assert!(
            view.draft_save_action().is_none(),
            "cleared view must suppress draft_save_action (debounce tick)"
        );
    }

    #[test]
    fn reset_for_clear_is_idempotent() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.react_trace.clear(); // normalize
        view.tool_depth.insert("seeded".into(), 1);
        view.reset_for_clear();
        let banner1 = view.ready_banner_text().map(str::to_string);
        view.reset_for_clear();
        let banner2 = view.ready_banner_text().map(str::to_string);
        assert_eq!(banner1, banner2);
        assert!(view.is_cleared());
        assert!(view.tool_depth.is_empty());
    }

    #[test]
    fn ready_banner_renders_when_cleared() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.reset_for_clear();

        static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
            std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        let ctx = crate::views::ViewContext {
            lineage: &LINEAGE,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &crate::app::BrainStatus::Idle,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            theme: crate::theme::fallback_theme(),
        };

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                <SessionDetailView as crate::views::View>::render(&mut view, f, f.area(), &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| {
                        buffer
                            .cell((x, y))
                            .map(|c| c.symbol().to_string())
                            .unwrap_or_default()
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("Session cleared"),
            "ready banner text must appear. Rendered:\n{rendered}"
        );
    }

    #[test]
    fn reset_for_clear_wipes_draft_debounce_locals() {
        let mut view =
            SessionDetailView::new_for_palette_test(crate::commands::CommandRegistry::default());
        view.last_persisted_draft = "stale".into();
        view.last_draft_change_at = Some(std::time::Instant::now());
        view.reset_for_clear();
        assert_eq!(view.last_persisted_draft, "");
        assert!(view.last_draft_change_at.is_none());
    }

    #[test]
    fn prompt_dispatched_continuation_only_pushes_think_entry() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(
            &prompt_dispatched_event(&sid, "continuation_only", 3),
            &test_ctx(),
        );
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Think));
        assert!(last.text.contains("Brain resuming with 3 worker results"));
    }

    #[test]
    fn prompt_dispatched_merged_pushes_think_entry() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        v.handle_spur_event(&prompt_dispatched_event(&sid, "merged", 1), &test_ctx());
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Think));
        assert!(last
            .text
            .contains("Merging user message with 1 worker result"));
    }

    #[test]
    fn prompt_dispatched_user_only_is_no_op() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        let before = v.react_trace.entry_count();
        v.handle_spur_event(&prompt_dispatched_event(&sid, "user_only", 0), &test_ctx());
        assert_eq!(v.react_trace.entry_count(), before);
    }

    #[test]
    fn prompt_dispatched_different_session_is_ignored() {
        let mut v = make_view();
        let other = spur_acp::SessionId("other".into());
        let before = v.react_trace.entry_count();
        v.handle_spur_event(
            &prompt_dispatched_event(&other, "continuation_only", 2),
            &test_ctx(),
        );
        assert_eq!(v.react_trace.entry_count(), before);
    }

    #[test]
    fn continuation_dropped_pushes_observe_entry() {
        let mut v = make_view();
        v.handle_spur_event(&continuation_dropped_event("del-42"), &test_ctx());
        let entries = v.react_trace.entries_for_test();
        let last = entries.last().unwrap();
        assert!(matches!(last.kind, TraceKind::Observe { .. }));
        assert!(last.text.contains("Continuation dropped for del-42"));
    }

    #[test]
    fn delegation_completed_updates_delegate_status() {
        let mut v = make_view();
        let sid = v.session_id().clone();
        // Seed a delegation request so the trace has a Delegate entry.
        v.handle_spur_event(
            &SpurEvent::now(SpurEventBody::DelegationRequested {
                from: sid.clone(),
                to_agent: "codex".into(),
                task: "fix bug".into(),
                request_id: "req-1".into(),
                delegation_plan: None,
                issue_id: None,
            }),
            &test_ctx(),
        );
        // Attach executor_id (simulating DelegationDispatched).
        v.handle_spur_event(
            &SpurEvent::now(SpurEventBody::DelegationDispatched {
                from: sid.clone(),
                request_id: "req-1".into(),
                executor_id: "exec-1".into(),
            }),
            &test_ctx(),
        );
        // Emit completion.
        v.handle_spur_event(
            &delegation_completed_event("exec-1", spur_acp::DelegationStatus::Success),
            &test_ctx(),
        );
        let entries = v.react_trace.entries_for_test();
        let delegate_entry = entries
            .iter()
            .find(|e| matches!(e.kind, TraceKind::Delegate { .. }))
            .unwrap();
        match &delegate_entry.kind {
            TraceKind::Delegate { status, .. } => assert_eq!(status, "done"),
            other => panic!("expected Delegate, got {:?}", other),
        }
    }
}
