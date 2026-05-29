use std::{io, path::Path, time::Duration};

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
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<&'a str>,
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
    match parse_notebook_command(arg)? {
        NotebookCommand::Reopen => send_control("reopen", None, None, None, socket_path).await,
        NotebookCommand::New => send_control("new", None, None, None, socket_path).await,
        NotebookCommand::Close => send_control("close", None, None, None, socket_path).await,
        NotebookCommand::Open { path } => {
            send_control("open", Some(&path), None, None, socket_path).await
        }
        NotebookCommand::AttachDatasource { path, name, group } => {
            send_control(
                "attach_datasource",
                Some(&path),
                Some(&name),
                group.as_deref(),
                socket_path,
            )
            .await
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NotebookCommand {
    Reopen,
    New,
    Close,
    Open {
        path: String,
    },
    AttachDatasource {
        path: String,
        name: String,
        group: Option<String>,
    },
}

fn parse_notebook_command(arg: &str) -> anyhow::Result<NotebookCommand> {
    let trimmed = arg.trim();
    match trimmed {
        "" => return Ok(NotebookCommand::Reopen),
        "new" => return Ok(NotebookCommand::New),
        "close" => return Ok(NotebookCommand::Close),
        _ => {}
    }

    if let Some(rest) = strip_data_add(trimmed) {
        return parse_attach_datasource(rest);
    }

    Ok(NotebookCommand::Open {
        path: trimmed.to_string(),
    })
}

fn strip_data_add(trimmed: &str) -> Option<&str> {
    if trimmed == "data add" {
        Some("")
    } else {
        trimmed.strip_prefix("data add ").map(str::trim_start)
    }
}

fn parse_attach_datasource(rest: &str) -> anyhow::Result<NotebookCommand> {
    let tokens = split_notebook_args(rest)?;
    let Some(path) = tokens.first() else {
        anyhow::bail!("usage: /notebook data add <path> [--name X] [--group G]");
    };
    if path.starts_with("--") {
        anyhow::bail!("usage: /notebook data add <path> [--name X] [--group G]");
    }

    let mut name = None;
    let mut group = None;
    let mut index = 1;
    while index < tokens.len() {
        let token = &tokens[index];
        match token.as_str() {
            "--name" => {
                index += 1;
                let Some(value) = tokens.get(index) else {
                    anyhow::bail!("--name requires a value");
                };
                name = Some(value.clone());
            }
            "--group" => {
                index += 1;
                let Some(value) = tokens.get(index) else {
                    anyhow::bail!("--group requires a value");
                };
                group = Some(value.clone());
            }
            _ if token.starts_with("--name=") => {
                name = Some(token["--name=".len()..].to_string());
            }
            _ if token.starts_with("--group=") => {
                group = Some(token["--group=".len()..].to_string());
            }
            _ if token.starts_with("--") => {
                anyhow::bail!("unknown /notebook data add option: {token}");
            }
            _ => {
                anyhow::bail!("unexpected /notebook data add argument: {token}");
            }
        }
        index += 1;
    }

    let name = match name {
        Some(name) if !name.trim().is_empty() => name,
        Some(_) => anyhow::bail!("--name requires a non-empty value"),
        None => infer_datasource_name(path)?,
    };
    if group
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        anyhow::bail!("--group requires a non-empty value");
    }

    Ok(NotebookCommand::AttachDatasource {
        path: path.clone(),
        name,
        group,
    })
}

fn split_notebook_args(input: &str) -> anyhow::Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaping = false;

    for ch in input.chars() {
        if escaping {
            current.push(ch);
            escaping = false;
            continue;
        }
        if ch == '\\' {
            escaping = true;
            continue;
        }
        if let Some(quote_ch) = quote {
            if ch == quote_ch {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if escaping {
        current.push('\\');
    }
    if quote.is_some() {
        anyhow::bail!("unterminated quote in /notebook data add");
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    Ok(tokens)
}

fn infer_datasource_name(path: &str) -> anyhow::Result<String> {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("could not infer datasource name from path; pass --name"))
}

async fn send_control(
    command: &str,
    path: Option<&str>,
    name: Option<&str>,
    group: Option<&str>,
    socket_path: &Path,
) -> anyhow::Result<ControlResponse> {
    let mut stream = connect_control_socket(socket_path).await?;
    let request = ControlRequest {
        daemon: "notebook.v1",
        command,
        path,
        name,
        group,
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
