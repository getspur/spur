use super::*;

/// Result of building a beads epic subgraph for a persisted plan.
#[derive(Debug, Clone)]
pub struct EpicSubgraph {
    pub epic_id: String,
    /// Maps each `PlanTask.task_id` → beads child issue ID.
    pub task_map: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PlanSubmitAuditContext<'a> {
    pub base_snapshot_branch: Option<&'a str>,
    pub base_snapshot_oid: Option<&'a str>,
    pub execution_mode: Option<&'a str>,
    pub brain_session_id: Option<&'a SessionId>,
    pub explicit_base: Option<&'a crate::tools::BaseTarget>,
}

/// Compose a beads epic + child issues + dependency edges from a
/// validated plan. Labels each child with `spur:plan-id:<plan_id>` so
/// review_task can correlate approvals back to beads.
///
/// Creates issues in topological order (deps-first) so each child's
/// `depends_on` references beads IDs that already exist. Callers must
/// ensure the plan is validated (no cycles) before invoking.
///
/// On failure mid-creation: partial state lands in beads (epic +
/// whatever children succeeded), but the epic keeps `spur:plan-pending`
/// and never gains `spur:plan-complete`, so the reconciler will not
/// dispatch the partial graph. Startup sweep quarantines stale pending
/// graphs after the configured grace period.
pub async fn build_epic_subgraph(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<EpicSubgraph, String> {
    build_epic_subgraph_with_activation_labels(
        pm,
        feature_gate,
        plan_id,
        epic_title,
        epic_body,
        tasks,
        None,
        Vec::new(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn build_epic_subgraph_with_activation_labels(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
    parent: Option<&str>,
    activation_add_labels: Vec<String>,
) -> Result<EpicSubgraph, String> {
    let (mut epic_create, child_specs) =
        plan_epic_issue_creates(plan_id, epic_title, epic_body, tasks)?;
    epic_create.parent = parent.map(String::from);
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let advanced = pm.advanced();

    let epic_id = pm
        .create_issue(epic_create)
        .await
        .map_err(|e| format!("failed to create beads epic: {e}"))?;

    let mut task_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (task_id, mut child_create) in child_specs {
        // Rewrite `depends_on` from task_id keys → created beads IDs.
        child_create.depends_on = child_create
            .depends_on
            .iter()
            .map(|dep_key| {
                task_map.get(dep_key).cloned().ok_or_else(|| {
                    format!("task '{task_id}' depends on '{dep_key}' which was not yet created",)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        child_create.parent = Some(epic_id.clone());

        let child_id = pm
            .create_issue(child_create)
            .await
            .map_err(|e| format!("failed to create child for task '{task_id}': {e}"))?;
        let task = tasks
            .iter()
            .find(|task| task.task_id == task_id)
            .ok_or_else(|| format!("task spec for '{task_id}' disappeared during persistence"))?;
        if !task.context_files.is_empty() {
            let adv = advanced.ok_or_else(|| {
                format!(
                    "failed to persist child task spec for task '{task_id}': beads backend missing"
                )
            })?;
            crate::plan::emit_task_spec_audit(adv, &child_id, &task.task_id, &task.context_files)
                .await
                .map_err(|e| {
                    format!("failed to persist child task spec for task '{task_id}': {e}")
                })?;
        }
        task_map.insert(task_id, child_id);
    }

    let mut add_labels = activation_add_labels;
    add_labels.push(crate::plan::labels::PLAN_COMPLETE.to_string());
    pm.update_issue(
        &epic_id,
        spur_pm::types::IssueUpdate {
            add_labels,
            remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
            ..Default::default()
        },
    )
    .await
    .map_err(|e| {
        format!(
            "failed to activate beads epic '{epic_id}' (add {} / remove {}): {e}",
            crate::plan::labels::PLAN_COMPLETE,
            crate::plan::labels::PLAN_PENDING
        )
    })?;

    Ok(EpicSubgraph { epic_id, task_map })
}

/// Emit a `[[spur-audit v1]]` `PlanSubmit` sentinel comment on the epic issue.
///
/// Advisory: failure is logged via `tracing::warn!` and swallowed. Does not
/// abort the caller. See docs/superpowers/plans/2026-04-20-adaptive-plan-
/// repair-v0a.md "Review addendum II" for why comments are the audit
/// transport.
pub async fn emit_plan_submit_audit(
    advanced: &dyn spur_pm::BeadsAdvanced,
    plan_id: &str,
    sg: &EpicSubgraph,
    context: PlanSubmitAuditContext<'_>,
) {
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
        plan_id: plan_id.to_string(),
        epic_issue_id: sg.epic_id.clone(),
        task_ids: sg.task_map.values().cloned().collect(),
        base_snapshot_branch: context.base_snapshot_branch.map(str::to_string),
        base_snapshot_oid: context.base_snapshot_oid.map(str::to_string),
        execution_mode: context.execution_mode.map(str::to_string),
        brain_session_id: context.brain_session_id.map(ToString::to_string),
        explicit_base: context.explicit_base.cloned(),
    };
    let body = crate::plan::audit_sentinel::encode_comment(&kind);
    if let Err(e) = advanced.add_comment(&sg.epic_id, &body).await {
        tracing::warn!(
            target: "spur.audit.emit_failure",
            kind = "plan_submit",
            epic_id = %sg.epic_id,
            plan_id = %plan_id,
            "PlanSubmit audit comment emission failed (graph is persisted; audit missing): {e}"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedPlanBootstrap {
    #[allow(dead_code)]
    pub(crate) epic_id: String,
    pub(crate) base_snapshot_branch: Option<String>,
    pub(crate) base_snapshot_oid: Option<String>,
}

impl PersistedPlanBootstrap {
    pub(crate) fn preferred_base_ref(&self) -> Option<&str> {
        self.base_snapshot_oid
            .as_deref()
            .or(self.base_snapshot_branch.as_deref())
    }
}

pub(crate) async fn read_persisted_plan_bootstrap(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    plan_id: &str,
    epic_id: &str,
) -> Result<PersistedPlanBootstrap, String> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted bootstrap recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(epic_id)
        .await
        .map_err(|e| format!("failed to load comments for epic '{epic_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(epic_id, comments);

    audits
        .into_iter()
        .rev()
        .find_map(|audit| match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id: audit_plan_id,
                base_snapshot_branch,
                base_snapshot_oid,
                ..
            } if audit_plan_id == plan_id => Some(PersistedPlanBootstrap {
                epic_id: epic_id.to_string(),
                base_snapshot_branch,
                base_snapshot_oid,
            }),
            _ => None,
        })
        .ok_or_else(|| format!("plan '{plan_id}' has no PlanSubmit audit on epic '{epic_id}'"))
}

#[derive(Debug, Clone, PartialEq, Eq)]

pub(crate) struct PersistedTaskCompletion {
    pub(crate) worker_branch: Option<String>,
    pub(crate) summary: Option<String>,
}

pub(crate) async fn read_latest_task_completion(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
) -> Result<Option<PersistedTaskCompletion>, String> {
    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted task completion recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(issue_id)
        .await
        .map_err(|e| format!("failed to load comments for task '{issue_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(issue_id, comments);

    Ok(audits.into_iter().rev().find_map(|audit| match audit {
        crate::plan::audit_sentinel::AuditSentinelKind::Completion {
            worker_branch,
            result_summary,
            ..
        } => Some(PersistedTaskCompletion {
            worker_branch,
            summary: result_summary,
        }),
        _ => None,
    }))
}

pub(crate) async fn reconstruct_historical_attempts(
    pm: &dyn crate::plan::PmLike,
    feature_gate: &spur_license::FeatureGate,
    issue_id: &str,
    current_attempt: u32,
) -> Result<Vec<crate::plan::AttemptRecord>, String> {
    #[derive(Debug, Default)]
    struct AttemptAccumulator {
        attempt: u32,
        worker_branch: Option<String>,
        summary: Option<String>,
        feedback: String,
        reuse_prior_worktree: Option<bool>,
    }

    require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED, feature_gate)
        .map_err(feature_error_message)?;
    let adv = pm
        .advanced()
        .ok_or_else(|| "persisted attempt recovery requires beads backend".to_string())?;
    let comments = adv
        .list_comments(issue_id)
        .await
        .map_err(|e| format!("failed to load comments for task '{issue_id}': {e}"))?;
    let audits = crate::plan::projector::collect_sorted_audits_for_issue(issue_id, comments);

    let mut attempts_by_delegation: std::collections::HashMap<String, AttemptAccumulator> =
        std::collections::HashMap::new();
    for audit in audits {
        match audit {
            crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                delegation_id,
                attempt,
                ..
            } if attempt < current_attempt => {
                attempts_by_delegation
                    .entry(delegation_id)
                    .or_insert_with(|| AttemptAccumulator {
                        attempt,
                        ..Default::default()
                    });
            }
            crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                delegation_id,
                worker_branch,
                result_summary,
                ..
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.worker_branch = worker_branch;
                    record.summary = result_summary;
                }
            }
            crate::plan::audit_sentinel::AuditSentinelKind::Rejection {
                delegation_id,
                feedback,
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.feedback = feedback;
                }
            }
            // bd-33it: request_changes feedback also populates the historical
            // attempt record. Joined by delegation_id so the get_task_diff
            // operator-visible historical view sees the same feedback the
            // reconciler used to enrich the worker prompt on retry.
            crate::plan::audit_sentinel::AuditSentinelKind::ReviewFeedback {
                delegation_id,
                feedback,
                worker_branch,
                summary,
                reuse_prior_worktree,
                ..
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.feedback = feedback;
                    if record.worker_branch.is_none() {
                        record.worker_branch = worker_branch;
                    }
                    if record.summary.is_none() {
                        record.summary = summary;
                    }
                    record.reuse_prior_worktree = reuse_prior_worktree;
                }
            }
            crate::plan::audit_sentinel::AuditSentinelKind::RetryRequested {
                delegation_id,
                error,
                worker_branch,
                ..
            } => {
                if let Some(record) = attempts_by_delegation.get_mut(&delegation_id) {
                    record.feedback = crate::plan::worker_failure_recovery_feedback(&error);
                    if record.worker_branch.is_none() {
                        record.worker_branch = worker_branch;
                    }
                }
            }
            _ => {}
        }
    }

    let mut history: Vec<crate::plan::AttemptRecord> = attempts_by_delegation
        .into_values()
        .map(|record| crate::plan::AttemptRecord {
            attempt: record.attempt,
            worker_branch: record.worker_branch,
            diff_summary: None,
            summary: record.summary,
            feedback: record.feedback,
            dispatched_base_oid: None,
            reuse_prior_worktree: record.reuse_prior_worktree,
        })
        .collect();
    history.sort_by_key(|record| record.attempt);
    Ok(history)
}

/// bd-2m2u Phase 2d — best-effort lookup of the `plan_id` carried as a
/// `spur:plan-id:*` label on the trigger task issue, used by
/// `handle_submit_plan_mutation` to populate the `PlanMutationApplied` event.
/// Returns `None` if the issue cannot be fetched or carries no plan id label.
pub(crate) async fn derive_plan_id_from_trigger_issue(
    pm: &dyn crate::plan::PmLike,
    issue_id: &str,
) -> Option<String> {
    let issue = pm.get_issue(issue_id).await.ok()?;
    issue
        .labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_id(label).map(str::to_string))
}

/// Pure helper: compute the IssueCreate values that build_epic_subgraph
/// would dispatch to PmService. Returns the epic's IssueCreate plus a
/// Vec of (task_id, IssueCreate) for each child in topological order.
/// Child IssueCreate.depends_on carries task_id keys, NOT beads IDs —
/// the caller rewrites them as children are created.
pub fn plan_epic_issue_creates(
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<
    (
        spur_pm::types::IssueCreate,
        Vec<(String, spur_pm::types::IssueCreate)>,
    ),
    String,
> {
    let epic_create = spur_pm::types::IssueCreate {
        title: epic_title.to_string(),
        description: epic_body.map(String::from),
        issue_type: Some("epic".to_string()),
        labels: vec![
            crate::plan::labels::plan_id(plan_id),
            crate::plan::labels::PLAN_PENDING.to_string(),
        ],
        ..Default::default()
    };

    let order = topological_order(tasks)?;
    let mut child_specs = Vec::with_capacity(tasks.len());
    for idx in order {
        let task = &tasks[idx];
        let mut labels = vec![
            crate::plan::labels::plan_id(plan_id),
            crate::plan::labels::plan_task_id(&task.task_id),
            crate::plan::labels::agent(&task.agent),
        ];
        if let Some(existing) = &task.issue_id {
            labels.push(crate::plan::labels::source_issue(existing));
        }
        let child_create = spur_pm::types::IssueCreate {
            title: format!("{}: {}", task.task_id, truncate_for_title(&task.task)),
            description: Some(task.task.clone()),
            issue_type: Some("task".to_string()),
            labels,
            // depends_on carries task_id keys; rewritten by build_epic_subgraph.
            depends_on: task.depends_on.clone(),
            // parent set by build_epic_subgraph once epic_id is known.
            parent: None,
            ..Default::default()
        };
        child_specs.push((task.task_id.clone(), child_create));
    }
    Ok((epic_create, child_specs))
}

/// Build `PlanTaskEntry` values from a list of `PlanTask`s, optionally
/// backfilling `spec.issue_id` from a `task_map` produced by
/// `build_epic_subgraph`.
///
/// Backfill rule: a task's `issue_id` is set to the task_map value ONLY when
/// the field is currently `None`. Pre-existing values are NOT overwritten —
/// they represent a `spur:source-issue:` reference pointing to a pre-existing
/// issue and must be preserved so downstream audit logic can distinguish the
/// source issue from the newly-created beads child.
///
/// Ephemeral plans pass `task_map = None`; every entry keeps `issue_id: None`.
pub fn build_entries_with_task_map(
    tasks: Vec<crate::plan::PlanTask>,
    task_map: Option<&std::collections::HashMap<String, String>>,
) -> Vec<crate::plan::PlanTaskEntry> {
    tasks
        .into_iter()
        .map(|mut spec| {
            if spec.issue_id.is_none() {
                if let Some(map) = task_map {
                    if let Some(beads_id) = map.get(&spec.task_id) {
                        spec.issue_id = Some(beads_id.clone());
                    }
                }
            }
            crate::plan::PlanTaskEntry {
                spec,
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }
        })
        .collect()
}

/// Truncate a task description to a reasonable issue-title length.
/// Beads has no hard limit but overly long titles are unwieldy in UIs.
pub(crate) fn truncate_for_title(s: &str) -> String {
    const MAX_TITLE_LEN: usize = 80;
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= MAX_TITLE_LEN {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(MAX_TITLE_LEN - 3).collect();
        format!("{truncated}...")
    }
}

/// Return task indices in a valid topological order. Callers must have
/// already validated that the plan is acyclic via `plan::validate_plan`.
pub(crate) fn topological_order(tasks: &[crate::plan::PlanTask]) -> Result<Vec<usize>, String> {
    use std::collections::HashMap;
    let key_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();

    let mut in_degree: Vec<usize> = tasks.iter().map(|t| t.depends_on.len()).collect();
    let mut ready: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| if d == 0 { Some(i) } else { None })
        .collect();

    let mut out = Vec::with_capacity(tasks.len());
    while let Some(i) = ready.pop_front() {
        out.push(i);
        for (j, t) in tasks.iter().enumerate() {
            if t.depends_on
                .iter()
                .any(|dep| key_to_idx.get(dep.as_str()).copied() == Some(i))
            {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    if out.len() != tasks.len() {
        return Err(format!(
            "topological order incomplete: {} of {} tasks reachable (cycle?)",
            out.len(),
            tasks.len()
        ));
    }
    Ok(out)
}

pub(crate) async fn resolve_plan_base(
    repo_root: Option<&std::path::PathBuf>,
    base_target: Option<&crate::tools::BaseTarget>,
) -> Result<PlanBaseSnapshot, String> {
    let Some(root) = repo_root.cloned() else {
        return Ok(PlanBaseSnapshot::default());
    };
    let manager = WorktreeManager::new(root);

    let branch = match base_target {
        // Legacy / explicit RepoMain: snapshot the brain working tree.
        None | Some(crate::tools::BaseTarget::RepoMain) => manager
            .snapshot_brain_state()
            .await
            .map_err(|e| format!("failed to snapshot plan base: {e}"))?,
        // Explicit branch: resolve the ref and create a snapshot ref pointed
        // at the same OID. Brain working tree is never touched.
        Some(crate::tools::BaseTarget::Branch { name }) => manager
            .snapshot_at_ref(name)
            .await
            .map_err(|e| format!("failed to resolve plan base branch '{name}': {e}"))?,
        Some(crate::tools::BaseTarget::Commit { oid }) => manager
            .snapshot_at_ref(oid)
            .await
            .map_err(|e| format!("failed to resolve plan base commit '{oid}': {e}"))?,
    };

    let oid = Some(
        run_git_capture(
            &manager.repo_root,
            None,
            &["rev-parse", "--verify", branch.as_str()],
        )
        .await?,
    );
    Ok(PlanBaseSnapshot {
        branch: Some(branch),
        oid,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PlanBaseSnapshot {
    pub(crate) branch: Option<String>,
    pub(crate) oid: Option<String>,
}

#[derive(Debug)]
pub(crate) struct SubmitPlanAsEpicInput {
    pub(crate) tasks: Vec<crate::plan::PlanTask>,
    pub(crate) base: Option<crate::tools::BaseTarget>,
    pub(crate) parent_epic_id: Option<String>,
    pub(crate) epic_title: Option<String>,
    pub(crate) epic_body: Option<String>,
    pub(crate) brain_session_id: BrainSessionId,
    pub(crate) execution_mode: &'static str,
    pub(crate) precomputed_auto_serialized: Option<Vec<crate::plan::SiblingOverlap>>,
}

#[derive(Debug)]
pub(crate) struct SubmitPlanAsEpicResult {
    pub(crate) plan_id: String,
    pub(crate) task_count: usize,
    pub(crate) auto_serialized: Vec<crate::plan::SiblingOverlap>,
    pub(crate) epic_subgraph: EpicSubgraph,
}

#[cfg(test)]
mod resolve_plan_base_tests {
    use super::*;
    use crate::tools::BaseTarget;
    use std::process::Command;
    use tempfile::TempDir;

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
    }

    fn capture(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn seed_repo(repo: &std::path::Path) {
        run_git(repo, &["init", "-q", "-b", "main"]);
        run_git(repo, &["config", "user.email", "t@t"]);
        run_git(repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a"), "1").unwrap();
        run_git(repo, &["add", "a"]);
        run_git(repo, &["commit", "-q", "-m", "seed"]);
    }

    #[tokio::test]
    async fn resolve_plan_base_none_falls_back_to_brain_snapshot() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        let root = dir.path().to_path_buf();

        let snap = resolve_plan_base(Some(&root), None).await.unwrap();
        assert!(snap
            .branch
            .as_deref()
            .unwrap()
            .starts_with("spur/brain-snapshot-"));
        assert_eq!(snap.oid.as_deref(), Some(head_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_branch_target_skips_stash_and_uses_named_branch() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        run_git(dir.path(), &["checkout", "-q", "-b", "phase0"]);
        std::fs::write(dir.path().join("b"), "2").unwrap();
        run_git(dir.path(), &["add", "b"]);
        run_git(dir.path(), &["commit", "-q", "-m", "phase0 work"]);
        let phase0_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        run_git(dir.path(), &["checkout", "-q", "main"]);

        // Dirty the WT — must not affect snapshot.
        std::fs::write(dir.path().join("a"), "dirty").unwrap();

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch {
            name: "phase0".into(),
        };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();

        assert_eq!(snap.oid.as_deref(), Some(phase0_oid.as_str()));
        let a_contents = std::fs::read_to_string(dir.path().join("a")).unwrap();
        assert_eq!(a_contents, "dirty", "WT must be untouched");
    }

    #[tokio::test]
    async fn resolve_plan_base_commit_target_uses_oid() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let seed_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        std::fs::write(dir.path().join("a"), "2").unwrap();
        run_git(dir.path(), &["add", "a"]);
        run_git(dir.path(), &["commit", "-q", "-m", "second"]);
        let head_oid = capture(dir.path(), &["rev-parse", "HEAD"]);
        assert_ne!(seed_oid, head_oid);

        let root = dir.path().to_path_buf();
        let target = BaseTarget::Commit {
            oid: seed_oid.clone(),
        };
        let snap = resolve_plan_base(Some(&root), Some(&target)).await.unwrap();
        assert_eq!(snap.oid.as_deref(), Some(seed_oid.as_str()));
    }

    #[tokio::test]
    async fn resolve_plan_base_unknown_branch_fails_loudly() {
        let dir = TempDir::new().unwrap();
        seed_repo(dir.path());
        let root = dir.path().to_path_buf();
        let target = BaseTarget::Branch {
            name: "does-not-exist".into(),
        };
        let err = resolve_plan_base(Some(&root), Some(&target))
            .await
            .unwrap_err();
        assert!(
            err.contains("does-not-exist"),
            "error must mention the bad ref; got: {err}"
        );
    }
}

pub(crate) async fn run_git_capture(
    repo_root: &std::path::Path,
    cwd: Option<&std::path::Path>,
    args: &[&str],
) -> Result<String, String> {
    let work_dir = cwd.unwrap_or(repo_root);
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .output()
        .await
        .map_err(|e| format!("failed to execute git {}: {e}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub(crate) async fn resolve_worker_branch_tip_oid(
    repo_root: &std::path::Path,
    worker_branch: &str,
) -> Result<String, String> {
    let local_ref = format!("refs/heads/{worker_branch}");
    let remote_origin_ref = format!("refs/remotes/origin/{worker_branch}");
    let mut candidates = vec![local_ref, remote_origin_ref];
    if worker_branch.starts_with("refs/remotes/") || worker_branch.starts_with("origin/") {
        candidates.push(worker_branch.to_string());
    }

    let mut errors = Vec::new();
    for candidate in candidates {
        match run_git_capture(repo_root, None, &["rev-parse", "--verify", &candidate]).await {
            Ok(oid) => return Ok(oid),
            Err(error) => errors.push(format!("{candidate}: {error}")),
        }
    }

    Err(format!(
        "worker branch '{worker_branch}' not found locally or under refs/remotes/origin ({})",
        errors.join("; ")
    ))
}

pub(crate) async fn count_commits_between(
    repo_root: &std::path::Path,
    base_oid: &str,
    tip_oid: &str,
) -> Result<usize, String> {
    if run_git_capture(
        repo_root,
        None,
        &["merge-base", "--is-ancestor", base_oid, tip_oid],
    )
    .await
    .is_err()
    {
        return Err(
            "base_oid is not an ancestor of the worker branch tip — branch may have diverged or been rebased"
                .to_string(),
        );
    }

    let range = format!("{base_oid}..{tip_oid}");
    let count = run_git_capture(
        repo_root,
        None,
        &["rev-list", "--count", "--no-merges", &range],
    )
    .await?;
    count.parse::<usize>().map_err(|error| {
        format!("git rev-list --count --no-merges returned non-integer '{count}': {error}")
    })
}

pub(crate) async fn diff_text_from_branches(
    repo_root: &std::path::Path,
    base_ref: &str,
    worker_branch: &str,
) -> Result<String, String> {
    let range = format!("{base_ref}..{worker_branch}");
    run_git_capture(repo_root, None, &["diff", range.as_str()]).await
}

pub(crate) async fn integrate_plan_branches(
    repo_root: &std::path::Path,
    base_ref: &str,
    merge_branch: &str,
    ordered_branches: &[(String, String)],
) -> Result<crate::plan::PlanMergeState, String> {
    let integration_root = repo_root
        .join(".spur/merge")
        .join(uuid::Uuid::new_v4().to_string());
    if let Some(parent) = integration_root.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create integration worktree parent: {e}"))?;
    }
    let integration_root_str = integration_root
        .to_str()
        .ok_or_else(|| "integration worktree path is not valid UTF-8".to_string())?;

    run_git_capture(
        repo_root,
        None,
        &[
            "worktree",
            "add",
            integration_root_str,
            "-b",
            merge_branch,
            base_ref,
        ],
    )
    .await?;

    let mut merged_task_ids = Vec::with_capacity(ordered_branches.len());
    for (task_id, worker_branch) in ordered_branches {
        if let Err(err) = run_git_capture(
            repo_root,
            Some(&integration_root),
            &["cherry-pick", worker_branch.as_str()],
        )
        .await
        {
            let conflict_output = run_git_capture(
                repo_root,
                Some(&integration_root),
                &["diff", "--name-only", "--diff-filter=U"],
            )
            .await
            .unwrap_or_default();
            let files: Vec<String> = conflict_output
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            let _ = run_git_capture(
                repo_root,
                Some(&integration_root),
                &["cherry-pick", "--abort"],
            )
            .await;
            let _ = run_git_capture(
                repo_root,
                None,
                &["worktree", "remove", integration_root_str, "--force"],
            )
            .await;
            info!(
                merge_branch = %merge_branch,
                conflict_task_id = %task_id,
                conflict_worker_branch = %worker_branch,
                error = %err,
                "merge_plan detected cherry-pick conflict"
            );
            return Ok(crate::plan::PlanMergeState::Conflict {
                merge_branch: merge_branch.to_string(),
                conflict_task_id: task_id.clone(),
                conflict_worker_branch: worker_branch.clone(),
                merged_task_ids,
                files,
            });
        }
        merged_task_ids.push(task_id.clone());
    }

    run_git_capture(
        repo_root,
        None,
        &["worktree", "remove", integration_root_str, "--force"],
    )
    .await?;

    Ok(crate::plan::PlanMergeState::Succeeded {
        merge_branch: merge_branch.to_string(),
        merged_task_ids,
    })
}

#[cfg(test)]
mod topo_tests {
    use super::topological_order;
    use crate::plan::PlanTask;

    fn t(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.to_string(),
            agent: "x".to_string(),
            task: "body".to_string(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }
    }

    #[test]
    fn linear_chain_is_ordered() {
        let tasks = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        let order = topological_order(&tasks).unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn diamond_respects_all_parents() {
        // a → b, a → c, b+c → d
        let tasks = vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["a"]),
            t("d", &["b", "c"]),
        ];
        let order = topological_order(&tasks).unwrap();
        let pos_a = order.iter().position(|&i| i == 0).unwrap();
        let pos_b = order.iter().position(|&i| i == 1).unwrap();
        let pos_c = order.iter().position(|&i| i == 2).unwrap();
        let pos_d = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_a < pos_b && pos_a < pos_c);
        assert!(pos_b < pos_d && pos_c < pos_d);
    }

    #[test]
    fn cycle_is_detected() {
        let tasks = vec![t("a", &["b"]), t("b", &["a"])];
        let err = topological_order(&tasks).unwrap_err();
        assert!(err.contains("incomplete") || err.contains("cycle"));
    }
}
