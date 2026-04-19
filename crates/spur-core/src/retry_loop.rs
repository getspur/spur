//! DN-2: shared review-retry invariants for orchestrator.rs's production
//! retry site and test_support::run_gate_with_retries.
//!
//! Both sites must agree on:
//! - `attempt_n: u32` starts at 1.
//! - Bound is strict `>`: with `max_retries=3`, attempts 1..=4 run; attempt
//!   4's Retry outcome exceeds the bound and returns Failed.
//! - Error string on exhaustion: `"retry limit exceeded after {n} attempts"`
//!   where `n` is the actual count of attempts that ran (== final
//!   `attempt_n`), NOT `max_retries`.
//!
//! Two callers, two API shapes:
//!
//! 1. Pure/stateless callers (test_support::run_gate_with_retries) use
//!    `RetryLoop::new(max).run(|n| async { ... })` — the closure returns
//!    `RetryOutcome::Terminal(status)` or `RetryOutcome::Retry` and the
//!    combinator owns the bound check + exhaustion status.
//!
//! 2. Production (`orchestrator.rs` review gate) keeps its inline loop
//!    because per-attempt state (`retry_history`, `current_task`,
//!    `next_worker_session`, `worktrees`) is mutated across iterations and
//!    factoring it into a closure requires `Arc<tokio::sync::Mutex<_>>`
//!    around state that is otherwise cleanly stack-owned — a regression.
//!    Production calls `RetryLoop::check_exceeded(attempt_n, max)` which
//!    returns `Some(DelegationStatus::Failed { error: ... })` when the
//!    bound is exceeded, or `None` to continue. The bound check and error
//!    string live here — `grep -n 'attempt_n > .*max_review_retries'
//!    crates/spur-core/src/orchestrator.rs` returns zero hits.
//!
//! If someone changes the strict `>` semantic or the error string format,
//! both sites adjust together (both go through this module).

use spur_acp::DelegationStatus;
use std::future::Future;

/// Outcome of a single attempt inside `RetryLoop::run`.
pub enum RetryOutcome {
    /// Attempt reached a terminal status — return it unchanged.
    Terminal(DelegationStatus),
    /// Attempt produced a Retry decision — loop bumps `attempt_n` and
    /// calls the closure again unless the bound is exceeded.
    Retry,
}

/// Bounded retry loop for review-gated delegation attempts.
#[derive(Debug, Clone, Copy)]
pub struct RetryLoop {
    max_retries: u32,
}

impl RetryLoop {
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }

    /// Pure/stateless retry loop. The closure is called with
    /// 1-indexed `attempt_n` and returns a `RetryOutcome`. When the
    /// closure returns `Retry` with `attempt_n > max_retries`, the loop
    /// stops and returns `DelegationStatus::Failed { error: "retry
    /// limit exceeded after {attempt_n} attempts" }`.
    pub async fn run<F, Fut>(&self, mut attempt: F) -> DelegationStatus
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = RetryOutcome>,
    {
        let mut attempt_n: u32 = 1;
        loop {
            match attempt(attempt_n).await {
                RetryOutcome::Terminal(status) => return status,
                RetryOutcome::Retry => {
                    if let Some(failed) = Self::check_exceeded(attempt_n, self.max_retries) {
                        return failed;
                    }
                    attempt_n += 1;
                }
            }
        }
    }

    /// Strict-`>` bound check shared with production (orchestrator.rs).
    /// Returns `Some(DelegationStatus::Failed { error })` when
    /// `attempt_n > max_retries` (caller should stop retrying), else
    /// `None` (caller may continue).
    ///
    /// The error string format is a binding contract between this
    /// module, the orchestrator review gate, and `test_support::
    /// run_gate_with_retries`. Changing it here changes both call
    /// sites' surface.
    pub fn check_exceeded(attempt_n: u32, max_retries: u32) -> Option<DelegationStatus> {
        if attempt_n > max_retries {
            Some(DelegationStatus::Failed {
                error: format!("retry limit exceeded after {attempt_n} attempts"),
            })
        } else {
            None
        }
    }
}
