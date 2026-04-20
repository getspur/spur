//! Label vocabulary for SPUR plan tracking in beads.
//!
//! Every label emitted by brain / worker / reconciler MUST come from a helper
//! in this module. String-typing labels at the call site is a bug waiting to
//! happen — use these constructors instead.
//!
//! See `docs/superpowers/specs/2026-04-20-adaptive-plan-repair-design.md`
//! §Information Flow → Label vocabulary for the authoritative list.

pub fn plan_id(plan_id: &str) -> String {
    format!("spur.plan_id={plan_id}")
}

pub fn plan_task_id(task_id: &str) -> String {
    format!("spur.plan_task_id={task_id}")
}

pub fn delegation_id(delegation_id: &str) -> String {
    format!("delegation-id:{delegation_id}")
}

pub fn signal_kind(kind: &str) -> String {
    format!("signal:{kind}")
}

pub fn signal_kind_bucket(kind: &str, bucket: &str) -> String {
    format!("signal:{kind}:{bucket}")
}

pub const SIGNAL_LATE_ARRIVAL: &str = "signal:late-arrival";
pub const READY_FOR_REVIEW: &str = "ready-for-review";

pub fn mutation_id(mutation_id: &uuid::Uuid) -> String {
    format!("mutation-id:{mutation_id}")
}

pub fn superseded_by(child_ids: &[String]) -> String {
    format!("superseded-by:{}", child_ids.join(","))
}

/// Returns `Some(task_id)` if the given label is a `spur.plan_task_id=<id>` label.
pub fn parse_plan_task_id(label: &str) -> Option<&str> {
    label.strip_prefix("spur.plan_task_id=")
}

/// Returns `Some(plan_id)` if the given label is a `spur.plan_id=<id>` label.
pub fn parse_plan_id(label: &str) -> Option<&str> {
    label.strip_prefix("spur.plan_id=")
}

/// Returns `Some(kind)` if the given label is a `signal:<kind>` label
/// (not a bucketed variant `signal:<kind>:<bucket>`).
pub fn parse_signal_kind(label: &str) -> Option<&str> {
    let rest = label.strip_prefix("signal:")?;
    if rest.contains(':') {
        None
    } else {
        Some(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_produce_expected_strings() {
        assert_eq!(plan_id("P1"), "spur.plan_id=P1");
        assert_eq!(plan_task_id("T1"), "spur.plan_task_id=T1");
        assert_eq!(delegation_id("del-A"), "delegation-id:del-A");
        assert_eq!(signal_kind("scope-drift"), "signal:scope-drift");
        assert_eq!(
            signal_kind_bucket("scope-drift", "high"),
            "signal:scope-drift:high"
        );
        assert_eq!(SIGNAL_LATE_ARRIVAL, "signal:late-arrival");
    }

    #[test]
    fn parsers_invert_constructors() {
        let p = plan_task_id("T1");
        assert_eq!(parse_plan_task_id(&p), Some("T1"));
        assert_eq!(parse_plan_task_id("unrelated"), None);
        let plan = plan_id("P1");
        assert_eq!(parse_plan_id(&plan), Some("P1"));
        assert_eq!(parse_signal_kind("signal:scope-drift"), Some("scope-drift"));
        assert_eq!(parse_signal_kind("signal:scope-drift:high"), None);
    }

    #[test]
    fn superseded_by_joins_ids_with_comma() {
        assert_eq!(
            superseded_by(&["bd-1".into(), "bd-2".into(), "bd-3".into()]),
            "superseded-by:bd-1,bd-2,bd-3"
        );
    }
}
