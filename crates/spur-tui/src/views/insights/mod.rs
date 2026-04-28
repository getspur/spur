//! Experimental Insights view (analytics feature).
//!
//! Module skeleton; full implementation lands in C.4-C.9.

pub mod builder;
pub mod refresh;
pub mod state;
pub mod tabs;
pub mod widgets;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{layout::Rect, Frame};
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
            KeyCode::Char('a') => {
                self.dimension = Dimension::Agent;
                None
            }
            KeyCode::Char('m') => {
                self.dimension = Dimension::Model;
                None
            }
            KeyCode::Char('p') => {
                self.dimension = Dimension::Project;
                None
            }
            KeyCode::Char('D') => {
                self.granularity = Granularity::Daily;
                None
            }
            KeyCode::Char('W') => {
                self.granularity = Granularity::Weekly;
                None
            }
            KeyCode::Char('M') => {
                self.granularity = Granularity::Monthly;
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
        use ratatui::widgets::Paragraph;

        let _keep_engine_alive = &self.engine;

        let text = match self.state.try_read() {
            Ok(state) => {
                let body = if state.last_good.is_some() {
                    "(snapshot lines TBD by C.7)".to_string()
                } else if let Some(error) = &state.last_error {
                    format!("Error: {error:#}")
                } else {
                    "Loading...".to_string()
                };

                if state.refreshing {
                    format!("{body}\nRefreshing...")
                } else {
                    body
                }
            }
            Err(_) => "Refreshing...".to_string(),
        };

        frame.render_widget(Paragraph::new(text), area);
    }

    fn tick(&mut self) {}
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
        let engine = AnalyticsEngine::open(db_path).unwrap();
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
