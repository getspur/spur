use std::{future::Future, marker::PhantomData, path::Path, sync::Arc};

use rmcp::{
    service::{RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage},
    transport::Transport,
};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
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
            _role: PhantomData,
        }
    }

    pub async fn connect(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Ok(Self::new(UnixStream::connect(path).await?))
    }
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
        let mut len = [0_u8; 4];
        match self.reader.read_exact(&mut len).await {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                ) =>
            {
                return None;
            }
            Err(error) => {
                tracing::debug!(%error, "failed to read MCP frame length");
                return None;
            }
        }

        let len = u32::from_be_bytes(len) as usize;
        if len > MAX_FRAME_BYTES {
            tracing::debug!(len, "MCP frame exceeds maximum");
            return None;
        }

        let mut bytes = vec![0_u8; len];
        if let Err(error) = self.reader.read_exact(&mut bytes).await {
            tracing::debug!(%error, "failed to read MCP frame body");
            return None;
        }

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
