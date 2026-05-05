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

use sha2::{Digest, Sha256};

pub fn plan_id(plan_id: &str) -> String {
    format!("spur:plan-id:{plan_id}")
}

/// Token used to encode the `.` character (which the br label validator
/// rejects) inside label values that carry external identifiers. Beads issue
/// IDs are hierarchical (`bd-2dww.7`) once an issue gets a child, so any
/// label embedding such an id MUST round-trip through this encoding to remain
/// br-legal while preserving the original id for downstream consumers
/// (projector, reconciler).
const LABEL_ID_DOT_ENCODED: &str = "_dot_";

/// Encode external ids (beads or otherwise) for safe embedding in br labels.
/// `bd-42` → `bd-42` (no change). `bd-2dww.7` → `bd-2dww_dot_7`.
fn encode_label_id(id: &str) -> String {
    id.replace('.', LABEL_ID_DOT_ENCODED)
}

/// Reverse of `encode_label_id`. Allocates only when the encoded sentinel
/// is actually present.
fn decode_label_id(encoded: &str) -> String {
    encoded.replace(LABEL_ID_DOT_ENCODED, ".")
}

pub fn plan_task_id(task_id: &str) -> String {
    format!("spur:plan-task-id:{}", encode_label_id(task_id))
}

pub fn agent(agent_name: &str) -> String {
    format!("spur:agent:{agent_name}")
}

pub fn source_issue(issue_id: &str) -> String {
    format!("spur:source-issue:{}", encode_label_id(issue_id))
}

pub const DELEGATION_ID_PREFIX: &str = "spur:delegation-id:";
pub const LEASE_EXPIRES_AT_PREFIX: &str = "spur:lease-expires-at:";
pub const PLAN_OWNER_PREFIX: &str = "spur:plan-owner:";
pub const PLAN_OWNER_TOKEN_PREFIX: &str = "spur:plan-owner-token:";
pub const PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX: &str = "spur:plan-owner-lease-expires-at:";

fn assert_br_legal_compact_component(component: &str) {
    assert!(
        !component.is_empty()
            && component
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':')),
        "compacted br label component must be non-empty and contain only ASCII alphanumeric, dash, underscore, or colon characters: {component:?}"
    );
}

pub fn compact_label_component(value: &str) -> String {
    let compacted = value.replace('-', "");
    assert_br_legal_compact_component(&compacted);
    compacted
}

pub fn delegation_id(delegation_id: &str) -> String {
    format!("{DELEGATION_ID_PREFIX}{delegation_id}")
}

/// Mint a fresh 16-char hex delegation_id derived from a v4 UUID. The 16-char
/// length keeps `spur:delegation-id:<id>` (35 chars) under the `br create`
/// 50-char cap.
///
/// 60 bits of effective entropy: the high 64 bits of a v4 UUID include the
/// 4-bit version nibble (RFC 4122 §4.4 byte 6 upper nibble = `0100`), which
/// lands at hex position 12 of the output (always `'4'`). The remaining 60
/// random bits → ~1.15e18 values → birthday-collision-immune for any single
/// project's dispatch lifetime.
///
/// Output is `[0-9a-f]{16}` — `br`-legal under the `[A-Za-z0-9_:-]+` grammar.
pub fn mint_delegation_id() -> String {
    let uuid = uuid::Uuid::new_v4();
    // Take the high 64 bits of the UUID (git-hash-short style: a prefix of the
    // 128-bit form).
    let high = uuid.as_u128() >> 64;
    format!("{high:016x}")
}

/// Derive a stable 16-char hex `BrainSessionId` from an ACP `SessionId` via
/// truncated SHA-256. Deterministic - the same ACP session_id always maps to
/// the same BrainSessionId, so plan-owner labels match across spur restarts
/// when the user resumes the same ACP session.
///
/// Output: `[0-9a-f]{16}` - `br`-legal under `[A-Za-z0-9_:-]+`. The 19-char
/// `spur:plan-owner:` prefix + 16 hex = 35 chars, well under the 50-char
/// `br create --label` cap.
///
/// 64 bits of derived entropy from a SHA-256 prefix. Birthday-collision-immune
/// for any single project's brain-session lifetime.
pub fn derive_brain_session_id(acp_session_id: &spur_acp::SessionId) -> spur_acp::BrainSessionId {
    let mut hasher = Sha256::new();
    hasher.update(acp_session_id.0.as_bytes());
    let digest = hasher.finalize();
    // Take first 8 bytes (64 bits) -> 16 lowercase hex chars.
    let derived = digest
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        });
    spur_acp::BrainSessionId::new(spur_acp::SessionId(derived))
}

pub fn lease_expires_at(ts: i64) -> String {
    format!("{LEASE_EXPIRES_AT_PREFIX}{ts}")
}

pub fn plan_owner(owner: &str) -> String {
    format!("{PLAN_OWNER_PREFIX}{}", compact_label_component(owner))
}

/// Update-path only: prefix + compact UUID is 54 chars, exceeding the
/// 50-character `br create --label` cap.
pub fn plan_owner_token(token: &str) -> String {
    format!(
        "{PLAN_OWNER_TOKEN_PREFIX}{}",
        compact_label_component(token)
    )
}

pub fn plan_owner_lease_expires_at(ts: i64) -> String {
    format!("{PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX}{ts}")
}

pub fn signal_kind(kind: &str) -> String {
    format!("signal:{kind}")
}

pub fn signal_kind_bucket(kind: &str, bucket: &str) -> String {
    format!("signal:{kind}:{bucket}")
}

pub const SIGNAL_LATE_ARRIVAL: &str = "signal:late-arrival";
pub const SIGNAL_LABEL_INTEGRATION_CONFLICT: &str = "signal:integration-conflict";
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

pub fn parse_lease_expires_at(label: &str) -> Option<i64> {
    label.strip_prefix(LEASE_EXPIRES_AT_PREFIX)?.parse().ok()
}

pub fn parse_plan_owner(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_OWNER_PREFIX)
}

pub fn parse_plan_owner_token(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_OWNER_TOKEN_PREFIX)
}

pub fn parse_plan_owner_lease_expires_at(label: &str) -> Option<i64> {
    label
        .strip_prefix(PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX)?
        .parse()
        .ok()
}

/// Returns `Some(task_id)` if the given label is a `spur:plan-task-id:<id>` label.
/// Reverses the dot-encoding applied by `plan_task_id` so hierarchical beads
/// IDs (`bd-2dww.7`) round-trip through the br label validator.
pub fn parse_plan_task_id(label: &str) -> Option<String> {
    label.strip_prefix(PLAN_TASK_ID_PREFIX).map(decode_label_id)
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
/// Reverses the dot-encoding applied by `source_issue` so hierarchical beads
/// IDs (`bd-2dww.7`) round-trip through the br label validator (bd-18vs).
pub fn parse_source_issue(label: &str) -> Option<String> {
    label.strip_prefix(SOURCE_ISSUE_PREFIX).map(decode_label_id)
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
        assert_eq!(
            lease_expires_at(1_777_777_777),
            "spur:lease-expires-at:1777777777"
        );
        assert_eq!(signal_kind("scope-drift"), "signal:scope-drift");
        assert_eq!(
            signal_kind_bucket("scope-drift", "high"),
            "signal:scope-drift:high"
        );
        assert_eq!(SIGNAL_LATE_ARRIVAL, "signal:late-arrival");
        assert_eq!(
            SIGNAL_LABEL_INTEGRATION_CONFLICT,
            "signal:integration-conflict"
        );
        assert_eq!(READY_FOR_REVIEW, "spur:ready-for-review");
        assert_eq!(REVIEW_REJECTED, "spur:review-rejected");
        assert_eq!(PLAN_COMPLETE, "spur:plan-complete");
        assert_eq!(PLAN_PENDING, "spur:plan-pending");
        assert_eq!(INTEGRATION_PENDING, "spur:integration-pending");
    }

    #[test]
    fn parsers_invert_constructors() {
        assert_eq!(
            parse_plan_task_id(&plan_task_id("T1")),
            Some("T1".to_string())
        );
        assert_eq!(parse_plan_task_id("unrelated"), None);
        assert_eq!(parse_plan_id(&plan_id("P1")), Some("P1"));
        assert_eq!(parse_agent(&agent("codex")), Some("codex"));
        assert_eq!(
            parse_source_issue(&source_issue("bd-42")).as_deref(),
            Some("bd-42")
        );
        assert_eq!(parse_delegation_id(&delegation_id("del-A")), Some("del-A"));
        assert_eq!(
            parse_lease_expires_at(&lease_expires_at(1_777_777_777)),
            Some(1_777_777_777)
        );
        assert_eq!(
            parse_lease_expires_at("spur:lease-expires-at:not-a-ts"),
            None
        );
        assert_eq!(parse_lease_expires_at("unrelated"), None);
        assert_eq!(parse_signal_kind("signal:scope-drift"), Some("scope-drift"));
        assert_eq!(parse_signal_kind("signal:scope-drift:high"), None);
    }

    #[test]
    fn plan_owner_labels_normalize_uuid_components() {
        assert_eq!(
            plan_owner("550e8400-e29b-41d4-a716-446655440000"),
            "spur:plan-owner:550e8400e29b41d4a716446655440000"
        );
        assert_eq!(
            plan_owner_token("7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a"),
            "spur:plan-owner-token:7c6258f16a674f6aa9b45ea1ef59ff7a"
        );
        assert_eq!(
            plan_owner_lease_expires_at(1_777_777_777),
            "spur:plan-owner-lease-expires-at:1777777777"
        );
    }

    #[test]
    fn compact_label_component_rejects_br_illegal_characters() {
        let result = std::panic::catch_unwind(|| compact_label_component("bad/value"));
        assert!(result.is_err());
    }

    #[test]
    fn plan_owner_token_documents_update_path_length() {
        assert!(plan_owner_token("7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a").len() > 50);
    }

    #[test]
    fn plan_owner_parsers_invert_constructors() {
        assert_eq!(
            parse_plan_owner(&plan_owner("550e8400-e29b-41d4-a716-446655440000")),
            Some("550e8400e29b41d4a716446655440000")
        );
        assert_eq!(
            parse_plan_owner_token(&plan_owner_token("7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a")),
            Some("7c6258f16a674f6aa9b45ea1ef59ff7a")
        );
        assert_eq!(
            parse_plan_owner_lease_expires_at(&plan_owner_lease_expires_at(1_777_777_777)),
            Some(1_777_777_777)
        );
        assert_eq!(parse_plan_owner("unrelated"), None);
        assert_eq!(parse_plan_owner_token("unrelated"), None);
        assert_eq!(parse_plan_owner_lease_expires_at("unrelated"), None);
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
            lease_expires_at(1_777_777_777),
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
    fn lease_expires_at_label_fits_create_time_cap() {
        let label = lease_expires_at(9_999_999_999);
        assert!(
            label.len() <= 50,
            "lease label exceeds br create cap: {label}"
        );
        assert!(is_br_legal(&label), "lease label is br-illegal: {label}");
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

    /// Regression for `execute_epic("bd-2dww")`: hierarchical beads issue IDs
    /// like `bd-2dww.7` contain a `.` which the `br 0.1.14` label validator
    /// rejects (`Validation failed: label: only alphanumeric, dash, underscore,
    /// and colon allowed`). `plan_task_id` must emit a br-legal label AND
    /// `parse_plan_task_id` must return the original `bd-2dww.7` so downstream
    /// code (projector, reconciler) can still round-trip the task identity.
    #[test]
    fn plan_task_id_label_round_trips_hierarchical_beads_ids() {
        let task_id = "bd-2dww.7";
        let label = plan_task_id(task_id);
        assert!(
            is_br_legal(&label),
            "plan_task_id emitted br-illegal label for hierarchical id: {label}"
        );
        assert_eq!(
            parse_plan_task_id(&label).as_deref(),
            Some(task_id),
            "round-trip failed for hierarchical id"
        );
    }

    /// Regression for bd-18vs: `source_issue` had the same dot-rejection bug as
    /// `plan_task_id`. When `submit_plan` payloads set `task.issue_id` to a
    /// hierarchical beads ID, `plan_epic_issue_creates` (server.rs:1308) emits
    /// `spur:source-issue:bd-2dww.7` — rejected by the `br 0.1.14` label
    /// validator. `source_issue` must emit a br-legal label AND
    /// `parse_source_issue` must return the original hierarchical id.
    #[test]
    fn source_issue_label_round_trips_hierarchical_beads_ids() {
        let issue_id = "bd-2dww.7";
        let label = source_issue(issue_id);
        assert!(
            is_br_legal(&label),
            "source_issue emitted br-illegal label for hierarchical id: {label}"
        );
        assert_eq!(
            parse_source_issue(&label).as_deref(),
            Some(issue_id),
            "round-trip failed for hierarchical id"
        );
    }

    #[test]
    fn mint_delegation_id_returns_16_hex_chars() {
        for _ in 0..100 {
            let id = mint_delegation_id();
            assert_eq!(id.len(), 16, "expected 16 chars, got {}: {id:?}", id.len());
            assert!(
                id.chars().all(
                    |c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase())
                ),
                "expected lowercase hex, got: {id:?}"
            );
        }
    }

    #[test]
    fn delegation_id_label_under_50_chars_for_minted_id() {
        for _ in 0..100 {
            let minted = mint_delegation_id();
            let label = delegation_id(&minted);
            assert!(
                label.len() <= 50,
                "label exceeds 50-char br create cap: {} chars: {label}",
                label.len()
            );
            assert!(is_br_legal(&label), "label not br-legal: {label}");
        }
    }

    /// Locks the `:016x` format contract by injecting deterministic UUIDs via
    /// `Builder::from_random_bytes` and asserting exact output strings. The
    /// 100-iteration property test above almost never hits a leading-zero
    /// high half (P ≈ 1/4e9); without this, a future refactor that drops the
    /// zero-pad width (e.g. `{x:x}`) would slip past CI.
    #[test]
    fn mint_delegation_id_format_pads_zeros_and_pins_version_nibble() {
        let zeros = uuid::Builder::from_random_bytes([0u8; 16]).into_uuid();
        let high = zeros.as_u128() >> 64;
        // V4 sets byte 6 upper nibble to 0100; all-zero input → byte 6 = 0x40.
        assert_eq!(
            format!("{high:016x}"),
            "0000000000004000",
            "leading-zero high half must be left-padded to 16 chars"
        );

        let ones = uuid::Builder::from_random_bytes([0xffu8; 16]).into_uuid();
        let high = ones.as_u128() >> 64;
        // V4 still forces byte 6 upper nibble to 0100 → byte 6 = 0x4f.
        assert_eq!(
            format!("{high:016x}"),
            "ffffffffffff4fff",
            "version nibble lives at hex char index 12; remaining bits stay random"
        );
    }

    #[test]
    fn derive_brain_session_id_is_deterministic() {
        let acp = spur_acp::SessionId("550e8400-e29b-41d4-a716-446655440000".to_string());
        let a = derive_brain_session_id(&acp);
        let b = derive_brain_session_id(&acp);
        assert_eq!(a.as_session_id().0, b.as_session_id().0);
        assert_eq!(a.as_session_id().0.len(), 16);
    }

    #[test]
    fn derive_brain_session_id_distinguishes_different_inputs() {
        let a = derive_brain_session_id(&spur_acp::SessionId("a".to_string()));
        let b = derive_brain_session_id(&spur_acp::SessionId("b".to_string()));
        assert_ne!(a.as_session_id().0, b.as_session_id().0);
    }

    #[test]
    fn derive_brain_session_id_label_under_50_chars_and_br_legal() {
        let acp = spur_acp::SessionId(uuid::Uuid::new_v4().to_string());
        let derived = derive_brain_session_id(&acp);
        let label = plan_owner(&derived.as_session_id().0);
        assert!(label.len() <= 50, "label {} chars: {label}", label.len());
        assert!(is_br_legal(&label), "label not br-legal: {label}");
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
