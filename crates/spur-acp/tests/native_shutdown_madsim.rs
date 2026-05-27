#![cfg(madsim)]

extern crate madsim_tokio as tokio;

#[path = "../src/connection/native_shutdown.rs"]
mod native_shutdown;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use native_shutdown::{escalate_shutdown_stages, ShutdownGraceWindows, ShutdownStageOutcome};

#[test]
fn child_exiting_after_stdin_close_skips_process_group_signals() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_if_supplied();

        let exited = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let exits_after_stdin = exited.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            exits_after_stdin.store(true, Ordering::SeqCst);
        });

        let term_events = events.clone();
        let kill_events = events.clone();
        let outcome = escalate_shutdown_stages(
            || exited.load(Ordering::SeqCst),
            grace_windows(),
            || term_events.lock().unwrap().push("term"),
            || kill_events.lock().unwrap().push("kill"),
        )
        .await;

        assert_eq!(outcome, ShutdownStageOutcome::ExitedAfterStdinClose);
        assert!(
            events.lock().unwrap().is_empty(),
            "SIGTERM/SIGKILL stubs must not run when stdin-close exits inside grace"
        );
    });
}

#[test]
fn unresponsive_child_reaches_sigkill_at_sigterm_boundary() {
    madsim::runtime::Builder::from_env().run(|| async {
        assert_seed_if_supplied();

        let exited = Arc::new(AtomicBool::new(false));
        let events = Arc::new(Mutex::new(Vec::<(&'static str, Duration)>::new()));
        let started = tokio::time::Instant::now();

        let term_events = events.clone();
        let kill_events = events.clone();
        let kill_exits = exited.clone();
        let outcome = escalate_shutdown_stages(
            || exited.load(Ordering::SeqCst),
            grace_windows(),
            || {
                term_events
                    .lock()
                    .unwrap()
                    .push(("term", started.elapsed()))
            },
            || {
                kill_events
                    .lock()
                    .unwrap()
                    .push(("kill", started.elapsed()));
                kill_exits.store(true, Ordering::SeqCst);
            },
        )
        .await;

        assert_eq!(outcome, ShutdownStageOutcome::ExitedAfterSigkill);
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, "term");
        assert_eq!(events[1].0, "kill");
        assert!(
            events[1].1 <= Duration::from_millis(201),
            "SIGKILL should fire by stdin_grace + sigterm_grace + epsilon, got {:?}",
            events[1].1
        );
        assert!(
            started.elapsed() <= Duration::from_millis(201),
            "shutdown should complete when SIGKILL makes the child exit, got {:?}",
            started.elapsed()
        );
    });
}

fn grace_windows() -> ShutdownGraceWindows {
    ShutdownGraceWindows {
        stdin_grace: Duration::from_millis(100),
        sigterm_grace: Duration::from_millis(100),
        sigkill_grace: Duration::from_millis(100),
    }
}

fn assert_seed_if_supplied() {
    if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
        assert_eq!(
            madsim::runtime::Handle::current().seed(),
            seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
        );
    }
}
