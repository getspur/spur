//! End-to-end test: emit events via the funnel, verify broadcast
//! subscribers receive them in order AND they land in the JSONL sink.

use std::fs;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::types::SessionId;
use spur_core::event_funnel::spawn_funnel;
use spur_core::event_sink::spawn_sink;
use tokio::sync::broadcast;

#[tokio::test(flavor = "current_thread")]
async fn funnel_plus_sink_round_trip() {
    // Isolate events dir. `event_sink::events_dir()` is cwd-relative
    // (".spur/events"), so we cd into a tempdir for this test.
    let tmpdir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(tmpdir.path()).unwrap();
    fs::create_dir_all(".spur/events").unwrap();

    let (bcast_tx, mut bcast_rx) = broadcast::channel(256);
    let seq = Arc::new(AtomicU64::new(0));
    let funnel = spawn_funnel(bcast_tx.clone(), seq);
    spawn_sink(
        bcast_tx.subscribe(),
        spur_core::event_sink::DEFAULT_MAX_BYTES,
        u64::MAX,
    );

    for i in 0..10 {
        funnel.emit(SpurEventBody::TurnComplete {
            session: SessionId(format!("s-{i}")),
        });
    }

    // Drain broadcast: verify subscriber sees seq 0..10 in order.
    let mut seen_seqs = Vec::new();
    for _ in 0..10 {
        let ev = bcast_rx.recv().await.expect("recv");
        seen_seqs.push(ev.seq);
    }
    assert_eq!(seen_seqs, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

    // Give the sink time to flush (100ms flush interval, plus slack).
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Find the JSONL file.
    let files: Vec<_> = fs::read_dir(".spur/events")
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("ndjson"))
        .collect();
    assert_eq!(files.len(), 1, "expected one JSONL file");

    let contents = fs::read_to_string(files[0].path()).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 10, "expected 10 lines in JSONL");

    // Serde round-trip: parse each line back, verify seq sequence matches.
    let parsed: Vec<SpurEvent> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let persisted_seqs: Vec<u64> = parsed.iter().map(|e| e.seq).collect();
    assert_eq!(persisted_seqs, vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}
