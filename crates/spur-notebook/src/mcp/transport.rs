use std::{future::Future, marker::PhantomData, path::Path, sync::Arc};

use rmcp::{
    service::{RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage},
    transport::Transport,
};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
    sync::Mutex,
};

const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame length {0} exceeds maximum")]
    FrameTooLarge(usize),
}

pub struct LengthPrefixedJsonTransport<Role = rmcp::RoleClient>
where
    Role: ServiceRole,
{
    reader: tokio::net::unix::OwnedReadHalf,
    writer: Arc<Mutex<tokio::net::unix::OwnedWriteHalf>>,
    pending: Option<RxJsonRpcMessage<Role>>,
    _role: PhantomData<fn() -> Role>,
}

impl<Role> LengthPrefixedJsonTransport<Role>
where
    Role: ServiceRole,
{
    pub fn new(stream: UnixStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader,
            writer: Arc::new(Mutex::new(writer)),
            pending: None,
            _role: PhantomData,
        }
    }

    pub fn with_initial_message(stream: UnixStream, message: RxJsonRpcMessage<Role>) -> Self {
        let mut transport = Self::new(stream);
        transport.pending = Some(message);
        transport
    }

    pub async fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self::new(UnixStream::connect(path).await?))
    }
}

pub async fn read_frame_bytes<R>(reader: &mut R) -> Result<Vec<u8>, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut len = [0_u8; 4];
    reader.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut bytes = vec![0_u8; len];
    reader.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn read_frame_value<R>(reader: &mut R) -> Result<serde_json::Value, TransportError>
where
    R: AsyncRead + Unpin,
{
    Ok(serde_json::from_slice(&read_frame_bytes(reader).await?)?)
}

pub async fn write_frame_bytes<W>(writer: &mut W, bytes: &[u8]) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
{
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(TransportError::FrameTooLarge(bytes.len()));
    }
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn write_frame_json<W, T>(writer: &mut W, value: &T) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    write_frame_bytes(writer, &serde_json::to_vec(value)?).await
}

impl<Role> Transport<Role> for LengthPrefixedJsonTransport<Role>
where
    Role: ServiceRole,
    TxJsonRpcMessage<Role>: Serialize + Send + 'static,
    RxJsonRpcMessage<Role>: DeserializeOwned + Send,
{
    type Error = TransportError;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<Role>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        async move {
            let bytes = serde_json::to_vec(&item)?;
            if bytes.len() > MAX_FRAME_BYTES {
                return Err(TransportError::FrameTooLarge(bytes.len()));
            }

            let len = (bytes.len() as u32).to_be_bytes();
            let mut writer = writer.lock().await;
            writer.write_all(&len).await?;
            writer.write_all(&bytes).await?;
            writer.flush().await?;
            Ok(())
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<Role>> {
        if let Some(message) = self.pending.take() {
            return Some(message);
        }

        let bytes = match read_frame_bytes(&mut self.reader).await {
            Ok(bytes) => bytes,
            Err(error)
                if matches!(
                    &error,
                    TransportError::Io(io_error) if matches!(
                        io_error.kind(),
                        std::io::ErrorKind::UnexpectedEof
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::BrokenPipe
                    )
                ) =>
            {
                return None;
            }
            Err(error) => {
                tracing::debug!(%error, "failed to read MCP frame");
                return None;
            }
        };

        match serde_json::from_slice(&bytes) {
            Ok(message) => Some(message),
            Err(error) => {
                tracing::debug!(%error, "failed to parse MCP frame body");
                None
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.writer.lock().await.shutdown().await?;
        Ok(())
    }
}
