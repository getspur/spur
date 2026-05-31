use std::{path::Path, sync::Arc};

use jute::state::State;

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
) -> NotebookRunContext {
    notebook_run_context_with_runner(
        notebook_path,
        state,
        bridge,
        app,
        daemon,
        RunCellCommandRunner::new,
    )
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
    let deps = Arc::new(ServerDeps::new(bridge, Some(state), app, daemon));
    let runner = build_runner(Arc::clone(&deps));
    let engine = ReactiveEngine::new(
        store,
        runner,
        notebook_path,
        notebook_port_root(notebook_path),
    );

    NotebookRunContext { deps, engine }
}
