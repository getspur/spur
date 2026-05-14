#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry is not initialized")]
    NotInitialized,
    #[error("telemetry request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("telemetry request failed with status {0}")]
    HttpStatus(reqwest::StatusCode),
}

pub type Result<T> = std::result::Result<T, TelemetryError>;
