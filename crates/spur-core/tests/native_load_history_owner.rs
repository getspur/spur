//! Regression coverage for native load-history ownership and ordering.

#![recursion_limit = "256"]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use spur_acp::config::{AgentConfig, SpurConfig};
use spur_acp::domain::events::SpurEventBody;
use spur_core::continuation_bridge::new_overflow_buf;
use spur_core::orchestrator::InteractiveInput;
use spur_core::Orchestrator;
use tokio::sync::mpsc;

struct HomeOverride(Option<std::ffi::OsString>);

impl HomeOverride {
    fn new(home: &Path) -> Self {
        let original = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self(original)
    }
}

impl Drop for HomeOverride {
    fn drop(&mut self) {
        if let Some(home) = self.0.take() {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }
}

fn init_git_repo(path: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init should start");
    assert!(status.success(), "git init should succeed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_history_excludes_disk_replay_and_precedes_load_milestones() {
    let home = tempfile::tempdir().expect("home tempdir");
    let _home = HomeOverride::new(home.path());
    let sessions_dir = home.path().join(".kiro/sessions/cli");
    std::fs::create_dir_all(&sessions_dir).expect("create disk history directory");
    std::fs::write(
        sessions_dir.join("sequencing-session.jsonl"),
        r#"{"kind":"Prompt","data":{"content":[{"kind":"text","data":"disk-history"}]}}"#,
    )
    .expect("write disk history");

    let repo = tempfile::tempdir().expect("repo tempdir");
    init_git_repo(repo.path());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent_notification_sequencing.sh");
    assert!(fixture.exists(), "fixture missing at {}", fixture.display());

    let mut config = SpurConfig::default();
    config.brain.default = "sequencing-agent".to_owned();
    config.cost.db_path = repo.path().join("cost.db").display().to_string();
    let mut agent = AgentConfig::with_defaults("sequencing-agent");
    agent.command = "bash".to_owned();
    agent.args = vec![fixture.display().to_string()];
    config.agents.entries.push(agent);

    let orchestrator =
        Orchestrator::new(repo.path().to_path_buf(), config, None).expect("Orchestrator::new");
    let mut events = orchestrator.event_tx.subscribe();
    let (input_tx, input_rx) = mpsc::channel(4);
    let run = tokio::spawn(async move {
        Box::pin(orchestrator.run_interactive(input_rx, None, None, new_overflow_buf())).await
    });

    input_tx
        .send(InteractiveInput::ResumeSession {
            session_id: "sequencing-session".to_owned(),
        })
        .await
        .expect("resume input should be accepted");

    let order = tokio::time::timeout(Duration::from_secs(10), async {
        let mut order = Vec::new();
        while !(order.contains(&"wire") && order.contains(&"turn_complete")) {
            match events.recv().await {
                Ok(event) => match event.body {
                    SpurEventBody::AgentNotification { notification, .. }
                        if format!("{notification:?}").contains("wire-history") =>
                    {
                        order.push("wire");
                    }
                    SpurEventBody::SessionHistory { entries, .. }
                        if entries.iter().any(|entry| entry.text == "disk-history") =>
                    {
                        order.push("disk");
                    }
                    SpurEventBody::SessionLoaded { .. } => order.push("session_loaded"),
                    SpurEventBody::TurnComplete { .. } => order.push("turn_complete"),
                    SpurEventBody::BrainError { message, .. } => {
                        panic!("load failed unexpectedly: {message}")
                    }
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    panic!("test event receiver lagged by {skipped}")
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("event stream closed before load completed")
                }
            }
        }
        order
    })
    .await
    .expect("wire history and load terminal event should both arrive");

    drop(input_tx);
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("interactive loop should stop")
        .expect("interactive task should join")
        .expect("interactive loop should shut down cleanly");

    assert_eq!(
        order,
        vec!["wire", "session_loaded", "turn_complete"],
        "wire replay must own history exclusively and finish before load milestones"
    );
}
