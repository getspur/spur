use crate::plan::labels;
use spur_pm::{IssueFilter, IssueSummary, IssueUpdate};

impl super::Reconciler {
    /// Maintains durable owner liveness without inferring ownership transfers.
    ///
    /// Both scopes heartbeat only plans they already own. Legacy brain-owned
    /// L3 generations remain parked until an operator explicitly migrates one
    /// with `force_reclaim_plan(target_owner = "system_l3")`.
    pub(super) async fn reconcile_plan_owner_leases(&self) -> anyhow::Result<bool> {
        let now = unix_seconds(self.now());
        let lease_secs = self.config.dispatch_lease_duration.as_secs().max(1) as i64;
        let expires_at = now.saturating_add(lease_secs);
        let renew_after = now.saturating_add((lease_secs / 2).max(1));

        match self.config.plan_scope {
            super::PlanScope::BrainOwned => {
                let Some(dispatch) = self.dispatch.as_ref() else {
                    return Ok(false);
                };
                let owner = &dispatch.brain_session_id().as_session_id().0;
                let epics = self
                    .pm
                    .list_issues(IssueFilter {
                        labels: vec![
                            labels::PLAN_COMPLETE.to_string(),
                            labels::plan_owner(owner),
                            format!("{}l3", labels::AUTONOMY_PREFIX),
                        ],
                        status: Some("open".to_string()),
                        issue_type: Some("epic".to_string()),
                        limit: Some(10_000),
                        ..Default::default()
                    })
                    .await?;
                self.refresh_owned_epics(epics, renew_after, expires_at)
                    .await
            }
            super::PlanScope::SystemL3Only => {
                let system_owner = crate::plan::loops::LOOP_RUNTIME_OWNER_ID;
                let system_epics = self
                    .pm
                    .list_issues(IssueFilter {
                        labels: vec![
                            labels::PLAN_COMPLETE.to_string(),
                            labels::plan_owner(system_owner),
                            format!("{}l3", labels::AUTONOMY_PREFIX),
                        ],
                        status: Some("open".to_string()),
                        issue_type: Some("epic".to_string()),
                        limit: Some(10_000),
                        ..Default::default()
                    })
                    .await?;
                self.refresh_owned_epics(system_epics, renew_after, expires_at)
                    .await
            }
        }
    }

    async fn refresh_owned_epics(
        &self,
        epics: Vec<IssueSummary>,
        renew_after: i64,
        expires_at: i64,
    ) -> anyhow::Result<bool> {
        let mut did_work = false;
        for epic in epics {
            if owner_lease_expiry(&epic).is_some_and(|expiry| expiry > renew_after) {
                continue;
            }
            self.replace_owner_lease(&epic, expires_at).await?;
            did_work = true;
        }
        Ok(did_work)
    }

    async fn replace_owner_lease(
        &self,
        epic: &IssueSummary,
        expires_at: i64,
    ) -> anyhow::Result<()> {
        self.pm
            .update_issue(
                &epic.id,
                IssueUpdate {
                    add_labels: vec![labels::plan_owner_lease_expires_at(expires_at)],
                    remove_labels: owner_lease_labels(epic),
                    ..Default::default()
                },
            )
            .await
    }
}

fn owner_lease_labels(epic: &IssueSummary) -> Vec<String> {
    epic.labels
        .iter()
        .filter(|label| labels::parse_plan_owner_lease_expires_at(label).is_some())
        .cloned()
        .collect()
}

fn owner_lease_expiry(epic: &IssueSummary) -> Option<i64> {
    epic.labels
        .iter()
        .filter_map(|label| labels::parse_plan_owner_lease_expires_at(label))
        .max()
}

fn unix_seconds(now: std::time::SystemTime) -> i64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
