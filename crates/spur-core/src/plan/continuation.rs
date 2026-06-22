use std::sync::Arc;

pub(crate) const ORPHAN_CLEAR_REASON_RESTART: &str = "restart-orphan-cleared";

/// Boxed async callback invoked by the detached delegation collector when a
/// delegation finishes.
///
/// Arguments:
/// - `BrainContinuation` - the completed delegation result.
/// - `String` - worker-session identifier (delegation UUID used as proxy for
///   the `DelegationCompleted` UI event; unique per delegation).
///
/// Implementer routes the continuation back to the orchestrator ingress
/// (emit UI event first, then try_send / overflow - INV-C3).
pub type DetachedCompletionCallback = Arc<
    dyn Fn(
            spur_acp::domain::BrainContinuation,
            String, // worker_session proxy
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Bundle of handles required to funnel detached delegation completions back
/// into the orchestrator's ingress channel.
///
/// Uses a boxed async callback so that `spur-mcp` does not need to depend on
/// `spur-core` (which would create a circular dependency). `spur-core` wires
/// the real `report_detached_completion` implementation in
/// `Orchestrator::build_continuation_ctx`.
pub struct DetachedContinuationCtx {
    /// See [`DetachedCompletionCallback`] for the callback contract.
    pub on_complete: DetachedCompletionCallback,
}

pub(crate) fn notify_fast_forward(fast_forward: &Option<Arc<tokio::sync::Notify>>) {
    if let Some(notify) = fast_forward {
        notify.notify_one();
    }
}
