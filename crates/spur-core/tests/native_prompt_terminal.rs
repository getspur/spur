//! Regression coverage for native ACP prompt terminal failures.

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
async fn prompt_rpc_error_emits_brain_error_without_turn_complete() {
    let repo = tempfile::tempdir().expect("tempdir");
    init_git_repo(repo.path());

    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent_prompt_error.sh");
    assert!(fixture.exists(), "fixture missing at {}", fixture.display());

    let mut config = SpurConfig::default();
    config.brain.default = "prompt-error-agent".to_owned();
    config.cost.db_path = repo.path().join("cost.db").display().to_string();

    let mut agent = AgentConfig::with_defaults("prompt-error-agent");
    agent.command = "bash".to_owned();
    agent.args = vec![fixture.display().to_string()];
    config.agents.entries.push(agent);

    let orchestrator =
        Orchestrator::new(repo.path().to_path_buf(), config, None).expect("Orchestrator::new");
    let mut events = orchestrator.event_tx.subscribe();
    let (input_tx, input_rx) = mpsc::channel(4);

    let mut run = tokio::spawn(async move {
        Box::pin(orchestrator.run_interactive(input_rx, None, None, new_overflow_buf())).await
    });

    input_tx
        .send(InteractiveInput::Message {
            blocks: vec![ContentBlock::Text(TextContent::new(
                "trigger prompt error".to_owned(),
            ))],
            interrupt: false,
        })
        .await
        .expect("interactive input should be accepted");

    let (mut brain_error, mut saw_turn_complete) =
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(event) => match event.body {
                        SpurEventBody::BrainError { message, .. } => {
                            break (Some(message), false);
                        }
                        SpurEventBody::TurnComplete { .. } => break (None, true),
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break (None, false);
                    }
                }
            }
        })
        .await
        .expect("prompt should reach a terminal host event");

    drop(input_tx);
    let run_result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            tokio::select! {
                result = &mut run => break result,
                event = events.recv() => match event {
                    Ok(event) => match event.body {
                        SpurEventBody::BrainError { message, .. } => {
                            brain_error.get_or_insert(message);
                        }
                        SpurEventBody::TurnComplete { .. } => saw_turn_complete = true,
                        _ => {}
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break run.await;
                    }
                }
            }
        }
    })
    .await
    .expect("interactive loop should stop after input closes")
    .expect("interactive task should join");
    run_result.expect("interactive loop should shut down cleanly");

    while let Ok(event) = events.try_recv() {
        match event.body {
            SpurEventBody::BrainError { message, .. } => {
                brain_error.get_or_insert(message);
            }
            SpurEventBody::TurnComplete { .. } => saw_turn_complete = true,
            _ => {}
        }
    }

    assert!(
        !saw_turn_complete,
        "an RPC error must not be reported as TurnComplete"
    );
    let brain_error = brain_error.expect("prompt RPC failure should emit BrainError");
    assert!(
        brain_error.contains("prompt exploded"),
        "BrainError should preserve the RPC failure: {brain_error}"
    );
}
