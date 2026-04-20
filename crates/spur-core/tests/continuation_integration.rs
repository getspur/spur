//! Integration tests for async-continuation scheduling.
//! These exercise the bridge + orchestrator with a mock brain.

use spur_core::continuation_bridge::{
    new_overflow_buf, render_autonomous_continuation_turn, render_merged_turn_with_spill,
    MERGE_BUDGET_DEFAULT_BYTES,
};
use spur_core::scheduler::BrainScheduler;
use spur_core::orchestrator::InteractiveInput;
use spur_acp::domain::{BrainContinuation, ContinuationPayload, ContinuationSource};
use spur_acp::domain::delegation::DelegationStatus;
use spur_acp::types::SessionId;
use std::time::Instant;
use tokio::sync::mpsc;

fn mk_cont(id: &str) -> BrainContinuation {
    BrainContinuation {
        delegation_id: id.into(),
        source: ContinuationSource::AsyncRequested,
        payload: ContinuationPayload {
            status: DelegationStatus::Success,
            summary: Some("ok".into()),
            diff_summary: None,
            worker_branch: None,
            artifact: None,
        },
        created_at: Instant::now(),
    }
}

#[tokio::test]
async fn backpressure_overflow_on_full_channel() {
    let (tx, mut rx) = mpsc::channel::<InteractiveInput>(1);
    let overflow = new_overflow_buf();

    // Fill the channel.
    tx.try_send(InteractiveInput::Message { blocks: vec![], interrupt: false }).unwrap();

    // Simulate bridge calls — try_send into a full channel and overflow.
    for i in 0..5 {
        let input = InteractiveInput::SystemContinuation {
            session: SessionId::new(),
            continuation: mk_cont(&format!("id-{i}")),
        };
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(input) {
            overflow.lock().await.push_back((SessionId::new(), mk_cont(&format!("id-{i}"))));
        }
    }

    // All 5 should have overflowed (channel cap=1, already full).
    assert_eq!(overflow.lock().await.len(), 5);

    // Drain channel once → overflow still holds them until drained by scheduler.
    let _ = rx.recv().await;
    assert_eq!(overflow.lock().await.len(), 5);
}

#[test]
fn session_swap_drops_all_pending_continuations() {
    let mut s = BrainScheduler::new(Some(SessionId::new()));
    s.push_continuation(mk_cont("id-1"));
    s.push_continuation(mk_cont("id-2"));
    let evicted = s.note_session_swap(Some(SessionId::new()));
    assert_eq!(evicted.len(), 2);
    // Scheduler is now empty.
    let action = s.next(Instant::now());
    assert!(matches!(action, spur_core::scheduler::ScheduledAction::Idle));
}

#[test]
fn merged_turn_has_user_block_at_front_and_self_describing_marker() {
    use agent_client_protocol::{ContentBlock, TextContent};
    let user = vec![ContentBlock::Text(TextContent::new("what is the plan?"))];
    let (blocks, spilled) = render_merged_turn_with_spill(
        &user,
        &[mk_cont("id-1")],
        MERGE_BUDGET_DEFAULT_BYTES,
    );
    assert!(spilled.is_empty());
    // User block present byte-exact at position 0.
    assert_eq!(blocks[0], user[0]);
    // Separator marker present.
    let has_marker = blocks.iter().any(|b| matches!(b, ContentBlock::Text(t) if t.text.contains("[SPUR:background]")));
    assert!(has_marker, "merged turn must carry self-describing marker");
    // Resource with spur:// URI present.
    let has_resource = blocks.iter().any(|b| format!("{b:?}").contains("spur://continuation/id-1"));
    assert!(has_resource, "merged turn must carry spur://continuation/ resource");
}

#[test]
fn autonomous_turn_is_self_describing() {
    let blocks = render_autonomous_continuation_turn(&[mk_cont("id-42")]);
    let joined = format!("{blocks:?}");
    assert!(joined.contains("[SPUR:background]"), "must carry marker");
    assert!(joined.contains("spur://continuation/id-42"), "must carry resource URI");
}
