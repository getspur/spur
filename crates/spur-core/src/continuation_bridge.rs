//! Bridge from MCP detached completion → orchestrator ingress.
//! Enforces INV-C3 (UI event BEFORE model-visible continuation).

use spur_acp::domain::BrainContinuation;
use spur_acp::domain::events::SpurEventBody;
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

/// Exactly-once bridge from MCP result collector → orchestrator ingress.
/// Emits the UI event BEFORE sending `SystemContinuation` (INV-C3).
pub async fn report_detached_completion(
    sink: &dyn ContinuationEventSink,
    continuation_tx: &mpsc::Sender<InteractiveInput>,
    overflow: &OverflowBuf,
    session: SessionId,
    worker_session: SessionId,
    cont: BrainContinuation,
) {
    // 1) UI-visible event FIRST.
    sink.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: cont.payload.status.clone(),
    });
    // 2) Model-visible continuation SECOND (try_send + overflow fallback).
    match continuation_tx.try_send(InteractiveInput::SystemContinuation {
        session: session.clone(),
        continuation: cont.clone(),
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            overflow.lock().await.push_back((session, cont));
        }
        Err(TrySendError::Closed(_)) => {
            tracing::warn!(
                delegation_id = %cont.delegation_id,
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

use agent_client_protocol::{
    ContentBlock, EmbeddedResource, EmbeddedResourceResource,
    TextContent, TextResourceContents,
};

pub const MERGE_BUDGET_DEFAULT_BYTES: usize = 4096;

const MARKER_AUTONOMOUS: &str =
    "[SPUR:background] Detached delegation completed after tool call returned.";
const MARKER_SEPARATOR: &str =
    "[SPUR:background] The following blocks were injected by SPUR, not authored by the user.";
const ACTION_HINT: &str = "Review the result and decide the next action.";

fn continuation_uri(id: &str) -> String {
    format!("spur://continuation/{id}")
}

fn continuation_resource_block(c: &BrainContinuation) -> ContentBlock {
    // Serialize payload as JSON text inside an embedded resource.
    let json = serde_json::json!({
        "delegation_id": c.delegation_id,
        "source": format!("{:?}", c.source),
        "status": format!("{:?}", c.payload.status),
        "summary": c.payload.summary,
        "diff_summary": c.payload.diff_summary,
        "worker_branch": c.payload.worker_branch,
    }).to_string();

    ContentBlock::Resource(EmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(
            TextResourceContents::new(json, continuation_uri(&c.delegation_id))
                .mime_type(Some("application/json".into())),
        ),
    ))
}

fn text_block(s: &str) -> ContentBlock {
    ContentBlock::Text(TextContent::new(s))
}

/// Build an autonomous continuation-only turn.
pub fn render_autonomous_continuation_turn(conts: &[BrainContinuation]) -> Vec<ContentBlock> {
    let mut out = Vec::with_capacity(2 + conts.len());
    out.push(text_block(MARKER_AUTONOMOUS));
    for c in conts {
        out.push(continuation_resource_block(c));
    }
    out.push(text_block(ACTION_HINT));
    out
}

/// Build a merged user+continuation turn (no budget).
pub fn render_merged_turn(
    user_blocks: &[ContentBlock],
    conts: &[BrainContinuation],
) -> Vec<ContentBlock> {
    let mut out: Vec<ContentBlock> = user_blocks.to_vec();
    if !conts.is_empty() {
        out.push(text_block(MARKER_SEPARATOR));
        for c in conts {
            out.push(continuation_resource_block(c));
        }
    }
    out
}

/// Build a merged turn enforcing a byte budget for injected content.
/// Returns `(blocks, spilled_continuations)`. Continuations are delivered
/// oldest-first; the first one that would overflow and every following
/// continuation is returned for re-queueing.
pub fn render_merged_turn_with_spill(
    user_blocks: &[ContentBlock],
    conts: &[BrainContinuation],
    budget_bytes: usize,
) -> (Vec<ContentBlock>, Vec<BrainContinuation>) {
    let mut out: Vec<ContentBlock> = user_blocks.to_vec();
    let mut injected_bytes = 0usize;
    let separator_cost = MARKER_SEPARATOR.len();

    let mut to_inject: Vec<&BrainContinuation> = Vec::new();
    let mut spilled: Vec<BrainContinuation> = Vec::new();
    let mut separator_accounted = false;

    for c in conts {
        let block = continuation_resource_block(c);
        let cost = block_byte_cost(&block);
        let with_sep_if_first = if !separator_accounted { separator_cost } else { 0 };
        if injected_bytes + cost + with_sep_if_first > budget_bytes {
            spilled.push(c.clone());
        } else {
            if !separator_accounted {
                injected_bytes += separator_cost;
                separator_accounted = true;
            }
            injected_bytes += cost;
            to_inject.push(c);
        }
    }

    if !to_inject.is_empty() {
        out.push(text_block(MARKER_SEPARATOR));
        for c in to_inject {
            out.push(continuation_resource_block(c));
        }
    }
    (out, spilled)
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
    use spur_acp::domain::{ContinuationPayload, ContinuationSource};
    use spur_acp::domain::delegation::DelegationStatus;
    use std::time::Instant;

    fn mk_cont(id: &str) -> BrainContinuation {
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: None, diff_summary: None, worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[tokio::test]
    async fn overflow_buf_stores_on_try_send_full() {
        let buf = new_overflow_buf();
        let (_tx, _rx) = mpsc::channel::<InteractiveInput>(1);   // tiny cap
        // Fill the channel.
        _tx.try_send(InteractiveInput::Message { blocks: vec![], interrupt: false }).unwrap();

        let sid = SessionId::new();
        let c = mk_cont("id-overflow-1");
        let input = InteractiveInput::SystemContinuation {
            session: sid.clone(), continuation: c.clone()
        };
        match _tx.try_send(input) {
            Err(TrySendError::Full(_)) => {
                buf.lock().await.push_back((sid, c));
            }
            _ => panic!("expected Full"),
        }
        assert_eq!(buf.lock().await.len(), 1);
    }
}

#[cfg(test)]
mod builder_tests {
    use super::*;
    use agent_client_protocol::ContentBlock;

    fn mk_cont(id: &str, summary: &str) -> BrainContinuation {
        use spur_acp::domain::{ContinuationPayload, ContinuationSource};
        use spur_acp::domain::delegation::DelegationStatus;
        use std::time::Instant;
        BrainContinuation {
            delegation_id: id.into(),
            source: ContinuationSource::AsyncRequested,
            payload: ContinuationPayload {
                status: DelegationStatus::Success,
                summary: Some(summary.into()),
                diff_summary: None,
                worker_branch: None,
            },
            created_at: Instant::now(),
        }
    }

    #[test]
    fn autonomous_turn_has_marker_and_resource_blocks() {
        let blocks = render_autonomous_continuation_turn(&[mk_cont("id-1", "done")]);
        // Block 0: SPUR:background marker text.
        match &blocks[0] {
            ContentBlock::Text(t) => assert!(t.text.starts_with("[SPUR:background]")),
            _ => panic!("block 0 must be text marker"),
        }
        // Block 1: resource with spur://continuation/{id-1} URI.
        match &blocks[1] {
            ContentBlock::Resource(r) => {
                let uri_has_id = format!("{:?}", r).contains("spur://continuation/id-1");
                assert!(uri_has_id, "resource URI must contain delegation id");
            }
            _ => panic!("block 1 must be resource"),
        }
        // Last block: trailing action hint text.
        assert!(matches!(blocks.last(), Some(ContentBlock::Text(_))));
    }

    #[test]
    fn merged_turn_preserves_user_blocks_byte_exact_at_front() {
        let user_blocks = vec![ContentBlock::Text(agent_client_protocol::TextContent::new(
            "hello world",
        ))];
        let merged = render_merged_turn(&user_blocks, &[mk_cont("id-1", "done")]);
        assert_eq!(merged[0], user_blocks[0], "user block must be first, byte-exact");
        // Block 1: separator text marker.
        match &merged[1] {
            ContentBlock::Text(t) => {
                assert!(t.text.contains("[SPUR:background]"));
            }
            _ => panic!("separator must follow user blocks"),
        }
        // Block 2: resource.
        assert!(matches!(merged[2], ContentBlock::Resource(_)));
    }

    #[test]
    fn merged_turn_spills_when_over_budget() {
        let user_blocks = vec![ContentBlock::Text(agent_client_protocol::TextContent::new(
            "hi",
        ))];
        // 10 continuations × big summary each.
        let big = "x".repeat(4096);
        let conts: Vec<_> = (0..10).map(|i| mk_cont(&format!("id-{i}"), &big)).collect();
        let (merged, spilled) = render_merged_turn_with_spill(&user_blocks, &conts, 4096);
        assert!(spilled.len() > 0, "budget should force spill");
        // User block still present and still byte-exact.
        assert_eq!(merged[0], user_blocks[0]);
    }
}
