use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use jute::chat_state::{build_agent_connection, load_spur_config, select_default_agent};
use jute::state::State;
use spur_acp::config::AgentConfig;

use crate::dag::ai::{AcpAgentBackend, AiNodeBackend, NullAiBackend};
use crate::dag::NotebookCellRunner;
use crate::mcp::{bridge::BridgeRequester, NotebookDaemonControl, ServerDeps};

use super::{
    engine::{CellRunner, ReactiveEngine, RunCellCommandRunner},
    notebook_port_root,
};

pub struct NotebookRunContext<R = RunCellCommandRunner>
where
    R: CellRunner,
{
    pub deps: Arc<ServerDeps>,
    pub engine: ReactiveEngine<R>,
}

pub fn notebook_run_context(
    notebook_path: impl AsRef<Path>,
    state: Arc<State>,
    bridge: Arc<dyn BridgeRequester>,
    app: Option<tauri::AppHandle>,
    daemon: Option<NotebookDaemonControl>,
) -> NotebookRunContext<NotebookCellRunner<RunCellCommandRunner>> {
    let notebook_path = notebook_path.as_ref();
    let repo_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cwd = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| repo_root.clone());

    let config = load_spur_config(&repo_root);
    let agent = select_default_agent(&config);
    let backend = ai_backend_from_config(agent.as_ref(), cwd, &repo_root);

    notebook_run_context_with_runner(notebook_path, state, bridge, app, daemon, move |deps| {
        NotebookCellRunner::new(RunCellCommandRunner::new(deps), backend)
    })
}

/// Build the AI backend for the notebook run context: a live
/// `AcpAgentBackend` over a connection mirrored from the `AgentConfig`
/// transport when one is configured, or the `NullAiBackend` otherwise.
fn ai_backend_from_config(
    agent: Option<&AgentConfig>,
    cwd: PathBuf,
    repo_root: &Path,
) -> Arc<dyn AiNodeBackend> {
    match agent {
        Some(config) => {
            let connection = build_agent_connection(config, repo_root, None);
            Arc::new(AcpAgentBackend::new(connection, cwd))
        }
        None => Arc::new(NullAiBackend),
    }
}

pub fn notebook_run_context_with_runner<R>(
    notebook_path: impl AsRef<Path>,
    state: Arc<State>,
    bridge: Arc<dyn BridgeRequester>,
    app: Option<tauri::AppHandle>,
    daemon: Option<NotebookDaemonControl>,
    build_runner: impl FnOnce(Arc<ServerDeps>) -> R,
) -> NotebookRunContext<R>
where
    R: CellRunner,
{
    let notebook_path = notebook_path.as_ref();
    let store = state.get_notebook();
    let deps = Arc::new(ServerDeps::new(bridge, Some(state), app, daemon, None));
    let runner = build_runner(Arc::clone(&deps));
    let engine = ReactiveEngine::new(
        store,
        runner,
        notebook_path,
        notebook_port_root(notebook_path),
    );

    NotebookRunContext { deps, engine }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::pin::Pin;

    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
        NewSessionResponse, PromptRequest, ProtocolVersion, SessionId, SessionNotification,
        SessionUpdate, TextContent,
    };
    use arrow_array::StringArray;
    use async_trait::async_trait;
    use jute::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, MultilineString, NotebookMetadata,
        NotebookRoot, PortSpec, SpurCellMetadata,
    };
    use serde_json::json;
    use spur_acp::config::{AgentConfig, AgentsConfig, BrainConfig, SpurConfig};
    use spur_acp::connection::AgentConnection;
    use spur_acp::types::{AgentHealth, TransportKind};
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use crate::dag::ai::acp_backend::AcpAgentBackend;
    use crate::dag::cell_runner::NotebookCellRunner;
    use crate::dag::engine::{
        CellRunOutcome, CellRunRequest, CellRunStatus, CellRunner, EngineError,
        KernelEnsureRequest, RunCellCommandRunner,
    };
    use crate::dag::{notebook_port_root, PortStore};
    use crate::mcp::bridge::{BridgeError, BridgeRequestFuture, BridgeRequester};

    /// Minimal fake ACP connection: replays canned text chunks per prompt turn.
    struct FakeConn {
        lines: Vec<String>,
    }

    #[async_trait]
    impl AgentConnection for FakeConn {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            Ok(InitializeResponse::new(ProtocolVersion::LATEST))
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            Ok(NewSessionResponse::new(SessionId::new("test-session")))
        }

        async fn prompt(
            &mut self,
            request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = SessionNotification> + Send>>>
        {
            let lines = self.lines.clone();
            let notifications = lines.into_iter().map(move |line| {
                let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(line)));
                SessionNotification::new(
                    request.session_id.clone(),
                    SessionUpdate::AgentMessageChunk(chunk),
                )
            });
            Ok(Box::pin(futures::stream::iter(notifications)))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }
    }

    /// Inner runner stub: spur cells never reach it, so any non-spur dispatch
    /// is a routing bug. Used so the engine's kernel-ensure path does not try
    /// to spawn a real kernel for the spur cell's resolved code type.
    #[derive(Clone, Default)]
    struct NoopInnerRunner;

    impl CellRunner for NoopInnerRunner {
        fn run_cell<'a>(
            &'a self,
            _request: CellRunRequest,
        ) -> Pin<
            Box<dyn std::future::Future<Output = Result<CellRunOutcome, EngineError>> + Send + 'a>,
        > {
            Box::pin(async {
                Ok(CellRunOutcome {
                    status: CellRunStatus::Succeeded,
                })
            })
        }

        fn ensure_kernel<'a>(
            &'a self,
            _request: KernelEnsureRequest,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<(), EngineError>> + Send + 'a>>
        {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct TestBridge;

    impl BridgeRequester for TestBridge {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            _method: &'static str,
            _params: serde_json::Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async {
                Err::<serde_json::Value, BridgeError>(BridgeError::Handler {
                    code: "unsupported".to_owned(),
                    message: "test bridge does not handle requests".to_owned(),
                })
            })
        }
    }

    fn spur_cell(id: &str, source: &str, produces: &[&str], consumes: &[&str]) -> Cell {
        let mut metadata = CellMetadata {
            spur: Some(SpurCellMetadata {
                version: 1,
                last_edited_by: None,
                datasource_setup: None,
                dag: Some(CellDagMetadata {
                    produces: produces
                        .iter()
                        .map(|port| PortSpec {
                            port: (*port).to_owned(),
                            repr: "arrow".to_owned(),
                            display: None,
                            class: None,
                            schema: None,
                        })
                        .collect(),
                    consumes: consumes.iter().map(|port| (*port).to_owned()).collect(),
                    source: None,
                }),
                code_type: None,
                frontend: None,
            }),
            jute_deck: None,
            other: Default::default(),
        };
        metadata
            .other
            .insert("kernelspec".to_owned(), json!({ "name": "spur" }));

        Cell::Code(CodeCell {
            id: Some(id.to_owned()),
            metadata,
            source: MultilineString::Single(source.to_owned()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn notebook_root(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn write_and_load(notebook_path: &std::path::Path, root: NotebookRoot, state: &Arc<State>) {
        std::fs::write(
            notebook_path,
            serde_json::to_vec(&root).expect("notebook json"),
        )
        .expect("write notebook");
        state.get_notebook().load(notebook_path, root);
    }

    fn read_text_port(notebook_path: &std::path::Path, port: &str) -> String {
        let store =
            PortStore::open_read_only_at(notebook_port_root(notebook_path)).expect("port store");
        let read = store.get(port).expect("read output port");
        let crate::dag::PortRead::Arrow { batches, .. } = read else {
            panic!("expected Arrow output port");
        };
        let column = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .expect("utf8 column");
        column.value(0).to_owned()
    }

    #[tokio::test]
    async fn run_context_wires_ai_backend_and_runs_spur_cell() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ai.ipynb");
        let state = Arc::new(State::new());
        write_and_load(
            &notebook_path,
            notebook_root(vec![spur_cell("ai", "summarize", &["answer"], &[])]),
            &state,
        );

        let conn: Arc<Mutex<dyn AgentConnection>> = Arc::new(Mutex::new(FakeConn {
            lines: vec!["Hello, ".to_owned(), "world".to_owned()],
        }));
        let backend = Arc::new(AcpAgentBackend::new(conn, temp.path().to_path_buf()));

        let mut context = notebook_run_context_with_runner(
            &notebook_path,
            Arc::clone(&state),
            Arc::new(TestBridge),
            None,
            None,
            move |_deps| NotebookCellRunner::new_with_inner(NoopInnerRunner, backend),
        );

        let report = context.engine.run_cell("ai").await.expect("run spur cell");
        assert_eq!(report.status, CellRunStatus::Succeeded);
        assert_eq!(read_text_port(&notebook_path, "answer"), "Hello, world");
    }

    #[tokio::test]
    async fn run_context_with_null_backend_surfaces_init_error() {
        let temp = TempDir::new().expect("temp dir");
        let notebook_path = temp.path().join("ai.ipynb");
        let state = Arc::new(State::new());
        write_and_load(
            &notebook_path,
            notebook_root(vec![spur_cell("ai", "summarize", &["answer"], &[])]),
            &state,
        );

        let backend = ai_backend_from_config(None, temp.path().to_path_buf(), temp.path());

        let mut context = notebook_run_context_with_runner(
            &notebook_path,
            Arc::clone(&state),
            Arc::new(TestBridge),
            None,
            None,
            move |_deps| NotebookCellRunner::new_with_inner(NoopInnerRunner, backend),
        );

        let error = context
            .engine
            .run_cell("ai")
            .await
            .expect_err("null backend must surface an init error");
        assert!(
            error.to_string().contains("no agent configured"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn select_default_agent_prefers_brain_then_first() {
        let stdio = AgentConfig {
            transport: TransportKind::Stdio,
            command: "echo".to_owned(),
            ..AgentConfig::with_defaults("worker")
        };
        let brain = AgentConfig {
            transport: TransportKind::Acp,
            command: "claude".to_owned(),
            ..AgentConfig::with_defaults("claude-code")
        };

        let config = SpurConfig {
            brain: BrainConfig {
                default: "claude-code".to_owned(),
                ..BrainConfig::default()
            },
            agents: AgentsConfig {
                entries: vec![stdio.clone(), brain.clone()],
            },
            ..SpurConfig::default()
        };
        assert_eq!(
            select_default_agent(&config).map(|agent| agent.name),
            Some("claude-code".to_owned())
        );

        // No brain match -> first entry.
        let config = SpurConfig {
            brain: BrainConfig {
                default: "missing".to_owned(),
                ..BrainConfig::default()
            },
            agents: AgentsConfig {
                entries: vec![stdio.clone()],
            },
            ..SpurConfig::default()
        };
        assert_eq!(
            select_default_agent(&config).map(|agent| agent.name),
            Some("worker".to_owned())
        );

        // No entries -> None.
        assert!(select_default_agent(&SpurConfig::default()).is_none());
    }

    #[tokio::test]
    async fn build_agent_connection_constructs_for_each_transport() {
        for transport in [
            TransportKind::Acp,
            TransportKind::Stdio,
            TransportKind::CliWrap,
            TransportKind::StreamJson,
        ] {
            let config = AgentConfig {
                transport,
                command: "echo".to_owned(),
                ..AgentConfig::with_defaults("agent")
            };
            let conn = build_agent_connection(&config, std::path::Path::new("/tmp"));
            assert_eq!(conn.lock().await.health(), AgentHealth::Unknown);
        }
    }

    #[allow(dead_code)]
    fn _runner_type_is_notebook_cell_runner(
        context: NotebookRunContext<NotebookCellRunner<RunCellCommandRunner>>,
    ) {
        let _ = context;
    }
}
