use std::time::{Duration, Instant};

use spur_tui::action::ViewId;
use spur_tui::components::tombstone::{Tombstone, TombstoneKind, TombstoneSlots};

#[test]
fn badge_renders_when_current_view_matches_slot() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(Tombstone {
        view: ViewId::SessionPicker,
        kind: TombstoneKind::Reversible {
            inverse: spur_tui::action::Action::ToggleSessionArchive {
                session_id: "s1".into(),
                via_legacy_key: false,
            },
        },
        label: "archived 'foo'".into(),
        created_at: now,
        expires_at: now + Duration::from_secs(45),
    });
    let badge = spur_tui::components::status_bar::render_tombstone_badge(
        slots.peek(&ViewId::SessionPicker),
        now,
    );
    let text = format!("{}", badge); // ratatui::text::Line Display impl
    assert!(text.contains("[u:"), "expected `[u:` prefix, got: {text}");
    assert!(
        text.contains("archived 'foo'"),
        "expected label, got: {text}"
    );
    assert!(text.contains("45s"), "expected countdown, got: {text}");
}

#[test]
fn badge_returns_empty_line_when_slot_is_none() {
    let now = Instant::now();
    let badge = spur_tui::components::status_bar::render_tombstone_badge(None, now);
    let text = format!("{}", badge);
    assert!(text.is_empty(), "expected empty line, got: {text}");
}

#[test]
fn badge_uses_revert_verb_for_queued_remote() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(Tombstone {
        view: ViewId::Dashboard,
        kind: TombstoneKind::QueuedRemote {
            pending: spur_tui::action::Action::SubmitReviewDispatch {
                executor_id: "x".into(),
                attempt_n: 1,
                decision: spur_core::ReviewDecision::Approve,
            },
        },
        label: "Approve".into(),
        created_at: now,
        expires_at: now + Duration::from_secs(2),
    });
    let badge = spur_tui::components::status_bar::render_tombstone_badge(
        slots.peek(&ViewId::Dashboard),
        now,
    );
    let text = format!("{}", badge);
    assert!(
        text.contains("revert"),
        "expected `revert` verb, got: {text}"
    );
    assert!(text.contains("2s"), "expected 2s countdown, got: {text}");
}

#[test]
fn badge_truncates_long_labels() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    let long = "archived 'verylongsessionnametotest'";
    slots.install(Tombstone {
        view: ViewId::SessionPicker,
        kind: TombstoneKind::Reversible {
            inverse: spur_tui::action::Action::ToggleSessionArchive {
                session_id: "s1".into(),
                via_legacy_key: false,
            },
        },
        label: long.into(),
        created_at: now,
        expires_at: now + Duration::from_secs(60),
    });
    let badge = spur_tui::components::status_bar::render_tombstone_badge(
        slots.peek(&ViewId::SessionPicker),
        now,
    );
    let text = format!("{}", badge);
    assert!(
        text.contains("…"),
        "expected ellipsis truncation, got: {text}"
    );
    assert!(
        text.len() <= 40,
        "badge text too long: {} chars in: {text}",
        text.len()
    );
}
