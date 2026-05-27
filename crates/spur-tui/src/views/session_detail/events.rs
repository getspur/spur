#[cfg(feature = "markdown")]
use crate::action::Action;
use crate::components::react_trace::{TraceEntry, TraceKind};
use crate::views::ViewContext;
use spur_acp::{
    DelegationStatus, LoadOutcome, ResponseRenderKind, SessionUpdate, SpurEvent, SpurEventBody,
};

use super::{LoadState, SessionDetailView};

impl SessionDetailView {
    /// Update `load_state` from a milestone event scoped to this view's
    /// session id. Intended for tests that exercise the pre-ready load
    /// pipeline without a full `ViewContext`.
    ///
    /// The full `View::handle_spur_event` trait method also calls
    /// `apply_milestone_event` internally; this is the test-facing entry
    /// point.
    #[cfg(any(test, debug_assertions))]
    pub fn apply_spur_event(&mut self, event: &SpurEvent) {
        self.apply_milestone_event(event);
    }

    /// Inner projection: update `load_state` from a milestone event scoped to
    /// this view's session id.
    pub(super) fn apply_milestone_event(&mut self, event: &SpurEvent) {
        match &event.body {
            SpurEventBody::BrainConnecting {
                session,
                brain_name,
            } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Connecting {
                    brain_name: brain_name.clone(),
                };
            }
            SpurEventBody::SessionLoading { session } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Loading;
            }
            SpurEventBody::SessionLoaded { session } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Ready;
            }
            SpurEventBody::BrainError { session, message } if session.0 == self.session_id.0 => {
                self.load_state = LoadState::Failed {
                    message: message.clone(),
                };
            }
            _ => {}
        }
    }

    pub(super) fn on_stream(&mut self, event: &SpurEvent, ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                // Read-only mirror of session-scoped state (mode, usage,
                // available commands). Handled before the trace-rendering
                // match so we always capture it regardless of whether a
                // display arm fires below.
                crate::app::apply_session_update(self, &notification.update);

                // Flag streaming state for observable turn progress. This is
                // the caller's responsibility -- the shared dispatcher is
                // agnostic to session lifecycle state. Tool-bearing progress
                // and plan updates arm the stream alongside text/thought
                // chunks: a valid ACP turn can begin with ToolCall /
                // ToolCallUpdate / Plan before any text chunk, and Esc-cancel
                // plus the in-flight hint both key off stream_in_flight.
                // UsageUpdate / CurrentModeUpdate are mirrored session state
                // and do not by themselves prove visible turn progress.
                match &notification.update {
                    SessionUpdate::AgentThoughtChunk(_)
                    | SessionUpdate::AgentMessageChunk(_)
                    | SessionUpdate::ToolCall(_)
                    | SessionUpdate::ToolCallUpdate(_)
                    | SessionUpdate::Plan(_) => {
                        self.stream_in_flight = true;
                    }
                    _ => {}
                }

                let agent_name = self.agent_name.clone();
                let agent_kind = self.agent_kind();
                let skip_plan_trace = ctx
                    .plan_projection
                    .current_for_session(self.session_id())
                    .is_some();
                let mut ctx = crate::components::react_trace::dispatch::DispatchCtx {
                    agent_name: agent_name.as_str(),
                    agent_kind,
                    now_stamp: Self::now_stamp,
                    tool_depth: &mut self.tool_depth,
                    skip_plan_trace,
                };
                crate::components::react_trace::dispatch::dispatch_session_update(
                    &mut self.react_trace,
                    &notification.update,
                    &mut ctx,
                );
            }
            SpurEventBody::CostUpdate {
                session,
                estimated_cost_usd,
                ..
            } if session.0 == self.session_id.0 => {
                self.cost += estimated_cost_usd;
            }
            SpurEventBody::TurnComplete { session } if session.0 == self.session_id.0 => {
                self.stream_in_flight = false;
                self.cancelling_in_flight = false;
                // If the modal is still open when the turn ends naturally,
                // close it. Otherwise the modal-open key handler would
                // hijack the user's next keystroke for a question the
                // agent has already answered (cancel is moot).
                self.cancel_confirm_open = false;
                self.tool_depth.clear();
                #[cfg(feature = "markdown")]
                {
                    use crate::components::markdown_stream::StateLookup;

                    let (error_ids, pending_ids) = self.build_state_lookup_sets();
                    let states = StateLookup {
                        errors: &error_ids,
                        pending: &pending_ids,
                    };
                    for (_entry_idx, fence) in self.react_trace.force_flush_all(&states) {
                        self.mermaid_registry_insert(
                            fence.id,
                            crate::components::mermaid::MermaidState::Pending {
                                code: fence.code.clone(),
                            },
                        );
                        self.in_flight_renders.insert(fence.id);
                        self.pending_fence_actions
                            .push_back(Action::MermaidRenderRequest {
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
                                    let pane_w_cols =
                                        self.react_trace.last_render_width().unwrap_or(80);
                                    crate::components::mermaid::raster_width_for_pane(
                                        (pane_w_cols as u32).saturating_mul(cell_w_px as u32),
                                    )
                                },
                            });
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_tool_call(&mut self, event: &SpurEvent, _ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::DelegationRequested {
                from,
                to_agent,
                task,
                request_id,
                delegation_plan: _,
                issue_id: _,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                self.set_brain_status(&format!("delegating to {}", to_agent));
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Delegate {
                        agent: to_agent.clone(),
                        task: task.clone(),
                        status: "delegated".to_string(),
                        request_id: Some(request_id.clone()),
                        executor_id: None,
                    },
                    text: String::new(),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::DelegationDispatched {
                from,
                request_id,
                executor_id,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                // Find the most recent Delegate entry with matching request_id
                // and attach the executor_id.
                self.react_trace.attach_executor_id(request_id, executor_id);
            }
            SpurEventBody::DelegationCompleted {
                worker_session,
                status,
            } => {
                // Update the matching Delegate trace entry so its status
                // renders correctly even when lineage isn't yet available.
                // worker_session.0 carries the request_id / executor_id.
                let status_label = match status {
                    DelegationStatus::Success => "done",
                    DelegationStatus::Failed { .. } => "failed",
                    DelegationStatus::Conflict { .. } => "conflict",
                    DelegationStatus::Timeout => "timed out",
                    DelegationStatus::Rejected { .. } => "rejected",
                    DelegationStatus::Modified { .. } => "modified",
                    DelegationStatus::TimedOut { .. } => "timed out",
                    DelegationStatus::Cancelled { .. } => "cancelled",
                    _ => "completed",
                };
                self.react_trace
                    .update_delegate_status(&worker_session.0, status_label);
            }
            _ => {}
        }
    }

    pub(super) fn on_plan_perm(&mut self, event: &SpurEvent, _ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::PromptDispatched {
                session,
                turn_kind,
                continuations_count,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                // Friendly trace note when the brain is re-entered with
                // worker continuations (autonomous or merged turn).
                let note = match turn_kind.as_str() {
                    "continuation_only" => {
                        if *continuations_count == 1 {
                            "▸ Brain resuming with 1 worker result".to_string()
                        } else {
                            format!(
                                "▸ Brain resuming with {} worker results",
                                continuations_count
                            )
                        }
                    }
                    "merged" => {
                        if *continuations_count == 1 {
                            "▸ Merging user message with 1 worker result".to_string()
                        } else {
                            format!(
                                "▸ Merging user message with {} worker results",
                                continuations_count
                            )
                        }
                    }
                    _ => return, // user_only -- no note needed
                };
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Think,
                    text: note,
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::ContinuationDropped {
                delegation_id,
                reason,
                ..
            } => {
                // This is a system-level event without session scoping;
                // show it for the active session so the user knows a
                // promised continuation was lost.
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: format!("⚠ Continuation dropped for {}: {:?}", delegation_id, reason),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            _ => {}
        }
    }

    pub(super) fn on_auth_banner(&mut self, event: &SpurEvent, _ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::BrainError { session, message } if session.0 == self.session_id.0 => {
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: format!("BRAIN ERROR: {}", message),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::BrainReconnecting {
                session,
                brain_name,
                reason,
            } if session.0 == self.session_id.0 => {
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: format!("brain '{}' reconnecting… ({})", brain_name, reason),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::BrainReconnected {
                session,
                brain_name,
                outcome,
            } if session.0 == self.session_id.0 => {
                let text = match outcome {
                    LoadOutcome::Restored => {
                        format!(
                            "brain '{}' reconnected — state restored. Your last prompt/command was dropped; retype to retry.",
                            brain_name
                        )
                    }
                    LoadOutcome::FellBackToNew { reason } => {
                        format!(
                            "brain '{}' reconnected — started FRESH ({}); prior context wiped. Retype to continue.",
                            brain_name, reason
                        )
                    }
                };
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text,
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::BrainReconnectFailed {
                session,
                brain_name,
                reason,
            } if session.0 == self.session_id.0 => {
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Observe { payload: None },
                    text: format!("brain '{}' reconnect FAILED: {}", brain_name, reason),
                    timestamp: Self::now_stamp(),
                    #[cfg(feature = "markdown")]
                    markdown: None,
                });
            }
            SpurEventBody::AgentSessionReady {
                session,
                resumed,
                cancel_mode,
                fs_unsafe,
                ..
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                self.cancel_mode = Some(*cancel_mode);
                self.fs_unsafe = *fs_unsafe;
                if *resumed {
                    self.push_system_note("Resumed from prior conversation".to_string());
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_mermaid_caps(&mut self, event: &SpurEvent, _ctx: &ViewContext) {
        match &event.body {
            SpurEventBody::AgentExtNotification {
                session,
                method,
                params,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                let cfg = self.agent_cfg.clone();

                // Ingest bindings: decode params -> delegate to apply_available_commands.
                for binding in &cfg.commands.ingest {
                    if &binding.method != method {
                        continue;
                    }
                    if let Some(parsed) = crate::agents::run_ingest_hook(binding, params) {
                        self.apply_available_commands(&parsed);
                    }
                }

                // Response bindings: render the payload according to `render` kind.
                for binding in &cfg.commands.response {
                    if &binding.method != method {
                        continue;
                    }
                    match binding.render {
                        ResponseRenderKind::SystemNote => {
                            let handle = self.agent_handle_for_commands();
                            self.push_system_note(format!(
                                "\u{27e8}{handle}\u{27e9} response: {}",
                                params
                            ));
                        }
                    }
                }
            }
            SpurEventBody::CommandRegistryDirty {
                session,
                caps,
                config_options,
            } => {
                if session.0 != self.session_id.0 {
                    return;
                }
                self.apply_advertised_commands(caps.as_deref(), config_options);
            }
            _ => {}
        }
    }
}
