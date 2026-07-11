use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::plan::audit_sentinel::{AuditSentinelKind, CompletionState, SystemReviewDecision};

const MAX_REVIEW_ATTEMPTS: usize = 3;

#[derive(Debug, Clone)]
struct ReviewIdentity {
    plan_id: String,
    task_id: String,
    attempt: u32,
    maker_delegation_id: String,
    maker_branch: String,
    target_issue_id: String,
}

#[derive(Debug, Clone)]
struct ReviewDispatchRecord {
    maker_delegation_id: String,
    reviewer_delegation_id: String,
    review_issue_id: String,
}

#[derive(Debug, Clone)]
struct ReviewVerdictRecord {
    reviewer_delegation_id: String,
    decision: SystemReviewDecision,
    feedback: String,
    evidence: Vec<String>,
}

#[derive(Debug, Clone)]
struct SignalFact {
    signal_id: String,
    kind: String,
    reason: String,
    delegation_id: Option<String>,
}

#[derive(Debug, Default)]
struct ReviewSignals {
    mark_noop: Vec<SignalFact>,
    blocking: Vec<SignalFact>,
}

struct ReviewDispatchRequest<'a> {
    task: &'a crate::plan::PlanTaskEntry,
    identity: &'a ReviewIdentity,
    signals: &'a ReviewSignals,
    existing_companion: Option<&'a str>,
    previous_dispatch: Option<&'a ReviewDispatchRecord>,
    review_attempt: u32,
}

impl super::Reconciler {
    /// Dispatches and consumes authenticated independent reviews for
    /// system-owned L3 maker completions.
    pub(super) async fn reconcile_system_l3_reviews(&self) -> anyhow::Result<bool> {
        if !matches!(self.config.plan_scope, super::PlanScope::SystemL3Only) {
            return Ok(false);
        }
        crate::server::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            self.feature_gate.as_ref(),
        )?;
        let Some(advanced) = self.pm.advanced() else {
            anyhow::bail!("system L3 review requires the beads advanced backend");
        };

        let mut did_work = false;
        for plan_id in self.scoped_plan_ids().await? {
            let projected = self.project_plan_from_beads(&plan_id).await?;
            let awaiting = projected
                .tasks
                .iter()
                .filter(|task| {
                    matches!(
                        task.status,
                        crate::plan::PlanTaskStatus::AwaitingReview { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            for task in awaiting {
                let Some(issue_id) = task.spec.issue_id.as_deref() else {
                    tracing::warn!(%plan_id, task_id = %task.spec.task_id, "system L3 review skipped task without issue id");
                    continue;
                };
                let issue = self.pm.get_issue(issue_id).await?;
                let comments = advanced.list_comments(issue_id).await?;
                let audits = crate::plan::projector::collect_sorted_audits_for_issue(
                    issue_id,
                    comments.clone(),
                )?;
                let Some(identity) = review_identity(&plan_id, &task, &audits) else {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %issue_id,
                        "system L3 review rejected stale or non-reviewable maker completion"
                    );
                    continue;
                };
                let signals = classify_review_signals(
                    &comments,
                    &issue.labels,
                    &identity.maker_delegation_id,
                )?;
                if !signals.blocking.is_empty() {
                    tracing::warn!(
                        %plan_id,
                        task_id = %task.spec.task_id,
                        %issue_id,
                        blocking_signals = ?signals.blocking.iter().map(|signal| (&signal.kind, &signal.signal_id)).collect::<Vec<_>>(),
                        "system L3 review remains parked on unresolved blocking signals"
                    );
                    continue;
                }

                let dispatches = matching_review_dispatches(&audits, &identity);
                if let Some(current) = dispatches.last() {
                    if let Some(verdict) = matching_verdict(&audits, current) {
                        self.apply_system_review_verdict(&identity, current, &verdict)
                            .await?;
                        close_review_companion(
                            self.pm.as_ref(),
                            &current.review_issue_id,
                            &current.reviewer_delegation_id,
                        )
                        .await?;
                        did_work = true;
                        continue;
                    }

                    let companion = self.pm.get_issue(&current.review_issue_id).await?;
                    if !review_dispatch_is_stale(
                        &companion,
                        self.now(),
                        self.config.dispatch_lease_duration,
                    ) {
                        continue;
                    }
                    if dispatches.len() >= MAX_REVIEW_ATTEMPTS {
                        park_review_failure(self.pm.as_ref(), advanced, &identity, current).await?;
                        close_review_companion(
                            self.pm.as_ref(),
                            &current.review_issue_id,
                            &current.reviewer_delegation_id,
                        )
                        .await?;
                        did_work = true;
                        continue;
                    }
                }

                let Some(dispatch) = self.dispatch.as_ref() else {
                    tracing::warn!(%plan_id, task_id = %task.spec.task_id, "system L3 review has no dispatch context");
                    continue;
                };
                let existing_companion = dispatches
                    .last()
                    .map(|record| record.review_issue_id.as_str());
                self.dispatch_system_review(
                    dispatch.as_ref(),
                    ReviewDispatchRequest {
                        task: &task,
                        identity: &identity,
                        signals: &signals,
                        existing_companion,
                        previous_dispatch: dispatches.last(),
                        review_attempt: dispatches.len() as u32 + 1,
                    },
                )
                .await?;
                did_work = true;
            }
        }
        did_work |= self.sweep_completed_review_companions().await?;
        if did_work {
            self.fast_forward.notify_one();
        }
        Ok(did_work)
    }

    async fn sweep_completed_review_companions(&self) -> anyhow::Result<bool> {
        let companions = self
            .pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![crate::plan::labels::SYSTEM_REVIEW.to_string()],
                status: Some("open".to_string()),
                issue_type: Some("task".to_string()),
                include_closed: false,
                limit: Some(500),
                ..Default::default()
            })
            .await?;
        let mut did_work = false;
        for companion in companions {
            let detail = self.pm.get_issue(&companion.id).await?;
            let target_issue_id = detail
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_review_target(label));
            let maker_delegation_id = detail
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_review_maker_delegation(label));
            let reviewer_delegation_id = detail
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_review_reviewer_delegation(label));
            let (Some(target_issue_id), Some(maker_delegation_id), Some(reviewer_delegation_id)) =
                (target_issue_id, maker_delegation_id, reviewer_delegation_id)
            else {
                tracing::warn!(
                    review_issue_id = %companion.id,
                    "system review companion is missing durable binding labels"
                );
                continue;
            };
            if review_verdict_exists(
                self.pm.as_ref(),
                &target_issue_id,
                maker_delegation_id,
                reviewer_delegation_id,
                &companion.id,
            )
            .await?
            {
                close_review_companion(self.pm.as_ref(), &companion.id, reviewer_delegation_id)
                    .await?;
                did_work = true;
            }
        }
        Ok(did_work)
    }

    async fn dispatch_system_review(
        &self,
        dispatch: &dyn super::ReconcilerDispatch,
        request: ReviewDispatchRequest<'_>,
    ) -> anyhow::Result<()> {
        let ReviewDispatchRequest {
            task,
            identity,
            signals,
            existing_companion,
            previous_dispatch,
            review_attempt,
        } = request;
        let reviewer_delegation_id = crate::plan::labels::mint_delegation_id();
        let review_issue_id = match existing_companion {
            Some(issue_id) => {
                if let Some(previous) = previous_dispatch {
                    crate::plan::clear_dispatch_intent(
                        self.pm.as_ref(),
                        issue_id,
                        &previous.reviewer_delegation_id,
                    )
                    .await?;
                }
                let detail = self.pm.get_issue(issue_id).await?;
                let old_reviewer_labels = detail
                    .labels
                    .iter()
                    .filter(|label| {
                        crate::plan::labels::parse_review_reviewer_delegation(label).is_some()
                    })
                    .cloned()
                    .collect();
                self.pm
                    .update_issue(
                        issue_id,
                        spur_pm::IssueUpdate {
                            status: Some("open".into()),
                            add_labels: vec![crate::plan::labels::review_reviewer_delegation(
                                &reviewer_delegation_id,
                            )],
                            remove_labels: old_reviewer_labels,
                            ..Default::default()
                        },
                    )
                    .await?;
                issue_id.to_string()
            }
            None => {
                self.find_or_create_review_companion(identity, &reviewer_delegation_id)
                    .await?
            }
        };

        self.pm
            .advanced()
            .expect("system L3 review already required beads advanced")
            .add_comment(
                &identity.target_issue_id,
                &crate::plan::audit_sentinel::encode_comment(
                    &AuditSentinelKind::SystemReviewDispatch {
                        plan_id: identity.plan_id.clone(),
                        task_id: identity.task_id.clone(),
                        attempt: identity.attempt,
                        maker_delegation_id: identity.maker_delegation_id.clone(),
                        reviewer_delegation_id: reviewer_delegation_id.clone(),
                        review_issue_id: review_issue_id.clone(),
                    },
                ),
            )
            .await?;
        crate::plan::persist_dispatch_intent(
            self.pm.as_ref(),
            &review_issue_id,
            self.feature_gate.as_ref(),
            &identity.plan_id,
            &reviewer_delegation_id,
            &task.spec.agent,
            review_attempt,
            self.config.dispatch_lease_duration,
        )
        .await?;

        let prompt = reviewer_prompt(task, identity, signals);
        let (respond_to, result_rx) = tokio::sync::oneshot::channel();
        let (dispatched_base_oid_tx, _dispatched_base_oid_rx) = tokio::sync::watch::channel(None);
        let request = crate::DelegationRequest {
            id: reviewer_delegation_id.clone().into(),
            agent: task.spec.agent.clone(),
            profile: task.spec.profile.clone(),
            skills: task.spec.skills.clone(),
            model: task.spec.model.clone(),
            effort: task.spec.effort.clone(),
            config_overrides: task.spec.config_overrides.clone(),
            task: prompt,
            context_files: task.spec.context_files.clone(),
            prior_branch_for_reuse: None,
            respond_to,
            brain_session_id: dispatch.brain_session_id().clone(),
            delegation_plan: None,
            issue_id: Some(review_issue_id.clone()),
            base: Some(crate::BaseSpec::Branch {
                name: identity.maker_branch.clone(),
            }),
            dispatched_base_oid_tx: Some(dispatched_base_oid_tx),
            attempt_tracker: Arc::new(std::sync::atomic::AtomicU32::new(review_attempt)),
            enable_worker_mcp: Some(true),
        };
        if let Err(error) = dispatch.send_delegation(request).await {
            crate::plan::clear_dispatch_intent(
                self.pm.as_ref(),
                &review_issue_id,
                &reviewer_delegation_id,
            )
            .await?;
            return Err(error);
        }

        let pm = Arc::clone(&self.pm);
        let target_issue_id = identity.target_issue_id.clone();
        let maker_delegation_id = identity.maker_delegation_id.clone();
        let reviewer_id_for_result = reviewer_delegation_id.clone();
        let review_issue_for_result = review_issue_id.clone();
        let fast_forward = Arc::clone(&self.fast_forward);
        dispatch.track_task(Box::pin(async move {
            let _ = result_rx.await;
            let verdict_exists = review_verdict_exists(
                pm.as_ref(),
                &target_issue_id,
                &maker_delegation_id,
                &reviewer_id_for_result,
                &review_issue_for_result,
            )
            .await
            .unwrap_or(false);
            if verdict_exists {
                if let Err(error) = close_review_companion(
                    pm.as_ref(),
                    &review_issue_for_result,
                    &reviewer_id_for_result,
                )
                .await
                {
                    tracing::warn!(%error, review_issue_id = %review_issue_for_result, "failed to close completed review companion");
                }
            }
            fast_forward.notify_one();
        }));

        tracing::info!(
            plan_id = %identity.plan_id,
            task_id = %identity.task_id,
            maker_delegation_id = %identity.maker_delegation_id,
            %reviewer_delegation_id,
            %review_issue_id,
            review_attempt,
            "dispatched independent system L3 reviewer"
        );
        Ok(())
    }

    async fn find_or_create_review_companion(
        &self,
        identity: &ReviewIdentity,
        reviewer_delegation_id: &str,
    ) -> anyhow::Result<String> {
        let target_label = crate::plan::labels::review_target(&identity.target_issue_id);
        let maker_label =
            crate::plan::labels::review_maker_delegation(&identity.maker_delegation_id);
        let existing = self
            .pm
            .list_issues(spur_pm::IssueFilter {
                labels: vec![
                    crate::plan::labels::SYSTEM_REVIEW.to_string(),
                    target_label.clone(),
                    maker_label.clone(),
                ],
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await?;
        if let Some(existing) = existing.first() {
            let detail = self.pm.get_issue(&existing.id).await?;
            let old_reviewer_labels = detail
                .labels
                .iter()
                .filter(|label| {
                    crate::plan::labels::parse_review_reviewer_delegation(label).is_some()
                })
                .cloned()
                .collect();
            self.pm
                .update_issue(
                    &existing.id,
                    spur_pm::IssueUpdate {
                        status: Some("open".into()),
                        add_labels: vec![crate::plan::labels::review_reviewer_delegation(
                            reviewer_delegation_id,
                        )],
                        remove_labels: old_reviewer_labels,
                        ..Default::default()
                    },
                )
                .await?;
            return Ok(existing.id.clone());
        }

        self.pm
            .create_issue(spur_pm::IssueCreate {
                title: format!(
                    "Review {} / {} attempt {}",
                    identity.plan_id, identity.task_id, identity.attempt
                ),
                description: Some(format!(
                    "Independent system L3 review for maker delegation {} on task {}.",
                    identity.maker_delegation_id, identity.task_id
                )),
                issue_type: Some("task".into()),
                labels: vec![
                    crate::plan::labels::SYSTEM_REVIEW.to_string(),
                    target_label,
                    maker_label,
                    crate::plan::labels::review_reviewer_delegation(reviewer_delegation_id),
                ],
                ..Default::default()
            })
            .await
    }

    async fn apply_system_review_verdict(
        &self,
        identity: &ReviewIdentity,
        dispatch: &ReviewDispatchRecord,
        verdict: &ReviewVerdictRecord,
    ) -> anyhow::Result<()> {
        let fresh = self.project_plan_from_beads(&identity.plan_id).await?;
        let fresh_task = fresh
            .tasks
            .iter()
            .find(|task| task.spec.task_id == identity.task_id)
            .ok_or_else(|| anyhow::anyhow!("review target disappeared from projected plan"))?;
        let _target_guard = crate::plan::system_review_target_lock(&identity.target_issue_id)
            .lock()
            .await;
        let target = self.pm.get_issue(&identity.target_issue_id).await?;
        let comments = self
            .pm
            .advanced()
            .expect("beads advanced")
            .list_comments(&identity.target_issue_id)
            .await?;
        let signals =
            classify_review_signals(&comments, &target.labels, &identity.maker_delegation_id)?;
        if !signals.blocking.is_empty() {
            anyhow::bail!("system review verdict blocked by a signal that arrived before apply");
        }
        let audits = crate::plan::projector::collect_sorted_audits_for_issue(
            &identity.target_issue_id,
            comments,
        )?;
        if reviewable_completion(&audits) != Some(identity.maker_delegation_id.as_str())
            || review_identity(&identity.plan_id, fresh_task, &audits)
                .is_none_or(|current| current.maker_delegation_id != identity.maker_delegation_id)
        {
            anyhow::bail!("system review verdict no longer binds the latest maker completion");
        }
        let companion = self.pm.get_issue(&dispatch.review_issue_id).await?;
        for expected in [
            crate::plan::labels::SYSTEM_REVIEW.to_string(),
            crate::plan::labels::review_target(&identity.target_issue_id),
            crate::plan::labels::review_maker_delegation(&identity.maker_delegation_id),
            crate::plan::labels::review_reviewer_delegation(&dispatch.reviewer_delegation_id),
        ] {
            if !companion.labels.contains(&expected) {
                anyhow::bail!("system review companion binding changed before verdict apply");
            }
        }
        let (decision, reuse_prior_worktree) = match verdict.decision {
            SystemReviewDecision::Approve => ("approve", false),
            SystemReviewDecision::RequestChanges => ("request_changes", true),
        };
        let feedback = format!(
            "Independent reviewer {}: {}",
            verdict.reviewer_delegation_id, verdict.feedback
        );
        let plan_arc = Arc::new(tokio::sync::Mutex::new(fresh));
        crate::plan::handle_review_task_with_write_mode(
            plan_arc,
            &identity.plan_id,
            &identity.task_id,
            decision,
            Some(&feedback),
            reuse_prior_worktree,
            Some(Arc::clone(&self.pm)),
            self.dispatch
                .as_ref()
                .and_then(|current| current.event_sink())
                .map(|sink| sink.as_ref()),
            None,
            None,
            Arc::clone(&self.feature_gate),
            crate::plan::ReviewWriteMode::NonAdvisory,
        )
        .await
        .map_err(anyhow::Error::msg)?;
        tracing::info!(
            plan_id = %identity.plan_id,
            task_id = %identity.task_id,
            maker_delegation_id = %identity.maker_delegation_id,
            reviewer_delegation_id = %dispatch.reviewer_delegation_id,
            %decision,
            "applied authenticated system L3 review verdict"
        );
        Ok(())
    }
}

fn reviewable_completion(audits: &[AuditSentinelKind]) -> Option<&str> {
    let completion = audits
        .iter()
        .rev()
        .find(|audit| matches!(audit, AuditSentinelKind::Completion { .. }))?;
    let maker_delegation_id = match completion {
        AuditSentinelKind::Completion {
            delegation_id,
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch),
            ..
        } if !delegation_id.is_empty() && !worker_branch.is_empty() => delegation_id.as_str(),
        AuditSentinelKind::Completion { .. } => return None,
        _ => unreachable!("latest audit was filtered to a completion"),
    };
    let dispatch = audits.iter().rev().find_map(|audit| match audit {
        AuditSentinelKind::SystemReviewDispatch {
            maker_delegation_id: maker,
            reviewer_delegation_id,
            review_issue_id,
            ..
        } if maker == maker_delegation_id => {
            Some((reviewer_delegation_id.as_str(), review_issue_id.as_str()))
        }
        _ => None,
    })?;
    audits.iter().rev().find_map(|audit| match audit {
        AuditSentinelKind::SystemReviewVerdict {
            maker_delegation_id: maker,
            reviewer_delegation_id,
            review_issue_id,
            ..
        } if maker == maker_delegation_id
            && reviewer_delegation_id == dispatch.0
            && review_issue_id == dispatch.1 =>
        {
            Some(maker_delegation_id)
        }
        _ => None,
    })
}

fn review_identity(
    plan_id: &str,
    task: &crate::plan::PlanTaskEntry,
    audits: &[AuditSentinelKind],
) -> Option<ReviewIdentity> {
    let completion = audits
        .iter()
        .rev()
        .find(|audit| matches!(audit, AuditSentinelKind::Completion { .. }))?;
    let (maker_delegation_id, maker_branch) = match completion {
        AuditSentinelKind::Completion {
            delegation_id,
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch),
            ..
        } if !delegation_id.is_empty() && !worker_branch.is_empty() => {
            (delegation_id, worker_branch)
        }
        _ => return None,
    };
    if task.last_delegation_id.as_deref() != Some(maker_delegation_id.as_str()) {
        return None;
    }
    Some(ReviewIdentity {
        plan_id: plan_id.to_string(),
        task_id: task.spec.task_id.clone(),
        attempt: task.attempt,
        maker_delegation_id: maker_delegation_id.clone(),
        maker_branch: maker_branch.clone(),
        target_issue_id: task.spec.issue_id.clone()?,
    })
}

fn matching_review_dispatches(
    audits: &[AuditSentinelKind],
    identity: &ReviewIdentity,
) -> Vec<ReviewDispatchRecord> {
    audits
        .iter()
        .filter_map(|audit| match audit {
            AuditSentinelKind::SystemReviewDispatch {
                plan_id,
                task_id,
                attempt,
                maker_delegation_id,
                reviewer_delegation_id,
                review_issue_id,
            } if plan_id == &identity.plan_id
                && task_id == &identity.task_id
                && *attempt == identity.attempt
                && maker_delegation_id == &identity.maker_delegation_id =>
            {
                Some(ReviewDispatchRecord {
                    maker_delegation_id: maker_delegation_id.clone(),
                    reviewer_delegation_id: reviewer_delegation_id.clone(),
                    review_issue_id: review_issue_id.clone(),
                })
            }
            _ => None,
        })
        .collect()
}

fn matching_verdict(
    audits: &[AuditSentinelKind],
    dispatch: &ReviewDispatchRecord,
) -> Option<ReviewVerdictRecord> {
    let mut matching = audits.iter().filter_map(|audit| match audit {
        AuditSentinelKind::SystemReviewVerdict {
            maker_delegation_id,
            reviewer_delegation_id,
            review_issue_id,
            decision,
            feedback,
            evidence,
        } if maker_delegation_id == &dispatch.maker_delegation_id
            && reviewer_delegation_id == &dispatch.reviewer_delegation_id
            && review_issue_id == &dispatch.review_issue_id =>
        {
            Some(ReviewVerdictRecord {
                reviewer_delegation_id: reviewer_delegation_id.clone(),
                decision: decision.clone(),
                feedback: feedback.clone(),
                evidence: evidence.clone(),
            })
        }
        _ => None,
    });
    let first = matching.next()?;
    if matching.any(|candidate| {
        candidate.decision != first.decision
            || candidate.feedback != first.feedback
            || candidate.evidence != first.evidence
    }) {
        tracing::error!(
            maker_delegation_id = %dispatch.maker_delegation_id,
            reviewer_delegation_id = %dispatch.reviewer_delegation_id,
            review_issue_id = %dispatch.review_issue_id,
            "conflicting durable reviewer verdicts; parking target"
        );
        return None;
    }
    Some(first)
}

fn classify_review_signals(
    comments: &[spur_pm::Comment],
    labels: &[String],
    maker_delegation_id: &str,
) -> anyhow::Result<ReviewSignals> {
    #[derive(serde::Deserialize)]
    struct RawSignal {
        signal_id: String,
        kind: String,
        #[serde(default)]
        reason: String,
    }

    let mut durable_facts = std::collections::HashMap::<String, SignalFact>::new();
    let mut legacy_facts = Vec::<SignalFact>::new();
    for comment in comments {
        if let Some(Ok(AuditSentinelKind::Signal {
            signal_id,
            delegation_id,
            kind,
            reason,
            ..
        })) = crate::plan::audit_sentinel::parse_comment(&comment.body)
        {
            durable_facts
                .entry(signal_id.clone())
                .or_insert(SignalFact {
                    signal_id,
                    kind,
                    reason,
                    delegation_id: (!delegation_id.is_empty()).then_some(delegation_id),
                });
        }
        let Some(rest) = comment
            .body
            .trim_start()
            .strip_prefix(crate::plan::signals::SENTINEL_PREFIX)
        else {
            continue;
        };
        let raw: RawSignal = serde_json::from_str(rest.trim_start()).map_err(|error| {
            anyhow::anyhow!("malformed worker signal blocks system review: {error}")
        })?;
        if !durable_facts.contains_key(&raw.signal_id) {
            legacy_facts.push(SignalFact {
                signal_id: raw.signal_id,
                kind: raw.kind,
                reason: raw.reason,
                delegation_id: None,
            });
        }
    }

    let mut facts = durable_facts.into_values().collect::<Vec<_>>();
    facts.extend(legacy_facts);
    let normalized_kind = |kind: &str| kind.to_ascii_lowercase().replace('_', "-");
    let fact_kinds = facts
        .iter()
        .map(|fact| normalized_kind(&fact.kind))
        .collect::<std::collections::HashSet<_>>();

    let processed = labels
        .iter()
        .filter_map(|label| label.strip_prefix("spur:signal-processed:"))
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let mut classified = ReviewSignals::default();
    for fact in facts {
        if !seen.insert(fact.signal_id.clone()) {
            continue;
        }
        let compact_id = fact.signal_id.replace('-', "");
        if processed.contains(compact_id.as_str()) {
            continue;
        }
        if normalized_kind(&fact.kind) == "mark-noop" {
            match fact.delegation_id.as_deref() {
                Some(delegation_id) if delegation_id == maker_delegation_id => {
                    classified.mark_noop.push(fact);
                }
                Some(_) => {
                    // A MarkNoop is attempt-local evidence. Historical maker
                    // attempts neither authorize nor block the current review.
                }
                None => classified.blocking.push(fact),
            }
        } else {
            classified.blocking.push(fact);
        }
    }
    for label in labels {
        let Some(rest) = label.strip_prefix("signal:") else {
            continue;
        };
        let kind = rest.split(':').next().unwrap_or_default();
        if kind.is_empty() || fact_kinds.contains(&normalized_kind(kind)) {
            continue;
        }
        classified.blocking.push(SignalFact {
            signal_id: format!("orphan-label:{label}"),
            kind: kind.to_string(),
            reason: format!("signal label {label} has no matching durable signal fact"),
            delegation_id: None,
        });
    }
    Ok(classified)
}

fn reviewer_prompt(
    task: &crate::plan::PlanTaskEntry,
    identity: &ReviewIdentity,
    signals: &ReviewSignals,
) -> String {
    let mark_noop = if signals.mark_noop.is_empty() {
        "none".to_string()
    } else {
        signals
            .mark_noop
            .iter()
            .map(|signal| format!("MarkNoop {}: {}", signal.signal_id, signal.reason))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are the independent reviewer for a system-owned L3 maker result. This is a read-only review: never modify or merge code.\n\nPlan: {}\nTask: {}\nTarget issue: {}\nMaker delegation: {}\nMaker branch: {}\nMaker attempt: {}\n\nAcceptance criteria / task text:\n{}\n\nRelevant non-blocking signals:\n{}\n\nRequired procedure:\n1. Call get_task_diff with {{\"plan_id\":\"{}\",\"task_id\":\"{}\"}}.\n2. Inspect the maker branch diff, acceptance criteria, relevant tests, and signals.\n3. Make exactly one submit_review_verdict call for target_issue_id {} with decision approve or request_changes, non-empty feedback, and concrete evidence.\n4. An empty diff without a justified MarkNoop or reviewable non-code artifact must request changes.\n\nYour branch is throwaway and will never be merged.",
        identity.plan_id,
        identity.task_id,
        identity.target_issue_id,
        identity.maker_delegation_id,
        identity.maker_branch,
        identity.attempt,
        task.spec.task,
        mark_noop,
        identity.plan_id,
        identity.task_id,
        identity.target_issue_id,
    )
}

fn review_dispatch_is_stale(
    companion: &spur_pm::Issue,
    now: SystemTime,
    lease_duration: Duration,
) -> bool {
    let now_ts = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if let Some(expires_at) = companion
        .labels
        .iter()
        .filter_map(|label| crate::plan::labels::parse_lease_expires_at(label))
        .max()
    {
        return expires_at <= now_ts;
    }
    let now: chrono::DateTime<chrono::Utc> = now.into();
    now.signed_duration_since(companion.updated_at)
        .to_std()
        .is_ok_and(|age| age >= lease_duration)
}

async fn review_verdict_exists(
    pm: &dyn crate::plan::PmLike,
    target_issue_id: &str,
    maker_delegation_id: &str,
    reviewer_delegation_id: &str,
    review_issue_id: &str,
) -> anyhow::Result<bool> {
    let comments = pm
        .advanced()
        .ok_or_else(|| anyhow::anyhow!("review verdict lookup requires beads advanced"))?
        .list_comments(target_issue_id)
        .await?;
    let audits =
        crate::plan::projector::collect_sorted_audits_for_issue(target_issue_id, comments)?;
    Ok(audits.iter().any(|audit| {
        matches!(
            audit,
            AuditSentinelKind::SystemReviewVerdict {
                maker_delegation_id: maker,
                reviewer_delegation_id: reviewer,
                review_issue_id: issue,
                ..
            } if maker == maker_delegation_id
                && reviewer == reviewer_delegation_id
                && issue == review_issue_id
        )
    }))
}

async fn close_review_companion(
    pm: &dyn crate::plan::PmLike,
    review_issue_id: &str,
    reviewer_delegation_id: &str,
) -> anyhow::Result<()> {
    crate::plan::clear_dispatch_intent(pm, review_issue_id, reviewer_delegation_id).await?;
    pm.update_issue(
        review_issue_id,
        spur_pm::IssueUpdate {
            status: Some(pm.closed_status().to_string()),
            ..Default::default()
        },
    )
    .await
}

async fn park_review_failure(
    pm: &dyn crate::plan::PmLike,
    advanced: &dyn spur_pm::BeadsAdvanced,
    identity: &ReviewIdentity,
    dispatch: &ReviewDispatchRecord,
) -> anyhow::Result<()> {
    let issue = pm.get_issue(&identity.target_issue_id).await?;
    if issue
        .labels
        .contains(&crate::plan::labels::signal_kind("review-failed"))
    {
        return Ok(());
    }
    let signal_id = uuid::Uuid::new_v4();
    let reason = format!(
        "independent reviewer failed to submit a durable verdict after {MAX_REVIEW_ATTEMPTS} attempts"
    );
    advanced
        .add_comment(
            &identity.target_issue_id,
            &crate::plan::audit_sentinel::encode_comment(&AuditSentinelKind::Signal {
                signal_id: signal_id.to_string(),
                delegation_id: dispatch.reviewer_delegation_id.clone(),
                kind: "review-failed".into(),
                severity: 1.0,
                reason: reason.clone(),
            }),
        )
        .await?;
    advanced
        .add_comment(
            &identity.target_issue_id,
            &format!(
                "{}\n{}",
                crate::plan::signals::SENTINEL_PREFIX,
                serde_json::json!({
                    "kind": "review_failed",
                    "signal_id": signal_id,
                    "severity": 1.0,
                    "reason": reason,
                    "estimated_subtasks": 0
                })
            ),
        )
        .await?;
    pm.update_issue(
        &identity.target_issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![crate::plan::labels::signal_kind("review-failed")],
            ..Default::default()
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_metadata_alone_is_not_independent_review() {
        let completion = AuditSentinelKind::Completion {
            delegation_id: "maker-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/maker-1".into()),
            result_summary: Some("maker says done".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };

        assert_eq!(reviewable_completion(&[completion]), None);
    }

    #[test]
    fn authenticated_verdict_must_still_bind_the_latest_maker_completion() {
        let completion = AuditSentinelKind::Completion {
            delegation_id: "maker-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("spur/worker/maker-1".into()),
            result_summary: Some("maker says done".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
            estimated_cost_micros: None,
        };
        let dispatch = AuditSentinelKind::SystemReviewDispatch {
            plan_id: "P1".into(),
            task_id: "T1".into(),
            attempt: 1,
            maker_delegation_id: "maker-1".into(),
            reviewer_delegation_id: "reviewer-1".into(),
            review_issue_id: "bd-review".into(),
        };
        let verdict = AuditSentinelKind::SystemReviewVerdict {
            maker_delegation_id: "maker-1".into(),
            reviewer_delegation_id: "reviewer-1".into(),
            review_issue_id: "bd-review".into(),
            decision: crate::plan::audit_sentinel::SystemReviewDecision::Approve,
            feedback: "independent review passed".into(),
            evidence: vec!["inspected diff".into()],
        };

        assert_eq!(
            reviewable_completion(&[completion, dispatch, verdict]),
            Some("maker-1")
        );
    }

    #[test]
    fn conflicting_durable_verdicts_fail_closed() {
        let dispatch = ReviewDispatchRecord {
            maker_delegation_id: "maker-1".into(),
            reviewer_delegation_id: "reviewer-1".into(),
            review_issue_id: "bd-review".into(),
        };
        let verdict = |decision, evidence: &str| AuditSentinelKind::SystemReviewVerdict {
            maker_delegation_id: "maker-1".into(),
            reviewer_delegation_id: "reviewer-1".into(),
            review_issue_id: "bd-review".into(),
            decision,
            feedback: "reviewed".into(),
            evidence: vec![evidence.into()],
        };
        let audits = vec![
            verdict(SystemReviewDecision::Approve, "diff-a"),
            verdict(SystemReviewDecision::Approve, "diff-b"),
        ];

        assert!(matching_verdict(&audits, &dispatch).is_none());
    }
}
