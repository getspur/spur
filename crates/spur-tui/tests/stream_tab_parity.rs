//! Parity check: the new Stream-tab render path (via WorkerStreams +
//! ReactTrace) surfaces fidelity the old DetailPane::render_stream
//! path dropped — specifically tool-call lifecycle.

use spur_acp::{ContentBlock, ContentChunk, SessionUpdate, TextContent};
use spur_acp::{
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate, ToolCallUpdateFields,
};
use spur_tui::components::react_trace::{ActStatus, TraceKind};
use spur_tui::worker_streams::WorkerStreams;

fn msg(text: &str) -> SessionUpdate {
    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
        text,
    ))))
}

#[test]
fn new_path_covers_old_kinds_and_adds_lifecycle() {
    let mut ws = WorkerStreams::new();
    let exec = "exec-parity";
    ws.route(exec, "claude", &msg("one"));
    ws.route(
        exec,
        "claude",
        &SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "thinking",
        )))),
    );

    // ToolCall creation using the canonical pattern from
    // crates/spur-acp/src/protocol/claude_events.rs:139 and
    // crates/spur-acp/tests/adapter_smoke.rs:12.
    let tool_call_id = spur_acp::ToolCallId::new("t1");
    let tc = make_tool_call(&tool_call_id, "read");
    ws.route(exec, "claude", &SessionUpdate::ToolCall(tc));

    // ToolCallUpdate — advance the status to Completed.
    let tcu = make_tool_call_update(&tool_call_id, spur_acp::ToolCallStatus::Completed);
    ws.route(exec, "claude", &SessionUpdate::ToolCallUpdate(tcu));

    let trace = ws.get(exec).expect("trace");

    // Expect: message (coalesced), think, and act = 3 entries.
    assert_eq!(trace.entry_count(), 3, "message + think + act");

    // The key parity assertion: the Act entry has advanced from
    // Pending → Completed via ToolCallUpdate. The old render_stream
    // path dropped ToolCallUpdate entirely and could never represent this.
    let act = trace
        .entries()
        .iter()
        .find(|e| matches!(e.kind, TraceKind::Act { .. }))
        .expect("act entry");
    match &act.kind {
        TraceKind::Act { status, .. } => {
            assert!(
                matches!(status, ActStatus::Completed(_)),
                "expected Completed, got {:?}",
                status
            );
        }
        _ => unreachable!(),
    }
}

// Helper constructors using the canonical shape from:
// crates/spur-acp/src/protocol/claude_events.rs:139 and
// crates/spur-acp/tests/adapter_smoke.rs:12.
fn make_tool_call(id: &spur_acp::ToolCallId, title: &str) -> spur_acp::AcpToolCall {
    AcpToolCall::new(id.clone(), title)
}

fn make_tool_call_update(
    id: &spur_acp::ToolCallId,
    status: spur_acp::ToolCallStatus,
) -> spur_acp::AcpToolCallUpdate {
    let fields = ToolCallUpdateFields::new().status(status);
    AcpToolCallUpdate::new(id.clone(), fields)
}
