//! Regression coverage for prompt-terminal notification ordering.

#![recursion_limit = "256"]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use spur_acp::config::{AgentConfig, SpurConfig};
use spur_acp::domain::events::SpurEventBody;
use spur_acp::{ContentBlock, TextContent};
use spur_core::continuation_bridge::new_overflow_buf;
use spur_core::orchestrator::InteractiveInput;
use spur_core::Orchestrator;
use tokio::sync::mpsc;

fn init_git_repo(path: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(path)
        .status()
        .expect("git init should start");
    assert!(status.success(), "git init should succeed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn trailing_notification_precedes_turn_complete() {
    let repo = tempfile::tempdir().expect("tempdir");
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
        .send(InteractiveInput::Message {
            blocks: vec![ContentBlock::Text(TextContent::new(
                "sequence it".to_owned(),
            ))],
            interrupt: false,
        })
        .await
        .expect("interactive input should be accepted");

    let order = tokio::time::timeout(Duration::from_secs(10), async {
        let mut order = Vec::new();
        while order.len() < 2 {
            match events.recv().await {
                Ok(event) => match event.body {
                    SpurEventBody::AgentNotification { notification, .. }
                        if format!("{notification:?}").contains("prompt-tail") =>
                    {
                        order.push("notification");
                    }
                    SpurEventBody::TurnComplete { .. } => order.push("turn_complete"),
                    SpurEventBody::BrainError { message, .. } => {
                        panic!("prompt failed unexpectedly: {message}")
                    }
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    panic!("test event receiver lagged by {skipped}")
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    panic!("event stream closed before prompt completed")
                }
            }
        }
        order
    })
    .await
    .expect("prompt notification and terminal event should both arrive");

    drop(input_tx);
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("interactive loop should stop")
        .expect("interactive task should join")
        .expect("interactive loop should shut down cleanly");

    assert_eq!(
        order,
        vec!["notification", "turn_complete"],
        "TurnComplete must not overtake a trailing notification from the same prompt"
    );
}
