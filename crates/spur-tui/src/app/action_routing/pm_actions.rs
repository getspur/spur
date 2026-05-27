use super::*;

impl App {
    pub(super) fn process_pm_action(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::RefreshIssues => {
                if let Some(ref mut browser) = self.issue_browser {
                    browser.invalidate_graph_cache();
                }
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshIssues);
                }
                None
            }

            Action::RefreshPlans => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::RefreshPlans);
                }
                None
            }

            Action::ClaimPlan { plan_id } => {
                self.flash_hint_short(format!("Claiming plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ClaimPlan { plan_id });
                }
                None
            }

            Action::ForceReclaimPlan { plan_id } => {
                self.flash_hint_short(format!("Force reclaiming plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ForceReclaimPlan { plan_id });
                }
                None
            }

            Action::ResumePlan { plan_id } => {
                // Immediate user feedback: the orchestrator -> MCP round-trip can take
                // 1-3s; without this hint the TUI looks frozen and invites double-press.
                self.flash_hint_short(format!("Starting plan {plan_id}..."));
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::ResumePlan { plan_id });
                }
                None
            }

            Action::OpenIssueInBacklog { id } => {
                let just_created = self.issue_browser.is_none();
                if just_created {
                    let mut view = IssueBrowserView::new();
                    view.seed_issues(self.dashboard.tracked_issues().to_vec());
                    self.issue_browser = Some(view);
                }
                // Inc 3 (bd-d587.3): only caller today is PlanBrowser View-Epic,
                // so pass `FocusGraph` - selects the row in the left pane and
                // arms the detail-fetch handler to flip to Graph mode after
                // both `IssueDetailFetched` and `IssueSubgraphLoaded` arrive.
                let (pending, needs_refresh) = self
                    .issue_browser
                    .as_mut()
                    .map(|browser| {
                        browser.open_external_detail(
                            id.clone(),
                            crate::views::issue_browser::OpenMode::FocusGraph,
                        );
                        (browser.take_pending_action(), browser.has_pending_select())
                    })
                    .unwrap_or((None, false));
                self.navigate_to(ViewId::IssueBrowser);
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::GetIssueDetail { id });
                    // Refresh on first creation OR when the requested id
                    // wasn't found in the cached list - otherwise pending_select
                    // would sit forever and the left-pane row would never align
                    // with the right-pane detail.
                    if just_created || needs_refresh {
                        let _ = tx.try_send(UserInput::RefreshIssues);
                    }
                }
                pending
            }

            Action::GetIssueGraph { id } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::GetIssueGraph { id });
                }
                None
            }

            Action::Issue(issue_action) => self.process_issue_action(issue_action),

            _ => None,
        }
    }

    fn process_issue_action(&mut self, issue_action: crate::action::IssueAction) -> Option<Action> {
        match issue_action {
            crate::action::IssueAction::ViewDetail { id } => {
                if let Some(ref tx) = self.user_input_tx {
                    // PROBE: issue_detail_latency
                    tracing::info!(
                        target: "issue_probe",
                        site = "ui_send",
                        id = %id,
                        queue_len = tx.capacity().saturating_sub(tx.max_capacity()),
                        ts_ns = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0),
                        "GetIssueDetail dispatched from TUI",
                    );
                    let _ = tx.try_send(UserInput::GetIssueDetail { id });
                }
            }
            crate::action::IssueAction::UpdateStatus {
                id,
                status,
                via_legacy_key,
            } => self.process_issue_status_update(id, status, via_legacy_key),
            crate::action::IssueAction::WorkOn { id } => self.process_issue_work_on(id),
            crate::action::IssueAction::Execute { id } => self.process_issue_execute(id),
            crate::action::IssueAction::ExecuteEdit { id } => self.process_issue_execute_edit(id),
            crate::action::IssueAction::AddComment { issue_id, body } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::AddIssueComment { issue_id, body });
                }
            }
        }
        None
    }

    fn process_issue_status_update(&mut self, id: String, status: String, via_legacy_key: bool) {
        let show_legacy_close_hint =
            via_legacy_key && status == "closed" && !self.legacy_issue_close_hint_shown;
        if show_legacy_close_hint {
            self.legacy_issue_close_hint_shown = true;
        }
        if !self.tombstone_undo_replay {
            let previous_status = self.issue_browser.as_ref().and_then(|view| {
                view.tracked_issues()
                    .iter()
                    .find(|issue| issue.id.as_str() == id.as_str())
                    .map(|issue| issue.status.clone())
            });

            if let Some(previous_status) = previous_status {
                let label = format!("Issue '{}' → {}", id, status);
                let now = Instant::now();
                let inverse = Action::Issue(crate::action::IssueAction::UpdateStatus {
                    id: id.clone(),
                    status: previous_status,
                    via_legacy_key: false,
                });
                self.tombstones.install(Tombstone {
                    view: ViewId::IssueBrowser,
                    kind: TombstoneKind::Reversible { inverse },
                    label: label.clone(),
                    created_at: now,
                    expires_at: now + Duration::from_secs(60),
                });
                if !show_legacy_close_hint {
                    self.flash_hint(
                        format!("{} — press u to undo", label),
                        Duration::from_secs(2),
                    );
                }
            } else {
                tracing::warn!(
                    issue_id = %id,
                    "issue not in tracked_issues; skipping tombstone install (undo unavailable for this update)"
                );
            }
        }
        if let Some(ref tx) = self.user_input_tx {
            let _ = tx.try_send(UserInput::UpdateIssue {
                id,
                update: spur_pm::IssueUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            });
        }
        if show_legacy_close_hint {
            self.flash_hint_short(LEGACY_CLOSE_HINT);
        }
    }

    fn process_issue_work_on(&mut self, id: String) {
        // Construct issue prompt from cached summary
        let prompt =
            if let Some(issue) = self.dashboard.tracked_issues().iter().find(|i| i.id == id) {
                let pri = issue
                    .priority
                    .map(|p| format!("P{}", p))
                    .unwrap_or_default();
                let itype = issue.issue_type.as_deref().unwrap_or("task");
                format!(
                    "Work on this issue:\n\n\
                 Issue: {} — {}\n\
                 Priority: {} | Type: {} | Status: {}\n\n\
                 Use `get_issue` tool to read full details if needed.\n\
                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.\n\
                 Update issue status as you progress.",
                    id, issue.title, pri, itype, issue.status, id,
                )
            } else {
                format!(
                    "Work on issue {}.\n\n\
                 Use `get_issue` tool to read full details.\n\
                 Use `delegate_to_worker` with issue_id=\"{}\" for delegations.",
                    id, id,
                )
            };

        let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(
            prompt,
        ))];

        if self.session_detail.is_some() {
            self.process_action(Action::SendMessage {
                session: spur_acp::SessionId(String::new()),
                blocks,
                interrupt: false,
            });
        } else {
            self.process_action(Action::NewSessionWithMessage {
                blocks,
                interrupt: false,
            });
        }
    }

    fn process_issue_execute(&mut self, id: String) {
        let prompt = self.build_execute_prompt(&id);

        let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(
            prompt,
        ))];

        if self.session_detail.is_some() {
            self.process_action(Action::SendMessage {
                session: spur_acp::SessionId(String::new()),
                blocks,
                interrupt: false,
            });
        } else {
            self.process_action(Action::NewSessionWithMessage {
                blocks,
                interrupt: false,
            });
        }
    }

    fn process_issue_execute_edit(&mut self, id: String) {
        let prompt = self.build_execute_prompt(&id);

        if let Some(session_id) = self
            .session_detail
            .as_ref()
            .map(|detail| detail.session_id().clone())
        {
            self.process_action(Action::NavigateTo(ViewId::SessionDetail(session_id)));
        } else {
            self.process_action(Action::NavigateTo(ViewId::Dashboard));
        }

        self.process_action(Action::PrefillInput { text: prompt });
        self.process_action(Action::FlashHint {
            message: EXECUTE_EDIT_HINT.to_string(),
        });
    }
}
