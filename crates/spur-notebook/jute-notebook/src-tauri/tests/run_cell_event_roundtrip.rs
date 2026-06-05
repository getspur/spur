//! Verifies notebook run-cell events round-trip through serde JSON.

use jute::backend::{
    commands::RunCellEvent,
    wire_protocol::{CommMessage, CommOpen},
};
use serde_json::json;

fn roundtrip(event: &RunCellEvent) -> RunCellEvent {
    let json = serde_json::to_string(event).expect("serialize RunCellEvent");
    serde_json::from_str(&json).expect("deserialize RunCellEvent")
}

#[test]
fn run_cell_event_comm_open_roundtrips() {
    let payload = CommOpen {
        comm_id: "comm-1".into(),
        target_name: "jupyter.widget".into(),
        data: json!({
            "state": {
                "value": 42
            }
        }),
    };
    let event = RunCellEvent::CommOpen(payload.clone());

    let round = roundtrip(&event);

    assert!(matches!(
        round,
        RunCellEvent::CommOpen(round_payload) if round_payload == payload
    ));
}

#[test]
fn run_cell_event_comm_msg_roundtrips() {
    let payload = CommMessage {
        comm_id: "comm-1".into(),
        data: json!({
            "method": "update",
            "state": {
                "value": 43
            }
        }),
    };
    let event = RunCellEvent::CommMsg(payload.clone());

    let round = roundtrip(&event);

    assert!(matches!(
        round,
        RunCellEvent::CommMsg(round_payload) if round_payload == payload
    ));
}

#[test]
fn run_cell_event_comm_close_roundtrips() {
    let payload = CommMessage {
        comm_id: "comm-1".into(),
        data: json!({
            "reason": "frontend closed"
        }),
    };
    let event = RunCellEvent::CommClose(payload.clone());

    let round = roundtrip(&event);

    assert!(matches!(
        round,
        RunCellEvent::CommClose(round_payload) if round_payload == payload
    ));
}
