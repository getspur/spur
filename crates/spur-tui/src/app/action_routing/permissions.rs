use super::*;
use crate::action::PermissionChoice;

impl App {
    pub(super) fn process_permission(&mut self, choice: PermissionChoice) -> Option<Action> {
        if let Some((perm, _)) = self.pending_permission.take() {
            match choice {
                PermissionChoice::Allow => {
                    let id = perm
                        .args
                        .options
                        .first()
                        .map(|o| o.option_id.to_string())
                        .unwrap_or_else(|| "allow".to_string());
                    let _ = perm
                        .reply_tx
                        .send(spur_acp::types::PermissionResponse { option_id: id });
                }
                PermissionChoice::AlwaysAllow => {
                    let id = perm
                        .args
                        .options
                        .iter()
                        .find(|o| o.name.to_lowercase().contains("always"))
                        .or(perm.args.options.first())
                        .map(|o| o.option_id.to_string())
                        .unwrap_or_else(|| "allow".to_string());
                    let _ = perm
                        .reply_tx
                        .send(spur_acp::types::PermissionResponse { option_id: id });
                }
                PermissionChoice::Deny => {
                    // Drop reply_tx (signals denial to ACP thread).
                    drop(perm);
                }
            }
        }
        self.clear_pending_permission_trace();
        None
    }

    pub(in crate::app) fn handle_permission_request(
        &mut self,
        request: spur_acp::types::PermissionRequest,
    ) {
        self.pending_permission.take();

        let description = request
            .args
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| "Tool call".to_string());

        if let Some(ref mut detail) = self.session_detail {
            detail.push_permission(&description, 30);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        self.pending_permission = Some((request, deadline));
        self.dirty = true;
    }

    /// Mark all pending permission trace entries as resolved.
    pub(in crate::app) fn clear_pending_permission_trace(&mut self) {
        if let Some(ref mut detail) = self.session_detail {
            detail.resolve_pending_permissions();
        }
    }
}
