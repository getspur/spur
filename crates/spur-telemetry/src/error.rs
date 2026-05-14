#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("telemetry is not initialized")]
    NotInitialized,
}

pub type Result<T> = std::result::Result<T, TelemetryError>;
