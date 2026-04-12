pub mod config;
pub mod connection;
pub mod domain;
pub mod registry;
pub mod transport;
pub mod types;

pub use config::AgentConfig;
pub use registry::AgentRegistry;
pub use transport::{AgentTransport, AcpTransport, CliWrapTransport, StdioTransport};

// Re-export domain types
pub use domain::{DelegationResult, DelegationStatus, SpurEvent};

// Re-export all remaining types for backward compatibility
pub use types::*;
