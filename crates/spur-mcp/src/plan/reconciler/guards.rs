use std::collections::HashMap;

use spur_pm::IssueFilter;

use super::{PlanDispatchState, Reconciler};

impl Reconciler {
    pub async fn plan_allows_dispatch(
        &self,
        plan_id: &str,
        cache: &mut HashMap<String, PlanDispatchState>,
    ) -> anyhow::Result<PlanDispatchState> {
        if let Some(state) = cache.get(plan_id) {
            return Ok(state.clone());
        }

        let mut open_complete_epic = None;
        let mut closed_complete_epic = None;
        let mut pending_epic = None;
        for summary in self
            .pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(100),
                ..Default::default()
            })
            .await?
        {
            let epic = self.pm.get_issue(&summary.id).await?;
            if pending_epic.is_none()
                && epic
                    .labels
                    .iter()
                    .any(|label| label == crate::plan::labels::PLAN_PENDING)
            {
                pending_epic = Some(epic.id.clone());
            }
            if epic
                .labels
                .iter()
                .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
            {
                if epic.status == "open" {
                    if let Some(dispatch) = self.dispatch.as_ref() {
                        match crate::plan::ownership::classify_owner(
                            &epic.labels,
                            dispatch.brain_session_id.as_session_id(),
                        ) {
                            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
                            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                                let state = PlanDispatchState::PlanOwnedByAnotherBrain {
                                    epic_id: epic.id.clone(),
                                    owner,
                                };
                                tracing::debug!(
                                    plan_id = %plan_id,
                                    ?state,
                                    "reconciler suppressed ready tasks for plan owned by another brain"
                                );
                                cache.insert(plan_id.to_string(), state.clone());
                                return Ok(state);
                            }
                            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                                let state = PlanDispatchState::PlanOwnedByAnotherBrain {
                                    epic_id: epic.id.clone(),
                                    owner: owners.join(","),
                                };
                                tracing::debug!(
                                    plan_id = %plan_id,
                                    ?state,
                                    "reconciler suppressed ready tasks for plan with ambiguous owner labels"
                                );
                                cache.insert(plan_id.to_string(), state.clone());
                                return Ok(state);
                            }
                            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                                let state = PlanDispatchState::PlanOwnedByAnotherBrain {
                                    epic_id: epic.id.clone(),
                                    owner: "unowned".to_string(),
                                };
                                tracing::debug!(
                                    plan_id = %plan_id,
                                    ?state,
                                    "reconciler suppressed ready tasks for unowned plan"
                                );
                                cache.insert(plan_id.to_string(), state.clone());
                                return Ok(state);
                            }
                        }
                    }
                    open_complete_epic = Some(epic.id.clone());
                } else if closed_complete_epic.is_none() {
                    closed_complete_epic = Some(epic.id.clone());
                }
            }
        }

        let state = if let Some(epic_id) = pending_epic {
            PlanDispatchState::PlanHasPendingEpic { epic_id }
        } else if open_complete_epic.is_some() {
            PlanDispatchState::Allowed
        } else if let Some(epic_id) = closed_complete_epic {
            PlanDispatchState::EpicNotOpen { epic_id }
        } else {
            PlanDispatchState::PlanMissingCompleteEpic
        };
        if !matches!(state, PlanDispatchState::Allowed) {
            tracing::debug!(
                plan_id = %plan_id,
                ?state,
                "reconciler suppressed ready tasks for inactive plan"
            );
        }
        cache.insert(plan_id.to_string(), state.clone());
        Ok(state)
    }

    pub(super) async fn plan_allows_writes(
        &self,
        plan_id: &str,
        complete_epic: Option<&spur_pm::IssueSummary>,
        cache: &mut HashMap<String, PlanDispatchState>,
    ) -> anyhow::Result<PlanDispatchState> {
        if let Some(state) = cache.get(plan_id) {
            return Ok(state.clone());
        }

        let state = if let Some(epic) = complete_epic {
            self.complete_epic_allows_current_brain_writes(&epic.id, &epic.labels)
        } else {
            let mut state = PlanDispatchState::PlanMissingCompleteEpic;
            if let Some(summary) = self
                .pm
                .list_issues(IssueFilter {
                    labels: vec![
                        crate::plan::labels::plan_id(plan_id),
                        crate::plan::labels::PLAN_COMPLETE.to_string(),
                    ],
                    issue_type: Some("epic".to_string()),
                    include_closed: true,
                    limit: Some(100),
                    ..Default::default()
                })
                .await?
                .into_iter()
                .next()
            {
                state =
                    self.complete_epic_allows_current_brain_writes(&summary.id, &summary.labels);
            }
            state
        };

        if !matches!(state, PlanDispatchState::Allowed) {
            tracing::debug!(
                plan_id = %plan_id,
                ?state,
                "reconciler suppressed plan write path for inactive owner"
            );
        }
        cache.insert(plan_id.to_string(), state.clone());
        Ok(state)
    }

    fn complete_epic_allows_current_brain_writes(
        &self,
        epic_id: &str,
        labels: &[String],
    ) -> PlanDispatchState {
        let Some(dispatch) = self.dispatch.as_ref() else {
            return PlanDispatchState::PlanOwnedByAnotherBrain {
                epic_id: epic_id.to_string(),
                owner: "unowned".to_string(),
            };
        };

        match crate::plan::ownership::classify_owner(
            labels,
            dispatch.brain_session_id.as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => PlanDispatchState::Allowed,
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                PlanDispatchState::PlanOwnedByAnotherBrain {
                    epic_id: epic_id.to_string(),
                    owner,
                }
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                PlanDispatchState::PlanOwnedByAnotherBrain {
                    epic_id: epic_id.to_string(),
                    owner: owners.join(","),
                }
            }
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                PlanDispatchState::PlanOwnedByAnotherBrain {
                    epic_id: epic_id.to_string(),
                    owner: "unowned".to_string(),
                }
            }
        }
    }
}
