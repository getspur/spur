use std::path::PathBuf;

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::Value;

pub(super) const PROTOCOL_VERSION: u8 = 1;
pub(super) const SPUR_EMBED_SOCKET_ENV: &str = "SPUR_EMBED_SOCKET";

const DEFAULT_SOCKET_RELATIVE_PATH: &str = ".spur/embed.sock";

#[derive(Debug, Deserialize)]
// Only the sidecar service's unix-only connection handler deserializes
// requests; on non-unix the type exists but nothing constructs it.
#[cfg_attr(not(unix), allow(dead_code))]
pub(super) struct EmbedRequest {
    #[serde(default)]
    pub(super) v: Option<u8>,
    #[serde(default)]
    pub(super) id: Option<Value>,
    pub(super) op: String,
    #[serde(default)]
    pub(super) texts: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct EmbedResponse {
    #[serde(default)]
    pub(super) v: Option<u8>,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) vectors: Option<Vec<Vec<f32>>>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PingResponse {
    #[serde(default)]
    pub(super) v: Option<u8>,
    #[serde(default)]
    pub(super) error: Option<String>,
    #[serde(default)]
    pub(super) ok: Option<bool>,
}

pub(super) fn resolve_socket_path(socket: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(socket) = socket {
        return Ok(socket);
    }
    if let Some(socket) = std::env::var_os(SPUR_EMBED_SOCKET_ENV).filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(socket));
    }

    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("failed to resolve default embed socket: HOME is not set"))?;
    Ok(PathBuf::from(home).join(DEFAULT_SOCKET_RELATIVE_PATH))
}
