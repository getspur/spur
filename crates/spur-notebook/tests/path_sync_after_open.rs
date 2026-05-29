#[cfg(unix)]
mod unix {
    use std::{
        ffi::OsString,
        path::{Path, PathBuf},
        sync::Arc,
    };

    use jute::{
        backend::notebook::{Cell, MultilineString, NotebookRoot},
        state::State,
    };
    use serde::Deserialize;
    use serde_json::json;
    use spur_notebook::mcp::{
        bridge::{AgentBridge, BridgeError},
        loopback_requester::LoopbackDaemonRequester,
        transport::{read_frame_value, write_frame_json},
        DaemonControlRequest, DaemonWindowOps, NotebookDaemonControl,
    };
    use tokio::{
        net::{UnixListener, UnixStream},
        sync::oneshot,
    };

    struct HomeGuard {
        original: Option<OsString>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let original = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { original }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[derive(Default)]
    struct RecordingWindowOps;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestDaemonResponse {
        ok: bool,
        path: Option<String>,
        error: Option<TestDaemonError>,
    }

    #[derive(Debug, Deserialize)]
    struct TestDaemonError {
        code: String,
        message: String,
    }

    impl DaemonWindowOps for RecordingWindowOps {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, _label: &str) {}

        fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
            Ok(format!("window-{}", path.display()))
        }

        fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

        fn exit(&self) {}
    }

    #[tokio::test]
    async fn new_insert_close_persists_cells_after_open_path_syncs_store() {
        let dir = tempfile::Builder::new()
            .prefix("spur-notebook-path-sync-")
            .tempdir()
            .expect("temp dir");
        let home = dir.path().join("home");
        tokio::fs::create_dir_all(&home).await.expect("home dir");
        let _home_guard = HomeGuard::set(&home);

        let socket_path = dir.path().join("notebook.sock");
        let state = Arc::new(State::new());
        let requester = Arc::new(LoopbackDaemonRequester::new(socket_path.clone()));
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            requester,
            state,
            Arc::new(RecordingWindowOps),
            Some(dir.path().join("last.json")),
        );

        let listener = UnixListener::bind(&socket_path).expect("bind temp daemon socket");
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
        let server = {
            let control = control.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = &mut shutdown_rx => break,
                        accepted = listener.accept() => {
                            let (stream, _addr) = accepted.expect("accept daemon client");
                            let control = control.clone();
                            tokio::spawn(async move {
                                handle_daemon_connection(stream, control).await;
                            });
                        }
                    }
                }
            })
        };

        let created = send_control(
            &socket_path,
            json!({
                "daemon": "notebook.v1",
                "command": "new"
            }),
        )
        .await;
        assert!(created.ok, "{:?}", created.error);
        let path = PathBuf::from(created.path.expect("new response path"));

        let inserted = send_control(
            &socket_path,
            json!({
                "daemon": "notebook.v1",
                "command": "insert_cell",
                "kind": "markdown",
                "source": "hello"
            }),
        )
        .await;
        assert!(inserted.ok, "{:?}", inserted.error);

        let closed = send_control(
            &socket_path,
            json!({
                "daemon": "notebook.v1",
                "command": "close"
            }),
        )
        .await;

        let contents = tokio::fs::read_to_string(&path)
            .await
            .expect("created notebook is readable");
        let root: NotebookRoot = serde_json::from_str(&contents).expect("notebook parses");
        let close_error = closed
            .error
            .as_ref()
            .map(|error| format!("{}: {}", error.code, error.message));
        assert!(
            closed.ok,
            "close failed: {:?}; disk cells len: {}",
            close_error,
            root.cells.len()
        );

        assert_eq!(root.cells.len(), 1);
        let Cell::Markdown(cell) = &root.cells[0] else {
            panic!("expected markdown cell");
        };
        assert!(source_text(&cell.source).contains("hello"));

        let _ = shutdown_tx.send(());
        server.await.expect("daemon server task joins");
    }

    async fn handle_daemon_connection(mut stream: UnixStream, control: NotebookDaemonControl) {
        let value = read_frame_value(&mut stream)
            .await
            .expect("read daemon frame");
        let request: DaemonControlRequest =
            serde_json::from_value(value).expect("decode daemon request");
        let response = control.handle(request).await;
        write_frame_json(&mut stream, &response)
            .await
            .expect("write daemon response");
    }

    async fn send_control(socket_path: &Path, request: serde_json::Value) -> TestDaemonResponse {
        let mut stream = UnixStream::connect(socket_path)
            .await
            .expect("connect daemon socket");
        write_frame_json(&mut stream, &request)
            .await
            .expect("write daemon request");
        serde_json::from_value(
            read_frame_value(&mut stream)
                .await
                .expect("read daemon response"),
        )
        .expect("decode daemon response")
    }

    fn source_text(source: &MultilineString) -> String {
        match source {
            MultilineString::Single(source) => source.clone(),
            MultilineString::Multi(lines) => lines.join(""),
        }
    }
}

#[cfg(not(unix))]
#[test]
fn path_sync_after_open_requires_unix_sockets() {}
