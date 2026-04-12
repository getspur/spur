use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use spur_acp::{DelegationStatus, SessionEvent, SessionId, SpurEvent};

use crate::action::{Action, ViewId};
use crate::components::input_bar::InputBar;
use crate::components::react_trace::{ReactTrace, TraceEntry, TraceKind};
use crate::components::status_bar::StatusBar;

use super::View;

/// Full-screen view of a brain session's ReAct trace with chat input.
pub struct SessionDetailView {
    session_id: SessionId,
    agent_name: String,
    role: String,
    react_trace: ReactTrace,
    input_bar: InputBar,
    cost: f64,
    started_at: Instant,
}

impl SessionDetailView {
    pub fn new(session_id: SessionId, agent_name: String, role: String) -> Self {
        Self {
            session_id,
            agent_name,
            role,
            react_trace: ReactTrace::new(),
            input_bar: InputBar::new(),
            cost: 0.0,
            started_at: Instant::now(),
        }
    }

    /// Current local time formatted as HH:MM:SS.
    fn now_stamp() -> String {
        crate::components::now_stamp()
    }

    /// Format elapsed time since view was opened.
    fn elapsed(&self) -> String {
        crate::components::format_elapsed(self.started_at)
    }

}

impl View for SessionDetailView {
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Priority 1: Permission handling when a permission is pending.
        if self.react_trace.has_pending_permission() {
            match key.code {
                KeyCode::Char('y') => {
                    // Auto-approve (placeholder — actual permission handling is Phase 2)
                    let _ = &self.session_id; // will wire to permission system later
                    return None;
                }
                KeyCode::Char('n') => {
                    // Deny (placeholder — actual permission handling is Phase 2)
                    let _ = &self.session_id;
                    return None;
                }
                KeyCode::Char('a') => {
                    // Approve-all (placeholder — actual permission handling is Phase 2)
                    let _ = &self.session_id;
                    return None;
                }
                _ => {
                    // Fall through to normal key handling for other keys.
                }
            }
        }

        // Priority 2: If the key is a printable char or an editing key, route to input_bar.
        let is_editing_key = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
        );

        if is_editing_key {
            if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
                return Some(Action::SendMessage {
                    session: self.session_id.clone(),
                    text,
                    interrupt,
                });
            }

            // If the input_bar is empty and the key was a navigation key (j/k/g/G),
            // we want scroll behavior instead. But since we already routed to
            // input_bar above, we only fall through for non-char keys when empty.
            // Actually, chars always go to input_bar first. The spec says:
            // "If input_bar is empty: j/k/Up/Down → scroll, g/G → jump, Esc → back"
            // But chars are "printable" so they go to input_bar which will insert them.
            // We need to check: if input was empty BEFORE this key and the key is
            // a scroll key, we should scroll instead. Let's re-check the spec:
            //
            // The spec says route printable/editing keys to input_bar. But it also
            // says when input_bar is empty, j/k/g/G should scroll. The resolution:
            // j/k/g/G when input is empty should scroll, not type.
            //
            // We already inserted the char though. Let's undo if it was a scroll
            // key and the bar was previously empty (now has exactly 1 char).
            if self.input_bar.text().len() == 1 {
                let ch = self.input_bar.text().chars().next().unwrap();
                if matches!(ch, 'j' | 'k' | 'g' | 'G') {
                    self.input_bar.clear();
                    return match ch {
                        'j' => {
                            self.react_trace.scroll_down(20);
                            Some(Action::ScrollDown)
                        }
                        'k' => {
                            self.react_trace.scroll_up();
                            Some(Action::ScrollUp)
                        }
                        'g' => {
                            self.react_trace.scroll_to_top();
                            Some(Action::ScrollToTop)
                        }
                        'G' => {
                            self.react_trace.scroll_to_bottom();
                            Some(Action::ScrollToBottom)
                        }
                        _ => None,
                    };
                }
            }

            return None;
        }

        // Priority 3: Non-editing keys when input_bar is empty → scroll/navigate.
        if self.input_bar.is_empty() {
            match key.code {
                KeyCode::Up => {
                    self.react_trace.scroll_up();
                    return Some(Action::ScrollUp);
                }
                KeyCode::Down => {
                    self.react_trace.scroll_down(20);
                    return Some(Action::ScrollDown);
                }
                KeyCode::Esc => {
                    return Some(Action::NavigateBack);
                }
                _ => {}
            }
        }

        None
    }

    fn handle_spur_event(&mut self, event: &SpurEvent) {
        match event {
            SpurEvent::AgentOutput {
                session,
                event: se,
            } => {
                // Only process events for this session.
                if session.0 != self.session_id.0 {
                    return;
                }

                match se {
                    SessionEvent::TextDelta(text) => {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            self.react_trace.push(TraceEntry {
                                kind: TraceKind::Think,
                                text: trimmed.to_string(),
                                timestamp: Self::now_stamp(),
                            });
                        }
                    }
                    SessionEvent::ToolCallStart { name, input, .. } => {
                        let args = input.to_string();
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Act {
                                tool: name.clone(),
                                args,
                            },
                            text: String::new(),
                            timestamp: Self::now_stamp(),
                        });
                    }
                    SessionEvent::ToolCallResult { output, .. } => {
                        let text = output.to_string();
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Observe,
                            text,
                            timestamp: Self::now_stamp(),
                        });
                    }
                    SessionEvent::Error { message, .. } => {
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Think,
                            text: format!("ERROR: {}", message),
                            timestamp: Self::now_stamp(),
                        });
                    }
                    SessionEvent::Complete { .. } => {
                        self.react_trace.push(TraceEntry {
                            kind: TraceKind::Think,
                            text: "Session complete".to_string(),
                            timestamp: Self::now_stamp(),
                        });
                    }
                    // StatusUpdate and RateLimitHit are not mapped to trace entries
                    // in the spec; ignore them here.
                    _ => {}
                }
            }

            SpurEvent::DelegationRequested {
                from,
                to_agent,
                task,
            } => {
                if from.0 != self.session_id.0 {
                    return;
                }
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Delegate {
                        agent: to_agent.clone(),
                        task: task.clone(),
                        status: "delegated".to_string(),
                    },
                    text: String::new(),
                    timestamp: Self::now_stamp(),
                });
            }

            SpurEvent::DelegationCompleted {
                worker_session,
                status,
            } => {
                // Update the most recent delegate entry that matches this worker.
                // Since we don't have a direct session→agent mapping here, update
                // the last delegate entry with an active status.
                let _ = worker_session; // avoid unused warning
                let status_str = match status {
                    DelegationStatus::Success => "done",
                    DelegationStatus::Failed { .. } => "failed",
                    DelegationStatus::Conflict { .. } => "conflict",
                    DelegationStatus::Timeout => "timeout",
                };
                // This is a best-effort update; walk entries in reverse to find
                // the most recent active delegation.
                // Note: ReactTrace doesn't expose entries mutably, so we just
                // push a new entry noting the completion instead.
                self.react_trace.push(TraceEntry {
                    kind: TraceKind::Think,
                    text: format!("Delegation completed: {}", status_str),
                    timestamp: Self::now_stamp(),
                });
            }

            SpurEvent::CostUpdate {
                session,
                estimated_cost_usd,
                ..
            } => {
                if session.0 == self.session_id.0 {
                    self.cost += estimated_cost_usd;
                }
            }

            SpurEvent::TurnComplete { session } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Think,
                        text: "Turn complete — ready for input".to_string(),
                        timestamp: Self::now_stamp(),
                    });
                }
            }

            SpurEvent::BrainError { session, message } => {
                if session.0 == self.session_id.0 {
                    self.react_trace.push(TraceEntry {
                        kind: TraceKind::Think,
                        text: format!("BRAIN ERROR: {}", message),
                        timestamp: Self::now_stamp(),
                    });
                }
            }

            // All other event types are not relevant to this session view.
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.react_trace.tick();
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let elapsed = self.elapsed();

        let input_height = self.input_bar.required_height();
        let chunks = Layout::vertical([
            Constraint::Length(1),            // header
            Constraint::Min(4),              // react trace (fills)
            Constraint::Length(input_height), // input bar
            Constraint::Length(1),            // status bar
        ])
        .split(area);

        // ── Header: breadcrumb + elapsed + cost ─────────────────────────
        let header = Line::from(vec![
            Span::styled(
                " Dashboard > ",
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(
                &self.agent_name,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" ({})", self.role),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(&elapsed, Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(
                format!("${:.2}", self.cost),
                Style::default().fg(Color::Yellow),
            ),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // ── React trace ─────────────────────────────────────────────────
        self.react_trace.render(frame, chunks[1]);

        // ── Input bar ───────────────────────────────────────────────────
        self.input_bar.render(frame, chunks[2]);

        // ── Status bar ──────────────────────────────────────────────────
        StatusBar::render(
            frame,
            chunks[3],
            &ViewId::SessionDetail(self.session_id.clone()),
            self.cost,
            &elapsed,
        );
    }
}
