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
pub struct InteractiveFrontendHandle {
    pub(crate) user_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
    pub(crate) review_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
}

impl InteractiveFrontendHandle {
    pub async fn send_command(&self, input: InteractiveInput) -> anyhow::Result<()> {
        validate_frontend_command(&input)?;
        // PROBE: issue_detail_latency
        let probe_kind = match &input {
            InteractiveInput::GetIssueDetail { id } => Some(("GetIssueDetail", id.clone())),
            _ => None,
        };
        let send_started = std::time::Instant::now();
        self.user_tx.send(input).await?;
        if let Some((kind, id)) = probe_kind {
            tracing::info!(
                target: "issue_probe",
                site = "host_send",
                kind = kind,
                id = %id,
                host_send_ms = send_started.elapsed().as_millis() as u64,
                user_tx_capacity = self.user_tx.capacity(),
                "InteractiveInput delivered to orchestrator mpsc",
            );
        }
        Ok(())
    }

    pub async fn send_review(&self, review: ReviewSubmission) -> anyhow::Result<()> {
        self.review_tx.send(review.into_input()).await?;
        Ok(())
    }
}

pub struct InteractiveFrontendHost {
    pub(crate) handle: InteractiveFrontendHandle,
    pub(crate) event_rx: Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>>,
    pub(crate) permission_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    pub(crate) orch_handle: tokio::task::JoinHandle<()>,
}

impl InteractiveFrontendHost {
    pub fn from_parts_for_test(
        user_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
        review_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
        event_rx: tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
        permission_rx: tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>,
        orch_handle: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            handle: InteractiveFrontendHandle { user_tx, review_tx },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            orch_handle,
        }
    }

    pub fn handle(&self) -> InteractiveFrontendHandle {
        self.handle.clone()
    }

    pub fn take_event_stream(
        &mut self,
    ) -> Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>> {
        self.event_rx.take()
    }

    pub fn take_permission_stream(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>> {
        self.permission_rx.take()
    }

    pub fn spawn(mut orch: spur_core::Orchestrator, brain: Option<String>) -> Self {
        let event_rx = orch.subscribe();
        let review_sink = orch.review_sink.clone();
        let (permission_tx, permission_rx) =
            tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();
        let (user_tx, user_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(32);
        let (review_tx, review_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(32);

        tokio::spawn(spur_core::review_dispatcher_loop(review_rx, review_sink));

        let overflow = spur_core::continuation_bridge::new_overflow_buf();
        orch.set_continuation_tx(user_tx.clone(), overflow.clone());

        let orch_handle = tokio::spawn(async move {
            if let Err(error) = orch
                .run_interactive(user_rx, brain, Some(permission_tx), overflow)
                .await
            {
                tracing::error!(%error, "interactive host run_interactive failed");
            }
        });

        Self {
            handle: InteractiveFrontendHandle { user_tx, review_tx },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            orch_handle,
        }
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.event_rx.take();
        self.permission_rx.take();
        drop(self.handle);
        let mut handle = self.orch_handle;
        match tokio::time::timeout(std::time::Duration::from_secs(5), &mut handle).await {
            Ok(_) => Ok(()),
            Err(_) => {
                handle.abort();
                anyhow::bail!("interactive host shutdown timed out")
            }
        }
    }
}
