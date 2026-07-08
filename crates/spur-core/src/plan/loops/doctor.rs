use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::plan::labels::{AutonomyLevel, LOOP_TRIAGE_TASK};
use crate::plan::loops::spec::LoopSpec;
use crate::plan::PlanTask;
use crate::tool_schemas::{
    LoopDoctorDraftTask, SpurLoopDoctorOutput, SpurLoopDoctorParams, SubmitLoopParams,
};

pub(crate) fn run(params: SpurLoopDoctorParams) -> SpurLoopDoctorOutput {
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let goal = params.draft.goal.trim().to_string();
    if goal.is_empty() {
        errors.push("goal must be non-empty".to_string());
    }

    let cadence_secs = match params.draft.cadence_secs {
        Some(cadence_secs) => cadence_secs,
        None => {
            errors.push("cadence_secs is required".to_string());
            0
        }
    };

    if params.draft.tasks.is_empty() {
        errors.push("tasks must contain at least one task".to_string());
    }

    let autonomy_text = params
        .draft
        .autonomy
        .as_deref()
        .unwrap_or("l1")
        .trim()
        .to_ascii_lowercase();
    let autonomy = match AutonomyLevel::parse(&autonomy_text) {
        Some(level) => level,
        None => {
            errors.push("autonomy must be one of l1, l2, or l3".to_string());
            AutonomyLevel::L1
        }
    };

    if matches!(autonomy, AutonomyLevel::L2 | AutonomyLevel::L3) {
        warnings.push(format!(
            "Explicit {} creation starts the loop at {} directly rather than using the post-create ratchet.",
            autonomy.as_str().to_ascii_uppercase(),
            autonomy.as_str().to_ascii_uppercase()
        ));
    }

    if mentions_wall_clock(&params.original_command)
        || params
            .draft
            .schedule_description
            .as_deref()
            .is_some_and(mentions_wall_clock)
    {
        warnings.push(
            "Exact wall-clock scheduling is not represented in v1; this preview uses the supplied cadence."
                .to_string(),
        );
    }

    let (tasks, task_extras) = normalize_tasks(&params.draft.tasks, &mut warnings, &mut errors);
    if !errors.is_empty() {
        return error_output(errors, warnings);
    }

    let template = build_template(&tasks, &task_extras);
    let mut spec = LoopSpec {
        loop_id: String::new(),
        goal,
        pattern: params
            .draft
            .pattern
            .as_ref()
            .map(|pattern| pattern.trim().to_string())
            .filter(|pattern| !pattern.is_empty()),
        cadence_secs,
        autonomy,
        template,
        governors: params.draft.governors,
        escalation: params.draft.escalation,
    };

    if let Err(message) = crate::plan::loops::validation::validate_loop_spec_for_submit(&spec) {
        errors.push(message);
    }
    if !errors.is_empty() {
        return error_output(errors, warnings);
    }

    spec.loop_id.clear();
    let mut canonical = SubmitLoopParams {
        spec,
        client_idempotency_key: None,
    };
    let fingerprint = approval_fingerprint(&canonical);
    let idempotency_key = format!("spur-loop:{fingerprint}");
    canonical.client_idempotency_key = Some(idempotency_key.clone());

    let friendly_preview = build_preview(
        &params.original_command,
        &canonical.spec,
        &tasks,
        &task_extras,
        &warnings,
    );
    SpurLoopDoctorOutput {
        status: if warnings.is_empty() {
            "ok".to_string()
        } else {
            "warnings".to_string()
        },
        friendly_preview,
        warnings,
        errors,
        canonical_submit_loop_params: Some(canonical),
        approval_fingerprint: Some(fingerprint),
        client_idempotency_key: Some(idempotency_key),
    }
}

#[derive(Debug, Clone, Default)]
struct TaskExtras {
    labels: Vec<String>,
    issue_labels: Vec<String>,
    output_path: Option<String>,
    assumptions: Vec<String>,
}

fn normalize_tasks(
    drafts: &[LoopDoctorDraftTask],
    warnings: &mut Vec<String>,
    errors: &mut Vec<String>,
) -> (Vec<PlanTask>, HashMap<String, TaskExtras>) {
    let mut tasks = Vec::with_capacity(drafts.len());
    let mut extras = HashMap::new();

    for (idx, draft) in drafts.iter().enumerate() {
        let task_id = draft
            .task_id
            .as_deref()
            .map(str::trim)
            .filter(|task_id| !task_id.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("task-{}", idx + 1));
        if draft.agent.trim().is_empty() {
            errors.push(format!("task '{task_id}' agent must be non-empty"));
        }
        if draft.task.trim().is_empty() {
            errors.push(format!("task '{task_id}' task must be non-empty"));
        }

        warn_pass_through("model", &task_id, draft.model.as_deref(), warnings);
        warn_pass_through("profile", &task_id, draft.profile.as_deref(), warnings);
        warn_pass_through("effort", &task_id, draft.effort.as_deref(), warnings);
        if draft.output_path.is_some() && draft.context_files.is_empty() {
            warnings.push(format!(
                "Task '{task_id}' mentions an output path but no concrete context_files entry."
            ));
        }

        let mut labels = dedupe_nonempty(draft.labels.clone());
        if draft.triage && !labels.iter().any(|label| label == LOOP_TRIAGE_TASK) {
            labels.push(LOOP_TRIAGE_TASK.to_string());
        }
        let mut depends_on = dedupe_nonempty(draft.depends_on.clone());
        depends_on.sort();

        tasks.push(PlanTask {
            task_id: task_id.clone(),
            agent: draft.agent.trim().to_string(),
            profile: trim_option(draft.profile.clone()),
            skills: None,
            model: trim_option(draft.model.clone()),
            effort: trim_option(draft.effort.clone()),
            config_overrides: draft.config_overrides.clone(),
            task: draft.task.trim().to_string(),
            depends_on,
            issue_id: None,
            issue_title: None,
            context_files: dedupe_nonempty(draft.context_files.clone()),
        });
        extras.insert(
            task_id,
            TaskExtras {
                labels,
                issue_labels: dedupe_nonempty(draft.issue_labels.clone()),
                output_path: trim_option(draft.output_path.clone()),
                assumptions: dedupe_nonempty(draft.assumptions.clone()),
            },
        );
    }

    if errors.is_empty() {
        if let Err(message) = crate::plan::submit_plan_normalize_tasks(&mut tasks) {
            errors.push(message);
        }
        tasks = topological_order(tasks);
    }

    (tasks, extras)
}

fn warn_pass_through(field: &str, task_id: &str, value: Option<&str>, warnings: &mut Vec<String>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        warnings.push(format!(
            "Task '{task_id}' {field} '{value}' is accepted as a worker pass-through value."
        ));
    }
}

fn trim_option(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn dedupe_nonempty(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();
    for value in values {
        let value = value.trim().to_string();
        if !value.is_empty() && seen.insert(value.clone()) {
            deduped.push(value);
        }
    }
    deduped
}

fn topological_order(mut tasks: Vec<PlanTask>) -> Vec<PlanTask> {
    let mut ordered = Vec::with_capacity(tasks.len());
    let mut emitted = HashSet::new();
    while !tasks.is_empty() {
        let Some(idx) = tasks
            .iter()
            .position(|task| task.depends_on.iter().all(|dep| emitted.contains(dep)))
        else {
            ordered.extend(tasks);
            return ordered;
        };
        let task = tasks.remove(idx);
        emitted.insert(task.task_id.clone());
        ordered.push(task);
    }
    ordered
}

fn build_template(tasks: &[PlanTask], extras: &HashMap<String, TaskExtras>) -> Value {
    let raw_tasks = tasks
        .iter()
        .map(|task| {
            let mut value = json!({
                "task_id": task.task_id,
                "agent": task.agent,
                "task": task.task,
            });
            insert_optional(&mut value, "profile", task.profile.as_ref());
            insert_optional(&mut value, "model", task.model.as_ref());
            insert_optional(&mut value, "effort", task.effort.as_ref());
            if let Some(config_overrides) = task.config_overrides.as_ref() {
                value["config_overrides"] = json!(config_overrides);
            }
            if !task.depends_on.is_empty() {
                value["depends_on"] = json!(task.depends_on);
            }
            if !task.context_files.is_empty() {
                value["context_files"] = json!(task.context_files);
            }
            if let Some(extra) = extras.get(&task.task_id) {
                if !extra.labels.is_empty() {
                    value["labels"] = json!(extra.labels);
                }
                if !extra.issue_labels.is_empty() {
                    value["issue_labels"] = json!(extra.issue_labels);
                }
            }
            value
        })
        .collect::<Vec<_>>();
    json!({ "tasks": raw_tasks })
}

fn insert_optional(value: &mut Value, key: &str, field: Option<&String>) {
    if let Some(field) = field {
        value[key] = json!(field);
    }
}

fn approval_fingerprint(params: &SubmitLoopParams) -> String {
    let mut normalized = params.clone();
    normalized.spec.loop_id.clear();
    normalized.client_idempotency_key = None;
    let mut value = serde_json::to_value(normalized).expect("SubmitLoopParams serializes");
    sort_json_keys(&mut value);
    let bytes = serde_json::to_vec(&value).expect("canonical JSON serializes");
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sort_json_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for nested in map.values_mut() {
                sort_json_keys(nested);
            }
            let mut entries = std::mem::take(map).into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            map.extend(entries);
        }
        Value::Array(values) => {
            for nested in values {
                sort_json_keys(nested);
            }
        }
        _ => {}
    }
}

fn build_preview(
    original_command: &str,
    spec: &LoopSpec,
    tasks: &[PlanTask],
    extras: &HashMap<String, TaskExtras>,
    warnings: &[String],
) -> String {
    let mut lines = vec![
        "Loop Preview".to_string(),
        String::new(),
        "Goal".to_string(),
        spec.goal.clone(),
        String::new(),
        "Schedule".to_string(),
        format!("Runs every {} seconds.", spec.cadence_secs),
        "First generation will be armed immediately after approval.".to_string(),
        String::new(),
        "Autonomy".to_string(),
        spec.autonomy.as_str().to_ascii_uppercase(),
        String::new(),
        "Tasks".to_string(),
    ];

    for (idx, task) in tasks.iter().enumerate() {
        lines.push(format!(
            "{}. {}{}{}{}",
            idx + 1,
            task.agent,
            task.model
                .as_ref()
                .map(|model| format!(" / {model}"))
                .unwrap_or_default(),
            task.effort
                .as_ref()
                .map(|effort| format!(" / {effort}"))
                .unwrap_or_default(),
            task.profile
                .as_ref()
                .map(|profile| format!(" / profile:{profile}"))
                .unwrap_or_default()
        ));
        lines.push(format!("   {}", task.task));
        if !task.depends_on.is_empty() {
            lines.push(format!("   Starts after {}.", task.depends_on.join(", ")));
        }
        if !task.context_files.is_empty() {
            lines.push(format!("   Context: {}.", task.context_files.join(", ")));
        }
        if let Some(output_path) = extras
            .get(&task.task_id)
            .and_then(|extra| extra.output_path.as_ref())
        {
            lines.push(format!("   Output: {output_path}."));
        }
        if let Some(extra) = extras.get(&task.task_id) {
            for assumption in &extra.assumptions {
                lines.push(format!("   Assumption: {assumption}"));
            }
        }
    }

    if let Some(escalation) = spec.escalation.as_ref() {
        lines.extend([
            String::new(),
            "Escalation".to_string(),
            format!(
                "Escalates after {} unresolved generation(s).",
                escalation.after_unresolved_generations
            ),
        ]);
    }

    lines.extend([
        String::new(),
        "Controls".to_string(),
        "No loop has been created yet.".to_string(),
        format!("Original command: {original_command}"),
    ]);

    if !warnings.is_empty() {
        lines.extend([String::new(), "Warnings".to_string()]);
        lines.extend(warnings.iter().map(|warning| format!("- {warning}")));
    }

    lines.join("\n")
}

fn mentions_wall_clock(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("9am")
        || lower.contains("9:00")
        || lower.contains("a.m.")
        || lower.contains("p.m.")
        || lower.contains(" am")
        || lower.contains(" pm")
}

fn error_output(errors: Vec<String>, warnings: Vec<String>) -> SpurLoopDoctorOutput {
    SpurLoopDoctorOutput {
        status: "error".to_string(),
        friendly_preview: String::new(),
        warnings,
        errors,
        canonical_submit_loop_params: None,
        approval_fingerprint: None,
        client_idempotency_key: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_schemas::{LoopDoctorDraft, SpurLoopDoctorParams};

    fn task(task_id: &str, task: &str) -> LoopDoctorDraftTask {
        LoopDoctorDraftTask {
            task_id: Some(task_id.to_string()),
            agent: "codex".to_string(),
            profile: None,
            model: None,
            effort: None,
            config_overrides: None,
            task: task.to_string(),
            depends_on: Vec::new(),
            context_files: Vec::new(),
            triage: false,
            labels: Vec::new(),
            issue_labels: Vec::new(),
            output_path: None,
            assumptions: Vec::new(),
        }
    }

    fn valid_params() -> SpurLoopDoctorParams {
        let mut triage = task("triage", "Triage the current CI state");
        triage.triage = true;
        SpurLoopDoctorParams {
            original_command: "/spur-loop daily keep CI green".to_string(),
            draft: LoopDoctorDraft {
                goal: "Keep CI green".to_string(),
                pattern: Some("ci-sweeper".to_string()),
                cadence_secs: Some(86_400),
                schedule_description: Some("Runs every 24 hours.".to_string()),
                autonomy: Some("l1".to_string()),
                tasks: vec![triage],
                governors: Default::default(),
                escalation: None,
                assumptions: Vec::new(),
            },
        }
    }

    #[test]
    fn valid_draft_returns_canonical_params_and_fingerprint() {
        let output = run(valid_params());

        assert_eq!(output.status, "ok");
        assert!(output.errors.is_empty());
        assert!(output.friendly_preview.contains("Keep CI green"));
        let canonical = output
            .canonical_submit_loop_params
            .as_ref()
            .expect("valid draft must return canonical params");
        assert_eq!(canonical.spec.goal, "Keep CI green");
        assert_eq!(canonical.spec.cadence_secs, 86_400);
        assert_eq!(canonical.spec.loop_id, "");
        assert_eq!(
            output.client_idempotency_key.as_deref(),
            output
                .approval_fingerprint
                .as_ref()
                .map(|fingerprint| format!("spur-loop:{fingerprint}"))
                .as_deref()
        );
    }

    #[test]
    fn triage_marker_emits_loop_triage_label_in_raw_template_json() {
        let output = run(valid_params());
        let canonical = output
            .canonical_submit_loop_params
            .expect("canonical params");
        let labels = canonical.spec.template["tasks"][0]["labels"]
            .as_array()
            .expect("labels array");

        assert!(
            labels
                .iter()
                .any(|label| label.as_str() == Some(LOOP_TRIAGE_TASK)),
            "canonical raw template must preserve loop triage label: {:?}",
            canonical.spec.template
        );
    }

    #[test]
    fn missing_goal_blocks_without_canonical_params() {
        let mut params = valid_params();
        params.draft.goal = "   ".to_string();

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output.errors.iter().any(|error| error.contains("goal")),
            "expected goal error, got {:?}",
            output.errors
        );
        assert!(output.canonical_submit_loop_params.is_none());
        assert!(output.approval_fingerprint.is_none());
        assert!(output.client_idempotency_key.is_none());
    }

    #[test]
    fn missing_cadence_blocks() {
        let mut params = valid_params();
        params.draft.cadence_secs = None;

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.contains("cadence_secs")),
            "expected cadence error, got {:?}",
            output.errors
        );
        assert!(output.canonical_submit_loop_params.is_none());
    }

    #[test]
    fn missing_tasks_blocks() {
        let mut params = valid_params();
        params.draft.tasks.clear();

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output.errors.iter().any(|error| error.contains("tasks")),
            "expected tasks error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn missing_triage_task_blocks() {
        let mut params = valid_params();
        params.draft.tasks[0].triage = false;

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output.errors.iter().any(|error| error.contains("triage")),
            "expected triage error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn invalid_autonomy_blocks() {
        let mut params = valid_params();
        params.draft.autonomy = Some("l4".to_string());

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output.errors.iter().any(|error| error.contains("autonomy")),
            "expected autonomy error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn non_positive_governor_cap_blocks() {
        let mut params = valid_params();
        params.draft.governors.max_tasks_per_generation = Some(0);

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.contains("max_tasks_per_generation")),
            "expected governor error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn missing_dependency_blocks_via_shared_plan_validation() {
        let mut params = valid_params();
        params.draft.tasks.push(LoopDoctorDraftTask {
            depends_on: vec!["missing".to_string()],
            ..task("summary", "Summarize the CI result")
        });

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output
                .errors
                .iter()
                .any(|error| error.contains("unknown task")),
            "expected dependency validation error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn cyclic_dependency_blocks_via_shared_plan_validation() {
        let mut params = valid_params();
        params.draft.tasks[0].depends_on = vec!["summary".to_string()];
        params.draft.tasks.push(LoopDoctorDraftTask {
            depends_on: vec!["triage".to_string()],
            ..task("summary", "Summarize the CI result")
        });

        let output = run(params);

        assert_eq!(output.status, "error");
        assert!(
            output.errors.iter().any(|error| error.contains("Cycle")),
            "expected cycle validation error, got {:?}",
            output.errors
        );
    }

    #[test]
    fn fingerprint_ignores_raw_wording_but_changes_with_canonical_params() {
        let first = run(valid_params());
        let mut same_behavior = valid_params();
        same_behavior.original_command = "/spur-loop every day keep CI green please".to_string();
        let second = run(same_behavior);
        let mut changed = valid_params();
        changed.draft.cadence_secs = Some(3_600);
        let third = run(changed);

        assert_eq!(first.approval_fingerprint, second.approval_fingerprint);
        assert_ne!(first.approval_fingerprint, third.approval_fingerprint);
    }

    #[test]
    fn fingerprint_excludes_server_minted_loop_id() {
        let output = run(valid_params());
        let mut canonical = output
            .canonical_submit_loop_params
            .expect("canonical params");
        let before = approval_fingerprint(&canonical);
        canonical.spec.loop_id = "server-minted".to_string();
        let after = approval_fingerprint(&canonical);

        assert_eq!(before, after);
    }

    #[test]
    fn escalation_preview_shows_concrete_threshold() {
        let mut params = valid_params();
        params.draft.escalation = Some(crate::plan::loops::spec::LoopEscalation {
            after_unresolved_generations: 3,
        });

        let output = run(params);

        assert_eq!(output.status, "ok");
        assert!(
            output.friendly_preview.contains("3 unresolved"),
            "expected escalation threshold in preview, got {:?}",
            output.friendly_preview
        );
    }

    #[test]
    fn wall_clock_and_direct_l3_are_warnings_not_blockers() {
        let mut params = valid_params();
        params.original_command = "/spur-loop daily 9AM L3 keep CI green".to_string();
        params.draft.autonomy = Some("L3".to_string());
        params.draft.tasks[0].model = Some("gpt-5.3-spark".to_string());
        params.draft.tasks[0].effort = Some("max".to_string());

        let output = run(params);

        assert_eq!(output.status, "warnings");
        assert!(output.canonical_submit_loop_params.is_some());
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("wall-clock")),
            "expected wall-clock warning, got {:?}",
            output.warnings
        );
        assert!(
            output.warnings.iter().any(|warning| warning.contains("L3")),
            "expected direct L3 warning, got {:?}",
            output.warnings
        );
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning.contains("model")),
            "expected model pass-through warning, got {:?}",
            output.warnings
        );
    }
}
