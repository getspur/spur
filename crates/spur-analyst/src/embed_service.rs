use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use spur_graph::{
    embedding_query_text_for_model, EmbeddingModelSelection, EMBEDDING_VECTOR_DIMENSIONS,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

pub const SPUR_EMBED_SOCKET_ENV: &str = "SPUR_EMBED_SOCKET";
pub const MAX_EMBED_TEXTS: usize = 16;

const PROTOCOL_VERSION: u8 = 1;
const DEFAULT_SOCKET_RELATIVE_PATH: &str = ".spur/embed.sock";
const SOCKET_PERMISSIONS: u32 = 0o600;

type Embedder = dyn Fn(Vec<String>) -> Result<Vec<Vec<f32>>, String> + Send + Sync;
type Ready = dyn Fn() -> bool + Send + Sync;

#[derive(Clone)]
pub struct EmbedService {
    model_name: Arc<str>,
    ready: Arc<Ready>,
    embedder: Arc<Embedder>,
}

#[derive(Debug, Deserialize)]
struct EmbedRequest {
    #[serde(default)]
    v: Option<u8>,
    #[serde(default)]
    id: Option<Value>,
    op: String,
    #[serde(default)]
    texts: Option<Vec<String>>,
}

impl EmbedService {
    pub fn new<M, R, E>(model_name: M, ready: R, embedder: E) -> Self
    where
        M: Into<String>,
        R: Fn() -> bool + Send + Sync + 'static,
        E: Fn(Vec<String>) -> Result<Vec<Vec<f32>>, String> + Send + Sync + 'static,
    {
        Self {
            model_name: Arc::from(model_name.into()),
            ready: Arc::new(ready),
            embedder: Arc::new(embedder),
        }
    }

    pub async fn serve_socket(self, socket_path: PathBuf) -> Result<()> {
        bind_socket_path(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("failed to bind embed socket at {}", socket_path.display()))?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(SOCKET_PERMISSIONS))
            .with_context(|| {
                format!(
                    "failed to set permissions on embed socket {}",
                    socket_path.display()
                )
            })?;

        loop {
            let (stream, _) = listener.accept().await.with_context(|| {
                format!(
                    "failed to accept embed socket connection at {}",
                    socket_path.display()
                )
            })?;
            let service = self.clone();
            tokio::spawn(async move {
                if let Err(error) = service.handle_connection(stream).await {
                    tracing::debug!(%error, "embed sidecar connection ended with error");
                }
            });
        }
    }

    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut lines = BufReader::new(reader).lines();

        while let Some(line) = lines
            .next_line()
            .await
            .context("failed to read embed request line")?
        {
            if line.trim().is_empty() {
                continue;
            }

            let response = self.handle_line(&line).await;
            let response = serde_json::to_vec(&response).context("failed to encode response")?;
            writer
                .write_all(&response)
                .await
                .context("failed to write embed response")?;
            writer
                .write_all(b"\n")
                .await
                .context("failed to write embed response newline")?;
        }

        Ok(())
    }

    async fn handle_line(&self, line: &str) -> Value {
        let request: EmbedRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return error_response(None, format!("malformed request: {error}"));
            }
        };

        if request.v != Some(PROTOCOL_VERSION) {
            return error_response(
                request.id,
                format!("unsupported protocol version: {:?}", request.v),
            );
        }

        match request.op.as_str() {
            "ping" => self.handle_ping(request.id),
            "embed" => self.handle_embed(request.id, request.texts).await,
            other => error_response(request.id, format!("unknown op: {other}")),
        }
    }

    fn handle_ping(&self, id: Option<Value>) -> Value {
        json!({
            "v": PROTOCOL_VERSION,
            "id": id,
            "ok": true,
            "model": self.model_name.as_ref(),
            "ready": (self.ready)(),
        })
    }

    async fn handle_embed(&self, id: Option<Value>, texts: Option<Vec<String>>) -> Value {
        let Some(texts) = texts else {
            return error_response(id, "embed request missing texts");
        };
        if texts.len() > MAX_EMBED_TEXTS {
            return error_response(
                id,
                format!("embed request accepts at most {MAX_EMBED_TEXTS} texts"),
            );
        }

        let embedding_model = EmbeddingModelSelection::from_env();
        let normalized = texts
            .iter()
            .map(|text| embedding_query_text_for_model(text, embedding_model).into_owned())
            .collect::<Vec<_>>();
        let embedder = Arc::clone(&self.embedder);
        let result = tokio::task::spawn_blocking(move || embedder(normalized)).await;
        let vectors = match result {
            Ok(Ok(vectors)) => vectors,
            Ok(Err(error)) => return error_response(id, error),
            Err(error) => return error_response(id, format!("embed task failed: {error}")),
        };

        if vectors.len() != texts.len() {
            return error_response(
                id,
                format!(
                    "embedder returned {} vectors for {} texts",
                    vectors.len(),
                    texts.len()
                ),
            );
        }
        if vectors
            .iter()
            .any(|vector| vector.len() != EMBEDDING_VECTOR_DIMENSIONS)
        {
            return error_response(
                id,
                format!("embedder returned vector with dimension other than {EMBEDDING_VECTOR_DIMENSIONS}"),
            );
        }

        json!({
            "v": PROTOCOL_VERSION,
            "id": id,
            "vectors": vectors,
        })
    }
}

pub fn resolve_socket_path(socket: Option<PathBuf>) -> Result<PathBuf> {
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

#[cfg(feature = "embed")]
pub async fn serve(socket: Option<PathBuf>) -> Result<()> {
    let socket_path = resolve_socket_path(socket)?;
    let embedding_model = EmbeddingModelSelection::from_env();
    production_service(embedding_model)
        .serve_socket(socket_path)
        .await
}

#[cfg(not(feature = "embed"))]
pub async fn serve(_socket: Option<PathBuf>) -> Result<()> {
    bail!("spur embed serve requires a binary built with the `embed` feature")
}

#[cfg(feature = "embed")]
fn production_service(embedding_model: EmbeddingModelSelection) -> EmbedService {
    EmbedService::new(
        embedding_model.model_name(),
        move || {
            crate::mcp::embed_model_cell(embedding_model)
                .ready()
                .is_some()
        },
        move |texts| embed_with_production_model(embedding_model, texts),
    )
}

#[cfg(feature = "embed")]
fn embed_with_production_model(
    embedding_model: EmbeddingModelSelection,
    texts: Vec<String>,
) -> Result<Vec<Vec<f32>>, String> {
    let model = ready_or_load_model(embedding_model)?;
    let refs = texts.iter().map(String::as_str).collect::<Vec<_>>();
    let mut model = model
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    model.embed(refs, None).map_err(|error| error.to_string())
}

#[cfg(feature = "embed")]
fn ready_or_load_model(
    embedding_model: EmbeddingModelSelection,
) -> Result<Arc<std::sync::Mutex<fastembed::TextEmbedding>>, String> {
    let cell = crate::mcp::embed_model_cell(embedding_model);
    loop {
        if let Some(model) = cell.ready() {
            return Ok(model);
        }

        if let Some(permit) = cell.begin_load() {
            let loaded = crate::mcp::load_embed_model(embedding_model);
            return match loaded {
                Ok(model) => permit
                    .complete(Some(model))
                    .ok_or_else(|| "embedding model loaded but was not available".to_owned()),
                Err(error) => {
                    let _ = permit.complete(None);
                    Err(error)
                }
            };
        }

        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn bind_socket_path(path: &Path) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create embed socket parent directory {}",
                parent.display()
            )
        })?;
    }

    if path.exists() {
        match StdUnixStream::connect(path) {
            Ok(_) => bail!("embed socket already in use at {}", path.display()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                fs::remove_file(path).with_context(|| {
                    format!("failed to remove stale embed socket {}", path.display())
                })?;
            }
            Err(error) => {
                fs::remove_file(path).with_context(|| {
                    format!(
                        "failed to remove stale embed socket {} after connect failed: {error}",
                        path.display()
                    )
                })?;
            }
        }
    }

    Ok(())
}

fn error_response(id: Option<Value>, error: impl Into<String>) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "error": error.into(),
    })
}
