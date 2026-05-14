pub mod error;
pub mod events;

pub use error::{Result, TelemetryError};

#[cfg(telemetry_disabled)]
pub const TELEMETRY_COMPILED: bool = false;

#[cfg(not(telemetry_disabled))]
pub const TELEMETRY_COMPILED: bool = true;
