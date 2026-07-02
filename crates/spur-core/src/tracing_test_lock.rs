//! Serializes tests that install thread-local tracing subscribers.
//!
//! Installing or dropping any `Dispatch` (via `with_default`/`set_default`)
//! rebuilds tracing's GLOBAL callsite interest cache. When that rebuild races
//! a `warn!` emitted on another thread, the event can be evaluated against a
//! filter that says "disabled" and silently dropped — observed as
//! `shadow_projector_suppresses_superseded_status_partial_match_only` failing
//! with 0 captured warnings roughly once per thousand parallel lib runs.
//! Every test that captures tracing output must hold this lock while its
//! subscriber is installed.

use std::sync::{Mutex, MutexGuard, PoisonError};

static TRACING_SUBSCRIBER_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn guard() -> MutexGuard<'static, ()> {
    TRACING_SUBSCRIBER_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}
