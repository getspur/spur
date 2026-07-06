use super::spec::LoopSpec;
use serde_json::Value;

pub(crate) fn validate_loop_spec_for_submit(spec: &LoopSpec) -> Result<(), String> {
    if spec.goal.trim().is_empty() {
        return Err("goal must be non-empty".to_string());
    }
    if spec.cadence_secs < 60 {
        return Err("cadence_secs must be at least 60".to_string());
    }
    if !template_has_triage_task(&spec.template) {
        return Err(format!(
            "template must contain at least one triage task labeled {}",
            crate::plan::labels::LOOP_TRIAGE_TASK
        ));
    }
    if matches!(spec.governors.max_cost_micros_per_generation, Some(0)) {
        return Err("max_cost_micros_per_generation must be greater than 0".to_string());
    }
    if matches!(spec.governors.max_generations_per_day, Some(0)) {
        return Err("max_generations_per_day must be greater than 0".to_string());
    }
    if matches!(spec.governors.max_tasks_per_generation, Some(0)) {
        return Err("max_tasks_per_generation must be greater than 0".to_string());
    }
    if let Some(backoff) = spec.governors.consecutive_failure_backoff.as_ref() {
        if backoff.k == 0 {
            return Err("consecutive_failure_backoff.k must be greater than 0".to_string());
        }
        if backoff.factor == 0 {
            return Err("consecutive_failure_backoff.factor must be greater than 0".to_string());
        }
        if backoff.auto_pause_after == 0 {
            return Err(
                "consecutive_failure_backoff.auto_pause_after must be greater than 0".to_string(),
            );
        }
    }
    Ok(())
}

pub(crate) fn template_has_triage_task(template: &Value) -> bool {
    template
        .get("tasks")
        .and_then(Value::as_array)
        .is_some_and(|tasks| tasks.iter().any(task_has_triage_label))
}

fn task_has_triage_label(task: &Value) -> bool {
    ["labels", "issue_labels"].iter().any(|key| {
        task.get(*key)
            .and_then(Value::as_array)
            .is_some_and(|labels| {
                labels
                    .iter()
                    .any(|label| label.as_str() == Some(crate::plan::labels::LOOP_TRIAGE_TASK))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::labels::{AutonomyLevel, LOOP_TRIAGE_TASK};
    use crate::plan::loops::spec::{FailureBackoff, LoopGovernors};
    use serde_json::json;

    fn valid_spec() -> LoopSpec {
        LoopSpec {
            loop_id: String::new(),
            goal: "Keep CI green".to_string(),
            pattern: None,
            cadence_secs: 60,
            autonomy: AutonomyLevel::L1,
            template: json!({
                "tasks": [{
                    "task_id": "triage",
                    "agent": "codex",
                    "task": "Triage the loop state",
                    "labels": [LOOP_TRIAGE_TASK],
                }]
            }),
            governors: LoopGovernors {
                max_cost_micros_per_generation: Some(1),
                max_generations_per_day: Some(1),
                max_tasks_per_generation: Some(1),
                denylist_globs: Vec::new(),
                consecutive_failure_backoff: Some(FailureBackoff {
                    k: 1,
                    factor: 1,
                    auto_pause_after: 1,
                }),
            },
            escalation: None,
        }
    }

    #[test]
    fn accepts_valid_loop_spec_with_triage_label() {
        assert_eq!(validate_loop_spec_for_submit(&valid_spec()), Ok(()));
    }

    #[test]
    fn accepts_triage_task_from_issue_labels() {
        let mut spec = valid_spec();
        spec.template["tasks"][0]["labels"] = json!([]);
        spec.template["tasks"][0]["issue_labels"] = json!([LOOP_TRIAGE_TASK]);

        assert_eq!(validate_loop_spec_for_submit(&spec), Ok(()));
    }

    #[test]
    fn rejects_template_without_triage_task() {
        let mut spec = valid_spec();
        spec.template["tasks"][0]["labels"] = json!([]);

        let error = validate_loop_spec_for_submit(&spec).expect_err("missing triage must fail");

        assert_eq!(
            error,
            format!("template must contain at least one triage task labeled {LOOP_TRIAGE_TASK}")
        );
    }

    #[test]
    fn rejects_zero_backoff_governor_fields() {
        let mut spec = valid_spec();
        spec.governors.consecutive_failure_backoff = Some(FailureBackoff {
            k: 1,
            factor: 1,
            auto_pause_after: 0,
        });

        let error = validate_loop_spec_for_submit(&spec).expect_err("zero backoff must fail");

        assert_eq!(
            error,
            "consecutive_failure_backoff.auto_pause_after must be greater than 0"
        );
    }
}
