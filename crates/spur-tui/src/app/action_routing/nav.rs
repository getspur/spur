use super::*;

impl App {
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
                let Some(current_session) = self
                    .session_detail
                    .as_ref()
                    .map(|detail| detail.session_id().clone())
                else {
                    return Some(Action::FlashHint {
                        message: "Select a brain session first (S)".into(),
                    });
                };
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
                let Some(current_session) = self
                    .session_detail
                    .as_ref()
                    .map(|detail| detail.session_id().clone())
                else {
                    return Some(Action::FlashHint {
                        message: "Select a brain session first (S)".into(),
                    });
                };
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
