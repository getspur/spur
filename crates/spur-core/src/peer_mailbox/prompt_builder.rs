use crate::peer_mailbox::ledger::PeerMailboxLedger;
use crate::peer_mailbox::limits::{
    aggregate_budget_for_context_window, effective_max_message_size,
};
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::PeerMessageId;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct InjectionRecord {
    pub message_id: PeerMessageId,
    pub injected_chars: u32,
}

#[derive(Debug, Clone)]
pub struct BuiltContext {
    pub target_prompt_id: String,
    pub orchestrator_authored_text: String,
    pub injection_records: Vec<InjectionRecord>,
}

pub struct PeerPromptContextBuilder {
    ledger: Arc<dyn PeerMailboxLedger>,
}

impl PeerPromptContextBuilder {
    pub fn new(ledger: Arc<dyn PeerMailboxLedger>) -> Self {
        Self { ledger }
    }

    pub async fn build_for_target(
        &self,
        target_delegation_id: &DelegationId,
        target_context_window_chars: u64,
        max_pending_mailbox_depth: usize,
        configured_max_message_size: usize,
    ) -> BuiltContext {
        let target_prompt_id = format!("prompt-{}", Uuid::new_v4().simple());
        let pending = self.ledger.pending_for_target(target_delegation_id).await;

        let aggregate_budget = aggregate_budget_for_context_window(target_context_window_chars);
        let per_msg_cap = effective_max_message_size(
            configured_max_message_size,
            aggregate_budget,
            max_pending_mailbox_depth,
        );

        let mut text = String::new();
        let mut injections = Vec::new();
        let mut budget_remaining = aggregate_budget as usize;

        for entry in pending.into_iter().take(max_pending_mailbox_depth) {
            if entry.injected_into_prompts.contains(&target_prompt_id) {
                continue;
            }
            let truncated_body = truncate_at_char_boundary(&entry.envelope.body, per_msg_cap);
            let block = format!(
                "\n\n[peer:{kind:?} from={src} seq={seq}]\n{body}\n",
                kind = entry.envelope.kind,
                src = entry.envelope.source_executor_id,
                seq = entry.envelope.sequence,
                body = truncated_body,
            );
            if block.len() > budget_remaining {
                break;
            }
            budget_remaining -= block.len();
            text.push_str(&block);
            injections.push(InjectionRecord {
                message_id: entry.envelope.message_id,
                injected_chars: block.len() as u32,
            });
        }

        BuiltContext {
            target_prompt_id,
            orchestrator_authored_text: text,
            injection_records: injections,
        }
    }
}

fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::{InMemoryLedger, PeerMailboxLedger};
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope, PeerMessageId};
    use std::sync::Arc;
    use uuid::Uuid;

    fn envelope(body: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Handoff,
            body: body.into(),
            sequence: 1,
        }
    }

    #[tokio::test]
    async fn builder_generates_unique_target_prompt_id_per_call() {
        let ledger = Arc::new(InMemoryLedger::new());
        let builder = PeerPromptContextBuilder::new(ledger);
        let a = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        let b = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        assert_ne!(a.target_prompt_id, b.target_prompt_id);
    }

    #[tokio::test]
    async fn builder_returns_pending_messages_within_budget() {
        let ledger = Arc::new(InMemoryLedger::new());
        ledger.accept(envelope("first")).await.unwrap();
        ledger.accept(envelope("second")).await.unwrap();
        let builder = PeerPromptContextBuilder::new(ledger);
        let ctx = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        assert_eq!(ctx.injection_records.len(), 2);
        assert!(ctx.orchestrator_authored_text.contains("first"));
        assert!(ctx.orchestrator_authored_text.contains("second"));
    }

    #[tokio::test]
    async fn builder_truncates_oversized_messages_to_per_msg_cap() {
        let ledger = Arc::new(InMemoryLedger::new());
        // 32k window -> 3200 budget; depth 8 -> 400 derived per-message cap.
        ledger.accept(envelope(&"X".repeat(2_000))).await.unwrap();
        let builder = PeerPromptContextBuilder::new(ledger);
        let ctx = builder
            .build_for_target(&DelegationId("tgt".into()), 32_000, 8, 2_048)
            .await;
        assert_eq!(ctx.injection_records.len(), 1);
        assert!(ctx.injection_records[0].injected_chars <= 500);
    }

    #[tokio::test]
    async fn builder_does_not_panic_on_multibyte_utf8_truncation() {
        let ledger = Arc::new(InMemoryLedger::new());
        ledger
            .accept(envelope(&"日本語".repeat(2_000)))
            .await
            .unwrap();
        let builder = PeerPromptContextBuilder::new(ledger);
        let ctx = builder
            .build_for_target(&DelegationId("tgt".into()), 32_000, 8, 401)
            .await;
        assert_eq!(ctx.injection_records.len(), 1);
        assert!(ctx
            .orchestrator_authored_text
            .is_char_boundary(ctx.orchestrator_authored_text.len()));
        assert!(ctx.injection_records[0].injected_chars <= 500);
    }
}
