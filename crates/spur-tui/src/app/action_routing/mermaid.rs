#[cfg(feature = "markdown")]
use super::*;

#[cfg(feature = "markdown")]
impl App {
    pub(super) fn process_mermaid(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::MermaidRenderRequest {
                session,
                ref_id,
                code,
                target_width,
            } => {
                let tx = self.mermaid_tx.clone();
                let session_cloned = session.clone();
                tokio::task::spawn_blocking(move || {
                    let result =
                        crate::components::mermaid::render_mermaid_hybrid(&code, target_width)
                            .map(|rendered| match rendered {
                                crate::components::mermaid::MermaidRendered::Image(image) => {
                                    crate::components::mermaid::MermaidRenderOutput::Image(
                                        std::sync::Arc::new(image),
                                    )
                                }
                                crate::components::mermaid::MermaidRendered::Text { text } => {
                                    crate::components::mermaid::MermaidRenderOutput::Text(
                                        std::sync::Arc::<str>::from(text),
                                    )
                                }
                            })
                            .map_err(|e| e.to_string());
                    let _ = tx.send(Action::MermaidRenderCompleted {
                        session: session_cloned,
                        ref_id,
                        target_width,
                        result,
                    });
                });
                None
            }
            Action::MermaidRenderCompleted {
                session,
                ref_id,
                target_width,
                result,
            } => {
                if let Some(ref mut detail) = self.session_detail {
                    if detail.session_id().0 == session.0 {
                        detail.handle_mermaid_completed(ref_id, target_width, result);
                    }
                }
                self.dirty = true;
                None
            }
            _ => None,
        }
    }
}
