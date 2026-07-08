use super::*;

impl App {
    fn plan_browser_session(&self) -> SessionId {
        self.session_detail
            .as_ref()
            .map(|detail| detail.session_id().clone())
            .unwrap_or_else(|| SessionId(String::new()))
    }

    pub(super) fn process_nav(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::Quit => {
                self.request_quit();
                None
            }
            Action::NavigateTo(ViewId::SessionDetail(session_id)) => {
                if self.session_detail.is_some() {
                    // Just switch view; BrainSpawned is the only creator.
                    self.navigate_to(ViewId::SessionDetail(session_id));
                }
                None
            }
            Action::NavigateTo(ViewId::Dashboard) => {
                self.navigate_to(ViewId::Dashboard);
                None
            }
            Action::NavigateTo(ViewId::SessionPicker) => {
                self.navigate_to(ViewId::SessionPicker);
                None
            }
            Action::NavigateTo(ViewId::AgentConfigBrowser { preselect }) => {
                let entries = self.config.agents.entries.clone();
                if let Some(view) = self.agent_config_browser.as_mut() {
                    view.set_entries(entries, preselect.clone());
                } else {
                    self.agent_config_browser =
                        Some(AgentConfigBrowserView::new(entries, preselect.clone()));
                }
                self.navigate_to(ViewId::AgentConfigBrowser { preselect });
                None
            }
            Action::NavigateTo(ViewId::PlanInspector(session)) => {
                self.plan_inspector = Some(PlanInspectorView::new(session.clone()));
                self.navigate_to(ViewId::PlanInspector(session));
                None
            }
            Action::InspectPlan {
                session_id,
                plan_id,
            } => {
                if self.plan_projection.plan(&plan_id).is_none() {
                    if let Some(ref tx) = self.user_input_tx {
                        let _ = tx.try_send(UserInput::InspectPlan {
                            plan_id: plan_id.clone(),
                        });
                    }
                }
                self.plan_inspector =
                    Some(PlanInspectorView::new_for_plan(session_id.clone(), plan_id));
                self.navigate_to(ViewId::PlanInspector(session_id));
                None
            }
            Action::OpenPlanInBrowser { plan_id } => {
                let current_session = self.plan_browser_session();
                let just_created = self.plan_browser.is_none();
                let mut session_changed = false;
                if self.plan_browser.is_none() {
                    self.plan_browser = Some(PlanBrowserView::new(current_session.clone()));
                }
                if let Some(browser) = self.plan_browser.as_mut() {
                    session_changed = browser.set_current_session(current_session);
                    browser.focus_plan_id(plan_id);
                }
                self.navigate_to(ViewId::PlanBrowser);
                (just_created || session_changed).then_some(Action::RefreshPlans)
            }
            Action::NavigateTo(ViewId::PlanBrowser) => {
                let current_session = self.plan_browser_session();
                let just_created = self.plan_browser.is_none();
                let mut session_changed = false;
                if self.plan_browser.is_none() {
                    self.plan_browser = Some(PlanBrowserView::new(current_session.clone()));
                }
                if let Some(browser) = self.plan_browser.as_mut() {
                    session_changed = browser.set_current_session(current_session);
                }
                self.navigate_to(ViewId::PlanBrowser);
                (just_created || session_changed).then_some(Action::RefreshPlans)
            }
            Action::NavigateTo(ViewId::LoopBrowser) => {
                let just_created = self.loop_browser.is_none();
                if just_created {
                    self.loop_browser = Some(LoopBrowserView::new());
                }
                self.navigate_to(ViewId::LoopBrowser);
                just_created.then_some(Action::RefreshLoops)
            }
            Action::NavigateTo(ViewId::IssueBrowser) => {
                let just_created = self.issue_browser.is_none();
                if just_created {
                    let mut view = IssueBrowserView::new();
                    view.seed_issues(self.dashboard.tracked_issues().to_vec());
                    self.issue_browser = Some(view);
                }
                self.navigate_to(ViewId::IssueBrowser);
                just_created.then_some(Action::RefreshIssues)
            }
            Action::OpenInsights | Action::NavigateTo(ViewId::Insights) => {
                #[cfg(feature = "analytics")]
                self.start_insights_init();
                self.navigate_to(ViewId::Insights);
                None
            }
            #[cfg(feature = "markdown")]
            Action::NavigateTo(ViewId::MermaidOverlay(session)) => {
                use crate::views::mermaid_viewer::MermaidViewerView;
                self.mermaid_viewer = Some(MermaidViewerView::new(session.clone()));
                self.navigate_to(ViewId::MermaidOverlay(session));
                None
            }
            Action::NavigateBack => {
                self.navigate_back();
                None
            }
            _ => None,
        }
    }
}
