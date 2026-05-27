use super::*;

pub mod agents_tree;
pub mod mermaid;
pub mod nav;
pub mod overlays;
pub mod permissions;
pub mod picker_metadata;
pub mod pm_actions;
pub mod review;
pub mod scroll;
pub mod session_config;
pub mod session_lifecycle;
pub mod slash_commands;

impl App {
    pub(super) fn build_execute_prompt(&self, id: &str) -> String {
        let issue = self
            .dashboard
            .tracked_issues()
            .iter()
            .find(|issue| issue.id == id);

        let (title, status, pri, issue_type) = issue
            .map(|issue| {
                (
                    issue.title.clone(),
                    issue.status.clone(),
                    issue
                        .priority
                        .map(|p| format!("P{}", p))
                        .unwrap_or_default(),
                    issue.issue_type.clone().unwrap_or_else(|| "task".into()),
                )
            })
            .unwrap_or_else(|| {
                (
                    String::new(),
                    "unknown".into(),
                    String::new(),
                    "unknown".into(),
                )
            });

        format!(
            "The user wants to execute this work item.\n\n\
             Item: {} \u{2014} {}\n\
             Type: {} | Status: {} | Priority: {}\n\n\
             Please analyze this item, gather necessary information, and determine how to execute it. \
             If it requires a multi-step plan, use the appropriate tools to structure it. \
             If it's a single task, figure out the best way to get it done.",
            id, title, issue_type, status, pri,
        )
    }

    /// Process a single Action returned by a view.
    pub(crate) fn process_action(&mut self, action: Action) {
        #[cfg(any(test, debug_assertions))]
        {
            self.last_action = Some(action.clone());
        }
        let next = match action {
            action @ (Action::Quit
            | Action::NavigateTo(_)
            | Action::NavigateBack
            | Action::OpenInsights
            | Action::InspectPlan { .. }
            | Action::OpenPlanInBrowser { .. }) => self.process_nav(action),

            action @ (Action::SendMessage { .. }
            | Action::ClearSession
            | Action::NewSessionWithMessage { .. }
            | Action::RequestSessions
            | Action::ResumeSession { .. }
            | Action::NewSessionRequested
            | Action::RefreshSessions
            | Action::CancelStream { .. }
            | Action::CopySessionId(_)) => self.process_session_lifecycle(action),

            action @ (Action::VendorExec { .. }
            | Action::SetSessionConfigOption { .. }
            | Action::SetSessionModel { .. }
            | Action::TogglePlanMode
            | Action::ToggleVimMode
            | Action::ToggleVerbose) => self.process_session_config(action),

            action @ (Action::ToggleSessionPin { .. }
            | Action::ToggleSessionArchive { .. }
            | Action::ToggleShowArchived
            | Action::RenameSession { .. }
            | Action::SaveDraft { .. }) => self.process_picker_metadata(action),

            action @ (Action::ShowHelp
            | Action::HideHelp
            | Action::PanicReset
            | Action::ShowSessionCost
            | Action::ShowUpgradeModal { .. }
            | Action::FlashHint { .. }
            | Action::PrefillInput { .. }) => self.process_overlay(action),

            action @ (Action::SelectNextBy(_)
            | Action::SelectPrevBy(_)
            | Action::FocusNode
            | Action::UnfocusNode
            | Action::JumpToReview
            | Action::JumpToPreviousReview
            | Action::ToggleCollapse
            | Action::InspectWorkers
            | Action::FocusWorkerInDashboard { .. }) => self.process_agents_focus(action),

            action @ (Action::SubmitReview { .. } | Action::SubmitReviewDispatch { .. }) => {
                self.process_review(action)
            }
            Action::PermissionGrant(choice) => self.process_permission(choice),

            #[cfg(feature = "markdown")]
            action @ Action::MermaidRenderRequest { .. } => self.process_mermaid(action),
            #[cfg(feature = "markdown")]
            action @ Action::MermaidRenderCompleted { .. } => self.process_mermaid(action),

            action @ (Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::CycleFocus
            | Action::Tick) => self.process_scroll(action),

            Action::ThemeCommand { arg } => self.process_theme_cmd(arg),
            Action::NotebookCommand { arg } => self.process_notebook_cmd(arg),

            action @ (Action::RefreshIssues
            | Action::RefreshPlans
            | Action::ClaimPlan { .. }
            | Action::ForceReclaimPlan { .. }
            | Action::ResumePlan { .. }
            | Action::GetIssueGraph { .. }
            | Action::OpenIssueInBacklog { .. }
            | Action::Issue(_)) => self.process_pm_action(action),
        };
        if let Some(action) = next {
            self.process_action(action);
        }
    }
}

pub(super) fn to_wire_decision(d: &spur_core::ReviewDecision) -> spur_acp::ReviewDecision {
    d.clone()
}
