use spur_mcp::tools::{BaseSpec, BaseTarget};

/// Resolve a BaseSpec into the concrete ref passed to create_worktree.
pub(in crate::orchestrator) fn resolve_base_branch(
    spec: &BaseSpec,
    snapshot_branch: &str,
) -> String {
    match spec {
        BaseSpec::RepoMain => snapshot_branch.to_string(),
        BaseSpec::Branch { name } => name.clone(),
        BaseSpec::Commit { oid } => oid.clone(),
        BaseSpec::WithOverlay { base, .. } => resolve_base_target(base, snapshot_branch),
    }
}

fn resolve_base_target(base: &BaseTarget, snapshot_branch: &str) -> String {
    match base {
        BaseTarget::RepoMain => snapshot_branch.to_string(),
        BaseTarget::Branch { name } => name.clone(),
        BaseTarget::Commit { oid } => oid.clone(),
    }
}

/// Extract the overlay list from a BaseSpec, preserving reconciler order.
pub(in crate::orchestrator) fn extract_overlays(spec: &BaseSpec) -> Vec<(String, String, String)> {
    match spec {
        BaseSpec::WithOverlay { overlays, .. } => overlays
            .iter()
            .map(|overlay| {
                (
                    overlay.source_task_id.clone(),
                    overlay.base_oid.clone(),
                    overlay.tip_oid.clone(),
                )
            })
            .collect(),
        BaseSpec::RepoMain | BaseSpec::Branch { .. } | BaseSpec::Commit { .. } => Vec::new(),
    }
}

/// Whether the dispatch path needs to call `snapshot_brain_state`.
/// Required only when the resolved base would fall back to the snapshot
/// branch — i.e. `None` / `RepoMain` / `WithOverlay { base: RepoMain }`.
/// Explicit `Branch` / `Commit` bases consume no snapshot, so taking one
/// just to throw it away is wasted work and (per br-osl) actively breaks
/// dispatch when the brain WT is dirty.
pub(in crate::orchestrator) fn snapshot_required_for_dispatch(spec: Option<&BaseSpec>) -> bool {
    match spec {
        None => true,
        Some(BaseSpec::RepoMain) => true,
        Some(BaseSpec::Branch { .. }) | Some(BaseSpec::Commit { .. }) => false,
        Some(BaseSpec::WithOverlay { base, .. }) => matches!(base, BaseTarget::RepoMain),
    }
}

pub(in crate::orchestrator) fn emit_dispatch_overlay_applied(
    funnel: &crate::event_funnel::FunnelHandle,
    request_id: &str,
    base: Option<&BaseSpec>,
    dispatched_base_oid: &str,
    overlays: &[(String, String, String)],
) {
    funnel.emit(spur_acp::SpurEventBody::DispatchOverlayApplied {
        request_id: request_id.to_string(),
        base_spec: serde_json::to_value(base).unwrap_or(serde_json::Value::Null),
        dispatched_base_oid: dispatched_base_oid.to_string(),
        overlay_task_ids: overlays.iter().map(|(id, _, _)| id.clone()).collect(),
    });
}
