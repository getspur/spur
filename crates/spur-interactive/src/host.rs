use anyhow::{bail, Result};
use spur_core::InteractiveInput;

#[derive(Debug, Clone)]
pub struct ReviewSubmission {
    pub executor_id: String,
    pub attempt_n: u32,
    pub decision: spur_acp::ReviewDecision,
}

impl ReviewSubmission {
    pub fn into_input(self) -> InteractiveInput {
        InteractiveInput::SubmitReview {
            executor_id: self.executor_id,
            attempt_n: self.attempt_n,
            decision: self.decision,
        }
    }
}

pub fn validate_frontend_command(input: &InteractiveInput) -> Result<()> {
    if matches!(input, InteractiveInput::SubmitReview { .. }) {
        bail!("SubmitReview must be routed through send_review");
    }
    Ok(())
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct InteractiveFrontendHandle {
    pub(crate) user_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
    pub(crate) review_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
}

#[allow(dead_code)]
pub struct InteractiveFrontendHost {
    pub(crate) handle: InteractiveFrontendHandle,
    pub(crate) event_rx: Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>>,
    pub(crate) permission_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    pub(crate) orch_handle: tokio::task::JoinHandle<()>,
}
