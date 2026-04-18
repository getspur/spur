use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::broadcast;

use crate::{LicenseEvent, LicenseState, Result};

#[derive(Debug, Clone, Copy)]
pub struct RefreshPolicy {
    pub validate_interval: Duration,
    pub heartbeat_interval: Duration,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            validate_interval: Duration::from_secs(3600),
            heartbeat_interval: Duration::from_secs(300),
        }
    }
}

#[async_trait]
pub trait LicenseProvider: Send + Sync {
    fn current_state(&self) -> LicenseState;
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent>;
    fn refresh_policy(&self) -> RefreshPolicy;
    fn has_entitlement(&self, feature: &str) -> bool;

    async fn activate(&self, key: &str) -> Result<LicenseState>;
    async fn validate(&self) -> Result<LicenseState>;
    async fn heartbeat(&self) -> Result<LicenseState>;
    async fn deactivate(&self) -> Result<LicenseState>;
}
