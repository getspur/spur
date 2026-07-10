use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::labels;
use spur_pm::{IssueFilter, IssueSummary, IssueUpdate};

impl super::Reconciler {
    /// Maintains durable owner liveness and adopts stale historical L3 plans.
    ///
    /// Brain-owned reconcilers heartbeat only plans they already own. The
    /// elected project runtime first gives pre-upgrade (lease-less) L3 plans a
    /// full lease grace window, then transfers only an expired owner to the
    /// stable system identity. The reclaim audit and owner fencing labels are
    /// written in the same backend update.
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
                let mut did_work = false;
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
                did_work |= self
                    .refresh_owned_epics(system_epics, renew_after, expires_at)
                    .await?;

                let historical = self
                    .pm
                    .list_issues(IssueFilter {
                        labels: vec![
                            labels::PLAN_COMPLETE.to_string(),
                            format!("{}l3", labels::AUTONOMY_PREFIX),
                        ],
                        status: Some("open".to_string()),
                        issue_type: Some("epic".to_string()),
                        limit: Some(10_000),
                        ..Default::default()
                    })
                    .await?;
                for epic in historical {
                    let owners = owner_labels(&epic);
                    if owners.is_empty() || owners.iter().any(|(_, owner)| owner == system_owner) {
                        continue;
                    }
                    match owner_lease_expiry(&epic) {
                        None => {
                            self.replace_owner_lease(&epic, expires_at).await?;
                            tracing::info!(
                                epic_id = %epic.id,
                                owners = ?owners.iter().map(|(_, owner)| owner).collect::<Vec<_>>(),
                                expires_at,
                                "project L3 runtime established pre-upgrade owner grace"
                            );
                            did_work = true;
                        }
                        Some(expiry) if expiry > now => {}
                        Some(expiry) => {
                            self.adopt_expired_l3_epic(&epic, &owners, expiry, expires_at)
                                .await?;
                            did_work = true;
                        }
                    }
                }
                Ok(did_work)
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

    async fn adopt_expired_l3_epic(
        &self,
        epic: &IssueSummary,
        owners: &[(String, String)],
        expired_at: i64,
        new_expiry: i64,
    ) -> anyhow::Result<()> {
        let Some(plan_id) = epic
            .labels
            .iter()
            .find_map(|label| labels::parse_plan_id(label))
        else {
            tracing::warn!(epic_id = %epic.id, "stale L3 epic missing plan id; refusing adoption");
            return Ok(());
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let prior_owner = self.durable_prior_owner(epic, owners).await?;
        let reason = format!("system L3 owner lease expired at {expired_at}");
        let audit =
            crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::PlanForceReclaimed {
                plan_id: plan_id.to_string(),
                prior_owner: Some(prior_owner.clone()),
                new_owner: crate::plan::loops::LOOP_RUNTIME_OWNER_ID.to_string(),
                token: token.clone(),
                reason: Some(reason),
            });
        let mut remove_labels = owners
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        remove_labels.extend(owner_lease_labels(epic));
        remove_labels.extend(
            epic.labels
                .iter()
                .filter(|label| labels::parse_plan_owner_token(label).is_some())
                .cloned(),
        );
        self.pm
            .update_issue(
                &epic.id,
                IssueUpdate {
                    comment: Some(audit),
                    add_labels: vec![
                        labels::plan_owner(crate::plan::loops::LOOP_RUNTIME_OWNER_ID),
                        labels::plan_owner_token(&token),
                        labels::plan_owner_lease_expires_at(new_expiry),
                    ],
                    remove_labels,
                    ..Default::default()
                },
            )
            .await?;
        tracing::warn!(
            %plan_id,
            epic_id = %epic.id,
            %prior_owner,
            new_owner = crate::plan::loops::LOOP_RUNTIME_OWNER_ID,
            "project L3 runtime adopted expired historical owner"
        );
        Ok(())
    }

    async fn durable_prior_owner(
        &self,
        epic: &IssueSummary,
        owners: &[(String, String)],
    ) -> anyhow::Result<String> {
        let fallback = owners
            .iter()
            .map(|(_, owner)| owner.as_str())
            .collect::<Vec<_>>()
            .join(",");
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )?;
        let Some(advanced) = self.pm.advanced() else {
            return Ok(fallback);
        };
        let audits = crate::plan::projector::collect_sorted_audits_for_issue(
            &epic.id,
            advanced.list_comments(&epic.id).await?,
        )?;
        let mut durable_owner = None;
        for audit in audits {
            match audit {
                AuditSentinelKind::PlanSubmit {
                    brain_session_id: Some(owner),
                    ..
                }
                | AuditSentinelKind::PlanOwnershipAcquired { owner, .. } => {
                    durable_owner = Some(owner);
                }
                AuditSentinelKind::PlanOwnershipTransferred { to, .. } => {
                    durable_owner = Some(to);
                }
                AuditSentinelKind::PlanForceReclaimed { new_owner, .. } => {
                    durable_owner = Some(new_owner);
                }
                _ => {}
            }
        }
        Ok(durable_owner
            .filter(|owner| {
                let compact = labels::compact_label_component(owner);
                owners.iter().any(|(_, candidate)| candidate == &compact)
            })
            .unwrap_or(fallback))
    }
}

fn owner_labels(epic: &IssueSummary) -> Vec<(String, String)> {
    epic.labels
        .iter()
        .filter_map(|label| {
            labels::parse_plan_owner(label).map(|owner| (label.clone(), owner.to_string()))
        })
        .collect()
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
