use super::*;

#[cfg(unix)]
pub(crate) async fn daemon_fixture(
    socket: &Path,
    responses: Vec<Value>,
) -> tokio::task::JoinHandle<Vec<Value>> {
    use crate::orchestrator::{read_notebook_daemon_frame, write_notebook_daemon_frame};
    let listener = tokio::net::UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let mut calls = Vec::new();
        for response in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let init: Value =
                serde_json::from_slice(&read_notebook_daemon_frame(&mut stream).await.unwrap())
                    .unwrap();
            assert_eq!(init["method"], "initialize");
            let initialized = json!({"jsonrpc":"2.0", "id":init["id"], "result":{
                "protocolVersion":"2025-03-26", "capabilities":{"tools":{}},
                "serverInfo":{"name":"notebook-test", "version":"1"}
            }});
            write_notebook_daemon_frame(&mut stream, &serde_json::to_vec(&initialized).unwrap())
                .await
                .unwrap();
            let notification: Value =
                serde_json::from_slice(&read_notebook_daemon_frame(&mut stream).await.unwrap())
                    .unwrap();
            assert_eq!(notification["method"], "notifications/initialized");
            let call: Value =
                serde_json::from_slice(&read_notebook_daemon_frame(&mut stream).await.unwrap())
                    .unwrap();
            assert_eq!(call["method"], "tools/call");
            let reply = json!({"jsonrpc":"2.0", "id":call["id"], "result":response});
            calls.push(call);
            write_notebook_daemon_frame(&mut stream, &serde_json::to_vec(&reply).unwrap())
                .await
                .unwrap();
        }
        calls
    })
}

pub(crate) fn cell_response(path: &Path, id: &str) -> Value {
    json!({"content": [], "isError": false, "structuredContent": {
        "notebook_id":"book-A", "path":path, "revision":7,
        "id":id, "kind":"code", "version":3, "cell_etag":"cell:effective",
        "source":"unsaved source", "exec_count":2, "status":"success",
        "outputs":[{"output_type":"stream", "name":"stdout", "text":"previous output"}]
    }})
}

pub(crate) fn grant_fixture(dir: &Path) -> NotebookReadGrant {
    let path = dir.join("book.ipynb");
    std::fs::write(&path, "{}").unwrap();
    NotebookReadGrant::new(&path, Some(["allowed".to_owned()].into())).unwrap()
}

#[test]
fn worker_notebook_context_grants_bind_only_explicit_brain_paths() {
    let dir = tempfile::tempdir().unwrap();
    let brain = dir.path().join("brain");
    let worker = dir.path().join("worker");
    std::fs::create_dir_all(&brain).unwrap();
    std::fs::create_dir_all(&worker).unwrap();
    let expected = grant_fixture(&brain).path;
    grant_fixture(&worker);
    let grants = context_grants(
        &brain,
        &[
            "book.ipynb".into(),
            expected.display().to_string(),
            "src/main.rs".into(),
        ],
    )
    .unwrap();
    assert_eq!(grants.len(), 1, "deduplicate explicit notebooks only");
    assert_eq!(
        grants[0].path, expected,
        "never resolve against worker worktree/focus"
    );
    assert!(grants[0].cell_ids.is_none(), "whole-notebook context grant");
    assert!(context_grants(&brain, &[]).unwrap().is_empty());
    assert!(context_grants(&brain, &["missing.ipynb".into()]).is_err());
}

#[tokio::test]
async fn worker_notebook_list_rejects_even_null_id_as_undeclared_argument() {
    let dir = tempfile::tempdir().unwrap();
    let grant = grant_fixture(dir.path());
    let path = grant.path.clone();
    let reader = NotebookReader::new(dir.path().join("absent.sock"), vec![grant]);
    let error = reader
        .call(
            "notebook_list_cells",
            json!({"notebook_path":path,"id":null}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, rmcp::model::ErrorCode(-32602));
}

#[tokio::test]
async fn worker_notebook_denies_scope_expansion_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    let grant = grant_fixture(dir.path());
    let path = grant.path.clone();
    let reader = NotebookReader::new(dir.path().join("absent.sock"), vec![grant]);
    for (tool, args) in [
        (
            "notebook_write_cell",
            json!({"notebook_path":path,"id":"allowed"}),
        ),
        (
            "notebook_run_cell",
            json!({"notebook_path":path,"id":"allowed"}),
        ),
        ("notebook.open", json!({"notebook_path":path})),
        ("notebook_unknown", json!({"notebook_path":path})),
        (
            "notebook_read_cell",
            json!({"notebook_path":path,"id":"other"}),
        ),
        (
            "notebook_read_cell",
            json!({"notebook_path":dir.path().join("other.ipynb"),"id":"allowed"}),
        ),
    ] {
        let error = reader.call(tool, args).await.unwrap_err();
        assert_eq!(error.code, rmcp::model::ErrorCode(-32001), "{error}");
    }
    for args in [
        json!({"notebook_path":path, "id":"allowed", "notebook_id":"spoof"}),
        json!({"notebook_path":path, "id":"allowed", "project":"other"}),
        json!({"notebook_path":path, "id":"allowed", "source":"write"}),
    ] {
        assert_eq!(
            reader
                .call("notebook_read_cell", args)
                .await
                .unwrap_err()
                .code,
            rmcp::model::ErrorCode(-32602)
        );
    }
    assert!(reader
        .call(
            "notebook_read_cell",
            json!({"notebook_path":path,"id":"allowed"})
        )
        .await
        .unwrap_err()
        .message
        .contains("unavailable"));
}

#[tokio::test]
#[cfg(unix)]
async fn worker_notebook_preserves_effective_reads_and_filters_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let grant = grant_fixture(dir.path());
    let path = grant.path.clone();
    let socket = dir.path().join("daemon.sock");
    let expected = cell_response(&path, "allowed");
    let daemon = daemon_fixture(
        &socket,
        vec![
            expected.clone(),
            json!({
                "content":[], "structuredContent": {"notebook_id":"book-A", "path":path,
                "revision":7, "cells":[{"id":"allowed","kind":"code"},
                    {"id":"other","kind":"markdown", "source":"secret"}]}
            }),
        ],
    )
    .await;
    let reader = NotebookReader::new(socket, vec![grant]);
    let actual = reader
        .call(
            "notebook_read_cell",
            json!({"notebook_path":path,"id":"allowed"}),
        )
        .await
        .unwrap();
    assert_eq!(actual, expected["structuredContent"]);
    let list = reader
        .call("notebook_list_cells", json!({"notebook_path":path}))
        .await
        .unwrap();
    assert_eq!(list["cells"], json!([{"id":"allowed","kind":"code"}]));
    let calls = daemon.await.unwrap();
    assert_eq!(calls[0]["params"]["name"], "notebook_read_cell");
    assert_eq!(
        calls[0]["params"]["arguments"],
        json!({"notebook_path":path,"id":"allowed"})
    );
    assert_eq!(calls[1]["params"]["name"], "notebook_list_cells");
    reader.revoke();
    assert_eq!(
        reader
            .call("notebook_list_cells", json!({"notebook_path":path}))
            .await
            .unwrap_err()
            .code,
        rmcp::model::ErrorCode(-32001)
    );
}

#[tokio::test]
#[cfg(unix)]
async fn worker_notebook_rejects_wrong_upstream_identity_and_preserves_errors() {
    let dir = tempfile::tempdir().unwrap();
    let grant = grant_fixture(dir.path());
    let path = grant.path.clone();
    let socket = dir.path().join("daemon.sock");
    let daemon = daemon_fixture(&socket, vec![
        cell_response(&dir.path().join("wrong.ipynb"), "allowed"),
        cell_response(&path, "wrong-cell"),
        json!({"content":[], "isError":true, "structuredContent":{"code":"durable_commit_pending"}}),
    ]).await;
    let reader = NotebookReader::new(socket, vec![grant]);
    for _ in 0..2 {
        let error = reader
            .call(
                "notebook_read_cell",
                json!({"notebook_path":path,"id":"allowed"}),
            )
            .await
            .unwrap_err();
        assert!(error.message.contains("identity"), "{error}");
    }
    let error = reader
        .call(
            "notebook_read_cell",
            json!({"notebook_path":path,"id":"allowed"}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.data.unwrap()["structuredContent"]["code"],
        "durable_commit_pending"
    );
    daemon.await.unwrap();
}
