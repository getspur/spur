//! Injectable clock for cache TTL accounting (2026-08-27 mentions spec §4:
//! the initial compatibility TTL is 600s and time is injected so tests
//! advance it without sleeping).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Monotonic time source. `Send + Sync` so [`crate::MentionEngine`] stays
/// `Send` while holding `Arc<dyn Clock>`.
pub trait Clock: Send + Sync {
    /// Current point on the engine's monotonic timeline.
    fn now(&self) -> Instant;
}

/// Production clock backed by [`Instant::now`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Virtual clock for tests and simulations: time advances only through
/// [`ManualClock::advance`], so TTL boundaries are exercised exactly
/// (including the exclusive `age > ttl` edge) without real waiting.
#[derive(Debug)]
pub struct ManualClock {
    start: Instant,
    offset_nanos: AtomicU64,
}

impl ManualClock {
    /// Freeze a clock at the current instant.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            offset_nanos: AtomicU64::new(0),
        }
    }

    /// Move virtual time forward by `delta`.
    pub fn advance(&self, delta: Duration) {
        let nanos = u64::try_from(delta.as_nanos()).unwrap_or(u64::MAX);
        self.offset_nanos.fetch_add(nanos, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.start + Duration::from_nanos(self.offset_nanos.load(Ordering::Relaxed))
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}
