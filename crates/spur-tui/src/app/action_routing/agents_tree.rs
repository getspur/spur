use super::*;

impl App {
    pub(super) fn process_agents_focus(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::SelectNextBy(n) => {
                if matches!(&self.current_view, ViewId::IssueBrowser) {
                    return self
                        .issue_browser
                        .as_mut()
                        .and_then(|view| view.take_pending_action());
                }

                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_next(&self.lineage);
                }
                None
            }

            Action::SelectPrevBy(n) => {
                if matches!(&self.current_view, ViewId::IssueBrowser) {
                    return self
                        .issue_browser
                        .as_mut()
                        .and_then(|view| view.take_pending_action());
                }

                for _ in 0..n {
                    self.dashboard.agents_tree_mut().select_prev(&self.lineage);
                }
                None
            }

            Action::FocusNode => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.set_focused_node(Some(id));
                }
                None
            }

            Action::UnfocusNode => {
                self.dashboard.set_focused_node(None);
                None
            }

            Action::JumpToReview => {
                // Cycle forward through pending reviews in DISPLAY order
                // (newest first), so `r`/`N` flows top-to-bottom on screen
                // matching the AgentsTree visual ordering.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let next = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| reviews.get(i + 1).cloned())
                    .or_else(|| reviews.into_iter().next());
                if let Some(id) = next {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
                None
            }

            Action::JumpToPreviousReview => {
                // Cycle backward through pending reviews in DISPLAY order
                // (newest first); "previous" means visually upward on screen.
                let current = self.dashboard.focused_node().cloned();
                let mut reviews = self.lineage.pending_reviews();
                reviews.reverse();
                let prev = reviews
                    .iter()
                    .position(|id| Some(id) == current.as_ref())
                    .and_then(|i| i.checked_sub(1).and_then(|j| reviews.get(j).cloned()))
                    .or_else(|| reviews.last().cloned());
                if let Some(id) = prev {
                    self.dashboard
                        .agents_tree_mut()
                        .set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                    self.dashboard
                        .detail_pane_mut()
                        .jump_to_tab(crate::components::detail_pane::DetailTab::Review);
                }
                None
            }

            Action::ToggleCollapse => {
                let selected = self.dashboard.agents_tree_mut().selected().cloned();
                if let Some(id) = selected {
                    self.dashboard.agents_tree_mut().toggle_collapsed(&id);
                }
                None
            }

            Action::InspectWorkers => {
                use crate::views::dashboard::Panel;
                use spur_acp::LifecycleState;

                // Toggle: Dashboard -> SessionDetail, otherwise -> Dashboard with Agents focused.
                if matches!(self.current_view, ViewId::Dashboard) {
                    if let Some(ref detail) = self.session_detail {
                        tracing::info!(
                            session_id = %detail.session_id().0,
                            "InspectWorkers: toggling Dashboard -> SessionDetail"
                        );
                        self.navigate_to(ViewId::SessionDetail(detail.session_id().clone()));
                    } else {
                        tracing::info!("InspectWorkers: no session_detail, staying in Dashboard");
                    }
                    return None;
                }
                tracing::info!(
                    current_view = ?self.current_view,
                    "InspectWorkers: navigating to Dashboard"
                );

                // Pre-select: AwaitingReview > Running > most recent worker.
                let priority = self
                    .lineage
                    .nodes()
                    .filter(|n| n.role == spur_acp::Role::Executor)
                    .max_by_key(|n| match n.phase {
                        LifecycleState::AwaitingReview => 3,
                        LifecycleState::Running
                        | LifecycleState::Resuming
                        | LifecycleState::Spawning => 2,
                        _ => 1,
                    })
                    .map(|n| n.id.clone());
                self.dashboard.set_focused_panel(Panel::Agents);
                self.dashboard.set_focused_node(priority);
                self.navigate_to(ViewId::Dashboard);
                None
            }

            Action::FocusWorkerInDashboard { executor_id, tab } => {
                use crate::views::dashboard::Panel;
                let eid = spur_core::ExecutorId(executor_id);
                self.dashboard.set_focused_panel(Panel::Agents);
                self.dashboard
                    .agents_tree_mut()
                    .set_selected(Some(eid.clone()));
                self.dashboard.set_focused_node(Some(eid));
                self.dashboard.detail_pane_mut().jump_to_tab(tab);
                self.navigate_to(ViewId::Dashboard);
                None
            }

            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, ViewId};
    use crate::components::detail_pane::DetailTab;
    use crate::views::dashboard::Panel;

    #[test]
    fn focus_worker_action_navigates_to_dashboard_and_targets_executor() {
        let mut app = App::new_for_tests();

        // Seed an executor so AgentsTree has a node to select.
        app.handle_spur_event(SpurEvent::now(SpurEventBody::ExecutorSpawned {
            id: "worker-session-1".into(),
            parent_id: None,
            session_id: SessionId("brain-1".into()),
            agent: "codex".into(),
            role: spur_acp::Role::Executor,
            task_spec: "test task".into(),
        }));

        app.process_action(Action::FocusWorkerInDashboard {
            executor_id: "worker-session-1".into(),
            tab: DetailTab::Stream,
        });

        assert_eq!(app.current_view(), &ViewId::Dashboard);
        assert_eq!(app.dashboard_for_test().focused_panel(), Panel::Agents);
        assert_eq!(
            app.dashboard_for_test().focused_node(),
            Some(&spur_core::ExecutorId("worker-session-1".into()))
        );
        assert_eq!(
            app.dashboard_for_test().detail_pane().current_tab(),
            DetailTab::Stream
        );
    }
}
