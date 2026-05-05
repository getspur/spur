//! Exponential-backoff-with-jitter retry policy for transient flock contention.

use std::time::Duration;

#[derive(Debug, Clone)]
pub struct BackoffPolicy {
    pub initial: Duration,
    pub max_step: Duration,
    pub factor: f64,
    pub jitter: f64,
    pub ceiling: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(50),
            max_step: Duration::from_secs(2),
            factor: 1.5,
            jitter: 0.25,
            ceiling: Duration::from_secs(10),
        }
    }
}

impl BackoffPolicy {
    /// Compute the nth step delay given a deterministic random source.
    /// Returns None if `elapsed >= ceiling` (caller should give up).
    pub fn step(&self, attempt: u32, elapsed: Duration, rand_unit: f64) -> Option<Duration> {
        if elapsed >= self.ceiling {
            return None;
        }
        let base_ms = self.initial.as_secs_f64() * 1000.0 * self.factor.powi(attempt as i32);
        let capped_ms = base_ms.min(self.max_step.as_secs_f64() * 1000.0);
        // jitter: rand_unit in [0,1] → multiplier in [1-jitter, 1+jitter]
        let jitter_mult = 1.0 - self.jitter + (2.0 * self.jitter * rand_unit);
        let step_ms = (capped_ms * jitter_mult).max(0.0);
        Some(Duration::from_millis(step_ms as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_step_is_near_initial() {
        let p = BackoffPolicy::default();
        let d = p.step(0, Duration::ZERO, 0.5).unwrap();
        // initial=50ms, jitter=0.25 → range [37.5, 62.5]ms; with rand=0.5 → 50ms
        assert!(
            d >= Duration::from_millis(37) && d <= Duration::from_millis(63),
            "{:?}",
            d
        );
    }

    #[test]
    fn caps_at_max_step() {
        let p = BackoffPolicy::default();
        // attempt=20: 50ms * 1.5^20 = ~3.3s, but max_step=2s
        let d = p.step(20, Duration::ZERO, 0.5).unwrap();
        // jitter range around 2000ms: [1500, 2500]
        assert!(d <= Duration::from_millis(2500));
    }

    #[test]
    fn returns_none_past_ceiling() {
        let p = BackoffPolicy::default();
        assert!(p.step(0, Duration::from_secs(11), 0.5).is_none());
    }

    #[test]
    fn jitter_extremes_bounded() {
        let p = BackoffPolicy::default();
        let lo = p.step(0, Duration::ZERO, 0.0).unwrap();
        let hi = p.step(0, Duration::ZERO, 1.0).unwrap();
        assert!(lo < hi);
        assert!(lo >= Duration::from_millis(37));
        assert!(hi <= Duration::from_millis(63));
    }
}
