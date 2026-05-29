#[cfg(unix)]
mod unix {
    use std::{sync::Arc, time::Duration};

    use jute::{
        backend::notebook::{
            Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, NotebookRoot,
            SpurCellMetadata,
        },
        commands::{handle_daemon_control_request, DaemonControlRequest},
        state::State,
    };
    use serde_json::json;
    use spur_notebook::mcp::{
        bridge::BridgeRequester,
        loopback_requester::LoopbackDaemonRequester,
        transport::{read_frame_value, write_frame_json},
    };
    use tokio::net::UnixListener;

    const CELL_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    #[tokio::test]
    async fn write_and_read_cell_round_trip_matches_bridge_contract() {
        let dir = tempfile::Builder::new()
            .prefix("spur-notebook-loopback-requester-")
            .tempdir()
            .expect("temp dir");
        let socket_path = dir.path().join("notebook.sock");
        let notebook_path = dir.path().join("notebook.ipynb");

        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load(&notebook_path, notebook_with_source("initial", 1));

        let listener = UnixListener::bind(&socket_path).expect("bind temp daemon socket");
        let server_state = Arc::clone(&state);
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _addr) = listener.accept().await.expect("accept daemon client");
                let value = read_frame_value(&mut stream)
                    .await
                    .expect("read daemon frame");
                let request: DaemonControlRequest =
                    serde_json::from_value(value).expect("decode daemon request");
                let response = handle_daemon_control_request(request, &server_state).await;
                write_frame_json(&mut stream, &response)
                    .await
                    .expect("write daemon response");
            }
        });

        let requester = LoopbackDaemonRequester::new(socket_path);
        assert!(requester.listener_registered());
        assert!(requester.window_alive());
        assert!(requester.notebook_open());

        let write = requester
            .request(
                "notebook.write_cell",
                json!({
                    "id": CELL_ID,
                    "source": "updated",
                    "expected_version": 1,
                    "last_edited_by": "brain"
                }),
                Duration::from_secs(2),
            )
            .await
            .expect("write_cell succeeds");
        assert_eq!(write, json!({ "version": 2 }));

        let read = requester
            .request(
                "notebook.read_cell",
                json!({ "id": CELL_ID }),
                Duration::from_secs(2),
            )
            .await
            .expect("read_cell succeeds");
        assert_eq!(
            read,
            json!({
                "id": CELL_ID,
                "kind": "code",
                "version": 2,
                "lastEditedBy": "brain",
                "source": "updated",
                "exec_count": null,
                "status": "idle",
                "outputs": []
            })
        );

        let insert = requester
            .request(
                "notebook.insert_cell",
                json!({
                    "kind": "markdown",
                    "after_id": CELL_ID,
                    "source": "notes",
                    "last_edited_by": "brain"
                }),
                Duration::from_secs(2),
            )
            .await
            .expect("insert_cell succeeds");
        assert_eq!(insert["version"], 3);
        let inserted_id = insert["id"].as_str().expect("inserted id").to_string();

        let (snapshot, _version) = state.get_notebook().snapshot();
        let inserted = snapshot
            .cells
            .iter()
            .find(|cell| match cell {
                Cell::Raw(cell) => cell.id.as_deref() == Some(inserted_id.as_str()),
                Cell::Markdown(cell) => cell.id.as_deref() == Some(inserted_id.as_str()),
                Cell::Code(cell) => cell.id.as_deref() == Some(inserted_id.as_str()),
            })
            .expect("inserted cell is present");
        let Cell::Markdown(cell) = inserted else {
            panic!("expected markdown cell");
        };
        assert_eq!(
            cell.metadata
                .spur
                .as_ref()
                .and_then(|spur| spur.last_edited_by.as_deref()),
            Some("brain")
        );

        requester.drain_on_shutdown().await;
        server.await.expect("daemon server task joins");
    }

    fn notebook_with_source(source: &str, version: u64) -> NotebookRoot {
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
            cells: vec![Cell::Code(CodeCell {
                id: Some(CELL_ID.to_string()),
                metadata: CellMetadata {
                    spur: Some(SpurCellMetadata {
                        version,
                        last_edited_by: None,
                        datasource_setup: None,
                    }),
                    jute_deck: None,
                    other: Default::default(),
                },
                source: MultilineString::Single(source.to_string()),
                execution_count: None,
                outputs: Vec::new(),
            })],
        }
    }
}

#[cfg(not(unix))]
#[test]
fn loopback_requester_requires_unix_sockets() {}
