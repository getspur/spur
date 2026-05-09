//! Supplementary edge-case tests for commit-path histograms and wake-reason
//! accounting (bd-db300.3.8.1).
//!
//! The canonical types (`PhaseHistogram`, `PhasePercentiles`,
//! `WakeReasonCounters`, `WakeReasonSnapshot`) live in `group_commit.rs`.
//! This module adds edge-case coverage via the global singleton.

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use crate::group_commit::{
        GLOBAL_CONSOLIDATION_METRICS, GLOBAL_CONSOLIDATION_METRICS_TEST_LOCK,
    };

    struct ResetGlobalMetrics;

    impl Drop for ResetGlobalMetrics {
        fn drop(&mut self) {
            GLOBAL_CONSOLIDATION_METRICS.reset();
        }
    }

    fn with_global_metrics<T>(body: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_CONSOLIDATION_METRICS_TEST_LOCK
            .lock()
            .expect("global consolidation metrics test lock poisoned");
        let _reset = ResetGlobalMetrics;
        GLOBAL_CONSOLIDATION_METRICS.reset();
        body()
    }

    // ── Global histogram recording and snapshot ─────────────────────

    #[test]
    fn global_hist_phase_b_records_and_snapshots() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_phase_b.record(100);
            GLOBAL_CONSOLIDATION_METRICS.hist_phase_b.record(200);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_phase_b.count, 2);
            assert_eq!(s.hist_phase_b.max, 200);
        });
    }

    #[test]
    fn global_hist_wal_append_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_wal_append.record(50);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_wal_append.count, 1);
            assert_eq!(s.hist_wal_append.max, 50);
        });
    }

    #[test]
    fn global_hist_exclusive_lock_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_exclusive_lock.record(10);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_exclusive_lock.count, 1);
            assert_eq!(s.hist_exclusive_lock.max, 10);
        });
    }

    #[test]
    fn global_hist_consolidator_lock_wait_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .hist_consolidator_lock_wait
                .record(5);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_consolidator_lock_wait.count, 1);
            assert_eq!(s.hist_consolidator_lock_wait.max, 5);
        });
    }

    #[test]
    fn global_hist_waiter_epoch_wait_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .hist_waiter_epoch_wait
                .record(200);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_waiter_epoch_wait.count, 1);
            assert_eq!(s.hist_waiter_epoch_wait.max, 200);
        });
    }

    #[test]
    fn global_hist_arrival_wait_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_arrival_wait.record(15);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_arrival_wait.count, 1);
            assert_eq!(s.hist_arrival_wait.max, 15);
        });
    }

    #[test]
    fn global_hist_wal_sync_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_wal_sync.record(80);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_wal_sync.count, 1);
            assert_eq!(s.hist_wal_sync.max, 80);
        });
    }

    #[test]
    fn global_hist_full_commit_records() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS.hist_full_commit.record(300);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.hist_full_commit.count, 1);
            assert_eq!(s.hist_full_commit.max, 300);
        });
    }

    // ── Wake-reason global counters ─────────────────────────────────

    #[test]
    fn global_wake_reason_notify_increments() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .notify
                .fetch_add(1, Ordering::Relaxed);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.notify, 1);
        });
    }

    #[test]
    fn global_wake_reason_timeout_increments() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .timeout
                .fetch_add(1, Ordering::Relaxed);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.timeout, 1);
        });
    }

    #[test]
    fn global_wake_reason_flusher_takeover_increments() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .flusher_takeover
                .fetch_add(1, Ordering::Relaxed);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.flusher_takeover, 1);
        });
    }

    #[test]
    fn global_wake_reason_failed_epoch_increments() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .failed_epoch
                .fetch_add(1, Ordering::Relaxed);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.failed_epoch, 1);
        });
    }

    #[test]
    fn global_wake_reason_busy_retry_increments() {
        with_global_metrics(|| {
            GLOBAL_CONSOLIDATION_METRICS
                .wake_reasons
                .busy_retry
                .fetch_add(1, Ordering::Relaxed);
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.busy_retry, 1);
        });
    }

    #[test]
    fn global_wake_reason_total_is_nonnegative() {
        with_global_metrics(|| {
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            assert_eq!(s.wake_reasons.total(), 0);
        });
    }

    // ── Histogram percentile structure ──────────────────────────────

    #[test]
    fn global_hist_percentiles_are_ordered() {
        with_global_metrics(|| {
            for i in 1..=100u64 {
                GLOBAL_CONSOLIDATION_METRICS
                    .hist_wal_backend_lock_wait
                    .record(i);
            }
            let s = GLOBAL_CONSOLIDATION_METRICS.snapshot();
            let p = s.hist_wal_backend_lock_wait;
            assert_eq!(p.count, 100);
            assert!(p.p50 <= p.p95, "p50 <= p95");
            assert!(p.p95 <= p.p99, "p95 <= p99");
        });
    }
}
