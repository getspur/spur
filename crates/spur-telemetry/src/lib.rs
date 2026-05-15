pub(crate) mod batch;
pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod consent;
pub(crate) mod crash;
pub mod error;
pub mod events;
pub(crate) mod ratelimit;
pub(crate) mod redact;
pub mod tier1_events;
pub mod tier2_events;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use batch::BatchSender;
use chrono::Utc;
use client::{PosthogClient, PosthogEvent};
use consent::{is_event_allowed, EventKind};
use ratelimit::{TokenBucket, EVENTS_PER_MINUTE_CAPACITY, EVENTS_PER_SECOND_REFILL};
use tokio::runtime::RuntimeFlavor;

pub use crate::config::TelemetryConfig;
pub use error::{Result, TelemetryError};
pub use events::{Event, Props, Tier};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

pub struct InitConfig {
    pub spur_version: &'static str,
}

pub struct TelemetryGuard;

struct RuntimeState {
    active: bool,
    consent: consent::Consent,
    runtime_disabled: AtomicBool,
    anonymous_id: uuid::Uuid,
    spur_version: &'static str,
    rate_limit: Mutex<TokenBucket>,
    batch: Arc<BatchSender>,
}

static STATE: OnceLock<RuntimeState> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
pub enum TelemetryScope {
    Crash,
    Perf,
    Usage,
    All,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        shutdown_blocking();
    }
}

pub fn init(cfg: InitConfig) -> TelemetryGuard {
    let config_existed = config::config_path().exists();
    let loaded = config::load_or_default();
    let consent = consent::resolve(&loaded);
    let active = consent.crash || consent.perf || consent.usage;

    if !config_existed {
        let _ = config::save_atomic(&loaded);
    }

    STATE.get_or_init(|| {
        if active && consent.crash {
            crash::install(loaded.anonymous_id);
        }

        let client = PosthogClient::new();
        if active && consent.crash {
            let upload_client = client.clone();
            let anonymous_id = loaded.anonymous_id;
            tokio::spawn(async move {
                let _ = crash::upload_pending(&upload_client, anonymous_id).await;
            });
        }

        let send_client = client.clone();
        let batch = Arc::new(BatchSender::new(move |events| {
            let send_client = send_client.clone();
            async move { send_client.send_batch(&events).await }
        }));

        if active {
            tokio::spawn(async {
                if tokio::signal::ctrl_c().await.is_ok() {
                    shutdown(TelemetryGuard);
                    re_raise_sigint();
                }
            });
        }

        RuntimeState {
            active,
            consent,
            runtime_disabled: AtomicBool::new(false),
            anonymous_id: loaded.anonymous_id,
            spur_version: cfg.spur_version,
            rate_limit: Mutex::new(TokenBucket::new(
                EVENTS_PER_MINUTE_CAPACITY,
                EVENTS_PER_SECOND_REFILL,
            )),
            batch,
        }
    });

    TelemetryGuard
}

pub fn shutdown(_guard: TelemetryGuard) {
    shutdown_blocking();
}

pub fn telemetry_active() -> bool {
    STATE
        .get()
        .map(|s| s.active && !s.runtime_disabled.load(Ordering::Relaxed))
        .unwrap_or(false)
}

pub(crate) fn emit<E: Event>(event: E) {
    let Some(state) = STATE.get() else {
        return;
    };

    let event_kind = match E::TIER {
        events::Tier::One => EventKind::Perf,
        events::Tier::Two => EventKind::Usage,
    };

    if !is_event_allowed(&state.consent, E::TIER, event_kind) {
        return;
    }

    if state.runtime_disabled.load(Ordering::Relaxed) {
        return;
    }

    let rate_ok = {
        let mut bucket = state
            .rate_limit
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bucket.try_acquire()
    };
    if !rate_ok {
        return;
    }

    let mut props = serde_json::Map::new();
    props.insert("spur_version".to_string(), state.spur_version.into());
    for (key, value) in event.into_props() {
        props.insert(key.to_string(), value);
    }

    state.batch.try_send(PosthogEvent {
        event: E::NAME.to_string(),
        distinct_id: state.anonymous_id.to_string(),
        properties: serde_json::Value::Object(props),
        timestamp: Utc::now(),
    });
}

fn shutdown_blocking() {
    let Some(state) = STATE.get() else {
        return;
    };

    if state.runtime_disabled.swap(true, Ordering::Relaxed) {
        return;
    }

    let batch = Arc::clone(&state.batch);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == RuntimeFlavor::CurrentThread {
            handle.block_on(async {
                batch.shutdown(Some(SHUTDOWN_TIMEOUT)).await;
            });
        } else {
            tokio::task::block_in_place(move || {
                handle.block_on(async {
                    batch.shutdown(Some(SHUTDOWN_TIMEOUT)).await;
                });
            });
        }
    } else if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        rt.block_on(async {
            batch.shutdown(Some(SHUTDOWN_TIMEOUT)).await;
        });
    }
}

pub fn config_path() -> std::path::PathBuf {
    config::config_path()
}

pub fn load_config_or_default() -> TelemetryConfig {
    config::load_or_default()
}

pub fn save_config(cfg: &TelemetryConfig) -> Result<()> {
    config::save_atomic(cfg)
}

pub fn set_enabled(scope: TelemetryScope, enabled: bool) -> Result<TelemetryConfig> {
    let mut cfg = config::load_or_default();
    match scope {
        TelemetryScope::Crash => cfg.tier1_crash = enabled,
        TelemetryScope::Perf => cfg.tier1_perf = enabled,
        TelemetryScope::Usage => cfg.tier2_usage = enabled,
        TelemetryScope::All => {
            cfg.tier1_crash = enabled;
            cfg.tier1_perf = enabled;
            cfg.tier2_usage = enabled;
        }
    }
    config::save_atomic(&cfg)?;
    Ok(cfg)
}

pub fn reset_anonymous_id() -> Result<TelemetryConfig> {
    let mut cfg = config::load_or_default();
    cfg.anonymous_id = uuid::Uuid::new_v4();
    config::save_atomic(&cfg)?;
    Ok(cfg)
}

pub fn shutdown_sync() {
    shutdown_blocking();
}

#[cfg(unix)]
fn re_raise_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
}

#[cfg(not(unix))]
fn re_raise_sigint() {
    std::process::exit(130);
}

#[macro_export]
#[cfg(telemetry_disabled)]
macro_rules! emit {
    ($event_expr:expr $(,)?) => {
        ()
    };
}

#[macro_export]
#[cfg(not(telemetry_disabled))]
macro_rules! emit {
    ($event_expr:expr $(,)?) => {{
        if $crate::telemetry_active() {
            let __e = $event_expr;
            $crate::emit(__e);
        }
    }};
}

#[cfg(telemetry_disabled)]
pub const TELEMETRY_COMPILED: bool = false;

#[cfg(not(telemetry_disabled))]
pub const TELEMETRY_COMPILED: bool = true;
