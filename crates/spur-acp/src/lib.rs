pub mod config;
pub mod connection;
pub mod domain;
pub mod registry;
pub mod types;

pub use config::AgentConfig;
pub use connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter};
pub use registry::AgentRegistry;

// Re-export domain types
pub use domain::{DelegationResult, DelegationStatus, SpurEvent};

// Re-export all remaining types for backward compatibility
pub use types::*;
