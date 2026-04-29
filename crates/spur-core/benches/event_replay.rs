//! Criterion benchmark for `event_replay::replay_events`. Generates a
//! 50K-event fixture across 7 NDJSON files matching the realistic
//! disk-cap rotation pattern, with 1% intentionally-malformed lines.

use std::path::Path;
use std::time::SystemTime;

use criterion::{criterion_group, criterion_main, Criterion};

use spur_acp::domain::events::{SpurEvent, SpurEventBody};
use spur_acp::SessionId;
use spur_core::event_replay::{replay_events, ReplayConfig};

const FIXTURE_EVENTS: usize = 50_000;
const FILES: usize = 7;
const MALFORMED_RATIO: usize = 100; // every 100th line malformed (1%)

fn write_fixture(dir: &Path) {
    use std::io::Write;
    let per_file = FIXTURE_EVENTS / FILES;
    let mut event_idx = 0u64;
    for f_idx in 0..FILES {
        let path = dir.join(format!("100-{}-{}.ndjson", 1_000 + (f_idx as u128) * 10, f_idx));
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..per_file {
            if event_idx % MALFORMED_RATIO as u64 == 0 && event_idx > 0 {
                writeln!(f, "{{not valid json}}").unwrap();
            } else {
                let ev = SpurEvent {
                    occurred_at: SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(event_idx),
                    seq: event_idx,
                    body: SpurEventBody::TurnComplete {
                        session: SessionId(format!("s{}", i % 100)),
                    },
                };
                writeln!(f, "{}", serde_json::to_string(&ev).unwrap()).unwrap();
            }
            event_idx += 1;
        }
    }
}

fn bench_replay_full_cap(c: &mut Criterion) {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path());

    let cfg = ReplayConfig {
        events_dir: tmp.path().to_path_buf(),
        replay_horizon: std::time::Duration::from_secs(u64::MAX / 2),
        skip_pid: None,
        max_line_bytes: 8 * 1024 * 1024,
    };

    c.bench_function("replay_full_cap_50k_events", |b| {
        b.iter(|| {
            let mut count = 0u64;
            let _stats = replay_events(&cfg, |_| count += 1).unwrap();
            criterion::black_box(count);
        })
    });
}

criterion_group!(benches, bench_replay_full_cap);
criterion_main!(benches);
