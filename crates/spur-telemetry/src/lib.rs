pub(crate) mod batch;
pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod consent;
pub(crate) mod crash;
pub mod error;
pub mod events;
pub(crate) mod ratelimit;
pub(crate) mod redact;

pub use error::{Result, TelemetryError};

#[cfg(telemetry_disabled)]
pub const TELEMETRY_COMPILED: bool = false;

#[cfg(not(telemetry_disabled))]
pub const TELEMETRY_COMPILED: bool = true;
