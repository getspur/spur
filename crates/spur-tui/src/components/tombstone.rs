//! Tombstone model for Gmail-toast-style destructive-action undo.
//!
//! One slot per view. `TombstoneSlots` is owned by `App` and driven
//! by the 33ms tick loop. Install/evict/tick are all O(views) which is
//! bounded to ~6 entries.

use std::collections::HashMap;
use std::time::Instant;

use crate::action::{Action, ViewId};

/// A single tombstone entry for one destructive action.
#[derive(Debug, Clone)]
pub struct Tombstone {
    pub view: ViewId,
    pub kind: TombstoneKind,
    /// Human-readable description of the action (used in toast copy).
    pub label: String,
    pub created_at: Instant,
    /// Wall-clock deadline. Reversible: 60s. QueuedRemote: 3s.
    pub expires_at: Instant,
}

/// Determines what happens on undo and on expiry.
#[derive(Debug, Clone)]
pub enum TombstoneKind {
    /// Action already committed. `u` dispatches `inverse` through
    /// `App::process_action`. Closure-based revert rejected: beads-backed
    /// mutations must go through the normal dispatch path.
    Reversible { inverse: Action },

    /// Action is client-queued; not yet dispatched. `u` drops it silently.
    /// Expiry dispatches `pending` through `App::process_action`.
    QueuedRemote { pending: Action },
}

/// Per-view tombstone store. One slot per `ViewId`.
#[derive(Debug, Default)]
pub struct TombstoneSlots {
    by_view: HashMap<ViewId, Tombstone>,
}

impl TombstoneSlots {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a tombstone, overwriting any prior tombstone for the same view.
    /// The displaced tombstone (if any) is discarded silently.
    /// Callers that need to dispatch a displaced QueuedRemote immediately
    /// should use `install_and_get_displaced` instead.
    pub fn install(&mut self, tombstone: Tombstone) {
        self.by_view.insert(tombstone.view.clone(), tombstone);
    }

    /// Install a tombstone, returning the prior tombstone for the same view
    /// if one existed. The caller is responsible for dispatching any displaced
    /// `QueuedRemote` immediately (spec section 4.3 bullet 6: new action
    /// displaces old queue slot, old slot must fire before its 3s expires).
    pub fn install_and_get_displaced(&mut self, tombstone: Tombstone) -> Option<Tombstone> {
        self.by_view.insert(tombstone.view.clone(), tombstone)
    }

    /// Remove and return the tombstone for the given view, if any.
    /// Called by the undo handler when the user presses `u` / `Ctrl+Z`.
    pub fn evict(&mut self, view: &ViewId) -> Option<Tombstone> {
        self.by_view.remove(view)
    }

    /// Drive expiry. Called from `App::tick` on every 33ms frame.
    ///
    /// Expired reversible tombstones are silently dropped (action already
    /// committed; nothing to do). Expired `QueuedRemote` tombstones are
    /// removed and their `pending` action is returned for the caller to
    /// dispatch through `App::process_action`.
    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        let mut to_dispatch = Vec::new();
        self.by_view.retain(|_view, ts| {
            if now >= ts.expires_at {
                if let TombstoneKind::QueuedRemote { ref pending } = ts.kind {
                    to_dispatch.push(pending.clone());
                }
                false // evict
            } else {
                true // keep
            }
        });
        to_dispatch
    }

    /// Drop ALL tombstones without dispatching anything.
    ///
    /// Called by `Action::PanicReset` (quick-fixes spec section 4.10).
    /// Reversible tombstones have already committed, so dropping them just
    /// prevents undo. QueuedRemote tombstones are cancelled; the action is never
    /// sent. This is the intended escape hatch: the user pressed triple-Esc
    /// because they want out, and the queued-remote action is collateral.
    pub fn cancel_all_without_dispatch(&mut self) {
        self.by_view.clear();
    }

    /// Returns true if a tombstone is active for the given view.
    pub fn has(&self, view: &ViewId) -> bool {
        self.by_view.contains_key(view)
    }

    /// Returns the active tombstone for a view without removing it
    /// (used by the render layer for countdown display).
    pub fn peek(&self, view: &ViewId) -> Option<&Tombstone> {
        self.by_view.get(view)
    }
}
