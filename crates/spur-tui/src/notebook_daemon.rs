use std::{
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::sleep,
};

const FRAME_LIMIT: usize = 16 * 1024 * 1024;
const CONNECT_ATTEMPTS: usize = 5;
const CONNECT_INITIAL_DELAY: Duration = Duration::from_millis(100);
const CONNECT_MAX_DELAY: Duration = Duration::from_millis(800);

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

pub async fn send_notebook_command(
    arg: &str,
    socket_path: &Path,
) -> anyhow::Result<ControlResponse> {
    let trimmed = arg.trim();
    match trimmed {
        "" => send_control("reopen", None, socket_path).await,
        "new" => send_control("new", None, socket_path).await,
        "close" => send_control("close", None, socket_path).await,
        path => send_control("open", Some(PathBuf::from(path)), socket_path).await,
    }
}

async fn send_control(
    command: &str,
    path: Option<PathBuf>,
    socket_path: &Path,
) -> anyhow::Result<ControlResponse> {
    let mut stream = connect_control_socket(socket_path).await?;
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

async fn connect_control_socket(socket_path: &Path) -> io::Result<UnixStream> {
    let mut delay = CONNECT_INITIAL_DELAY;
    for attempt in 0..CONNECT_ATTEMPTS {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if should_retry_connect_error(&error) && attempt + 1 < CONNECT_ATTEMPTS => {
                sleep(delay).await;
                delay = delay.saturating_mul(2).min(CONNECT_MAX_DELAY);
            }
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::other("notebook daemon connect retry exhausted"))
}

fn should_retry_connect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::NotFound
            | io::ErrorKind::AddrNotAvailable
    )
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
