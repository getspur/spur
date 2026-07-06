use super::McpCallbackServer;
use super::*;

const RATCHET_MIN_STABLE_GENERATIONS: u32 = 3;

fn unix_now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn loop_issue_title(goal: &str) -> String {
    let trimmed = goal.trim();
    let title: String = trimmed.chars().take(80).collect();
    if title.is_empty() {
        "Loop".to_string()
    } else {
        format!("Loop: {title}")
    }
}

fn submit_loop_success_response(
    id: Value,
    loop_id: &str,
    issue_id: &str,
    next_run: i64,
    paused: bool,
) -> JsonRpcResponse {
    let output = json!({
        "loop_id": loop_id,
        "issue_id": issue_id,
        "next_run": next_run,
        "paused": paused,
    });
    let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    JsonRpcResponse::success(
        id,
        json!({
            "loop_id": output["loop_id"],
            "issue_id": output["issue_id"],
            "content": [{ "type": "text", "text": text }]
        }),
    )
}

fn spur_loop_doctor_response(
    id: Value,
    output: crate::tool_schemas::SpurLoopDoctorOutput,
) -> JsonRpcResponse {
    let output = serde_json::to_value(output).expect("SpurLoopDoctorOutput serializes");
    let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
    let mut result = output
        .as_object()
        .cloned()
        .expect("SpurLoopDoctorOutput serializes as a JSON object");
    result.insert(
        "content".to_string(),
        json!([{ "type": "text", "text": text }]),
    );
    JsonRpcResponse::success(id, Value::Object(result))
}

fn autonomy_label(level: crate::plan::labels::AutonomyLevel) -> String {
    format!("{}{}", crate::plan::labels::AUTONOMY_PREFIX, level.as_str())
}

fn parse_autonomy_level_param(level: &str) -> Result<crate::plan::labels::AutonomyLevel, String> {
    crate::plan::labels::AutonomyLevel::parse(level)
        .ok_or_else(|| "level must be one of l1, l2, or l3".to_string())
}

fn is_real_generation_outcome(outcome: &str) -> bool {
    matches!(outcome, "approved" | "partial" | "failed")
}

fn stable_approved_generations_at_level(
    audits: &[crate::plan::audit_sentinel::AuditSentinelKind],
    loop_id: &str,
    current_level: crate::plan::labels::AutonomyLevel,
) -> u32 {
    let current_level = current_level.as_str();
    let mut stable = 0u32;
    for audit in audits.iter().rev() {
        let crate::plan::audit_sentinel::AuditSentinelKind::LoopRun {
            loop_id: record_loop_id,
            autonomy,
            outcome,
            ..
        } = audit
        else {
            continue;
        };
        if record_loop_id != loop_id || !is_real_generation_outcome(outcome) {
            continue;
        }
        if outcome == "approved" && autonomy.as_deref() == Some(current_level) {
            stable = stable.saturating_add(1);
            continue;
        }
        break;
    }
    stable
}

async fn load_persisted_plan_task_map(
    pm: &dyn crate::plan::PmLike,
    plan_id: &str,
    task_ids: &[String],
) -> Result<std::collections::HashMap<String, String>, String> {
    let expected = task_ids
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![crate::plan::labels::plan_id(plan_id)],
            ..Default::default()
        })
        .await
        .map_err(|error| {
            format!("submit_plan: failed to list persisted plan issues for {plan_id}: {error}")
        })?;

    let mut task_map = std::collections::HashMap::new();
    for summary in summaries {
        let Some(task_id) = summary
            .labels
            .iter()
            .find_map(|label| crate::plan::labels::parse_plan_task_id(label))
        else {
            continue;
        };
        if expected.contains(&task_id) {
            task_map.insert(task_id, summary.id);
        }
    }

    for task_id in task_ids {
        if !task_map.contains_key(task_id) {
            return Err(format!(
                "submit_plan: persisted task map missing child for task '{task_id}' in plan {plan_id}"
            ));
        }
    }

    Ok(task_map)
}

fn validate_loop_id_param(loop_id: &str) -> Result<(), String> {
    if loop_id.is_empty()
        || !loop_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return Err(
            "loop_id must be non-empty and contain only ASCII alphanumeric, dash, underscore, or colon characters"
                .to_string(),
        );
    }
    Ok(())
}

async fn load_loop_issue(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
) -> Result<Option<spur_pm::Issue>, String> {
    load_loop_issue_with_closed(pm, loop_id, false).await
}

async fn load_loop_issue_including_closed(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
) -> Result<Option<spur_pm::Issue>, String> {
    load_loop_issue_with_closed(pm, loop_id, true).await
}

async fn load_loop_issue_with_closed(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
    include_closed: bool,
) -> Result<Option<spur_pm::Issue>, String> {
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![crate::plan::labels::loop_id_label(loop_id)],
            issue_type: Some(crate::plan::loops::LOOP_ISSUE_TYPE.to_string()),
            include_closed,
            limit: Some(2),
            ..Default::default()
        })
        .await
        .map_err(|error| error.to_string())?;
    let Some(summary) = summaries.first() else {
        return Ok(None);
    };
    pm.get_issue(&summary.id)
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

async fn next_loop_retirement_generation(
    pm: &dyn crate::plan::PmLike,
    loop_id: &str,
) -> Result<u32, String> {
    let summaries = pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![crate::plan::labels::loop_id_label(loop_id)],
            issue_type: Some("epic".to_string()),
            include_closed: true,
            limit: Some(10_000),
            ..Default::default()
        })
        .await
        .map_err(|error| error.to_string())?;
    let max_seen = summaries
        .iter()
        .filter_map(|summary| {
            summary
                .labels
                .iter()
                .find_map(|label| crate::plan::labels::parse_loop_generation(label))
        })
        .max()
        .unwrap_or(0);
    Ok(max_seen.saturating_add(1))
}

impl McpCallbackServer {
    pub(crate) async fn handle_merge_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'plan_id'"),
        };

        if let Err((code, message)) = self.check_plan_owner_for_op(&plan_id, "merge_plan").await {
            return JsonRpcResponse::error(id, code, message);
        }

        match self.merge_plan_impl(&plan_id).await {
            Ok(merge_state) => {
                let plan_arc = match self.load_or_project_plan(&plan_id).await {
                    Ok(p) => p,
                    Err(e) => return JsonRpcResponse::invalid_params(id, e),
                };
                {
                    let mut state = plan_arc.lock().await;
                    state.merge_state = merge_state;
                }
                let state = plan_arc.lock().await;
                let status = crate::plan::build_plan_status(&plan_id, &state);
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not fully approved yet") || msg.contains("Unknown plan_id") {
                    JsonRpcResponse::invalid_params(id, msg)
                } else {
                    JsonRpcResponse::internal_error(id, msg)
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: invoke `resume_plan` and reduce the
    /// JSON-RPC response to a simple Result. Error message is verbatim from the
    /// MCP tool's `JsonRpcError.message`.
    pub async fn call_resume_plan(&self, plan_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "plan_id": plan_id });
        let resp = self
            .handle_resume_plan_with(serde_json::Value::Null, args, false)
            .await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    pub async fn call_pause_loop(&self, loop_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "loop_id": loop_id });
        let resp = self.handle_pause_loop(serde_json::Value::Null, args).await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    pub async fn call_resume_loop(&self, loop_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "loop_id": loop_id });
        let resp = self.handle_resume_loop(serde_json::Value::Null, args).await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    pub async fn call_kill_loop(&self, loop_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "loop_id": loop_id });
        let resp = self.handle_kill_loop(serde_json::Value::Null, args).await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    /// Public bridge for orchestrator/TUI: retry one persisted plan task via
    /// the same `submit_plan_mutation` path exposed to brain tools.
    pub async fn call_retry_plan_task(
        &self,
        plan_id: Option<&str>,
        issue_id: &str,
    ) -> Result<(), String> {
        let mut args = serde_json::json!({
            "trigger_task_id": issue_id,
            "ops": [{
                "op": "retry_task",
                "issue_id": issue_id,
            }],
            "rationale": "Manual retry requested from Plan Inspector",
        });
        if let Some(plan_id) = plan_id {
            args["plan_id"] = serde_json::Value::String(plan_id.to_string());
        }

        let resp = self
            .handle_submit_plan_mutation(serde_json::Value::Null, args)
            .await;
        match resp.error {
            Some(e) => Err(e.message),
            None => Ok(()),
        }
    }

    /// Public bridge for orchestrator/TUI: claim ownership for a persisted plan
    /// without starting dispatch. The pending gate keeps the reconciler from
    /// observing ready work until `call_resume_plan` explicitly starts it.
    pub async fn call_claim_plan(&self, plan_id: &str) -> Result<(), String> {
        let pm = self
            .pm_service
            .as_deref()
            .ok_or_else(|| "claim_plan requires PM service".to_string())?;

        let epic_summary =
            find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "claim_plan").await?;
        let epic_id = epic_summary.id.clone();
        let epic = pm
            .get_issue(&epic_id)
            .await
            .map_err(|error| format!("claim_plan: failed to load epic {epic_id}: {error}"))?;

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                if let Some(active) = self
                    .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                    .await?
                {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); cannot claim different active plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }
                Ok(())
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => Err(format!(
                "claim_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
            )),
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => Err(format!(
                "claim_plan: plan {plan_id} has ambiguous owner labels: {}",
                owners.join(", ")
            )),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                if let Some(active) = self.current_brain_active_owned_plan(pm, None, None).await? {
                    return Err(format!(
                        "claim_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                        active.plan_id, active.epic_id
                    ));
                }

                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![
                            owner_label.clone(),
                            crate::plan::labels::PLAN_PENDING.to_string(),
                        ],
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| format!("claim_plan: failed to claim plan: {error}"))?;

                let epic = pm.get_issue(&epic_id).await.map_err(|error| {
                    format!("claim_plan: failed to reload claimed epic {epic_id}: {error}")
                })?;
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => Ok(()),
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        let _ = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await;
                        Err(format!(
                            "claim_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                            owners.join(", ")
                        ))
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => Err(format!(
                        "claim_plan: failed to claim plan {plan_id}; plan remains unowned"
                    )),
                }
            }
        }
    }

    /// Public bridge for orchestrator/TUI: project a persisted plan and emit a
    /// `PlanSnapshotUpdated` without claiming ownership or changing plan state.
    pub async fn call_inspect_plan(&self, plan_id: &str) -> Result<(), String> {
        let plan = self.load_or_project_plan(plan_id).await?;
        let state = plan.lock().await;
        crate::plan::snapshot::emit_plan_snapshot(self.event_sink.as_deref(), &state);
        Ok(())
    }

    pub(crate) async fn handle_resume_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_resume_plan_with(id, args, true).await
    }

    pub(crate) async fn handle_resume_plan_with(
        &self,
        id: Value,
        args: Value,
        allow_claim_unowned: bool,
    ) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|value| value.as_str()) {
            Some(plan_id) => plan_id,
            None => return JsonRpcResponse::invalid_params(id, "resume_plan: missing plan_id"),
        };
        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => return JsonRpcResponse::internal_error(id, "resume_plan requires PM service"),
        };

        let epic_summary =
            match find_plan_epic(pm, self.feature_gate.as_ref(), plan_id, "resume_plan").await {
                Ok(epic) => epic,
                Err(error) => {
                    return if error.contains("plan not found") {
                        JsonRpcResponse::error(id, -32004, error)
                    } else if error.contains("ambiguous plan lookup") {
                        JsonRpcResponse::error(id, -32009, error)
                    } else {
                        JsonRpcResponse::internal_error(id, error)
                    }
                }
            };
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("resume_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        match crate::plan::ownership::classify_owner(
            &epic.labels,
            self.brain_session_id().as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                match self.is_projected_plan_nonterminal(plan_id).await {
                    Ok(true) => {
                        match self
                            .current_brain_active_owned_plan(pm, Some(plan_id), Some(&epic_id))
                            .await
                        {
                            Ok(Some(active)) => {
                                return JsonRpcResponse::error(
                                    id,
                                    -32009,
                                    format!(
                                        "resume_plan: current brain session already owns active plan {} (epic {}); cannot resume different active plan {plan_id}",
                                        active.plan_id, active.epic_id
                                    ),
                                );
                            }
                            Ok(None) => {}
                            Err(error) => {
                                return JsonRpcResponse::internal_error(
                                    id,
                                    format!("resume_plan: {error}"),
                                )
                            }
                        }
                    }
                    Ok(false) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to project plan {plan_id}: {error}"),
                        )
                    }
                }
                let was_pending = epic
                    .labels
                    .iter()
                    .any(|label| label == crate::plan::labels::PLAN_PENDING);
                if was_pending {
                    if let Err(error) = apply_issue_update(
                        pm,
                        &epic_id,
                        IssueUpdate {
                            add_labels: vec![crate::plan::labels::PLAN_COMPLETE.to_string()],
                            remove_labels: vec![crate::plan::labels::PLAN_PENDING.to_string()],
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: failed to start plan: {error}"),
                        );
                    }
                }
                self.fast_forward_reconciler();
                let result = json!({
                    "status": if was_pending { "started" } else { "already_owner" },
                    "plan_id": plan_id,
                    "epic_id": epic_id,
                });
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                JsonRpcResponse::error(
                    id,
                    -32009,
                    format!(
                        "resume_plan: plan {plan_id} is owned by {owner}; active handoff is not supported"
                    ),
                )
            }
            crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "resume_plan: plan {plan_id} has ambiguous owner labels: {}",
                    owners.join(", ")
                ),
            ),
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                if !allow_claim_unowned {
                    return JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: plan {plan_id} is unowned; claim it before starting"),
                    );
                }
                let _active_plan_claim_guard = self.active_plan_claim_lock.lock().await;
                match self.current_brain_active_owned_plan(pm, None, None).await {
                    Ok(Some(active)) => {
                        return JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: current brain session already owns active plan {} (epic {}); finish it before claiming plan {plan_id}",
                                active.plan_id, active.epic_id
                            ),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("resume_plan: {error}"),
                        )
                    }
                }
                let owner_label =
                    crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
                if let Err(error) = apply_issue_update(
                    pm,
                    &epic_id,
                    IssueUpdate {
                        add_labels: vec![owner_label.clone()],
                        ..Default::default()
                    },
                )
                .await
                {
                    return JsonRpcResponse::internal_error(
                        id,
                        format!("resume_plan: failed to claim plan: {error}"),
                    );
                }
                self.fast_forward_reconciler();

                let epic = match pm.get_issue(&epic_id).await {
                    Ok(epic) => epic,
                    Err(error) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!(
                                "resume_plan: failed to reload claimed epic {epic_id}: {error}"
                            ),
                        )
                    }
                };
                match crate::plan::ownership::classify_owner(
                    &epic.labels,
                    self.brain_session_id().as_session_id(),
                ) {
                    crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
                        let result = json!({
                            "status": "claimed",
                            "plan_id": plan_id,
                            "epic_id": epic_id,
                        });
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        JsonRpcResponse::success(
                            id,
                            json!({ "content": [{ "type": "text", "text": text }] }),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; plan is owned by {owner}"
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Ambiguous { owners } => {
                        if let Err(error) = apply_issue_update(
                            pm,
                            &epic_id,
                            IssueUpdate {
                                remove_labels: vec![owner_label],
                                ..Default::default()
                            },
                        )
                        .await
                        {
                            return JsonRpcResponse::internal_error(
                                id,
                                format!(
                                    "resume_plan: failed to clean up contested owner claim for plan {plan_id}: {error}"
                                ),
                            );
                        }
                        JsonRpcResponse::error(
                            id,
                            -32009,
                            format!(
                                "resume_plan: failed to claim plan {plan_id}; ambiguous owner labels: {}",
                                owners.join(", ")
                            ),
                        )
                    }
                    crate::plan::ownership::PlanOwnerMatch::Unowned => JsonRpcResponse::error(
                        id,
                        -32009,
                        format!("resume_plan: failed to claim plan {plan_id}; plan remains unowned"),
                    ),
                }
            }
        }
    }

    /// Operator-initiated force-reclaim. Removes any existing
    /// `spur:plan-owner:*` labels from the plan's epic and stamps the current
    /// brain. Records a `PlanForceReclaimed` audit sentinel with the prior
    /// owner (or `None` if Unowned) and an optional operator-supplied reason.
    /// Refuses unless `confirm: true` is passed.
    pub(crate) async fn handle_force_reclaim_plan(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let plan_id = match args.get("plan_id").and_then(|v| v.as_str()) {
            Some(plan_id) => plan_id,
            None => {
                return JsonRpcResponse::invalid_params(id, "force_reclaim_plan: missing plan_id")
            }
        };
        let confirm = args
            .get("confirm")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !confirm {
            return JsonRpcResponse::invalid_params(
                id,
                "force_reclaim_plan: missing or false `confirm`. This tool clobbers any \
                 concurrent owner brain's in-flight state and is intended only for stuck or \
                 dead owners. Re-invoke with `confirm: true` to acknowledge.",
            );
        }
        let reason = args
            .get("reason")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let pm = match self.pm_service.as_deref() {
            Some(pm) => pm,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "force_reclaim_plan requires PM service",
                )
            }
        };

        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }

        let epics = match pm
            .list_issues(IssueFilter {
                labels: vec![crate::plan::labels::plan_id(plan_id)],
                issue_type: Some("epic".to_string()),
                include_closed: true,
                limit: Some(10),
                ..Default::default()
            })
            .await
        {
            Ok(epics) => epics,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to find plan: {error}"),
                )
            }
        };
        let Some(epic_summary) = epics.first() else {
            return JsonRpcResponse::error(
                id,
                -32004,
                format!("force_reclaim_plan: plan not found: {plan_id}"),
            );
        };
        if epics.len() > 1 {
            let epic_ids = epics
                .iter()
                .map(|epic| epic.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return JsonRpcResponse::error(
                id,
                -32009,
                format!(
                    "force_reclaim_plan: ambiguous plan lookup for {plan_id}; multiple epics matched: {epic_ids}"
                ),
            );
        }
        let epic_id = epic_summary.id.clone();
        let epic = match pm.get_issue(&epic_id).await {
            Ok(epic) => epic,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("force_reclaim_plan: failed to load epic {epic_id}: {error}"),
                )
            }
        };

        // Capture prior owner(s) for the audit sentinel BEFORE rewriting labels.
        // The single-owner case yields `Some("<owner>")`; the rare ambiguous
        // multi-owner case preserves the comma-joined list verbatim so operators
        // can see what was clobbered. Unowned → `None`.
        let prior_owners: Vec<String> = epic
            .labels
            .iter()
            .filter_map(|label| {
                crate::plan::labels::parse_plan_owner(label).map(|owner| owner.to_string())
            })
            .collect();
        let prior_owner = match prior_owners.len() {
            0 => None,
            1 => Some(prior_owners[0].clone()),
            _ => Some(prior_owners.join(",")),
        };

        let new_owner_label =
            crate::plan::labels::plan_owner(&self.brain_session_id().as_session_id().0);
        let mut remove_labels: Vec<String> = epic
            .labels
            .iter()
            .filter(|label| crate::plan::labels::parse_plan_owner(label).is_some())
            .cloned()
            .collect();
        let add_labels = vec![new_owner_label.clone()];
        filter_remove_labels(&mut remove_labels, &add_labels);

        if let Err(error) = apply_issue_update(
            pm,
            &epic_id,
            IssueUpdate {
                add_labels,
                remove_labels,
                ..Default::default()
            },
        )
        .await
        {
            return JsonRpcResponse::internal_error(
                id,
                format!(
                    "force_reclaim_plan: failed to write owner labels on epic {epic_id}: {error}"
                ),
            );
        }
        self.fast_forward_reconciler();

        let new_owner = self.brain_session_id().to_string();
        let token = uuid::Uuid::new_v4().to_string();

        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Some(adv) = pm.advanced() {
            let audit = crate::plan::audit_sentinel::AuditSentinelKind::PlanForceReclaimed {
                plan_id: plan_id.to_string(),
                prior_owner: prior_owner.clone(),
                new_owner: new_owner.clone(),
                token: token.clone(),
                reason: reason.clone(),
            };
            let body = crate::plan::audit_sentinel::encode_comment(&audit);
            if let Err(e) = adv.add_comment(&epic_id, &body).await {
                tracing::warn!(
                    target: "spur.audit.emit_failure",
                    kind = "plan_force_reclaimed",
                    epic_id = %epic_id,
                    plan_id = %plan_id,
                    "PlanForceReclaimed audit comment emission failed (owner label is persisted; audit missing): {e}"
                );
            }
        }

        let result = json!({
            "prior_owner": prior_owner,
            "new_owner": new_owner,
            "audit_token": token,
        });
        let text = serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    // ─── Plan execution handlers ──────────────────────────────────

    async fn submit_plan_as_epic_internal(
        &self,
        mut input: SubmitPlanAsEpicInput,
    ) -> Result<SubmitPlanAsEpicResult, String> {
        let pm = self
            .submit_plan_substrate_pm()
            .ok_or_else(|| "submit_plan: persist_as_epic requires a beads PM backend (configured backend: none)".to_string())?;
        if pm.source_str() != "beads" {
            return Err(format!(
                "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                pm.source_str()
            ));
        }

        let auto_serialized = match input.precomputed_auto_serialized.take() {
            Some(overlaps) => overlaps,
            None => crate::plan::submit_plan_normalize_tasks(&mut input.tasks)?,
        };
        let task_count = input.tasks.len();
        let task_ids = input
            .tasks
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();

        let plan_id = crate::plan::persist_plan_as_epic(
            pm,
            self.feature_gate.as_ref(),
            crate::plan::PersistPlanAsEpicInput {
                tasks: input.tasks,
                base: input.base,
                parent_epic_id: input.parent_epic_id,
                epic_title: input.epic_title,
                epic_body: input.epic_body,
                epic_labels: Vec::new(),
                brain_session_id: input.brain_session_id,
                execution_mode: input.execution_mode.to_string(),
                precomputed_auto_serialized: input.precomputed_auto_serialized,
                repo_root: self.repo_root.clone(),
                active_plans: Arc::clone(&self.active_plans),
                event_sink: self.event_sink.clone(),
                reconciler_fast_forward: self.reconciler_fast_forward.clone(),
            },
        )
        .await
        .map_err(|error| error.to_string())?;

        let state = {
            let active_plans = self.active_plans.lock().await;
            active_plans
                .get(&plan_id)
                .map(|cached| Arc::clone(&cached.state))
        }
        .ok_or_else(|| {
            format!("submit_plan: persisted plan '{plan_id}' missing from active plan cache")
        })?;
        let epic_id = {
            let state = state.lock().await;
            state
                .epic_id
                .clone()
                .ok_or_else(|| format!("submit_plan: persisted plan '{plan_id}' has no epic_id"))?
        };
        let task_map = load_persisted_plan_task_map(pm, &plan_id, &task_ids).await?;

        Ok(SubmitPlanAsEpicResult {
            plan_id,
            task_count,
            auto_serialized,
            epic_subgraph: EpicSubgraph { epic_id, task_map },
        })
    }

    pub async fn call_force_reclaim_plan(&self, plan_id: &str) -> Result<(), String> {
        let args = serde_json::json!({ "plan_id": plan_id, "confirm": true, "reason": "Operator-initiated takeover via TUI" });
        let resp = self
            .handle_force_reclaim_plan(serde_json::Value::Null, args)
            .await;
        match resp.error {
            Some(e) => Err(e.message),
            None => {
                let mut active_plans = self.active_plans.lock().await;
                active_plans.remove(plan_id);
                Ok(())
            }
        }
    }

    pub(crate) async fn handle_submit_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
        let client_idempotency_key = match args.get("client_idempotency_key") {
            None | Some(Value::Null) => None,
            Some(value) => match value.as_str() {
                Some(key) => {
                    let key = key.trim();
                    if key.is_empty() {
                        return JsonRpcResponse::invalid_params(
                            id,
                            "submit_plan: client_idempotency_key must be non-empty",
                        );
                    }
                    Some(key.to_string())
                }
                None => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        "submit_plan: client_idempotency_key must be a string",
                    )
                }
            },
        };

        if let Some(key) = client_idempotency_key.as_deref() {
            if self.pm_service.as_deref().map(|p| p.source_str()) == Some("beads") {
                let pm = self
                    .pm_service
                    .as_deref()
                    .expect("source check ensures pm exists");
                match crate::submit_plan_dedup::lookup(pm, key).await {
                    Ok(Some(hit)) => {
                        info!(
                            plan_id = %hit.plan_id,
                            dedup_issue_id = %hit.issue_id,
                            "submit_plan: client idempotency key hit"
                        );
                        let response_text = format!(
                            "Plan submitted: idempotency key hit.\n\
                             plan_id: {}\n\
                             Existing persisted plan was returned; no new beads epic was created.",
                            hit.plan_id
                        );
                        return JsonRpcResponse::success(
                            id,
                            json!({
                                "continuation_will_fire": true,
                                "auto_serialized": [],
                                "content": [{
                                    "type": "text",
                                    "text": response_text
                                }]
                            }),
                        );
                    }
                    Ok(None) => {}
                    Err(error) => {
                        error!("submit_plan: failed to resolve client idempotency key: {error}");
                        return JsonRpcResponse::error(
                            id,
                            -32000,
                            format!(
                                "submit_plan: failed to resolve client idempotency key: {error}"
                            ),
                        );
                    }
                }
            }
        }

        let tasks_val = match args.get("tasks").and_then(|v| v.as_array()) {
            Some(t) => t.clone(),
            None => return JsonRpcResponse::invalid_params(id, "Missing required field 'tasks'"),
        };

        let mut tasks: Vec<crate::plan::PlanTask> = match tasks_val
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(t) => t,
            Err(e) => {
                return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
            }
        };

        let auto_serialized = match crate::plan::submit_plan_normalize_tasks(&mut tasks) {
            Ok(overlaps) => overlaps,
            Err(e) => return JsonRpcResponse::invalid_params(id, e),
        };

        if args.get("persist_as_epic").and_then(|v| v.as_bool()) == Some(false) {
            return JsonRpcResponse::invalid_params(id, PERSIST_AS_EPIC_FALSE_REMOVED_MESSAGE);
        }
        let epic_title = args
            .get("epic_title")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(String::from)
            .or_else(|| {
                tasks
                    .first()
                    .map(|task| task.task.trim().chars().take(60).collect::<String>())
                    .filter(|title| !title.is_empty())
            });
        let epic_body = args
            .get("epic_body")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if epic_title.is_none() {
            return JsonRpcResponse::invalid_params(
                id,
                "submit_plan: cannot derive epic_title - provide epic_title or a non-whitespace tasks[0].task",
            );
        }
        let pm_source = self.submit_plan_substrate_pm().map(|p| p.source_str());
        if pm_source != Some("beads") {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                    pm_source.unwrap_or("none"),
                ),
            );
        }

        // Parse optional explicit base. Tolerant: `BaseTarget`'s manual
        // Deserialize accepts both `{"kind":...}` and JSON-stringified-object.
        let explicit_base: Option<crate::BaseTarget> = match args.get("base") {
            None | Some(serde_json::Value::Null) => None,
            Some(v) => match serde_json::from_value::<crate::BaseTarget>(v.clone()) {
                Ok(target) => Some(target),
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan: invalid 'base' parameter: {e}"),
                    );
                }
            },
        };

        let submitted = match self
            .submit_plan_as_epic_internal(SubmitPlanAsEpicInput {
                tasks,
                base: explicit_base,
                parent_epic_id: None,
                epic_title,
                epic_body,
                brain_session_id: self.brain_session_id().clone(),
                execution_mode: "submit_plan",
                precomputed_auto_serialized: Some(auto_serialized),
            })
            .await
        {
            Ok(submitted) => submitted,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_plan: failed to persist plan as beads epic: {error}"),
                );
            }
        };

        if let Some(key) = client_idempotency_key.as_deref() {
            let pm = self
                .pm_service
                .as_deref()
                .expect("submit_plan persistent path has pm_service");
            if let Err(error) = crate::submit_plan_dedup::record(pm, key, &submitted.plan_id).await
            {
                error!(
                    plan_id = %submitted.plan_id,
                    "submit_plan: failed to record client idempotency key: {error}"
                );
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_plan: failed to record client idempotency key: {error}"),
                );
            }
        }

        info!(plan_id = %submitted.plan_id, tasks = submitted.task_count, "Plan submitted");

        let task_map_json = serde_json::to_string(&submitted.epic_subgraph.task_map)
            .unwrap_or_else(|_| "{}".to_string());
        let response_text = format!(
            "Plan submitted: {task_count} tasks.\n\
             plan_id: {plan_id}\n\
             epic_id: {epic_id} (beads)\n\
             task_map: {task_map_json}\n\
             A continuation will fire on each per-task failure/awaiting-review and on plan completion. \
             Polling get_plan_status remains available as a safety net.",
            task_count = submitted.task_count,
            plan_id = submitted.plan_id,
            epic_id = submitted.epic_subgraph.epic_id,
        );

        let response_text = if submitted.auto_serialized.is_empty() {
            response_text
        } else {
            let edges: Vec<String> = submitted
                .auto_serialized
                .iter()
                .map(|o| {
                    format!(
                        "  {} → {} (shared: {})",
                        o.from,
                        o.to,
                        o.shared_files.join(", ")
                    )
                })
                .collect();
            format!(
                "{response_text}\n\nAuto-serialized {} sibling pair(s) with overlapping context_files:\n{}",
                submitted.auto_serialized.len(),
                edges.join("\n")
            )
        };

        JsonRpcResponse::success(
            id,
            json!({
                "continuation_will_fire": true,
                "auto_serialized": submitted.auto_serialized,
                "content": [{
                    "type": "text",
                    "text": response_text
                }]
            }),
        )
    }

    pub(crate) async fn handle_submit_loop(&self, id: Value, args: Value) -> JsonRpcResponse {
        let mut input: crate::tool_schemas::SubmitLoopParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("submit_loop: invalid parameters: {error}"),
                )
            }
        };
        let client_idempotency_key = match input.client_idempotency_key.as_deref() {
            Some(key) => {
                let key = key.trim();
                if key.is_empty() {
                    return JsonRpcResponse::invalid_params(
                        id,
                        "submit_loop: client_idempotency_key must be non-empty",
                    );
                }
                Some(key.to_string())
            }
            None => None,
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "submit_loop: requires a beads PM backend (configured backend: none)",
            );
        };
        if pm.source_str() != "beads" {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "submit_loop: requires a beads PM backend (configured backend: {})",
                    pm.source_str()
                ),
            );
        }

        if let Some(key) = client_idempotency_key.as_deref() {
            match crate::submit_plan_dedup::lookup_loop(pm, key).await {
                Ok(Some(hit)) => {
                    info!(
                        loop_id = %hit.loop_id,
                        issue_id = %hit.issue_id,
                        dedup_issue_id = %hit.dedup_issue_id,
                        "submit_loop: client idempotency key hit"
                    );
                    return submit_loop_success_response(
                        id,
                        &hit.loop_id,
                        &hit.issue_id,
                        hit.next_run,
                        hit.paused,
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    error!("submit_loop: failed to resolve client idempotency key: {error}");
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("submit_loop: failed to resolve client idempotency key: {error}"),
                    );
                }
            }
        }

        if let Err(message) =
            crate::plan::loops::validation::validate_loop_spec_for_submit(&input.spec)
        {
            return JsonRpcResponse::invalid_params(id, format!("submit_loop: {message}"));
        }

        let loop_id = crate::plan::labels::mint_delegation_id();
        input.spec.loop_id = loop_id.clone();
        let now = unix_now_secs();
        let issue_id = match pm
            .create_issue(spur_pm::IssueCreate {
                title: loop_issue_title(&input.spec.goal),
                description: Some(input.spec.to_sentinel_body()),
                issue_type: Some(crate::plan::loops::LOOP_ISSUE_TYPE.to_string()),
                priority: Some(2),
                labels: vec![
                    crate::plan::labels::loop_id_label(&loop_id),
                    autonomy_label(input.spec.autonomy),
                    crate::plan::labels::loop_next_run_label(now),
                ],
                ..Default::default()
            })
            .await
        {
            Ok(issue_id) => issue_id,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_loop: failed to create loop issue: {error}"),
                )
            }
        };
        if let Some(key) = client_idempotency_key.as_deref() {
            if let Err(error) =
                crate::submit_plan_dedup::record_loop(pm, key, &loop_id, &issue_id, now, false)
                    .await
            {
                error!(
                    loop_id = %loop_id,
                    issue_id = %issue_id,
                    "submit_loop: failed to record client idempotency key: {error}"
                );
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("submit_loop: failed to record client idempotency key: {error}"),
                );
            }
        }
        self.fast_forward_reconciler();

        submit_loop_success_response(id, &loop_id, &issue_id, now, false)
    }

    pub(crate) async fn handle_spur_loop_doctor(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::SpurLoopDoctorParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("spur_loop_doctor: invalid parameters: {error}"),
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "spur_loop_doctor: requires a beads PM backend (configured backend: none)",
            );
        };
        if pm.source_str() != "beads" {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "spur_loop_doctor: requires a beads PM backend (configured backend: {})",
                    pm.source_str()
                ),
            );
        }

        spur_loop_doctor_response(id, crate::plan::loops::doctor::run(input))
    }

    pub(crate) async fn handle_pause_loop(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::LoopIdParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("pause_loop: invalid parameters: {error}"),
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if let Err(message) = validate_loop_id_param(&input.loop_id) {
            return JsonRpcResponse::invalid_params(id, format!("pause_loop: {message}"));
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "pause_loop: requires a beads PM backend (configured backend: none)",
            );
        };
        let issue = match load_loop_issue(pm, &input.loop_id).await {
            Ok(Some(issue)) => issue,
            Ok(None) => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("pause_loop: unknown loop_id '{}'", input.loop_id),
                )
            }
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("pause_loop: failed to load loop issue: {error}"),
                )
            }
        };
        if let Err(error) = pm
            .update_issue(
                &issue.id,
                spur_pm::IssueUpdate {
                    add_labels: vec![crate::plan::labels::LOOP_PAUSED.to_string()],
                    ..Default::default()
                },
            )
            .await
        {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "pause_loop: failed to pause loop '{}': {error}",
                    input.loop_id
                ),
            );
        }
        if let Some(sink) = self.event_sink.as_deref() {
            sink.emit(spur_acp::SpurEventBody::LoopPaused {
                loop_id: input.loop_id.clone(),
                by: "paused".to_string(),
            });
        }
        let output = json!({
            "loop_id": input.loop_id,
            "issue_id": issue.id,
            "paused": true,
        });
        let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_resume_loop(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::LoopIdParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("resume_loop: invalid parameters: {error}"),
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if let Err(message) = validate_loop_id_param(&input.loop_id) {
            return JsonRpcResponse::invalid_params(id, format!("resume_loop: {message}"));
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "resume_loop: requires a beads PM backend (configured backend: none)",
            );
        };
        let issue = match load_loop_issue(pm, &input.loop_id).await {
            Ok(Some(issue)) => issue,
            Ok(None) => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("resume_loop: unknown loop_id '{}'", input.loop_id),
                )
            }
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("resume_loop: failed to load loop issue: {error}"),
                )
            }
        };
        let now = unix_now_secs();
        let mut remove_labels: Vec<String> = issue
            .labels
            .iter()
            .filter(|label| {
                label.as_str() == crate::plan::labels::LOOP_PAUSED
                    || crate::plan::labels::parse_loop_next_run(label).is_some()
            })
            .cloned()
            .collect();
        remove_labels.sort();
        remove_labels.dedup();
        if let Err(error) = pm
            .update_issue(
                &issue.id,
                spur_pm::IssueUpdate {
                    add_labels: vec![crate::plan::labels::loop_next_run_label(now)],
                    remove_labels,
                    ..Default::default()
                },
            )
            .await
        {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "resume_loop: failed to resume loop '{}': {error}",
                    input.loop_id
                ),
            );
        }
        self.fast_forward_reconciler();
        if let Some(sink) = self.event_sink.as_deref() {
            sink.emit(spur_acp::SpurEventBody::LoopPaused {
                loop_id: input.loop_id.clone(),
                by: "resumed".to_string(),
            });
        }
        let output = json!({
            "loop_id": input.loop_id,
            "issue_id": issue.id,
            "paused": false,
            "next_run": now,
        });
        let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_kill_loop(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::LoopIdParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("kill_loop: invalid parameters: {error}"),
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if let Err(message) = validate_loop_id_param(&input.loop_id) {
            return JsonRpcResponse::invalid_params(id, format!("kill_loop: {message}"));
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "kill_loop: requires a beads PM backend (configured backend: none)",
            );
        };
        let issue = match load_loop_issue_including_closed(pm, &input.loop_id).await {
            Ok(Some(issue)) => issue,
            Ok(None) => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("kill_loop: unknown loop_id '{}'", input.loop_id),
                )
            }
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("kill_loop: failed to load loop issue: {error}"),
                )
            }
        };

        if issue.status != "open" {
            let next_run = issue
                .labels
                .iter()
                .filter_map(|label| crate::plan::labels::parse_loop_next_run(label))
                .max();
            let output = json!({
                "loop_id": input.loop_id,
                "issue_id": issue.id,
                "retired": true,
                "status": issue.status,
                "next_run": next_run,
            });
            let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
            return JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": text }] }),
            );
        }

        let now = unix_now_secs();
        let generation = match next_loop_retirement_generation(pm, &input.loop_id).await {
            Ok(generation) => generation,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("kill_loop: failed to compute retirement generation: {error}"),
                )
            }
        };
        let mut remove_labels: Vec<String> = issue
            .labels
            .iter()
            .filter(|label| crate::plan::labels::parse_loop_next_run(label).is_some())
            .cloned()
            .collect();
        remove_labels.sort();
        remove_labels.dedup();
        let run = crate::plan::loops::run_record::retired_loop_run(&input.loop_id, generation, now);
        if let Err(error) = pm
            .update_issue(
                &issue.id,
                spur_pm::IssueUpdate {
                    status: Some(pm.closed_status().to_string()),
                    comment: Some(crate::plan::audit_sentinel::encode_comment(&run)),
                    remove_labels,
                    ..Default::default()
                },
            )
            .await
        {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "kill_loop: failed to retire loop '{}': {error}",
                    input.loop_id
                ),
            );
        }
        if let Some(sink) = self.event_sink.as_deref() {
            sink.emit(spur_acp::SpurEventBody::LoopPaused {
                loop_id: input.loop_id.clone(),
                by: "retired".to_string(),
            });
        }
        let output = json!({
            "loop_id": input.loop_id,
            "issue_id": issue.id,
            "retired": true,
            "status": pm.closed_status(),
            "next_run": Value::Null,
        });
        let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_set_loop_autonomy(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::SetLoopAutonomyParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("set_loop_autonomy: invalid parameters: {error}"),
                )
            }
        };
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        if let Err(message) = validate_loop_id_param(&input.loop_id) {
            return JsonRpcResponse::invalid_params(id, format!("set_loop_autonomy: {message}"));
        }
        let target_level = match parse_autonomy_level_param(&input.level) {
            Ok(level) => level,
            Err(message) => {
                return JsonRpcResponse::invalid_params(id, format!("set_loop_autonomy: {message}"))
            }
        };
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "set_loop_autonomy: requires a beads PM backend (configured backend: none)",
            );
        };
        let issue = match load_loop_issue(pm, &input.loop_id).await {
            Ok(Some(issue)) => issue,
            Ok(None) => {
                return JsonRpcResponse::error(
                    id,
                    -32004,
                    format!("set_loop_autonomy: unknown loop_id '{}'", input.loop_id),
                )
            }
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("set_loop_autonomy: failed to load loop issue: {error}"),
                )
            }
        };
        let mut spec = match crate::plan::loops::spec::LoopSpec::parse(&issue.body) {
            Ok(spec) => spec,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!("set_loop_autonomy: failed to parse loop spec: {error}"),
                )
            }
        };
        let current_level = spec.autonomy;
        if target_level > current_level {
            let direct_steps = target_level as u8 - current_level as u8;
            if direct_steps != 1 {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!(
                        "set_loop_autonomy: promotions may advance one level per call (current {}, requested {})",
                        current_level.as_str(),
                        target_level.as_str()
                    ),
                );
            }
        }

        let mut stable_generations = 0;
        if target_level > current_level {
            if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
                return JsonRpcResponse::mcp_error(id, error);
            }
            let Some(advanced) = pm.advanced() else {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    "set_loop_autonomy: beads advanced comments API is unavailable",
                );
            };
            let audits = match crate::plan::projector::collect_sorted_audits_for_issue(
                &issue.id,
                match advanced.list_comments(&issue.id).await {
                    Ok(comments) => comments,
                    Err(error) => {
                        return JsonRpcResponse::error(
                            id,
                            -32000,
                            format!("set_loop_autonomy: failed to list loop comments: {error}"),
                        )
                    }
                },
            ) {
                Ok(audits) => audits,
                Err(error) => {
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("set_loop_autonomy: failed to parse loop audits: {error}"),
                    )
                }
            };
            stable_generations =
                stable_approved_generations_at_level(&audits, &input.loop_id, current_level);
            if stable_generations < RATCHET_MIN_STABLE_GENERATIONS {
                let shortfall = RATCHET_MIN_STABLE_GENERATIONS - stable_generations;
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!(
                        "set_loop_autonomy: promotion from {} to {} requires {} consecutive approved real generations at current level {}; observed {}, short by {}",
                        current_level.as_str(),
                        target_level.as_str(),
                        RATCHET_MIN_STABLE_GENERATIONS,
                        current_level.as_str(),
                        stable_generations,
                        shortfall
                    ),
                );
            }
        }

        spec.autonomy = target_level;
        let target_label = autonomy_label(target_level);
        let mut remove_labels: Vec<String> = issue
            .labels
            .iter()
            .filter(|label| {
                crate::plan::labels::parse_autonomy(label).is_some()
                    && label.as_str() != target_label.as_str()
            })
            .cloned()
            .collect();
        remove_labels.sort();
        remove_labels.dedup();
        if let Err(error) = pm
            .update_issue(
                &issue.id,
                spur_pm::IssueUpdate {
                    body: Some(spec.to_sentinel_body()),
                    add_labels: vec![target_label],
                    remove_labels,
                    ..Default::default()
                },
            )
            .await
        {
            return JsonRpcResponse::error(
                id,
                -32000,
                format!(
                    "set_loop_autonomy: failed to update loop '{}': {error}",
                    input.loop_id
                ),
            );
        }

        let output = json!({
            "loop_id": input.loop_id,
            "issue_id": issue.id,
            "previous_level": current_level.as_str(),
            "level": target_level.as_str(),
            "stable_generations": stable_generations,
        });
        let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_get_loop_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::GetLoopStatusParams = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => {
                return JsonRpcResponse::invalid_params(
                    id,
                    format!("get_loop_status: invalid parameters: {error}"),
                )
            }
        };
        if let Err(error) = self.require_feature(FeatureKey::PM_PRO_BEADS_ADVANCED) {
            return JsonRpcResponse::mcp_error(id, error);
        }
        if let Err(message) = validate_loop_id_param(&input.loop_id) {
            return JsonRpcResponse::invalid_params(id, format!("get_loop_status: {message}"));
        }
        let Some(pm) = self.submit_plan_substrate_pm() else {
            return JsonRpcResponse::error(
                id,
                -32000,
                "get_loop_status: requires a beads PM backend (configured backend: none)",
            );
        };
        let recent_limit = input.recent_runs.unwrap_or(10).min(100) as usize;
        let status =
            match crate::plan::loops::status::build_loop_status(pm, &input.loop_id, recent_limit)
                .await
            {
                Ok(Some(status)) => status,
                Ok(None) => {
                    return JsonRpcResponse::error(
                        id,
                        -32004,
                        format!("get_loop_status: unknown loop_id '{}'", input.loop_id),
                    )
                }
                Err(error) => {
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("get_loop_status: {error:#}"),
                    )
                }
            };
        let output = status.to_mcp_json();
        let text = serde_json::to_string_pretty(&output).unwrap_or_else(|_| output.to_string());
        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_get_plan_status(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        let plan_deps = self.plan_mcp_deps();
        match crate::handlers::get_plan_status(&plan_deps, &self.reconciler_outcomes, &ctx, args)
            .await
        {
            Ok(status) => {
                let text =
                    serde_json::to_string_pretty(&status).unwrap_or_else(|_| status.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("get_plan_status failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    pub(crate) async fn handle_get_reconciler_status(&self, id: Value) -> JsonRpcResponse {
        let status = self.reconciler_outcomes.lock().await.reconciler_status();
        let text = match serde_json::to_string_pretty(&status) {
            Ok(text) => text,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize reconciler status: {error}"),
                )
            }
        };

        JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
    }

    pub(crate) async fn handle_get_task_diff(&self, id: Value, args: Value) -> JsonRpcResponse {
        let ctx = crate::handlers::WorkerCallContext {
            delegation_id: String::new(),
            brain_session_id: self.brain_session_id().as_session_id().0.clone(),
        };
        let plan_deps = self.plan_mcp_deps();
        match crate::handlers::get_task_diff(
            self.pm_service.as_deref(),
            self.feature_gate.as_ref(),
            self.repo_root.as_deref(),
            &plan_deps,
            &ctx,
            args,
        )
        .await
        {
            Ok(value) => {
                let text = match serde_json::to_string_pretty(&value) {
                    Ok(t) => t,
                    Err(e) => {
                        return JsonRpcResponse::internal_error(
                            id,
                            format!("get_task_diff response serialization failed: {e}"),
                        )
                    }
                };
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(crate::handlers::McpHandlerError::InvalidParams(e)) => {
                JsonRpcResponse::invalid_params(id, e)
            }
            Err(crate::handlers::McpHandlerError::NotFound(e)) => {
                JsonRpcResponse::error(id, -32004, e)
            }
            Err(crate::handlers::McpHandlerError::Unauthorized(e)) => {
                JsonRpcResponse::error(id, -32001, e)
            }
            Err(crate::handlers::McpHandlerError::UpstreamPm(e)) => {
                JsonRpcResponse::internal_error(id, format!("get_task_diff failed: {e}"))
            }
            Err(crate::handlers::McpHandlerError::Internal(e)) => {
                JsonRpcResponse::internal_error(id, e)
            }
        }
    }

    /// bd-2m2u Phase 2c — `submit_plan_mutation` MCP entry. Builds a
    /// `MutationBatch` from the request payload, runs cycle detection +
    /// rollback via the generic executor, and clears `signal:escalated` labels
    /// on success.
    ///
    /// bd-2m2u Phase 2d — emits `PlanMutationApplied` on success so observers
    /// (TUI, brain, dashboards) follow the recovery without parsing the audit
    /// log directly.
    pub(crate) async fn handle_submit_plan_mutation(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        if let Some(response) =
            self.require_feature_response(id.clone(), FeatureKey::PM_PRO_BEADS_ADVANCED)
        {
            return response;
        }
        let pm = match self.pm_service.clone() {
            Some(pm) => pm,
            None => {
                return JsonRpcResponse::internal_error(
                    id,
                    "submit_plan_mutation: no PM service configured",
                )
            }
        };

        let trigger_task_id = match args.get("trigger_task_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return JsonRpcResponse::invalid_params(
                    id,
                    "submit_plan_mutation: trigger_task_id is required",
                )
            }
        };
        let ops_value = match args.get("ops").cloned() {
            Some(v) => v,
            None => {
                return JsonRpcResponse::invalid_params(id, "submit_plan_mutation: ops required")
            }
        };
        let ops: Vec<crate::plan::mutation::PlanMutationOp> =
            match serde_json::from_value(ops_value) {
                Ok(v) => v,
                Err(e) => {
                    return JsonRpcResponse::invalid_params(
                        id,
                        format!("submit_plan_mutation: invalid ops: {e}"),
                    )
                }
            };
        let mutation_id = args
            .get("mutation_id")
            .and_then(|v| v.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .unwrap_or_else(uuid::Uuid::new_v4);

        // bd-2m2u Phase 2d — snapshot the op tags + plan_id BEFORE moving
        // the ops into the executor, so we can emit `PlanMutationApplied`
        // on success. Plan id is recovered from the trigger task's beads
        // labels (best effort — observability only).
        let op_tags: Vec<String> = ops
            .iter()
            .map(|op| crate::plan::mutation::op_tag_for(op).to_string())
            .collect();
        let trigger_task_id_for_event = trigger_task_id.clone();

        match crate::plan::mutation_executor::submit_plan_mutation(
            pm.clone(),
            Arc::clone(&self.feature_gate),
            mutation_id,
            trigger_task_id,
            ops,
        )
        .await
        {
            Ok(result) => {
                if let Some(sink) = self.event_sink.as_deref() {
                    let plan_id =
                        derive_plan_id_from_trigger_issue(pm.as_ref(), &trigger_task_id_for_event)
                            .await
                            .unwrap_or_default();
                    sink.emit(spur_acp::SpurEventBody::PlanMutationApplied {
                        plan_id,
                        mutation_id: result.mutation_id.clone(),
                        trigger_task_id: trigger_task_id_for_event,
                        op_tags,
                        affected_task_ids: result.affected_task_ids.clone(),
                    });
                }
                let payload = json!({
                    "mutation_id": result.mutation_id,
                    "children_created": result.children_created,
                    "affected_task_ids": result.affected_task_ids,
                });
                let text =
                    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(error) => JsonRpcResponse::internal_error(
                id,
                format!("submit_plan_mutation failed: {error:#}"),
            ),
        }
    }

    pub(crate) async fn handle_preview_task_base(&self, id: Value, args: Value) -> JsonRpcResponse {
        let input: crate::tool_schemas::PreviewTaskBaseInput = match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
        };

        match self.preview_task_base_impl(input).await {
            Ok(output) => match serde_json::to_string_pretty(&output) {
                Ok(text) => JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(error) => JsonRpcResponse::internal_error(
                    id,
                    format!("failed to serialize preview_task_base response: {error}"),
                ),
            },
            Err(error) => {
                let message = error.to_string();
                if message.starts_with("unknown plan")
                    || message.starts_with("Unknown task_id")
                    || message.contains("Unknown plan_id")
                {
                    JsonRpcResponse::invalid_params(id, message)
                } else {
                    JsonRpcResponse::internal_error(id, message)
                }
            }
        }
    }

    pub(crate) async fn handle_plan_truncate_and_restart(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        let input: crate::tool_schemas::PlanTruncateAndRestartInput =
            match serde_json::from_value(args) {
                Ok(input) => input,
                Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
            };

        let repo_root = match self.repo_root.as_ref() {
            Some(root) => root.clone(),
            None => {
                return JsonRpcResponse::internal_error(id, "Repository root not configured");
            }
        };

        let plan_arc = match self.load_or_project_plan(&input.plan_id).await {
            Ok(plan) => plan,
            Err(error) => return JsonRpcResponse::invalid_params(id, error),
        };

        let snapshot = {
            let state = plan_arc.lock().await;
            crate::plan::PlanState {
                plan_id: state.plan_id.clone(),
                tasks: state.tasks.clone(),
                brain_session_id: state.brain_session_id.clone(),
                base_snapshot_branch: state.base_snapshot_branch.clone(),
                base_snapshot_oid: state.base_snapshot_oid.clone(),
                merge_state: state.merge_state.clone(),
                epic_id: state.epic_id.clone(),
            }
        };

        if snapshot.epic_id.is_none() {
            return JsonRpcResponse::invalid_params(
                id,
                "plan_truncate_and_restart requires a persisted parent plan; the supplied plan has no epic_id",
            );
        }

        if !snapshot
            .tasks
            .iter()
            .any(|entry| entry.spec.task_id == input.blocked_task_id)
        {
            return JsonRpcResponse::invalid_params(
                id,
                format!(
                    "Unknown blocked_task_id '{}' in plan '{}'",
                    input.blocked_task_id, input.plan_id
                ),
            );
        }

        let build = match crate::plan::staging::build_staging_branch(&snapshot, &repo_root).await {
            Ok(build) => build,
            Err(error) => return JsonRpcResponse::internal_error(id, error.to_string()),
        };

        let (new_tasks, superseded_task_ids) = crate::plan::staging::shape_new_plan(&snapshot);
        let superseded_set: HashSet<&str> =
            superseded_task_ids.iter().map(String::as_str).collect();

        let submitted = match self
            .submit_plan_as_epic_internal(SubmitPlanAsEpicInput {
                tasks: new_tasks,
                base: Some(crate::BaseTarget::Branch {
                    name: build.branch.clone(),
                }),
                parent_epic_id: snapshot.epic_id.clone(),
                epic_title: None,
                epic_body: Some(format!(
                    "Child recovery plan for `{}` rooted at `{}` after `{}` blocked.",
                    snapshot.plan_id, build.branch, input.blocked_task_id
                )),
                brain_session_id: snapshot.brain_session_id.clone(),
                execution_mode: "plan_truncate_and_restart",
                precomputed_auto_serialized: None,
            })
            .await
        {
            Ok(submitted) => submitted,
            Err(error) => {
                return JsonRpcResponse::internal_error(
                    id,
                    format!("new plan submission failed: {error}"),
                );
            }
        };

        {
            let mut state = plan_arc.lock().await;
            let mutation_id = uuid::Uuid::new_v4().to_string();
            for entry in state.tasks.iter_mut() {
                if superseded_set.contains(entry.spec.task_id.as_str()) {
                    entry.status = crate::plan::PlanTaskStatus::Superseded {
                        mutation_id: mutation_id.clone(),
                        by: Vec::new(),
                    };
                }
            }
        }

        let output = crate::tool_schemas::PlanTruncateAndRestartOutput {
            staging_branch: build.branch,
            superseded_task_ids,
            new_plan_id: submitted.plan_id,
            conflict: build.conflict,
        };

        match serde_json::to_string_pretty(&output) {
            Ok(text) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": text }] }),
            ),
            Err(error) => JsonRpcResponse::internal_error(
                id,
                format!("failed to serialize plan_truncate_and_restart response: {error}"),
            ),
        }
    }

    pub(crate) async fn handle_review_task(
        &self,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();
        let decision = args["decision"].as_str().ok_or("missing decision")?;
        let feedback = args["feedback"].as_str();
        let reuse_prior_worktree = args["reuse_prior_worktree"].as_bool().unwrap_or(false);

        let plan_arc = self.load_or_project_plan(&plan_id).await?;

        let sink: Option<&dyn spur_mcp::events::McpEventSink> = self.event_sink.as_deref();

        // INV-5: use handle_review_task so the plan lock is dropped before
        // pm.update_issue() is called. Tests may install a MockPm via the
        // substrate abstraction, so route review writes through the same PM
        // surface as the reconciler.
        let pm_arc: Option<std::sync::Arc<dyn crate::plan::PmLike>> = self.reconciler_pm();

        let write_mode = if self.nonadvisory_review_writes {
            crate::plan::ReviewWriteMode::NonAdvisory
        } else {
            crate::plan::ReviewWriteMode::Advisory
        };

        let result = crate::plan::handle_review_task_with_write_mode(
            Arc::clone(&plan_arc),
            &plan_id,
            &task_id,
            decision,
            feedback,
            reuse_prior_worktree,
            pm_arc,
            sink,
            Some(&self.delegation_tx),
            Some(&self.task_tracker),
            Arc::clone(&self.feature_gate),
            write_mode,
        )
        .await;
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if matches!(write_mode, crate::plan::ReviewWriteMode::NonAdvisory) {
                    self.active_plans.lock().await.remove(&plan_id);
                }
                return Err(error);
            }
        };
        let mut result = result;

        if decision == "approve" {
            let clobber_report = self
                .run_clobber_detector_for_review(&plan_arc, &task_id)
                .await?;
            if !clobber_report.signals.is_empty() {
                if let serde_json::Value::Object(ref mut m) = result {
                    m.insert(
                        "signals".into(),
                        serde_json::to_value(&clobber_report.signals).unwrap_or(json!(null)),
                    );
                }
            }
            for warning in clobber_report.warnings {
                append_review_warning(&mut result, warning);
            }
        }

        if let Some(sink) = self.event_sink.as_deref() {
            let projected = self.load_or_project_plan(&plan_id).await?;
            let state = projected.lock().await;
            crate::plan::snapshot::emit_plan_snapshot(Some(sink), &state);
        }

        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
include!("plan_tests.rs");
