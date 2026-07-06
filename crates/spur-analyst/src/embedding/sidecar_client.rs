use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;

use super::protocol::{self, EmbedResponse, PingResponse, PROTOCOL_VERSION};

pub(super) async fn embed_query(
    query: &str,
    timeout_duration: Duration,
) -> Option<[f32; EMBEDDING_VECTOR_DIMENSIONS]> {
    let response = sidecar_round_trip(
        json!({
            "v": PROTOCOL_VERSION,
            "id": "knowledge_context_query",
            "op": "embed",
            "texts": [query],
        }),
        timeout_duration,
    )
    .await;

    match response.and_then(parse_embed_response) {
        Ok(vector) => {
            tracing::debug!("knowledge_context_pack sidecar embed completed");
            Some(vector)
        }
        Err(error) => {
            tracing::debug!(
                %error,
                "knowledge_context_pack sidecar embed failed; degrading to BM25-only search"
            );
            None
        }
    }
}

pub(super) async fn ping(timeout_duration: Duration) -> bool {
    let response = sidecar_round_trip(
        json!({
            "v": PROTOCOL_VERSION,
            "id": "knowledge_context_auto_probe",
            "op": "ping",
        }),
        timeout_duration,
    )
    .await;

    match response.and_then(parse_ping_response) {
        Ok(()) => {
            tracing::debug!("knowledge_context_pack sidecar ping completed");
            true
        }
        Err(error) => {
            tracing::debug!(
                %error,
                "knowledge_context_pack sidecar ping failed"
            );
            false
        }
    }
}

async fn sidecar_round_trip(request: Value, timeout_duration: Duration) -> Result<Value, String> {
    tokio::time::timeout(timeout_duration, sidecar_round_trip_inner(request))
        .await
        .map_err(|_elapsed| "sidecar request timed out".to_owned())?
}

async fn sidecar_round_trip_inner(request: Value) -> Result<Value, String> {
    let socket_path = protocol::resolve_socket_path(None)
        .map_err(|error| format!("failed to resolve sidecar socket path: {error}"))?;
    let stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|error| format_socket_error("connect", &socket_path, error))?;
    let (reader, mut writer) = stream.into_split();
    let mut request =
        serde_json::to_vec(&request).map_err(|error| format!("encode request: {error}"))?;
    request.push(b'\n');
    writer
        .write_all(&request)
        .await
        .map_err(|error| format_socket_error("write request to", &socket_path, error))?;
    writer
        .flush()
        .await
        .map_err(|error| format_socket_error("flush request to", &socket_path, error))?;

    let mut response_line = String::new();
    let mut lines = BufReader::new(reader);
    let bytes_read = lines
        .read_line(&mut response_line)
        .await
        .map_err(|error| format_socket_error("read response from", &socket_path, error))?;
    if bytes_read == 0 {
        return Err(format!(
            "sidecar closed without response at {}",
            socket_path.display()
        ));
    }

    serde_json::from_str(response_line.trim_end())
        .map_err(|error| format!("decode sidecar response: {error}"))
}

fn parse_embed_response(response: Value) -> Result<[f32; EMBEDDING_VECTOR_DIMENSIONS], String> {
    let response: EmbedResponse = serde_json::from_value(response)
        .map_err(|error| format!("invalid embed response shape: {error}"))?;
    validate_protocol(response.v)?;
    if let Some(error) = response.error {
        return Err(format!("sidecar error response: {error}"));
    }
    let vectors = response
        .vectors
        .ok_or_else(|| "embed response missing vectors".to_owned())?;
    if vectors.len() != 1 {
        return Err(format!(
            "embed response returned {} vectors for one query",
            vectors.len()
        ));
    }
    let vector = vectors
        .into_iter()
        .next()
        .expect("vector length checked above");
    if vector.len() != EMBEDDING_VECTOR_DIMENSIONS {
        return Err(format!(
            "embed response vector dimension {} != {EMBEDDING_VECTOR_DIMENSIONS}",
            vector.len()
        ));
    }
    vector
        .try_into()
        .map_err(|vector: Vec<f32>| format!("embed response vector dimension {}", vector.len()))
}

fn parse_ping_response(response: Value) -> Result<(), String> {
    let response: PingResponse = serde_json::from_value(response)
        .map_err(|error| format!("invalid ping response shape: {error}"))?;
    validate_protocol(response.v)?;
    if let Some(error) = response.error {
        return Err(format!("sidecar error response: {error}"));
    }
    if response.ok == Some(true) {
        Ok(())
    } else {
        Err("ping response was not ok".to_owned())
    }
}

fn validate_protocol(version: Option<u8>) -> Result<(), String> {
    match version {
        Some(PROTOCOL_VERSION) => Ok(()),
        other => Err(format!("unsupported sidecar protocol version: {other:?}")),
    }
}

fn format_socket_error(action: &str, socket_path: &Path, error: std::io::Error) -> String {
    format!(
        "failed to {action} sidecar socket {}: {error}",
        socket_path.display()
    )
}
