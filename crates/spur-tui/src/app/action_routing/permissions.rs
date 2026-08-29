use super::*;
use crate::action::PermissionChoice;

impl App {
    pub(super) fn process_permission(&mut self, choice: PermissionChoice) -> Option<Action> {
        if let Some((perm, deadline)) = self.pending_permission.take() {
            match choice {
                PermissionChoice::SelectIndex(index) => {
                    let Some(id) = option_id_at(&perm.args.options, index) else {
                        self.pending_permission = Some((perm, deadline));
                        return None;
                    };
                    let _ = perm
                        .reply_tx
                        .send(spur_acp::types::PermissionResponse { option_id: id });
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

        let (title, description) = permission_presentation(&request.args);
        let option_count = request.args.options.len();
        let option_lines = permission_option_lines(&request.args.options);
        let details = description
            .into_iter()
            .chain(
                (option_count > 9)
                    .then_some("Type an option number, then press Enter.".to_string()),
            )
            .chain(option_lines)
            .collect::<Vec<_>>()
            .join("\n");

        if let Some(ref mut detail) = self.session_detail {
            detail.push_permission_with_details(&title, &details, 30, option_count);
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

fn option_id_at(options: &[spur_acp::PermissionOption], index: usize) -> Option<String> {
    options
        .get(index)
        .map(|option| option.option_id.to_string())
}

fn permission_option_lines(options: &[spur_acp::PermissionOption]) -> Vec<String> {
    options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let option_description = option
                .meta
                .as_ref()
                .and_then(|meta| meta.get("permission"))
                .and_then(serde_json::Value::as_object)
                .and_then(|permission| permission.get("description"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty());
            let label = index + 1;
            match option_description {
                Some(details) => format!("[{label}] {} — {details}", option.name),
                None => format!("[{label}] {}", option.name),
            }
        })
        .collect()
}

fn permission_presentation(args: &spur_acp::RequestPermissionRequest) -> (String, Option<String>) {
    let presentation = args
        .meta
        .as_ref()
        .and_then(|meta| meta.get("permission"))
        .and_then(serde_json::Value::as_object);
    let title = presentation
        .and_then(|permission| permission.get("title"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| args.tool_call.fields.title.clone())
        .unwrap_or_else(|| "Tool call".to_string());
    let description = presentation
        .and_then(|permission| permission.get("description"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    (title, description)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{PermissionOption, PermissionOptionId, PermissionOptionKind};

    #[test]
    fn every_permission_option_is_presented_with_its_numeric_identity() {
        let options = (1..=12)
            .map(|index| {
                PermissionOption::new(
                    PermissionOptionId::new(format!("opaque-{index}")),
                    format!("Option {index}"),
                    PermissionOptionKind::AllowOnce,
                )
            })
            .collect::<Vec<_>>();

        let lines = permission_option_lines(&options);
        assert_eq!(lines.len(), options.len());
        assert_eq!(lines[9], "[10] Option 10");

        assert_eq!(
            option_id_at(&options, 9).as_deref(),
            Some("opaque-10"),
            "selection must follow the displayed index's opaque identity"
        );
    }
}
