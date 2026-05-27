//! Shared helpers for SPUR's deterministic concurrency tests under
//! [`madsim`](https://github.com/madsim-rs/madsim).
//!
//! # Scope
//!
//! This crate is empty unless built with `--cfg madsim`. In normal builds the
//! lib body is gated out and no dependencies are pulled. Consumers add it as
//! a cfg-gated dev-dep:
//!
//! ```toml
//! [target.'cfg(madsim)'.dev-dependencies]
//! spur-test-madsim = { path = "../spur-test-madsim" }
//! ```
//!
//! # Patterns this crate consolidates
//!
//! The first wave of madsim tests (notification_drain, session_pump_retire,
//! delegation_watchdog, peer_mailbox_drain, native_shutdown) converged on a
//! few patterns worth sharing rather than reinventing per site.
//!
//! ## Source-include
//!
//! `madsim-tokio` is a drop-in replacement for `tokio` but the lib crate
//! under test must compile against it. Integration tests can include the
//! relevant source as a test module via `include!`, which then resolves
//! `tokio::` to the madsim alias inside that test binary only:
//!
//! ```ignore
//! #![cfg(madsim)]
//! use madsim::tokio as tokio;  // alias
//! mod pump { include!("../../src/notification_drain.rs"); }
//! ```
//!
//! Production code is untouched.
//!
//! ## `timeout_at` polyfill
//!
//! See [`timeout_at`]. `madsim-tokio` 0.2.30 does not expose
//! `tokio::time::timeout_at`, only `timeout(Duration)`. This crate maps the
//! absolute-deadline form onto the relative form using madsim's simulated
//! clock.

#![cfg(madsim)]

extern crate madsim_tokio as tokio;

use std::future::Future;

/// Polyfill for `tokio::time::timeout_at` against an absolute simulated
/// deadline. `madsim-tokio` 0.2.30 only exposes the relative-duration
/// `timeout`, so we compute the gap from the current simulated clock.
///
/// Returns `Err(Elapsed)` if `deadline` is in the past at call time, matching
/// the behavior of `tokio::time::timeout_at`.
pub async fn timeout_at<F: Future>(
    deadline: tokio::time::Instant,
    fut: F,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    let now = tokio::time::Instant::now();
    let dur = deadline.saturating_duration_since(now);
    tokio::time::timeout(dur, fut).await
}
