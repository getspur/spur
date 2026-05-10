use super::*;

#[cfg(feature = "analytics")]
pub struct LiveCostCache {
    pub by_session: std::collections::HashMap<SessionId, f64>,
    pub last_refresh: chrono::DateTime<chrono::Utc>,
    pub last_error: Option<std::sync::Arc<anyhow::Error>>,
}

#[cfg(feature = "analytics")]
impl Default for LiveCostCache {
    fn default() -> Self {
        Self {
            by_session: std::collections::HashMap::new(),
            last_refresh: chrono::Utc::now(),
            last_error: None,
        }
    }
}

#[cfg(feature = "analytics")]
pub(crate) struct InsightsInitState {
    pub(super) started_at: Instant,
    rx: tokio::sync::oneshot::Receiver<anyhow::Result<(spur_context::AsyncEngine, bool)>>,
    /// Whole-second elapsed value last shown on the placeholder. Used to
    /// throttle redraws to 1Hz when init is in flight (instead of forcing
    /// dirty on every 30Hz tick). Initialized to `u64::MAX` so the first
    /// drain after a `Some` insertion always paints.
    last_displayed_second: u64,
}

/// Render the cold-init placeholder shown when the user has switched to
/// `ViewId::Insights` but the analytics engine is still being built on
/// the background `spawn_blocking` worker. Uses the body area provided
/// by the App's view-render dispatch (the global header/footer rows are
/// already drawn around it).
#[cfg(feature = "analytics")]
pub(super) fn render_insights_init_placeholder(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    started_at: Instant,
) {
    use ratatui::{
        layout::Alignment,
        style::{Color, Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };

    let elapsed = started_at.elapsed().as_secs();
    let title = Line::from(Span::styled(
        "Indexing logs from agent JSONL files…",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let progress = Line::from(Span::styled(
        format!("Elapsed: {elapsed}s   (~90s typical on first open; warm runs are sub-second)"),
        Style::default().fg(Color::Gray),
    ));
    let hint = Line::from(Span::styled(
        "[Esc] return to Dashboard  (indexing continues in background)",
        Style::default().fg(Color::DarkGray),
    ));
    let body = vec![
        Line::from(""),
        title,
        Line::from(""),
        progress,
        Line::from(""),
        hint,
    ];
    frame.render_widget(Paragraph::new(body).alignment(Alignment::Center), area);
}

/// Cold init pipeline for the analytics engine. Blocks (DuckDB I/O +
/// JSONL scan); ALWAYS run inside `tokio::task::spawn_blocking`.
#[cfg(feature = "analytics")]
fn build_analytics_engine_blocking() -> anyhow::Result<(spur_context::AsyncEngine, bool)> {
    use spur_context::{AnalyticsEngine, AsyncEngine};

    let t0 = std::time::Instant::now();
    let cache_dir = directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".spur").join("cache"))
        .unwrap_or_else(|| std::path::PathBuf::from(".spur/cache"));
    std::fs::create_dir_all(&cache_dir)?;
    let cache_path = cache_dir.join("cost.duckdb");
    tracing::info!(target: "spur_tui::insights", path = %cache_path.display(), "opening DuckDB cache (background)");

    let (engine, recovered) = AnalyticsEngine::open(&cache_path)?;
    engine.initialize()?;
    let view_status = engine.create_agent_views()?;
    engine.load_pricing(&spur_cost::PricingRegistry::with_builtin_prices())?;
    let materialized = engine.refresh_cache()?;
    engine.use_cached_events()?;
    tracing::info!(
        target: "spur_tui::insights",
        total_ms = t0.elapsed().as_millis() as u64,
        materialized_rows = materialized,
        ?view_status,
        "analytics engine cold init done"
    );
    Ok((AsyncEngine::new(engine), recovered))
}

impl App {
    /// Lazily open the shared DuckDB analytics cache, materialise per-agent
    /// Kick off (or no-op) the analytics-engine cold init.
    ///
    /// Returns immediately. If no init is needed (engine already cached
    /// or insights_view already constructed), nothing happens. Otherwise
    /// spawns a `spawn_blocking` task that runs the heavy DuckDB pipeline
    /// (open / initialize / create_agent_views / load_pricing /
    /// refresh_cache / use_cached_events) on a worker thread and posts
    /// the resulting `AsyncEngine` (or error) through a `oneshot` that
    /// the App's tick path drains. While in flight, the Insights view
    /// renders an "indexing logs..." placeholder.
    ///
    /// Cold first run can take ~90s (full JSONL scan across all agent
    /// homes); warm runs reuse the cache at `~/.spur/cache/cost.duckdb`
    /// and return in milliseconds. Shares the cache path with `spur cost`
    /// so a prior CLI invocation primes the data.
    #[cfg(feature = "analytics")]
    pub(super) fn start_insights_init(&mut self) {
        if self.insights_view.is_some() || self.insights_init.is_some() {
            return;
        }

        if let Some(existing) = self.analytics_engine.clone() {
            // Engine already built (e.g., earlier cold init for the
            // dashboard's live-cost cache). Construct the view directly.
            self.insights_view = Some(crate::views::insights::InsightsView::new(existing));
            return;
        }

        tracing::info!(target: "spur_tui::insights", "start_insights_init: dispatching cold init to spawn_blocking");
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(build_analytics_engine_blocking());
        });
        self.insights_init = Some(InsightsInitState {
            started_at: Instant::now(),
            rx,
            last_displayed_second: u64::MAX,
        });
    }

    /// Drain a completed `insights_init` outcome, if any. Called from the
    /// tick path. On success: caches the engine, constructs the view,
    /// keeps `current_view = Insights` so the user sees the populated
    /// dashboard. On failure: surfaces a warning and routes back to
    /// Dashboard.
    #[cfg(feature = "analytics")]
    pub(super) fn drain_insights_init(&mut self) {
        let Some(mut state) = self.insights_init.take() else {
            return;
        };
        match state.rx.try_recv() {
            Ok(Ok((engine, recovered))) => {
                tracing::info!(target: "spur_tui::insights", elapsed_ms = state.started_at.elapsed().as_millis() as u64, "insights init complete; constructing view");
                self.analytics_engine = Some(engine.clone());
                self.insights_view = Some(crate::views::insights::InsightsView::new(engine));
                if recovered {
                    self.show_user_warning(
                        "Analytics WAL was corrupt; renamed to *.broken and re-opened. Last refresh window may be missing."
                            .to_string(),
                    );
                }
                self.dirty = true;
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "spur_tui::insights", error = %format!("{e:#}"), "insights init failed");
                self.show_user_warning(format!("Analytics unavailable: {e:#}"));
                if matches!(self.current_view, ViewId::Insights) {
                    self.navigate_to(ViewId::Dashboard);
                }
                self.dirty = true;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                // Still in flight. Throttle redraws to 1Hz: only mark
                // dirty when the user is actually viewing the placeholder
                // AND the displayed whole-second has advanced. Avoids
                // 30Hz x 90s = ~2700 wasted redraws when the user has
                // Esc'd back to Dashboard while init continues.
                let elapsed = state.started_at.elapsed().as_secs();
                let visible = matches!(self.current_view, ViewId::Insights);
                if visible && elapsed != state.last_displayed_second {
                    state.last_displayed_second = elapsed;
                    self.dirty = true;
                }
                self.insights_init = Some(state);
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                tracing::warn!(target: "spur_tui::insights", "insights init worker channel closed without sending result");
                self.show_user_warning(
                    "Analytics init worker exited before reporting a result".to_string(),
                );
                if matches!(self.current_view, ViewId::Insights) {
                    self.navigate_to(ViewId::Dashboard);
                }
                self.dirty = true;
            }
        }
    }

    #[cfg(feature = "analytics")]
    pub(super) fn sync_live_cost_active_sessions(&mut self) {
        let active_sessions: std::collections::HashSet<SessionId> = self
            .lineage
            .nodes()
            .filter(|node| {
                matches!(
                    node.phase,
                    spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning
                )
            })
            .filter_map(|node| {
                node.current_attempt()
                    .map(|attempt| attempt.session_id.clone())
            })
            .collect();

        let changed = self
            .live_cost_active_sessions
            .as_ref()
            .and_then(|shared| shared.try_write().ok())
            .map(|mut guard| {
                if *guard == active_sessions {
                    false
                } else {
                    *guard = active_sessions;
                    true
                }
            })
            .unwrap_or(false);

        if changed {
            if let Some(tx) = &self.live_cost_signal_tx {
                let _ = tx.try_send(());
            }
        }
    }

    #[cfg(feature = "analytics")]
    pub(super) fn spawn_live_cost_refresh(&mut self) {
        if self.live_cost_handle.is_some() {
            return;
        }

        let Some(engine) = self.analytics_engine.clone() else {
            return;
        };
        let Some(cache) = self.live_cost_cache.clone() else {
            return;
        };
        let Some(active_sessions) = self.live_cost_active_sessions.clone() else {
            return;
        };

        let (signal_tx, mut signal_rx) = mpsc::channel(8);
        self.live_cost_signal_tx = Some(signal_tx);
        self.live_cost_handle = Some(tokio::spawn(async move {
            loop {
                let interval = {
                    let guard = active_sessions.read().await;
                    if guard.is_empty() {
                        Duration::from_secs(30)
                    } else {
                        Duration::from_secs(5)
                    }
                };

                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    opt = signal_rx.recv() => {
                        if opt.is_none() {
                            return;
                        }
                    }
                }

                let active_ids: Vec<SessionId> =
                    active_sessions.read().await.iter().cloned().collect();
                let refresh = engine.run(move |e| {
                    let mut out = std::collections::HashMap::new();
                    for sid in active_ids {
                        if let Some(snapshot) = e.live_session_snapshot(&sid.0)? {
                            out.insert(sid, snapshot.cost_usd);
                        }
                    }
                    Ok(out)
                });

                // Timeout stops waiting; it does not cancel AsyncEngine's
                // spawn_blocking closure, which will still run to completion.
                let result = tokio::time::timeout(Duration::from_secs(30), refresh).await;
                let mut guard = cache.write().await;
                match result {
                    Ok(Ok(costs)) => {
                        guard.by_session = costs;
                        guard.last_refresh = chrono::Utc::now();
                        guard.last_error = None;
                    }
                    Ok(Err(error)) => {
                        guard.last_error = Some(std::sync::Arc::new(error));
                    }
                    Err(_) => {
                        guard.last_error = Some(std::sync::Arc::new(anyhow::anyhow!(
                            "live cost refresh timed out (30s)"
                        )));
                    }
                }
            }
        }));
    }

    #[cfg(feature = "analytics")]
    pub async fn shutdown_analytics(&mut self) {
        self.insights_view.take();
        self.live_cost_signal_tx.take();
        if let Some(handle) = self.live_cost_handle.take() {
            handle.abort();
        }

        let Some(engine) = self.analytics_engine.clone() else {
            return;
        };
        match timeout(Duration::from_secs(2), engine.run(|e| e.checkpoint())).await {
            Ok(Ok(())) => {
                tracing::debug!(target: "spur_tui::insights", "analytics checkpoint completed during shutdown");
            }
            Ok(Err(error)) => {
                tracing::warn!(target: "spur_tui::insights", error = %format!("{error:#}"), "analytics checkpoint failed during shutdown");
            }
            Err(_) => {
                tracing::warn!(target: "spur_tui::insights", "analytics checkpoint timed out during shutdown");
            }
        }
    }

    #[cfg(not(feature = "analytics"))]
    pub async fn shutdown_analytics(&mut self) {}

    #[cfg(feature = "analytics")]
    pub(super) fn via_analytics_visible_for_current_view(&self) -> bool {
        let Some(cache) = &self.live_cost_cache else {
            return false;
        };
        let Ok(guard) = cache.try_read() else {
            return false;
        };

        match &self.current_view {
            ViewId::Dashboard => {
                if let Some(node_id) = self.dashboard.focused_node() {
                    return self
                        .lineage
                        .node(node_id)
                        .and_then(|node| node.current_attempt())
                        .is_some_and(|attempt| guard.by_session.contains_key(&attempt.session_id));
                }
                self.lineage
                    .nodes()
                    .filter_map(|node| node.current_attempt())
                    .any(|attempt| guard.by_session.contains_key(&attempt.session_id))
            }
            ViewId::SessionDetail(session) | ViewId::PlanInspector(session) => {
                guard.by_session.contains_key(session)
            }
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(session) => guard.by_session.contains_key(session),
            ViewId::SessionPicker
            | ViewId::IssueBrowser
            | ViewId::PlanBrowser
            | ViewId::Insights => false,
        }
    }
}
