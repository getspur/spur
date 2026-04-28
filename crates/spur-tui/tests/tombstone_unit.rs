use std::time::{Duration, Instant};

use spur_tui::action::{Action, ViewId};
use spur_tui::components::tombstone::{Tombstone, TombstoneKind, TombstoneSlots};

fn reversible_tombstone(view: ViewId, now: Instant, window: Duration) -> Tombstone {
    Tombstone {
        view: view.clone(),
        kind: TombstoneKind::Reversible {
            inverse: Action::ToggleSessionArchive {
                session_id: "sess-1".into(),
                via_legacy_key: false,
            },
        },
        label: "Archived 'test'".into(),
        created_at: now,
        expires_at: now + window,
    }
}

fn queued_tombstone(view: ViewId, now: Instant, window: Duration) -> Tombstone {
    Tombstone {
        view: view.clone(),
        kind: TombstoneKind::QueuedRemote {
            pending: Action::SubmitReview {
                executor_id: "exec-1".into(),
                attempt_n: 1,
                decision: spur_core::ReviewDecision::Approve,
            },
        },
        label: "Approving...".into(),
        created_at: now,
        expires_at: now + window,
    }
}

#[test]
fn install_and_evict_returns_tombstone() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(reversible_tombstone(
        ViewId::SessionPicker,
        now,
        Duration::from_secs(60),
    ));
    let t = slots.evict(&ViewId::SessionPicker);
    assert!(t.is_some());
    assert!(slots.evict(&ViewId::SessionPicker).is_none());
}

#[test]
fn install_replaces_prior_tombstone_for_same_view() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    let first = reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60));
    let mut second = reversible_tombstone(ViewId::SessionPicker, now, Duration::from_secs(60));
    second.label = "Archived 'second'".into();
    slots.install(first);
    slots.install(second);
    let t = slots.evict(&ViewId::SessionPicker).unwrap();
    assert_eq!(t.label, "Archived 'second'");
}

#[test]
fn tick_drops_expired_reversible_without_dispatch() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(reversible_tombstone(
        ViewId::SessionPicker,
        now,
        Duration::from_millis(1),
    ));
    let future = now + Duration::from_millis(10);
    let dispatched = slots.tick(future);
    assert!(
        dispatched.is_empty(),
        "reversible expiry must not dispatch anything"
    );
    assert!(
        slots.evict(&ViewId::SessionPicker).is_none(),
        "tombstone must be evicted"
    );
}

#[test]
fn tick_dispatches_queued_remote_on_expiry() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(queued_tombstone(
        ViewId::Dashboard,
        now,
        Duration::from_millis(1),
    ));
    let future = now + Duration::from_millis(10);
    let dispatched = slots.tick(future);
    assert_eq!(dispatched.len(), 1);
    assert!(matches!(dispatched[0], Action::SubmitReview { .. }));
    assert!(slots.evict(&ViewId::Dashboard).is_none());
}

#[test]
fn cancel_all_without_dispatch_drops_queued_without_emitting() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(queued_tombstone(
        ViewId::Dashboard,
        now,
        Duration::from_secs(3),
    ));
    slots.cancel_all_without_dispatch();
    // tick after cancel must not dispatch anything
    let dispatched = slots.tick(now + Duration::from_secs(10));
    assert!(dispatched.is_empty());
    assert!(slots.evict(&ViewId::Dashboard).is_none());
}

#[test]
fn per_view_isolation_separate_slots() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    slots.install(reversible_tombstone(
        ViewId::SessionPicker,
        now,
        Duration::from_secs(60),
    ));
    assert!(
        slots.evict(&ViewId::Dashboard).is_none(),
        "Dashboard must have no tombstone"
    );
    assert!(slots.evict(&ViewId::SessionPicker).is_some());
}

#[test]
fn install_replaces_and_returns_displaced_queued_for_immediate_dispatch() {
    let mut slots = TombstoneSlots::new();
    let now = Instant::now();
    let first = queued_tombstone(ViewId::Dashboard, now, Duration::from_secs(3));
    slots.install(first);
    // Installing a second queued tombstone for same view should displace first
    let displaced = slots.install_and_get_displaced(queued_tombstone(
        ViewId::Dashboard,
        now,
        Duration::from_secs(3),
    ));
    assert!(displaced.is_some());
    assert!(matches!(
        displaced.unwrap().kind,
        TombstoneKind::QueuedRemote { .. }
    ));
}
