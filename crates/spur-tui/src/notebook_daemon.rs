use std::{path::PathBuf, process::Stdio, time::Duration};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
};

const FRAME_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonCommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl DaemonCommandSpec {
    pub fn for_current_installation() -> Self {
        Self {
            program: spur_core::notebook::notebook_binary_path()
                .display()
                .to_string(),
            args: vec!["--headless".to_string()],
        }
    }
}

pub struct Daemon {
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Daemon {
    pub fn spawn() -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let spec = DaemonCommandSpec::for_current_installation();
        let task = tokio::spawn(supervise(spec, shutdown_rx));
        Self {
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        }
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn supervise(spec: DaemonCommandSpec, mut shutdown_rx: oneshot::Receiver<()>) {
    let mut child = match spawn_child(&spec) {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(%error, "failed to spawn spur-notebook daemon");
            tokio::select! {
                _ = &mut shutdown_rx => return,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
            match spawn_child(&spec) {
                Ok(child) => child,
                Err(error) => {
                    tracing::warn!(%error, "failed to respawn spur-notebook daemon");
                    let _ = shutdown_rx.await;
                    return;
                }
            }
        }
    };

    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                shutdown_child(child).await;
                return;
            }
            status = child.wait() => {
                match status {
                    Ok(status) => tracing::warn!(%status, "spur-notebook daemon exited"),
                    Err(error) => tracing::warn!(%error, "spur-notebook daemon wait failed"),
                }
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                child = loop {
                    match spawn_child(&spec) {
                        Ok(next) => break next,
                        Err(error) => {
                            tracing::warn!(%error, "failed to respawn spur-notebook daemon");
                        }
                    }
                    tokio::select! {
                        _ = &mut shutdown_rx => return,
                        _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    }
                };
            }
        }
    }
}

fn spawn_child(spec: &DaemonCommandSpec) -> std::io::Result<Child> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.spawn()
}

async fn shutdown_child(mut child: Child) {
    let _ = send_control("shutdown", None).await;
    match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            let _ = child.kill().await;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControlRequest<'a> {
    daemon: &'static str,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a std::path::Path>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlResponse {
    pub ok: bool,
    pub path: Option<String>,
    pub error: Option<ControlError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlError {
    pub code: String,
    pub message: String,
}

pub async fn send_notebook_command(arg: &str) -> anyhow::Result<ControlResponse> {
    let trimmed = arg.trim();
    match trimmed {
        "" => send_control("reopen", None).await,
        "new" => send_control("new", None).await,
        "close" => send_control("close", None).await,
        path => send_control("open", Some(PathBuf::from(path))).await,
    }
}

async fn send_control(command: &str, path: Option<PathBuf>) -> anyhow::Result<ControlResponse> {
    let socket_path = spur_core::notebook::control_socket_path();
    let mut stream = UnixStream::connect(&socket_path).await?;
    let request = ControlRequest {
        daemon: "notebook.v1",
        command,
        path: path.as_deref(),
    };
    let bytes = serde_json::to_vec(&request)?;
    write_frame(&mut stream, &bytes).await?;
    let response = read_frame(&mut stream).await?;
    Ok(serde_json::from_slice(&response)?)
}

async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.len() > FRAME_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "notebook daemon frame too large",
        ));
    }
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

async fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > FRAME_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "notebook daemon frame too large",
        ));
    }
    let mut bytes = vec![0_u8; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}
