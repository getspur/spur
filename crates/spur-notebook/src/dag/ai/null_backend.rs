//! Graceful-degradation AI backend used when a notebook has no configured
//! agent. Every run surfaces `AiError::Init` so a `spur` cell fails with a
//! clear message instead of silently producing nothing; non-AI cells are
//! unaffected because `NotebookCellRunner` never routes them here.

use crate::dag::ai::{AiError, AiNodeBackend, AiRunOutput, AiRunRequest};

/// AI backend that always fails with `AiError::Init`. Installed by
/// `notebook_run_context` when no `AgentConfig` can be resolved.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullAiBackend;

#[async_trait::async_trait]
impl AiNodeBackend for NullAiBackend {
    async fn run(&self, _req: AiRunRequest) -> Result<AiRunOutput, AiError> {
        Err(AiError::Init(
            "no agent configured for this notebook".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn run_always_returns_init_error() {
        let backend = NullAiBackend;
        let error = backend
            .run(AiRunRequest {
                cell_id: "c1".into(),
                prompt: "hi".into(),
                context: vec![],
                cancel: CancellationToken::new(),
            })
            .await
            .expect_err("null backend must fail");
        assert!(matches!(error, AiError::Init(_)));
        assert!(error.to_string().contains("no agent configured"));
    }
}
