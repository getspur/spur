use super::*;

impl App {
    /// Inc 2 (bd-d587.2): switch to `view`, recording the leaving view in
    /// `view_history` so a subsequent `navigate_back()` returns to it.
    /// Dashboard is the canonical root and clears the stack on entry. Calls
    /// where `current_view == view` are no-ops (no self-push, no dirty flip).
    pub(super) fn navigate_to(&mut self, view: ViewId) {
        if self.current_view == view {
            return;
        }
        if matches!(view, ViewId::Dashboard) {
            self.view_history.clear();
        } else {
            self.push_history(self.current_view.clone());
        }
        self.current_view = view;
        self.dirty = true;
    }

    /// Push `view` onto `view_history`, respecting cap and no-dup-top invariants.
    /// Used internally by `navigate_to`; exposed via that method only.
    pub(super) fn push_history(&mut self, view: ViewId) {
        if self.view_history.last() == Some(&view) {
            return;
        }
        if self.view_history.len() >= NAV_HISTORY_MAX {
            self.view_history.remove(0);
        }
        self.view_history.push(view);
    }

    /// Pop the last view from `view_history` and switch to it. When the stack
    /// is empty: from Dashboard, fall back to the active session detail (the
    /// natural "back" from the activity log) if one exists; from any other
    /// view, fall back to Dashboard. Nulls overlay state when leaving an
    /// overlay view (PlanInspector, MermaidOverlay).
    pub(super) fn navigate_back(&mut self) {
        let leaving = self.current_view.clone();
        let next = self.view_history.pop().or_else(|| {
            if matches!(leaving, ViewId::Dashboard) {
                self.session_detail
                    .as_ref()
                    .map(|d| ViewId::SessionDetail(d.session_id().clone()))
            } else {
                Some(ViewId::Dashboard)
            }
        });
        let Some(next) = next else {
            return;
        };
        self.current_view = next;
        match leaving {
            #[cfg(feature = "markdown")]
            ViewId::MermaidOverlay(_) => {
                self.mermaid_viewer = None;
            }
            ViewId::PlanInspector(_) => {
                self.plan_inspector = None;
            }
            _ => {}
        }
        self.dirty = true;
    }

    pub fn current_view(&self) -> &ViewId {
        &self.current_view
    }
}
