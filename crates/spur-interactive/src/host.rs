use anyhow::{bail, Result};
use spur_core::InteractiveInput;
use tokio::sync::mpsc;

/// Read-only PM queries dispatched on a separate channel from `InteractiveInput` so they cannot
/// queue behind brain-stream traffic. See plan PR1 / docs.
#[derive(Debug, Clone)]
pub enum DataQuery {
    GetIssueDetail { id: String },
    GetIssueGraph { id: String },
}

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
    pub(crate) user_tx: mpsc::Sender<InteractiveInput>,
    pub(crate) review_tx: mpsc::Sender<InteractiveInput>,
    pub(crate) data_tx: mpsc::Sender<DataQuery>,
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

    pub async fn send_data_query(&self, query: DataQuery) -> anyhow::Result<()> {
        let probe = match &query {
            DataQuery::GetIssueDetail { id } => ("GetIssueDetail", id.clone()),
            DataQuery::GetIssueGraph { id } => ("GetIssueGraph", id.clone()),
        };
        let send_started = std::time::Instant::now();
        self.data_tx.send(query).await.map_err(|error| {
            anyhow::anyhow!("interactive frontend data query channel closed: {error}")
        })?;
        tracing::info!(
            target: "issue_probe",
            site = "data_send",
            kind = probe.0,
            id = %probe.1,
            data_send_ms = send_started.elapsed().as_millis() as u64,
            data_tx_capacity = self.data_tx.capacity(),
            "DataQuery delivered to interactive data mpsc",
        );
        Ok(())
    }
}

pub struct InteractiveFrontendHost {
    pub(crate) handle: InteractiveFrontendHandle,
    pub(crate) event_rx: Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>>,
    pub(crate) permission_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    pub(crate) data_rx: Option<mpsc::Receiver<DataQuery>>,
    pub(crate) orch_handle: tokio::task::JoinHandle<()>,
    data_loop_handle: Option<DataLoopTask>,
}

struct DataLoopTask(tokio::task::JoinHandle<()>);

impl Drop for DataLoopTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl InteractiveFrontendHost {
    pub fn from_parts_for_test(
        user_tx: mpsc::Sender<InteractiveInput>,
        review_tx: mpsc::Sender<InteractiveInput>,
        event_rx: tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
        permission_rx: tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>,
        orch_handle: tokio::task::JoinHandle<()>,
    ) -> Self {
        let (data_tx, data_rx) = mpsc::channel::<DataQuery>(64);
        Self {
            handle: InteractiveFrontendHandle {
                user_tx,
                review_tx,
                data_tx,
            },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            data_rx: Some(data_rx),
            orch_handle,
            data_loop_handle: None,
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

    pub fn take_data_rx(&mut self) -> Option<mpsc::Receiver<DataQuery>> {
        self.data_rx.take()
    }

    pub fn spawn(mut orch: spur_core::Orchestrator, brain: Option<String>) -> Self {
        let event_rx = orch.subscribe();
        let review_sink = orch.review_sink.clone();
        let (permission_tx, permission_rx) =
            tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();
        let (user_tx, user_rx) = mpsc::channel::<InteractiveInput>(32);
        let (review_tx, review_rx) = mpsc::channel::<InteractiveInput>(32);
        let (data_tx, data_rx) = mpsc::channel::<DataQuery>(64);
        let pm_service = orch.pm_service.clone();
        let funnel = orch.event_funnel_handle();

        tokio::spawn(spur_core::review_dispatcher_loop(review_rx, review_sink));
        let data_loop_handle = tokio::spawn(crate::data_loop::run_data_query_loop_with_provider(
            data_rx, pm_service, funnel,
        ));

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
            handle: InteractiveFrontendHandle {
                user_tx,
                review_tx,
                data_tx,
            },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            data_rx: None,
            orch_handle,
            data_loop_handle: Some(DataLoopTask(data_loop_handle)),
        }
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.event_rx.take();
        self.permission_rx.take();
        self.data_rx.take();
        self.data_loop_handle.take();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn data_channel_send_recv_roundtrip() {
        let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
        let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
        let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
        let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

        let mut host = InteractiveFrontendHost::from_parts_for_test(
            user_tx,
            review_tx,
            event_rx,
            perm_rx,
            tokio::spawn(async {}),
        );
        let handle = host.handle();
        let mut data_rx = host.take_data_rx().unwrap();

        handle
            .send_data_query(DataQuery::GetIssueDetail {
                id: "bd-test".into(),
            })
            .await
            .unwrap();

        let query = data_rx.recv().await.unwrap();
        assert!(matches!(
            query,
            DataQuery::GetIssueDetail { id } if id == "bd-test"
        ));
    }
}
