use super::*;
use crate::action::PermissionChoice;

/// Wall-clock cap on an unanswered interactive permission prompt.
///
/// `None` waits for the user's reply (fail-closed paths — channel drop,
/// session exit — still deny). A fixed cap silently denies users who step
/// away from the terminal mid-prompt.
const PENDING_PERMISSION_DEADLINE: Option<std::time::Duration> = None;

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

        // A zero countdown renders the untimed hint for an unbounded wait.
        let countdown = PENDING_PERMISSION_DEADLINE
            .map_or(0, |limit| u8::try_from(limit.as_secs()).unwrap_or(u8::MAX));
        if let Some(ref mut detail) = self.session_detail {
            detail.push_permission_with_details(&title, &details, countdown, option_count);
        }

        let deadline = PENDING_PERMISSION_DEADLINE.map(|limit| std::time::Instant::now() + limit);
        self.pending_permission = Some((request, deadline));
        self.dirty = true;
    }

    /// Revoke the reply before its conversation view is replaced or cleared.
    pub(in crate::app) fn cancel_pending_permission(&mut self) {
        // Dropping the sender makes the ACP handler cancel without selecting an option.
        drop(self.pending_permission.take());
        self.clear_pending_permission_trace();
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
    use tokio::sync::oneshot::{self, error::TryRecvError};

    fn app_with_pending_permission() -> (
        App,
        tempfile::TempDir,
        oneshot::Receiver<spur_acp::types::PermissionResponse>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new_with_metadata_path_for_test(temp.path().join("metadata.json"));
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "codex".into(),
            session: SessionId("original-session".into()),
        }));
        let (reply_tx, reply_rx) = oneshot::channel();
        app.handle_permission_request(spur_acp::types::PermissionRequest {
            args: spur_acp::RequestPermissionRequest::new(
                "original-session",
                spur_acp::ToolCallUpdate::new("tool", spur_acp::ToolCallUpdateFields::new()),
                vec![PermissionOption::new(
                    "allow-once",
                    "Allow once",
                    PermissionOptionKind::AllowOnce,
                )],
            ),
            reply_tx,
            generation: 1,
            operation_fence: 1,
        });
        (app, temp, reply_rx)
    }

    fn assert_permission_cancelled(
        app: &App,
        reply_rx: &mut oneshot::Receiver<spur_acp::types::PermissionResponse>,
    ) {
        assert!(
            matches!(reply_rx.try_recv(), Err(TryRecvError::Closed)),
            "destroying the prompt must close its reply channel without selecting an option"
        );
        assert!(app.pending_permission.is_none());
    }

    fn assert_permission_answerable(
        app: &mut App,
        reply_rx: &mut oneshot::Receiver<spur_acp::types::PermissionResponse>,
    ) {
        assert!(matches!(reply_rx.try_recv(), Err(TryRecvError::Empty)));
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_eq!(reply_rx.try_recv().unwrap().option_id, "allow-once");
        assert!(app.pending_permission.is_none());
    }

    #[test]
    fn pending_permission_is_cancelled_when_resuming_another_session() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        app.process_action(Action::ResumeSession {
            session_id: "other-session".into(),
        });
        app.tick();
        app.handle_crossterm_event_for_test(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
        assert_permission_cancelled(&app, &mut reply_rx);
    }

    #[test]
    fn pending_permission_is_cancelled_when_clear_resets_the_view() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        let (input_tx, mut input_rx) = mpsc::channel(1);
        app.user_input_tx = Some(input_tx);
        app.process_action(Action::ClearSession);
        assert!(matches!(
            input_rx.try_recv().unwrap(),
            UserInput::NewSessionWithMessage { .. }
        ));
        assert_permission_cancelled(&app, &mut reply_rx);
    }

    #[test]
    fn pending_permission_is_cancelled_when_brain_spawn_replaces_the_view() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "codex".into(),
            session: SessionId("other-session".into()),
        }));
        assert_permission_cancelled(&app, &mut reply_rx);
    }

    #[test]
    fn pending_permission_is_cancelled_when_retirement_clears_the_view() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainRetired {
            session: SessionId("original-session".into()),
            reason: BrainRetireReason::UserClear,
        }));
        assert_permission_cancelled(&app, &mut reply_rx);
    }

    #[test]
    fn pending_permission_remains_answerable_after_failed_clear() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        let (input_tx, input_rx) = mpsc::channel(1);
        drop(input_rx);
        app.user_input_tx = Some(input_tx);
        app.process_action(Action::ClearSession);
        assert_permission_answerable(&mut app, &mut reply_rx);
    }

    #[test]
    fn pending_permission_remains_answerable_when_same_view_is_retained() {
        let (mut app, _temp, mut reply_rx) = app_with_pending_permission();
        app.handle_spur_event(SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "codex".into(),
            session: SessionId("original-session".into()),
        }));
        app.tick();
        assert_permission_answerable(&mut app, &mut reply_rx);
    }

    #[test]
    fn unbounded_permission_prompt_does_not_advertise_auto_cancel() {
        let (mut app, _temp, _reply_rx) = app_with_pending_permission();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            rendered.contains("PERMISSION"),
            "permission prompt must be rendered: {rendered}"
        );
        assert!(rendered.contains("Select an advertised option"));
        assert!(
            !rendered.contains("auto-cancel"),
            "an unbounded prompt must not advertise a cancellation deadline"
        );
    }

    #[test]
    fn pending_permission_deadline_is_unbounded() {
        // The prompt must wait for the user's reply; a fixed cap silently
        // denies users who step away from the terminal mid-prompt.
        assert!(
            PENDING_PERMISSION_DEADLINE.is_none(),
            "interactive permission prompts must wait for the user's reply"
        );
    }
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
