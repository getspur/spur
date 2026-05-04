use std::collections::HashMap;

pub fn descendant_depth(root_id: &str, id: &str) -> Option<usize> {
    let suffix = id.strip_prefix(root_id)?;
    let suffix = suffix.strip_prefix('.')?;
    Some(suffix.split('.').count())
}

pub fn has_label(labels: &[String], expected: &str) -> bool {
    labels.iter().any(|label| label == expected)
}

pub fn has_plan_task_label(labels: &[String]) -> bool {
    labels
        .iter()
        .any(|label| label.starts_with("spur:plan-task-id:"))
}

pub fn find_plan_id_label(labels: &[String]) -> Option<&str> {
    labels
        .iter()
        .find_map(|label| label.strip_prefix("spur:plan-id:"))
}

pub fn insert_parent_id<'a>(
    parent_by_child_id: &mut HashMap<&'a str, &'a str>,
    child_id: &'a str,
    parent_id: &'a str,
) {
    parent_by_child_id
        .entry(child_id)
        .and_modify(|existing_parent_id| {
            if parent_id < *existing_parent_id {
                *existing_parent_id = parent_id;
            }
        })
        .or_insert(parent_id);
}

pub fn status_icon(status: &str) -> &'static str {
    match status {
        "open" => "○",
        "in_progress" => "●",
        "blocked" => "!",
        "closed" => "✓",
        _ => "○",
    }
}
