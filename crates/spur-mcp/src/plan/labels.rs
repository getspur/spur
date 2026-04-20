//! Label vocabulary for SPUR plan tracking in beads.
//!
//! Every label emitted by brain / worker / reconciler MUST come from a helper
//! in this module. String-typing labels at the call site is a bug waiting to
//! happen — use these constructors instead.
//!
//! # Grammar constraint
//!
//! `br 0.1.14` enforces label grammar `[A-Za-z0-9_:-]+` (empirically verified
//! via `br label add` — `VALIDATION_FAILED` error surface). Labels containing
//! `.`, `=`, `/`, or whitespace are rejected. All constructors in this module
//! produce br-legal labels. Callers supplying raw components (plan IDs, task
//! IDs, agent names) are responsible for ensuring those components use only
//! `[A-Za-z0-9_:-]` characters.
//!
//! See `docs/superpowers/specs/2026-04-20-adaptive-plan-repair-design.md`
//! §Information Flow → Label vocabulary for the authoritative list.

pub fn plan_id(plan_id: &str) -> String {
    format!("spur:plan-id:{plan_id}")
}

pub fn plan_task_id(task_id: &str) -> String {
    format!("spur:plan-task-id:{task_id}")
}

pub fn agent(agent_name: &str) -> String {
    format!("spur:agent:{agent_name}")
}

pub fn source_issue(issue_id: &str) -> String {
    format!("spur:source-issue:{issue_id}")
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
/// Marker applied to an epic after `build_epic_subgraph` successfully creates
/// ALL children + dependency edges. The v0a.2 reconciler filters on this label
/// to avoid observing partially-persisted plan graphs as ready work.
/// If creation fails mid-loop, the epic will NOT carry this label.
pub const PLAN_COMPLETE: &str = "spur:plan-complete";

pub fn mutation_id(mutation_id: &uuid::Uuid) -> String {
    format!("mutation-id:{mutation_id}")
}

/// Prefix strings for parsing. Use these with `label_value()` or `strip_prefix()`.
pub const PLAN_ID_PREFIX: &str = "spur:plan-id:";
pub const PLAN_TASK_ID_PREFIX: &str = "spur:plan-task-id:";
pub const AGENT_PREFIX: &str = "spur:agent:";
pub const SOURCE_ISSUE_PREFIX: &str = "spur:source-issue:";

/// Returns `Some(task_id)` if the given label is a `spur:plan-task-id:<id>` label.
pub fn parse_plan_task_id(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_TASK_ID_PREFIX)
}

/// Returns `Some(plan_id)` if the given label is a `spur:plan-id:<id>` label.
pub fn parse_plan_id(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_ID_PREFIX)
}

/// Returns `Some(agent_name)` if the given label is a `spur:agent:<name>` label.
pub fn parse_agent(label: &str) -> Option<&str> {
    label.strip_prefix(AGENT_PREFIX)
}

/// Returns `Some(issue_id)` if the given label is a `spur:source-issue:<id>` label.
pub fn parse_source_issue(label: &str) -> Option<&str> {
    label.strip_prefix(SOURCE_ISSUE_PREFIX)
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
        assert_eq!(plan_id("P1"), "spur:plan-id:P1");
        assert_eq!(plan_task_id("T1"), "spur:plan-task-id:T1");
        assert_eq!(agent("codex"), "spur:agent:codex");
        assert_eq!(source_issue("bd-42"), "spur:source-issue:bd-42");
        assert_eq!(delegation_id("del-A"), "delegation-id:del-A");
        assert_eq!(signal_kind("scope-drift"), "signal:scope-drift");
        assert_eq!(
            signal_kind_bucket("scope-drift", "high"),
            "signal:scope-drift:high"
        );
        assert_eq!(SIGNAL_LATE_ARRIVAL, "signal:late-arrival");
        assert_eq!(PLAN_COMPLETE, "spur:plan-complete");
    }

    #[test]
    fn parsers_invert_constructors() {
        assert_eq!(parse_plan_task_id(&plan_task_id("T1")), Some("T1"));
        assert_eq!(parse_plan_task_id("unrelated"), None);
        assert_eq!(parse_plan_id(&plan_id("P1")), Some("P1"));
        assert_eq!(parse_agent(&agent("codex")), Some("codex"));
        assert_eq!(parse_source_issue(&source_issue("bd-42")), Some("bd-42"));
        assert_eq!(parse_signal_kind("signal:scope-drift"), Some("scope-drift"));
        assert_eq!(parse_signal_kind("signal:scope-drift:high"), None);
    }

    /// `br 0.1.14` label grammar, verified empirically via
    /// `br label add` `VALIDATION_FAILED` error:
    /// `^[A-Za-z0-9_:-]+$` — alphanumeric, dash, underscore, colon only.
    fn is_br_legal(label: &str) -> bool {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    }

    #[test]
    fn constructors_emit_br_legal_labels() {
        for s in [
            plan_id("P1"),
            plan_task_id("T1"),
            agent("claude-code-acp"),
            source_issue("bd-42"),
            delegation_id("del-A"),
            signal_kind("scope-drift"),
            signal_kind_bucket("scope-drift", "high"),
            mutation_id(&uuid::Uuid::nil()),
        ] {
            assert!(is_br_legal(&s), "constructor emitted br-illegal label: {s}");
        }
        assert!(
            is_br_legal(PLAN_COMPLETE),
            "PLAN_COMPLETE is br-illegal: {PLAN_COMPLETE}"
        );
    }

    #[test]
    fn is_br_legal_matches_empirical_grammar() {
        // Positive cases (verified against real `br label add`):
        assert!(is_br_legal("alpha1"));
        assert!(is_br_legal("with-dash"));
        assert!(is_br_legal("with_under"));
        assert!(is_br_legal("with:colon"));
        assert!(is_br_legal("mix-ed:under_score1"));
        assert!(is_br_legal("UPPER"));
        assert!(is_br_legal("123-4"));
        // Negative cases (verified rejected by real `br label add`):
        assert!(!is_br_legal("with.dot"));
        assert!(!is_br_legal("with=eq"));
        assert!(!is_br_legal("with/slash"));
        assert!(!is_br_legal("with space"));
        assert!(!is_br_legal(""));
    }
}
