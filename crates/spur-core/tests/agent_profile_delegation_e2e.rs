//! End-to-end coverage for per-delegation agent profiles.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::Stream;
use spur_acp::connection::AgentConnection;
use spur_acp::types::AgentHealth;
use spur_acp::{
    AcpError, AcpSessionId, CloseSessionRequest, CloseSessionResponse, DeleteSessionRequest,
    DeleteSessionResponse, InitializeRequest, InitializeResponse, McpServer, NewSessionResponse,
    PromptRequest, ProtocolVersion, SessionNotification, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse,
};
use spur_core::test_support::run_worker_attempt_with_connection_for_test;

#[derive(Clone, Debug, PartialEq, Eq)]
enum RecordedCall {
    Initialize,
    NewSession { cwd: PathBuf, status: String },
    SetConfig { config_id: String, value: String },
    Prompt,
    CloseSession,
    DeleteSession,
    Shutdown,
}

struct RecordingConnection {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
    worktree: Arc<Mutex<Option<PathBuf>>>,
}

#[async_trait]
impl AgentConnection for RecordingConnection {
    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::Initialize);
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    async fn new_session(
        &mut self,
        cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let rendered = cwd.join(".claude/agents/code-reviewer.md");
        assert!(
            rendered.exists(),
            "managed profile must be rendered before session/new"
        );
        let status = run_git(&cwd, &["status", "--porcelain"]);
        *self.worktree.lock().expect("worktree lock") = Some(cwd.clone());
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::NewSession { cwd, status });
        Ok(NewSessionResponse::new(AcpSessionId::new("ap8-session")))
    }

    async fn prompt(
        &mut self,
        _request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let worktree = self
            .worktree
            .lock()
            .expect("worktree lock")
            .clone()
            .expect("session cwd should be recorded before prompt");
        std::fs::write(worktree.join("README.md"), "worker changed it\n")?;
        run_git(&worktree, &["add", "-A"]);
        run_git(&worktree, &["commit", "-q", "-m", "worker edit"]);

        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::Prompt);
        Ok(Box::pin(futures::stream::empty()))
    }

    async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::Shutdown);
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        AgentHealth::Ready
    }

    async fn set_session_config_option(
        &mut self,
        request: SetSessionConfigOptionRequest,
    ) -> anyhow::Result<SetSessionConfigOptionResponse> {
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::SetConfig {
                config_id: request.config_id.0.to_string(),
                value: request.value.0.to_string(),
            });
        Ok(SetSessionConfigOptionResponse::new(vec![]))
    }

    async fn close_session(
        &mut self,
        _request: CloseSessionRequest,
    ) -> Result<CloseSessionResponse, AcpError> {
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::CloseSession);
        Ok(CloseSessionResponse::new())
    }

    async fn delete_session(
        &mut self,
        _request: DeleteSessionRequest,
    ) -> Result<DeleteSessionResponse, AcpError> {
        self.calls
            .lock()
            .expect("recorded calls lock")
            .push(RecordedCall::DeleteSession);
        Ok(DeleteSessionResponse::new())
    }
}

#[tokio::test]
async fn claude_profile_materializes_selects_before_prompt_and_stays_out_of_diff() {
    let repo = setup_repo();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worktree = Arc::new(Mutex::new(None));
    let calls_for_factory = Arc::clone(&calls);
    let worktree_for_factory = Arc::clone(&worktree);
    let mut agent_config = spur_acp::AgentConfig::with_defaults("claude-code-acp");
    agent_config.kind = spur_acp::types::AgentKind::ClaudeCodeAcp;

    let outcome = run_worker_attempt_with_connection_for_test(
        repo.path().to_path_buf(),
        agent_config,
        Some("code-reviewer".to_string()),
        "review the task".to_string(),
        &move |_cfg, _spawn_args, _repo_root| {
            Box::new(RecordingConnection {
                calls: Arc::clone(&calls_for_factory),
                worktree: Arc::clone(&worktree_for_factory),
            })
        },
    )
    .await
    .expect("worker attempt should succeed");

    let rendered = outcome
        .worktree_path
        .join(".claude/agents/code-reviewer.md");
    assert!(rendered.exists(), "rendered claude profile should exist");
    let rendered_profile = std::fs::read_to_string(&rendered).expect("rendered profile readable");
    assert!(rendered_profile.starts_with(PROFILE));
    assert!(rendered_profile
        .contains("<!-- SPUR-MANAGED v=1 skill=agent-profile:code-reviewer sha256="));
    assert_eq!(
        run_git(&outcome.worktree_path, &["status", "--porcelain"]),
        "",
        "rendered profile must remain git-excluded after worker git add -A"
    );

    let recorded = calls.lock().expect("recorded calls lock").clone();
    let new_session = recorded
        .iter()
        .find_map(|call| match call {
            RecordedCall::NewSession { status, .. } => Some(status),
            _ => None,
        })
        .expect("new_session recorded");
    assert_eq!(
        new_session, "",
        "materialized profile alone must not dirty worker worktree"
    );

    let profile_set_idx = recorded
        .iter()
        .position(|call| {
            matches!(
                call,
                RecordedCall::SetConfig { config_id, value }
                    if config_id == "agent" && value == "code-reviewer"
            )
        })
        .expect("agent profile config selection recorded");
    let prompt_idx = recorded
        .iter()
        .position(|call| matches!(call, RecordedCall::Prompt))
        .expect("prompt recorded");
    assert!(
        profile_set_idx < prompt_idx,
        "profile selection must happen before prompt; calls={recorded:?}"
    );

    let diff = outcome.diff.expect("worker edit should produce a diff");
    assert!(
        diff.contains("README.md"),
        "diff should contain worker edit"
    );
    assert!(
        !diff.contains(".claude/agents/"),
        "injected agent profile leaked into collected diff:\n{diff}"
    );
}

const PROFILE: &str =
    "---\nname: code-reviewer\ndescription: Reviews diffs\n---\nReview carefully.\n";

fn setup_repo() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "test@spur.local"]);
    run_git(dir.path(), &["config", "user.name", "SPUR Test"]);
    std::fs::write(dir.path().join("README.md"), "base\n").expect("write seed");
    let profile_dir = dir.path().join(".spur/agents");
    std::fs::create_dir_all(&profile_dir).expect("create profile dir");
    std::fs::write(profile_dir.join("code-reviewer.md"), PROFILE).expect("write profile");
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
    dir
}

fn run_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}:\nstdout={}\nstderr={}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
