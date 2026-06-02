//! AI-node backend seam for the reactive DAG engine (Tier 1).

use tokio_util::sync::CancellationToken;

/// One consumed upstream port, already rendered to text for prompt context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortContext {
    pub port: String,
    pub rendered: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug)]
pub struct AiRunRequest {
    pub cell_id: String,
    /// The cell body (the prompt).
    pub prompt: String,
    /// Rendered consumed ports, injected as context.
    pub context: Vec<PortContext>,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiRunOutput {
    pub text: String,
    pub usage: Option<AiUsage>,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("agent connection init failed: {0}")]
    Init(String),
    #[error("prompt turn failed: {0}")]
    Prompt(String),
    #[error("ai node run timed out")]
    Timeout,
    #[error("ai node run cancelled")]
    Cancelled,
    #[error("ai node has no produced text port declared")]
    NoOutputPort,
}

/// The only AI abstraction the engine knows about. Tier-2 (session/Orchestrator)
/// becomes a second impl with no engine change.
#[async_trait::async_trait]
pub trait AiNodeBackend: Send + Sync {
    async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone)]
    struct EchoBackend;

    #[async_trait::async_trait]
    impl AiNodeBackend for EchoBackend {
        async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError> {
            Ok(AiRunOutput {
                text: req.prompt,
                usage: None,
            })
        }
    }

    #[tokio::test]
    async fn backend_trait_object_runs() {
        let backend: std::sync::Arc<dyn AiNodeBackend> = std::sync::Arc::new(EchoBackend);
        let out = backend
            .run(AiRunRequest {
                cell_id: "c1".into(),
                prompt: "hello".into(),
                context: vec![PortContext {
                    port: "df".into(),
                    rendered: "a,b".into(),
                }],
                cancel: CancellationToken::new(),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "hello");
    }
}
