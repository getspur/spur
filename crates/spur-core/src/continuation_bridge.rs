//! Bridge from MCP detached completion → orchestrator ingress.
//! Enforces INV-C3 (UI event BEFORE model-visible continuation).

use serde::Serialize;
use spur_acp::domain::events::SpurEventBody;
use spur_acp::domain::{BrainContinuation, DeferReason, DelegationKey};
use spur_acp::types::SessionId;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, Mutex};

use crate::orchestrator::InteractiveInput;

/// Overflow buffer for continuations when the `InteractiveInput` ingress
/// channel is full. Drained by the orchestrator on every scheduler tick.
pub type OverflowBuf = Arc<Mutex<VecDeque<(SessionId, BrainContinuation)>>>;

pub fn new_overflow_buf() -> OverflowBuf {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Abstract sink — decouples the helper from both `FunnelHandle` (spur-core)
/// and `McpEventSink` (spur-mcp). Both types implement this by simple
/// delegation; callers in orchestrator use a closure over `FunnelHandle::emit`
/// and callers in MCP use the existing `event_sink` via a small adapter.
pub trait ContinuationEventSink: Send + Sync {
    fn emit(&self, body: SpurEventBody);
}

/// Route a detached completion's continuation into the orchestrator ingress.
///
/// **INV-C3** is preserved structurally: the orchestrator's `execute_delegation`
/// emits `SpurEventBody::DelegationCompleted` before sending the result onto
/// the oneshot; MCP's result collector awaits that oneshot before invoking
/// this helper, so the UI event is always published before the
/// `SystemContinuation` reaches the orchestrator ingress.
///
/// Callers do NOT need to emit `DelegationCompleted` separately — doing so
/// causes duplicate dashboard log entries.
pub async fn report_detached_completion(
    continuation_tx: &mpsc::Sender<InteractiveInput>,
    overflow: &OverflowBuf,
    session: SessionId,
    _worker_session: SessionId, // kept for future use (correlation logs etc.)
    cont: BrainContinuation,
) {
    tracing::debug!(
        continuation_probe = true,
        site = "A_report_detached_completion",
        delegation_id = %cont.delegation_id,
        source = ?cont.source,
        session = %session,
        "continuation path: entering report_detached_completion"
    );
    // Route the model-visible continuation (try_send + overflow fallback).
    // DelegationCompleted is emitted by execute_delegation before the oneshot
    // fires — do NOT emit it here to avoid duplicate dashboard entries.
    match continuation_tx.try_send(InteractiveInput::SystemContinuation {
        session: session.clone(),
        continuation: cont.clone(),
    }) {
        Ok(()) => {
            tracing::debug!(
                continuation_probe = true,
                site = "A_report_detached_completion",
                delegation_id = %cont.delegation_id,
                outcome = "try_send_ok",
                "continuation path: SystemContinuation enqueued on ingress"
            );
        }
        Err(TrySendError::Full(_)) => {
            let mut buf = overflow.lock().await;
            buf.push_back((session, cont.clone()));
            tracing::debug!(
                continuation_probe = true,
                site = "A_report_detached_completion",
                delegation_id = %cont.delegation_id,
                outcome = "overflow_pushed",
                overflow_depth = buf.len(),
                "continuation path: ingress full, spilled to overflow deque"
            );
        }
        Err(TrySendError::Closed(_)) => {
            tracing::warn!(
                continuation_probe = true,
                site = "A_report_detached_completion",
                delegation_id = %cont.delegation_id,
                outcome = "channel_closed",
                "continuation channel disconnected — continuation lost (orchestrator shut down)"
            );
        }
    }
}

impl ContinuationEventSink for crate::event_funnel::FunnelHandle {
    fn emit(&self, body: SpurEventBody) {
        // Use UFCS to resolve the inherent method, avoiding infinite recursion
        // between the trait method and the inherent `FunnelHandle::emit`.
        crate::event_funnel::FunnelHandle::emit(self, body)
    }
}

// ── Prompt builders ──────────────────────────────────────────────────────────

use agent_client_protocol::schema::{
    ContentBlock, EmbeddedResource, EmbeddedResourceResource, TextContent, TextResourceContents,
};

pub use spur_acp::domain::merge_budget::MERGE_BUDGET_DEFAULT_BYTES;
pub const PRODUCER_MAX_FIELD_BYTES: usize = 8192;

const MARKER_AUTONOMOUS: &str =
    "[SPUR:background] Detached delegation completed after tool call returned.";
const MARKER_SEPARATOR: &str =
    "[SPUR:background] The following blocks were injected by SPUR, not authored by the user.";
const ACTION_HINT: &str = "Review the result and decide the next action.";

#[derive(Debug)]
pub struct RenderOutcome {
    pub blocks: Vec<ContentBlock>,
    pub delivered_keys: Vec<DelegationKey>,
    pub deferred_spill: Vec<(BrainContinuation, DeferReason)>,
    pub dropped_oversized: Vec<(DelegationKey, usize)>,
}

fn continuation_uri(id: &str) -> String {
    format!("spur://continuation/{id}")
}

#[derive(Serialize)]
struct ContinuationResourceBody<'a> {
    schema_version: u8,
    delegation_id: &'a spur_acp::domain::DelegationId,
    attempt: u32,
    brain_session: &'a SessionId,
    source: &'a spur_acp::domain::ContinuationSource,
    status: &'a spur_acp::domain::delegation::DelegationStatus,
    summary: &'a Option<String>,
    diff_summary: &'a Option<spur_acp::domain::events::DiffSummary>,
    worker_branch: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_ref: &'a Option<spur_acp::domain::ArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimated_cost_micros: &'a Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact_id: &'a Option<spur_acp::domain::outcome::OutcomeKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fetch_hint: &'a Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_hint: &'a Option<String>,
    created_at_wall: &'a chrono::DateTime<chrono::Utc>,
}

fn continuation_resource_body(c: &BrainContinuation) -> ContinuationResourceBody<'_> {
    ContinuationResourceBody {
        schema_version: 3,
        delegation_id: &c.delegation_id,
        attempt: c.attempt,
        brain_session: &c.brain_session,
        source: &c.source,
        status: &c.payload.status,
        summary: &c.payload.summary,
        diff_summary: &c.payload.diff_summary,
        worker_branch: &c.payload.worker_branch,
        artifact_ref: &c.payload.artifact_ref,
        estimated_cost_micros: &c.payload.estimated_cost_micros,
        artifact_id: &c.payload.artifact_id,
        fetch_hint: &c.payload.fetch_hint,
        base_hint: &c.payload.base_hint,
        created_at_wall: &c.created_at_wall,
    }
}

fn continuation_resource_block(c: &BrainContinuation) -> ContentBlock {
    let json = serde_json::to_string(&continuation_resource_body(c))
        .expect("continuation resource body must serialize");
    ContentBlock::Resource(EmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(
            TextResourceContents::new(json, continuation_uri(c.delegation_id.as_str()))
                .mime_type(Some("application/json".into())),
        ),
    ))
}

fn text_block(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(s))
}

pub use spur_acp::domain::clip::clip_with_ellipsis;

struct PackedContinuations<'a> {
    delivered: Vec<&'a BrainContinuation>,
    delivered_keys: Vec<DelegationKey>,
    deferred_spill: Vec<(BrainContinuation, DeferReason)>,
    dropped_oversized: Vec<(DelegationKey, usize)>,
}

fn pack_continuations<'a>(
    conts: &'a [BrainContinuation],
    budget_bytes: usize,
) -> PackedContinuations<'a> {
    let mut remaining_budget = budget_bytes;
    let mut delivered = Vec::new();
    let mut delivered_keys = Vec::new();
    let mut deferred_spill = Vec::new();
    let mut dropped_oversized = Vec::new();

    for c in conts {
        let key = DelegationKey::from(c);
        let cost = continuation_cost_bytes(c);
        if cost > budget_bytes {
            dropped_oversized.push((key, cost));
            continue;
        }
        if cost <= remaining_budget {
            remaining_budget -= cost;
            delivered_keys.push(key);
            delivered.push(c);
            continue;
        }
        deferred_spill.push((
            c.clone(),
            DeferReason::BudgetSpill {
                budget_bytes,
                continuation_bytes: cost,
            },
        ));
    }

    PackedContinuations {
        delivered,
        delivered_keys,
        deferred_spill,
        dropped_oversized,
    }
}

pub fn continuation_cost_bytes(c: &BrainContinuation) -> usize {
    block_byte_cost(&continuation_resource_block(c))
}

pub fn render_merged_turn_with_spill_v2(
    user_blocks: &[ContentBlock],
    conts: &[BrainContinuation],
    budget_bytes: usize,
) -> RenderOutcome {
    let packed = pack_continuations(conts, budget_bytes);
    let mut blocks: Vec<ContentBlock> = user_blocks.to_vec();

    if !packed.delivered.is_empty() {
        blocks.push(text_block(MARKER_SEPARATOR));
        for continuation in &packed.delivered {
            blocks.push(continuation_resource_block(continuation));
        }
    }

    RenderOutcome {
        blocks,
        delivered_keys: packed.delivered_keys,
        deferred_spill: packed.deferred_spill,
        dropped_oversized: packed.dropped_oversized,
    }
}

pub fn render_autonomous_turn_with_spill_v2(
    conts: &[BrainContinuation],
    budget_bytes: usize,
) -> RenderOutcome {
    let packed = pack_continuations(conts, budget_bytes);
    let mut blocks = Vec::new();

    if !packed.delivered.is_empty() {
        blocks.push(text_block(MARKER_AUTONOMOUS));
        for continuation in &packed.delivered {
            blocks.push(continuation_resource_block(continuation));
        }
        blocks.push(text_block(ACTION_HINT));
    }

    RenderOutcome {
        blocks,
        delivered_keys: packed.delivered_keys,
        deferred_spill: packed.deferred_spill,
        dropped_oversized: packed.dropped_oversized,
    }
}

fn block_byte_cost(b: &ContentBlock) -> usize {
    match b {
        ContentBlock::Text(t) => t.text.len(),
        ContentBlock::Resource(r) => {
            match &r.resource {
                EmbeddedResourceResource::TextResourceContents(t) => t.text.len() + t.uri.len(),
                EmbeddedResourceResource::BlobResourceContents(_) => 256, // best-effort
                _ => 256, // non_exhaustive catch-all
            }
        }
        _ => 128,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::{ContentBlock, TextContent};
    use chrono::{TimeZone, Utc};
    use serde_json::{json, Value};
    use spur_acp::domain::delegation::DelegationStatus;
    use spur_acp::domain::events::DiffSummary;
    use spur_acp::domain::{
        continuation::ArtifactKind, ArtifactRef, ContinuationPayload, ContinuationSource,
        DeferReason, DelegationKey,
    };
    use std::time::Instant;

    fn mk_cont(
        id: &str,
        attempt: u32,
        source: ContinuationSource,
        summary: Option<String>,
    ) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            attempt,
            brain_session: SessionId("brain-session-1".into()),
            source,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary,
                diff_summary: None,
                worker_branch: None,
                artifact_ref: None,
                estimated_cost_micros: None,
                artifact_id: None,
                fetch_hint: None,
                base_hint: None,
            },
            created_at_wall: Utc.with_ymd_and_hms(2026, 4, 24, 12, 34, 56).unwrap(),
            created_at_mono: Instant::now(),
        }
    }

    #[tokio::test]
    async fn overflow_buf_stores_on_try_send_full() {
        let buf = new_overflow_buf();
        let (_tx, _rx) = mpsc::channel::<InteractiveInput>(1); // tiny cap
                                                               // Fill the channel.
        _tx.try_send(InteractiveInput::Message {
            blocks: vec![],
            interrupt: false,
        })
        .unwrap();

        let sid = SessionId::new();
        let c = mk_cont("id-overflow-1", 1, ContinuationSource::AsyncRequested, None);
        let input = InteractiveInput::SystemContinuation {
            session: sid.clone(),
            continuation: c.clone(),
        };
        match _tx.try_send(input) {
            Err(TrySendError::Full(_)) => {
                buf.lock().await.push_back((sid, c));
            }
            _ => panic!("expected Full"),
        }
        assert_eq!(buf.lock().await.len(), 1);
    }

    fn continuation_cost(continuation: &BrainContinuation) -> usize {
        block_byte_cost(&continuation_resource_block(continuation))
    }

    fn continuation_with_cost_between(
        id: &str,
        min_exclusive: usize,
        max_inclusive: usize,
    ) -> BrainContinuation {
        for len in 0..16_384 {
            let candidate = mk_cont(
                id,
                1,
                ContinuationSource::AsyncRequested,
                Some("x".repeat(len)),
            );
            let cost = continuation_cost(&candidate);
            if cost > min_exclusive && cost <= max_inclusive {
                return candidate;
            }
        }
        panic!("no continuation cost found between {min_exclusive} and {max_inclusive} bytes");
    }

    fn continuation_with_cost_above(id: &str, min_exclusive: usize) -> BrainContinuation {
        for len in 0..16_384 {
            let candidate = mk_cont(
                id,
                1,
                ContinuationSource::AsyncRequested,
                Some("x".repeat(len)),
            );
            if continuation_cost(&candidate) > min_exclusive {
                return candidate;
            }
        }
        panic!("no continuation cost found above {min_exclusive} bytes");
    }

    fn first_resource_json(blocks: &[ContentBlock]) -> Value {
        let resource = blocks
            .iter()
            .find_map(|block| match block {
                ContentBlock::Resource(resource) => Some(resource),
                _ => None,
            })
            .expect("expected a resource block");

        match &resource.resource {
            EmbeddedResourceResource::TextResourceContents(text) => {
                serde_json::from_str(&text.text).expect("resource JSON must parse")
            }
            other => panic!("expected text resource contents, got {other:?}"),
        }
    }

    fn deferred_keys_with_reason(outcome: &RenderOutcome) -> Vec<(DelegationKey, DeferReason)> {
        outcome
            .deferred_spill
            .iter()
            .map(|(continuation, reason)| (DelegationKey::from(continuation), reason.clone()))
            .collect()
    }

    #[test]
    fn test_render_merged_user_blocks_byte_exact() {
        let user_blocks = vec![
            ContentBlock::Text(TextContent::new("hello")),
            ContentBlock::Text(TextContent::new("world")),
        ];
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let outcome = render_merged_turn_with_spill_v2(
            &user_blocks,
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );

        assert_eq!(&outcome.blocks[..user_blocks.len()], user_blocks.as_slice());
        assert_eq!(
            outcome.delivered_keys,
            vec![DelegationKey::from(&continuation)]
        );
        assert!(outcome.deferred_spill.is_empty());
        assert!(outcome.dropped_oversized.is_empty());
    }

    #[test]
    fn test_render_merged_oversized_goes_to_dropped() {
        let user_blocks = vec![ContentBlock::Text(TextContent::new("hello"))];
        let continuation = continuation_with_cost_above("id-huge", 256);
        let key = DelegationKey::from(&continuation);
        let cost = continuation_cost(&continuation);

        let outcome = render_merged_turn_with_spill_v2(
            &user_blocks,
            std::slice::from_ref(&continuation),
            cost - 1,
        );

        assert_eq!(outcome.blocks, user_blocks);
        assert!(outcome.delivered_keys.is_empty());
        assert!(outcome.deferred_spill.is_empty());
        assert_eq!(outcome.dropped_oversized, vec![(key, cost)]);
    }

    #[test]
    fn test_render_merged_best_fit_skips_oversized_but_packs_later_small() {
        let user_blocks = vec![ContentBlock::Text(TextContent::new("hello"))];
        let small_first = mk_cont(
            "id-small-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("a".into()),
        );
        let small_later = mk_cont(
            "id-small-2",
            1,
            ContinuationSource::AsyncRequested,
            Some("b".into()),
        );
        let budget = continuation_cost(&small_first) + continuation_cost(&small_later);
        let oversized = continuation_with_cost_above("id-oversized", budget);
        let oversized_cost = continuation_cost(&oversized);

        let outcome = render_merged_turn_with_spill_v2(
            &user_blocks,
            &[small_first.clone(), oversized.clone(), small_later.clone()],
            budget,
        );

        assert_eq!(
            outcome.delivered_keys,
            vec![
                DelegationKey::from(&small_first),
                DelegationKey::from(&small_later),
            ]
        );
        assert!(outcome.deferred_spill.is_empty());
        assert_eq!(
            outcome.dropped_oversized,
            vec![(DelegationKey::from(&oversized), oversized_cost)]
        );
    }

    #[test]
    fn test_render_merged_spill_carries_defer_reason_with_budget() {
        let user_blocks = vec![ContentBlock::Text(TextContent::new("hello"))];
        let first = mk_cont(
            "id-first",
            1,
            ContinuationSource::AsyncRequested,
            Some("a".into()),
        );
        let later = mk_cont(
            "id-later",
            1,
            ContinuationSource::AsyncRequested,
            Some("b".into()),
        );
        let budget = continuation_cost(&first) + continuation_cost(&later);
        let spill = continuation_with_cost_between("id-spill", continuation_cost(&later), budget);
        let spill_cost = continuation_cost(&spill);

        let outcome = render_merged_turn_with_spill_v2(
            &user_blocks,
            &[first.clone(), spill.clone(), later.clone()],
            budget,
        );

        assert_eq!(
            outcome.delivered_keys,
            vec![DelegationKey::from(&first), DelegationKey::from(&later)]
        );
        assert_eq!(
            deferred_keys_with_reason(&outcome),
            vec![(
                DelegationKey::from(&spill),
                DeferReason::BudgetSpill {
                    budget_bytes: budget,
                    continuation_bytes: spill_cost,
                },
            )]
        );
        assert!(outcome.dropped_oversized.is_empty());
    }

    #[test]
    fn test_render_autonomous_marker_and_action_hint() {
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );

        assert_eq!(
            outcome.delivered_keys,
            vec![DelegationKey::from(&continuation)]
        );
        assert!(matches!(
            outcome.blocks.first(),
            Some(ContentBlock::Text(text)) if text.text == MARKER_AUTONOMOUS
        ));
        assert!(matches!(
            outcome.blocks.last(),
            Some(ContentBlock::Text(text)) if text.text == ACTION_HINT
        ));
    }

    #[test]
    fn test_render_autonomous_same_budget_as_merged() {
        let first = mk_cont(
            "id-first",
            1,
            ContinuationSource::AsyncRequested,
            Some("a".into()),
        );
        let later = mk_cont(
            "id-later",
            1,
            ContinuationSource::AsyncRequested,
            Some("b".into()),
        );
        let budget = continuation_cost(&first) + continuation_cost(&later);
        let spill = continuation_with_cost_between("id-spill", continuation_cost(&later), budget);

        let merged = render_merged_turn_with_spill_v2(
            &[ContentBlock::Text(TextContent::new("hello"))],
            &[first.clone(), spill.clone(), later.clone()],
            budget,
        );
        let autonomous = render_autonomous_turn_with_spill_v2(
            &[first.clone(), spill.clone(), later.clone()],
            budget,
        );

        assert_eq!(merged.delivered_keys, autonomous.delivered_keys);
        assert_eq!(
            deferred_keys_with_reason(&merged),
            deferred_keys_with_reason(&autonomous)
        );
        assert_eq!(merged.dropped_oversized, autonomous.dropped_oversized);
    }

    #[test]
    fn test_wire_json_schema_version_3() {
        let mut continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        continuation.payload.estimated_cost_micros = Some(12_345);
        continuation.payload.artifact_id = Some(spur_acp::domain::outcome::OutcomeKey {
            brain_session_id: spur_acp::BrainSessionId::new(SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            delegation_id: "deadbeef-1111-2222-3333-444455556666".into(),
            attempt: 1,
        });
        continuation.payload.fetch_hint = Some("Call fetch_outcome_artifact.".into());
        continuation.payload.base_hint = Some("Pass worker_branch as base for follow-up.".into());
        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );
        let json = first_resource_json(&outcome.blocks);

        assert_eq!(json["schema_version"], Value::from(3));
        assert_eq!(json["estimated_cost_micros"], Value::from(12_345));
        assert!(json["artifact_id"].is_object());
        assert_eq!(
            json["fetch_hint"],
            Value::from("Call fetch_outcome_artifact.")
        );
        assert_eq!(
            json["base_hint"],
            Value::from("Pass worker_branch as base for follow-up.")
        );
    }

    #[test]
    fn v3_emits_v2_compatible_json_when_new_fields_are_none() {
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let json = serde_json::to_value(continuation_resource_body(&continuation))
            .expect("continuation resource body must serialize");

        assert_eq!(json["schema_version"], Value::from(3));
        assert!(json.get("estimated_cost_micros").is_none());
        assert!(json.get("artifact_id").is_none());
        assert!(json.get("fetch_hint").is_none());
        assert!(json.get("base_hint").is_none());
    }

    #[test]
    fn test_wire_json_snake_case_source() {
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );
        let json = first_resource_json(&outcome.blocks);

        assert_eq!(json["source"], json!({ "kind": "async_requested" }));
    }

    #[test]
    fn test_wire_json_attempt_1_based() {
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );
        let json = first_resource_json(&outcome.blocks);

        assert_eq!(json["attempt"], Value::from(1));
    }

    #[test]
    fn test_wire_json_created_at_wall_present_mono_absent() {
        let continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );
        let json = first_resource_json(&outcome.blocks);

        assert!(json.get("created_at_wall").is_some());
        assert!(json.get("created_at_mono").is_none());
    }

    #[test]
    fn test_wire_json_artifact_ref_serialized() {
        let mut continuation = mk_cont(
            "id-1",
            1,
            ContinuationSource::AsyncRequested,
            Some("done".into()),
        );
        continuation.payload.diff_summary = Some(DiffSummary {
            files_changed: 3,
            insertions: 42,
            deletions: 7,
            files: vec![],
        });
        continuation.payload.worker_branch = Some("spur/worker-codex-123".into());
        continuation.payload.artifact_ref = Some(ArtifactRef {
            kind: ArtifactKind::Patch,
            uri: "spur://artifact/abc".into(),
            byte_size: 123_456,
            sha256: Some("a".repeat(64)),
            git_object_ref: None,
            git_blob_sha: None,
        });

        let outcome = render_autonomous_turn_with_spill_v2(
            std::slice::from_ref(&continuation),
            continuation_cost(&continuation),
        );
        let json = first_resource_json(&outcome.blocks);

        assert_eq!(
            json["artifact_ref"],
            json!({
                "kind": "patch",
                "uri": "spur://artifact/abc",
                "byte_size": 123456,
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            })
        );
    }

    #[test]
    fn test_clip_with_ellipsis_utf8_safe() {
        let input = Some("éééé".to_string());
        let (clipped, truncated) = clip_with_ellipsis(input, 5);

        assert_eq!(clipped.as_deref(), Some("é…"));
        assert!(truncated);
    }
}
