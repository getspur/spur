//! Refresh task for the Insights view.
//!
//! Spawned by InsightsView::new. Loops on a tokio interval (5s while the
//! Live tab is active, 60s otherwise) and on manual refresh signals. Each
//! refresh runs the build_snapshot in a tokio::time::timeout. Timeout
//! STOPS WAITING; it does NOT cancel the in-flight spawn_blocking query
//! (Tokio docs explicit on this).

use super::builder::build_snapshot;
use super::state::RefreshState;
use anyhow::anyhow;
use spur_context::AsyncEngine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

pub(crate) fn spawn_refresh_task(
    engine: AsyncEngine,
    state: Arc<RwLock<RefreshState>>,
    is_live_tab: Arc<AtomicBool>,
    mut signal_rx: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let interval = if is_live_tab.load(Ordering::Relaxed) {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(60)
            };
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                opt = signal_rx.recv() => {
                    if opt.is_none() {
                        return;
                    }
                }
            }
            {
                let mut s = state.write().await;
                s.refreshing = true;
            }
            let result =
                tokio::time::timeout(Duration::from_secs(30), build_snapshot(&engine)).await;
            let mut s = state.write().await;
            s.refreshing = false;
            match result {
                Ok(Ok(snap)) => {
                    s.last_good = Some(snap);
                    s.last_error = None;
                }
                Ok(Err(e)) => {
                    s.last_error = Some(Arc::new(e));
                }
                Err(_) => {
                    s.last_error = Some(Arc::new(anyhow!("refresh timed out (30s)")));
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::insights::state::RefreshState;
    use spur_context::{AnalyticsEngine, AsyncEngine};
    use std::env;
    use std::ffi::OsString;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, RwLock};

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = env::var_os(key);
            env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                env::set_var(self.key, previous);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    #[tokio::test]
    async fn refresh_task_publishes_snapshot_after_signal() {
        let _env_guard = ENV_LOCK.lock().await;
        let tmp = TempDir::new().unwrap();
        let claude_projects = tmp.path().join(".claude/projects");
        let claude_dir = claude_projects.join("proj");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let jsonl = claude_dir.join("session-1.jsonl");
        std::fs::write(
            &jsonl,
            b"{\"timestamp\":\"2026-04-28T12:00:00Z\",\"sessionId\":\"s1\",\"requestId\":\"r1\",\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"model\":\"claude-sonnet-4-5\",\"usage\":{\"input_tokens\":100,\"output_tokens\":50,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}},\"costUSD\":0.001}\n",
        )
        .unwrap();

        let codex_home = tmp.path().join("codex-empty");
        let kiro_home = tmp.path().join("kiro-empty");
        let kimi_home = tmp.path().join("kimi-empty");
        let gemini_home = tmp.path().join("gemini-empty");
        let opencode_home = tmp.path().join("opencode-empty");
        for dir in [
            &codex_home,
            &kiro_home,
            &kimi_home,
            &gemini_home,
            &opencode_home,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }

        let _claude_config = EnvVarGuard::set("CLAUDE_CONFIG_DIR", &claude_projects);
        let _codex_home = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let _kiro_home = EnvVarGuard::set("KIRO_HOME", &kiro_home);
        let _kimi_home = EnvVarGuard::set("KIMI_HOME", &kimi_home);
        let _gemini_home = EnvVarGuard::set("GEMINI_HOME", &gemini_home);
        let _opencode_home = EnvVarGuard::set("OPENCODE_DATA_DIR", &opencode_home);

        let engine = AnalyticsEngine::open_in_memory().unwrap();
        engine.initialize().unwrap();
        engine.create_agent_views().unwrap();
        let async_engine = AsyncEngine::new(engine);
        // Prime the materialized cache so the 5s assertion below tests
        // refresh-task wake/publish behavior, not cold DuckDB scan latency.
        async_engine
            .run(|e| {
                e.refresh_cache()?;
                e.use_cached_events()?;
                Ok(())
            })
            .await
            .unwrap();

        let state = Arc::new(RwLock::new(RefreshState::default()));
        let is_live_tab = Arc::new(AtomicBool::new(false));
        let (signal_tx, signal_rx) = mpsc::channel(8);

        let _handle = spawn_refresh_task(async_engine, state.clone(), is_live_tab, signal_rx);

        signal_tx.send(()).await.unwrap();

        let mut tries = 50;
        while state.read().await.last_good.is_none() && tries > 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            tries -= 1;
        }

        let s = state.read().await;
        assert!(
            s.last_good.is_some(),
            "expected last_good to be populated after signal; refreshing={}, last_error={:?}",
            s.refreshing,
            s.last_error.as_ref().map(|e| format!("{:#}", e))
        );

        drop(signal_tx);
    }
}
