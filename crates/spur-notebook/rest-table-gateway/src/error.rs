use thiserror::Error;
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("http error: {0}")]
    Http(String),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("manifest error: {0}")]
    Manifest(String),
    #[error("adapter error: {0}")]
    Adapter(String),
    #[error("auth error: {0}")]
    Auth(String),
    #[error("unknown table: {0}")]
    UnknownTable(String),
}
pub type Result<T> = std::result::Result<T, GatewayError>;
