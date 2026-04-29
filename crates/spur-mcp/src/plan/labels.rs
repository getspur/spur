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
//! # Length cap (asymmetric)
//!
//! `br create --label <label>` enforces a **50-character cap** — longer labels
//! surface as `Validation failed: label: exceeds 50 characters`. `br label add`
//! imposes no such cap (accepts labels up to at least 512 chars). Constructors
//! that MAY be used at create time (`mutation_id_label`, `signal_processed_label`)
//! use the compact UUID form (32 hex chars, no hyphens) to stay under the cap.
//! This asymmetry is pinned by `labels_br_round_trip::br_create_enforces_50_char_cap`.
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

pub const DELEGATION_ID_PREFIX: &str = "spur:delegation-id:";

pub fn delegation_id(delegation_id: &str) -> String {
    format!("{DELEGATION_ID_PREFIX}{delegation_id}")
}

pub fn signal_kind(kind: &str) -> String {
    format!("signal:{kind}")
}

pub fn signal_kind_bucket(kind: &str, bucket: &str) -> String {
    format!("signal:{kind}:{bucket}")
}

pub const SIGNAL_LATE_ARRIVAL: &str = "signal:late-arrival";
pub const READY_FOR_REVIEW: &str = "spur:ready-for-review";
pub const REVIEW_REJECTED: &str = "spur:review-rejected";
/// Marker applied to an epic after `build_epic_subgraph` successfully creates
/// ALL children + dependency edges. The v0a.2 reconciler filters on this label
/// to avoid observing partially-persisted plan graphs as ready work.
/// If creation fails mid-loop, the epic will NOT carry this label.
pub const PLAN_COMPLETE: &str = "spur:plan-complete";
/// Marker applied to an epic while `build_epic_subgraph` is still creating
/// children + dependency edges. The reconciler must not dispatch tasks from a
/// plan while this marker is present.
pub const PLAN_PENDING: &str = "spur:plan-pending";
pub const INTEGRATION_PENDING: &str = "spur:integration-pending";

/// Prefix strings for parsing. Use these with `label_value()` or `strip_prefix()`.
pub const PLAN_ID_PREFIX: &str = "spur:plan-id:";
pub const PLAN_TASK_ID_PREFIX: &str = "spur:plan-task-id:";
pub const AGENT_PREFIX: &str = "spur:agent:";
pub const SOURCE_ISSUE_PREFIX: &str = "spur:source-issue:";

pub fn parse_delegation_id(label: &str) -> Option<&str> {
    label.strip_prefix(DELEGATION_ID_PREFIX)
}

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

/// Label marker set on beads issues created as part of a mutation batch.
/// Uses the compact (hyphen-free) UUID form: `br create --label` enforces a
/// 50-character cap (verified via `labels_br_round_trip.rs`), while
/// `br label add` does not. The compact form keeps a single label shape
/// across both code paths.
/// Example: `spur:mutation-id:f30c1a2e...` (total 41 chars).
pub fn mutation_id_label(mutation_id: &uuid::Uuid) -> String {
    format!("spur:mutation-id:{}", mutation_id.simple())
}

/// Labels attached to the SUPERSEDED parent task, one per replacement child.
/// Beads labels don't allow commas, pipes, or other common separators, so we
/// emit one label per child (labels are a set in beads — the idiomatic form).
/// Query via `br list --label-any spur:superseded-by:<child>`.
/// Example: `["spur:superseded-by:bd-201", "spur:superseded-by:bd-202"]`
pub fn superseded_by_labels(child_ids: &[String]) -> Vec<String> {
    child_ids
        .iter()
        .map(|id| format!("spur:superseded-by:{id}"))
        .collect()
}

/// Label set after a proposer consumes a signal. Preserves the original
/// `signal:<kind>` label for historical filtering. Uses the compact UUID
/// form for consistency with `mutation_id_label`.
///
/// Durable dedup is keyed by the triggering signal's `signal_id`, not by the
/// mutation or the issue as a whole. That allows distinct signals on one task
/// to be processed independently over time.
///
/// **Only safe via `br label add` (IssueUpdate.add_labels)**, not via
/// `br create --label`: `spur:signal-processed:` is a 22-char prefix, which
/// combined with the 32-char compact UUID totals 54 chars — over the 50-char
/// create-path cap. Callers at create time must use `mutation_id_label` instead.
/// Example: `spur:signal-processed:f30c1a2e...` (total 54 chars).
pub fn signal_processed_label(signal_id: &uuid::Uuid) -> String {
    format!("spur:signal-processed:{}", signal_id.simple())
}

/// Beads audit-reference label for a peer mailbox message.
///
/// Format: `spur:peer:{compact_uuid}` (42 chars). Fits the 50-char
/// `br create --label` cap, unlike `signal_processed_label`.
pub fn peer_message_label(message_id: &uuid::Uuid) -> String {
    format!("spur:peer:{}", message_id.simple())
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
        assert_eq!(delegation_id("del-A"), "spur:delegation-id:del-A");
        assert_eq!(signal_kind("scope-drift"), "signal:scope-drift");
        assert_eq!(
            signal_kind_bucket("scope-drift", "high"),
            "signal:scope-drift:high"
        );
        assert_eq!(SIGNAL_LATE_ARRIVAL, "signal:late-arrival");
        assert_eq!(READY_FOR_REVIEW, "spur:ready-for-review");
        assert_eq!(REVIEW_REJECTED, "spur:review-rejected");
        assert_eq!(PLAN_COMPLETE, "spur:plan-complete");
        assert_eq!(PLAN_PENDING, "spur:plan-pending");
        assert_eq!(INTEGRATION_PENDING, "spur:integration-pending");
    }

    #[test]
    fn parsers_invert_constructors() {
        assert_eq!(parse_plan_task_id(&plan_task_id("T1")), Some("T1"));
        assert_eq!(parse_plan_task_id("unrelated"), None);
        assert_eq!(parse_plan_id(&plan_id("P1")), Some("P1"));
        assert_eq!(parse_agent(&agent("codex")), Some("codex"));
        assert_eq!(parse_source_issue(&source_issue("bd-42")), Some("bd-42"));
        assert_eq!(parse_delegation_id(&delegation_id("del-A")), Some("del-A"));
        assert_eq!(parse_signal_kind("signal:scope-drift"), Some("scope-drift"));
        assert_eq!(parse_signal_kind("signal:scope-drift:high"), None);
    }

    #[test]
    fn delegation_and_review_labels_use_spur_namespace() {
        assert_eq!(delegation_id("del-A"), "spur:delegation-id:del-A");
        assert_eq!(
            parse_delegation_id("spur:delegation-id:del-A"),
            Some("del-A")
        );
        assert_eq!(READY_FOR_REVIEW, "spur:ready-for-review");
        assert_eq!(REVIEW_REJECTED, "spur:review-rejected");
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
            mutation_id_label(&uuid::Uuid::nil()),
            signal_processed_label(&uuid::Uuid::nil()),
            READY_FOR_REVIEW.to_string(),
            REVIEW_REJECTED.to_string(),
            PLAN_PENDING.to_string(),
            INTEGRATION_PENDING.to_string(),
        ] {
            assert!(is_br_legal(&s), "constructor emitted br-illegal label: {s}");
        }
        assert!(
            is_br_legal(PLAN_COMPLETE),
            "PLAN_COMPLETE is br-illegal: {PLAN_COMPLETE}"
        );
    }

    #[test]
    fn integration_pending_label_is_br_legal() {
        assert_eq!(INTEGRATION_PENDING, "spur:integration-pending");
        assert!(INTEGRATION_PENDING
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':')));
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

    #[test]
    fn mutation_and_signal_labels_round_trip_br_grammar() {
        let id = uuid::Uuid::new_v4();
        let label = mutation_id_label(&id);
        // br requires kebab-case + single `:` domain separator
        assert!(label.starts_with("spur:mutation-id:"));
        assert!(!label.contains(','));

        let by = superseded_by_labels(&["bd-201".into(), "bd-202".into()]);
        assert_eq!(
            by,
            vec![
                "spur:superseded-by:bd-201".to_string(),
                "spur:superseded-by:bd-202".to_string(),
            ]
        );

        let p = signal_processed_label(&id);
        assert!(p.starts_with("spur:signal-processed:"));
        assert!(!p.contains(','));

        // All new labels must be br-legal.
        assert!(is_br_legal(&label));
        for child_label in &by {
            assert!(is_br_legal(child_label));
        }
        assert!(is_br_legal(&p));
    }

    #[test]
    fn peer_message_label_is_under_50_chars_and_uses_compact_uuid() {
        let id = uuid::Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
        let label = peer_message_label(&id);
        assert_eq!(label, "spur:peer:0123456789abcdef0123456789abcdef");
        assert!(
            label.len() <= 50,
            "label exceeds 50-char br create cap: {} chars",
            label.len()
        );
        // Grammar: [A-Za-z0-9_:-]+
        assert!(label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-'));
    }
}
