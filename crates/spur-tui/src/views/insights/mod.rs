//! Experimental Insights view (analytics feature).
//!
//! Module skeleton; full implementation lands in C.4-C.9.

pub mod builder;
pub mod refresh;
pub mod state;
pub mod tabs;
pub mod widgets;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    Frame,
};
use spur_acp::SpurEvent;
use spur_context::AsyncEngine;
use state::{Dimension, Granularity, InsightsTab, RefreshState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

use crate::action::Action;

use super::{View, ViewContext};

pub struct InsightsView {
    #[allow(dead_code)]
    engine: AsyncEngine,
    state: Arc<RwLock<RefreshState>>,
    is_live_tab: Arc<AtomicBool>,
    signal_tx: mpsc::Sender<()>,
    refresh_handle: Option<JoinHandle<()>>,
    active_tab: InsightsTab,
    granularity: Granularity,
    dimension: Dimension,
}

impl InsightsView {
    pub fn new(engine: AsyncEngine) -> Self {
        let state = Arc::new(RwLock::new(RefreshState::default()));
        let is_live_tab = Arc::new(AtomicBool::new(false));
        let (signal_tx, signal_rx) = mpsc::channel(8);
        let refresh_handle = refresh::spawn_refresh_task(
            engine.clone(),
            state.clone(),
            is_live_tab.clone(),
            signal_rx,
        );
        signal_tx.try_send(()).ok();

        Self {
            engine,
            state,
            is_live_tab,
            signal_tx,
            refresh_handle: Some(refresh_handle),
            active_tab: InsightsTab::Overview,
            granularity: Granularity::Daily,
            dimension: Dimension::Agent,
        }
    }

    fn set_active_tab(&mut self, tab: InsightsTab) {
        self.active_tab = tab;
        self.is_live_tab.store(
            matches!(self.active_tab, InsightsTab::Live),
            Ordering::Relaxed,
        );
    }

    fn next_tab(&self) -> InsightsTab {
        match self.active_tab {
            InsightsTab::Overview => InsightsTab::Timeline,
            InsightsTab::Timeline => InsightsTab::Breakdown,
            InsightsTab::Breakdown => InsightsTab::Live,
            InsightsTab::Live => InsightsTab::Overview,
        }
    }
}

impl Drop for InsightsView {
    /// NOTE: per Tokio docs, abort STOPS WAITING but does NOT cancel the
    /// in-flight spawn_blocking task. The blocking thread runs to
    /// completion and its result is dropped when no receiver remains.
    fn drop(&mut self) {
        if let Some(handle) = self.refresh_handle.take() {
            handle.abort();
        }
    }
}

impl View for InsightsView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }

        match key.code {
            KeyCode::Esc => Some(Action::NavigateBack),
            KeyCode::Tab => {
                self.set_active_tab(self.next_tab());
                None
            }
            KeyCode::Char('1') => {
                self.set_active_tab(InsightsTab::Overview);
                None
            }
            KeyCode::Char('2') => {
                self.set_active_tab(InsightsTab::Timeline);
                None
            }
            KeyCode::Char('3') => {
                self.set_active_tab(InsightsTab::Breakdown);
                None
            }
            KeyCode::Char('4') => {
                self.set_active_tab(InsightsTab::Live);
                None
            }
            // Dimension keys (Breakdown tab only — `a` and `p` are unique
            // to Breakdown; `m` is shared with Timeline's Monthly so it
            // dispatches by active tab below).
            KeyCode::Char('a') if matches!(self.active_tab, InsightsTab::Breakdown) => {
                self.dimension = Dimension::Agent;
                None
            }
            KeyCode::Char('p') if matches!(self.active_tab, InsightsTab::Breakdown) => {
                self.dimension = Dimension::Project;
                None
            }
            // Granularity keys (Timeline tab only). Both upper- and
            // lowercase to match the lowercase-by-default Dimension keys.
            KeyCode::Char('D' | 'd') if matches!(self.active_tab, InsightsTab::Timeline) => {
                self.granularity = Granularity::Daily;
                None
            }
            KeyCode::Char('W' | 'w') if matches!(self.active_tab, InsightsTab::Timeline) => {
                self.granularity = Granularity::Weekly;
                None
            }
            // `m`/`M` is context-sensitive: Monthly on Timeline, Model on
            // Breakdown, ignored elsewhere.
            KeyCode::Char('M' | 'm') => {
                match self.active_tab {
                    InsightsTab::Timeline => self.granularity = Granularity::Monthly,
                    InsightsTab::Breakdown => self.dimension = Dimension::Model,
                    _ => {}
                }
                None
            }
            KeyCode::Char('r') => {
                self.signal_tx.try_send(()).ok();
                None
            }
            _ => None,
        }
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &ViewContext) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
        use ratatui::{layout::Layout, widgets::Paragraph};

        match self.state.try_read() {
            Ok(state) => {
                let Some(snapshot) = state.last_good.as_ref() else {
                    let body = if let Some(error) = &state.last_error {
                        format!("Error: {error:#}")
                    } else {
                        "Loading...".to_string()
                    };
                    let text = if state.refreshing {
                        format!("{body}\nRefreshing...")
                    } else {
                        body
                    };
                    tracing::debug!(target: "spur_tui::insights::render", refreshing = state.refreshing, has_error = state.last_error.is_some(), "rendering placeholder (no snapshot yet)");
                    frame.render_widget(Paragraph::new(text), area);
                    return;
                };

                let [header_area, body_area, footer_area] = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .areas(area);
                render_header(frame, header_area, self.active_tab, state.refreshing);

                match self.active_tab {
                    InsightsTab::Overview => tabs::OverviewTab::render(frame, body_area, snapshot),
                    InsightsTab::Timeline => {
                        tabs::TimelineTab::render(frame, body_area, snapshot, self.granularity)
                    }
                    InsightsTab::Breakdown => {
                        tabs::BreakdownTab::render(frame, body_area, snapshot, self.dimension)
                    }
                    InsightsTab::Live => tabs::LiveTab::render(frame, body_area, snapshot),
                }

                render_key_hint_footer(frame, footer_area, self.active_tab);
            }
            Err(_) => frame.render_widget(Paragraph::new("Refreshing..."), area),
        }
    }

    fn tick(&mut self) {}
}

fn render_header(frame: &mut Frame, area: Rect, active_tab: InsightsTab, refreshing: bool) {
    use ratatui::widgets::Paragraph;

    let left_width = " 1 Overview │ 2 Timeline │ 3 Breakdown │ 4 Live "
        .chars()
        .count();
    let right = if refreshing {
        "[via analytics] Refreshing"
    } else {
        "[via analytics]"
    };
    let pad = area
        .width
        .saturating_sub((left_width + right.chars().count()) as u16) as usize;

    let line = Line::from(vec![
        Span::raw(" "),
        tab_label("1 Overview", active_tab == InsightsTab::Overview),
        Span::raw(" │ "),
        tab_label("2 Timeline", active_tab == InsightsTab::Timeline),
        Span::raw(" │ "),
        tab_label("3 Breakdown", active_tab == InsightsTab::Breakdown),
        Span::raw(" │ "),
        tab_label("4 Live", active_tab == InsightsTab::Live),
        Span::raw(" ".repeat(pad + 1)),
        Span::styled(right, Style::default().fg(Color::Green)),
    ]);

    frame.render_widget(Paragraph::new(line), area);
}

fn tab_label(label: &'static str, active: bool) -> Span<'static> {
    let style = if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Span::styled(label, style)
}

/// Render a 1-row dim footer listing the keys that work in the current tab.
/// Universal keys (Tab, r, Esc) come first; tab-specific keys are appended.
fn render_key_hint_footer(frame: &mut Frame, area: Rect, active_tab: InsightsTab) {
    use ratatui::widgets::Paragraph;

    let universal = "[Tab] Next  [1-4] Tab  [r] Refresh  [Esc] Back";
    let tab_specific: &str = match active_tab {
        InsightsTab::Timeline => "  [d/w/m] Granularity",
        InsightsTab::Breakdown => "  [a/m/p] Dimension",
        _ => "",
    };
    let line = Line::from(vec![Span::styled(
        format!(" {universal}{tab_specific}"),
        Style::default().fg(Color::DarkGray),
    )]);
    frame.render_widget(Paragraph::new(line), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_context::{AnalyticsEngine, AsyncEngine};
    use std::sync::atomic::Ordering;
    use tempfile::TempDir;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn empty_engine() -> (TempDir, AsyncEngine) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("insights.duckdb");
        let (engine, _recovered) = AnalyticsEngine::open(db_path).unwrap();
        (tmp, AsyncEngine::new(engine))
    }

    fn empty_lineage() -> spur_core::lineage::projection::ExecutorLineage {
        spur_core::lineage::projection::ExecutorLineage::default()
    }

    #[tokio::test]
    async fn view_constructor_spawns_refresh_task() {
        let (_tmp, engine) = empty_engine();
        let view = InsightsView::new(engine);

        assert!(view.refresh_handle.is_some());

        drop(view);
    }

    #[test]
    fn tab_jump_keys_set_active_tab() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (_tmp, engine) = empty_engine();
        let lineage = empty_lineage();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = InsightsView::new(engine);

        view.handle_key(key(KeyCode::Char('1')), &ctx);
        assert_eq!(view.active_tab, InsightsTab::Overview);
        view.handle_key(key(KeyCode::Char('2')), &ctx);
        assert_eq!(view.active_tab, InsightsTab::Timeline);
        view.handle_key(key(KeyCode::Char('3')), &ctx);
        assert_eq!(view.active_tab, InsightsTab::Breakdown);
        view.handle_key(key(KeyCode::Char('4')), &ctx);
        assert_eq!(view.active_tab, InsightsTab::Live);
    }

    #[test]
    fn esc_key_returns_navigate_back() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (_tmp, engine) = empty_engine();
        let lineage = empty_lineage();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = InsightsView::new(engine);

        let action = view.handle_key(key(KeyCode::Esc), &ctx);

        assert!(matches!(action, Some(Action::NavigateBack)));
    }

    #[test]
    fn live_tab_flips_atomic_bool() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _guard = runtime.enter();
        let (_tmp, engine) = empty_engine();
        let lineage = empty_lineage();
        let ctx = ViewContext::test_ctx(&lineage);
        let mut view = InsightsView::new(engine);

        view.handle_key(key(KeyCode::Char('4')), &ctx);

        assert!(view.is_live_tab.load(Ordering::Relaxed));
    }
}
