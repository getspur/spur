use agent_client_protocol::schema::McpServer;
use serde_json::json;
use spur_acp::{
    BrainSessionId, Column, DatasourceEntry, DatasourceKind, SessionId, SpurConfig, SpurEventBody,
};
use spur_core::Orchestrator;
use std::io;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct HomeGuard {
    previous_home: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let lock = ENV_LOCK.lock().expect("env lock");
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        Self {
            previous_home,
            _lock: lock,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

#[test]
fn control_socket_path_maps_nonce_under_home() {
    let path = spur_core::notebook::control_socket_path("abc");

    assert!(path.ends_with(".spur/notebooks/sessions/abc.sock"));
}

#[test]
fn control_socket_path_distinguishes_socket_nonces() {
    let first = spur_core::notebook::control_socket_path("first");
    let second = spur_core::notebook::control_socket_path("second");

    assert_ne!(first, second);
}

#[test]
fn brain_mcp_servers_preinclude_notebook_stdio_proxy_on_nonce_socket() {
    let servers =
        spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp", "fixture-nonce");

    assert!(servers.iter().any(|server| match server {
        McpServer::Http(http) => http.name == "spur-mcp",
        _ => false,
    }));

    let notebook = servers
        .iter()
        .find_map(|server| match server {
            McpServer::Stdio(stdio) if stdio.name == "notebook" => Some(stdio),
            _ => None,
        })
        .expect("notebook MCP server should be preconfigured");

    assert_eq!(notebook.args[0], "--mcp-proxy");
    assert!(notebook.args[1].ends_with("/.spur/notebooks/sessions/fixture-nonce.sock"));
}

#[tokio::test]
async fn orchestrator_emits_one_notebook_socket_nonce_for_multiple_sessions() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut config = SpurConfig::default();
    config.cost.db_path = tmp.path().join("cost.db").display().to_string();
    let orchestrator =
        Orchestrator::new(tmp.path().to_path_buf(), config, None).expect("orchestrator");
    let mut events = orchestrator.subscribe();

    orchestrator.register_notebook_socket(BrainSessionId::from(SessionId("brain-one".to_string())));
    orchestrator.register_notebook_socket(BrainSessionId::from(SessionId("brain-two".to_string())));

    let first = next_notebook_socket_ready(&mut events).await;
    let second = next_notebook_socket_ready(&mut events).await;

    assert_ne!(first.0, second.0);
    assert_eq!(first.1, second.1);
    assert!(!first.1.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn bridge_reemits_as_spur_event() {
    let repo = short_tempdir("r").expect("repo tempdir");
    let home = short_tempdir("h").expect("home tempdir");
    let _home = HomeGuard::set(home.path());
    let mut config = SpurConfig::default();
    config.cost.db_path = repo.path().join("cost.db").display().to_string();
    let orchestrator =
        Orchestrator::new(repo.path().to_path_buf(), config, None).expect("orchestrator");
    let mut events = orchestrator.subscribe();
    let session = SessionId("brain-data".to_string());

    orchestrator.register_notebook_socket(BrainSessionId::from(session.clone()));
    let (_ready_session, nonce) = next_notebook_socket_ready(&mut events).await;
    let socket_path = spur_core::notebook::control_socket_path(&nonce);
    let entry = test_datasource_entry("sales");
    let server = tokio::spawn(serve_daemon_subscription(
        socket_path,
        Vec::new(),
        vec![entry.clone()],
    ));

    let (event_session, entries) = next_datasources_changed_with_entries(&mut events).await;

    assert_eq!(event_session, session);
    assert_eq!(entries, vec![entry]);
    server.await.expect("fake daemon task finishes");
}

async fn next_notebook_socket_ready(
    events: &mut tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
) -> (SessionId, String) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for NotebookSocketReady");
        let event = tokio::time::timeout(remaining, events.recv())
            .await
            .expect("timed out waiting for NotebookSocketReady")
            .expect("event broadcast should stay open");
        if let SpurEventBody::NotebookSocketReady {
            session,
            socket_nonce,
        } = event.body
        {
            return (session, socket_nonce);
        }
    }
}

async fn next_datasources_changed_with_entries(
    events: &mut tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
) -> (SessionId, Vec<DatasourceEntry>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for DatasourcesChanged");
        let event = tokio::time::timeout(remaining, events.recv())
            .await
            .expect("timed out waiting for DatasourcesChanged")
            .expect("event broadcast should stay open");
        if let SpurEventBody::DatasourcesChanged { session, entries } = event.body {
            if !entries.is_empty() {
                return (session, entries);
            }
        }
    }
}

async fn serve_daemon_subscription(
    socket_path: PathBuf,
    snapshot_entries: Vec<DatasourceEntry>,
    push_entries: Vec<DatasourceEntry>,
) {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .expect("socket parent creates");
    }
    match tokio::fs::remove_file(&socket_path).await {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => panic!("failed to remove stale socket: {error}"),
    }
    let listener = UnixListener::bind(&socket_path).expect("fake daemon binds");
    let (mut stream, _) = listener.accept().await.expect("bridge connects");
    let request = read_frame(&mut stream)
        .await
        .expect("subscribe frame reads");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request).expect("subscribe json"),
        json!({
            "daemon": "notebook.v1",
            "command": "subscribe"
        })
    );
    write_frame(
        &mut stream,
        &serde_json::to_vec(&json!({
            "daemon": "notebook.v1",
            "event": "datasources_changed",
            "snapshot": true,
            "entries": snapshot_entries
        }))
        .expect("snapshot serializes"),
    )
    .await
    .expect("snapshot writes");
    write_frame(
        &mut stream,
        &serde_json::to_vec(&json!({
            "daemon": "notebook.v1",
            "event": "datasources_changed",
            "snapshot": false,
            "entries": push_entries
        }))
        .expect("push serializes"),
    )
    .await
    .expect("push writes");
}

async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

async fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

fn test_datasource_entry(name: &str) -> DatasourceEntry {
    DatasourceEntry {
        name: name.to_owned(),
        path: format!("/tmp/{name}.csv"),
        kind: DatasourceKind::Csv,
        group: Some("quarterly".to_owned()),
        columns: vec![Column {
            name: "region".to_owned(),
            sql_type: "VARCHAR".to_owned(),
        }],
        row_count: Some(2),
        tables: Vec::new(),
    }
}

fn short_tempdir(prefix: &str) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix(prefix).tempdir_in("/tmp")
}
