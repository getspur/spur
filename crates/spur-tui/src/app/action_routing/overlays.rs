use super::*;

impl App {
    pub(super) fn process_overlay(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::ShowHelp => {
                self.help_visible = true;
                None
            }

            Action::HideHelp => {
                self.help_visible = false;
                None
            }

            Action::PanicReset => {
                self.quit_confirm_visible = false;
                self.collision_modal = None;
                self.upgrade_modal = None;
                self.help_visible = false;
                self.palette_visible = false;
                self.palette_state.reset();
                self.tombstones.cancel_all_without_dispatch();
                // Wire per 2026-04-28-tui-destructive-undo-design.md section 4.7.
                // Inc 2 (bd-d587.2): navigate_to(Dashboard) also clears view_history,
                // matching the panic-reset intent of returning to a clean root.
                self.navigate_to(ViewId::Dashboard);
                self.dashboard.reset_to_root();
                if let Some(detail) = self.session_detail.as_mut() {
                    detail.reset_to_root();
                }
                self.esc_chain.clear();
                self.flash_hint_short(PANIC_RESET_HINT);
                self.dirty = true;
                None
            }

            Action::ShowSessionCost => {
                // M1.3 - Pro-tier demo gate: community users get the upgrade
                // modal; Pro users see the per-project cost view.
                if let Err(err) = spur_license::require_feature(
                    &self.feature_gate,
                    spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
                ) {
                    let required_tier = spur_license::upgrade_cta::required_tier_for(
                        spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
                    );
                    return Some(Action::ShowUpgradeModal { err, required_tier });
                }

                if let Some(ref mut detail) = self.session_detail {
                    detail.push_cost_note();
                }
                None
            }

            Action::ShowUpgradeModal { err, required_tier } => {
                // Plan C Tier 2 - open the capability-tease modal.
                // Re-pop on every denial (no de-dup); the plan calls
                // out session-level suppression as YAGNI for the MVP.
                self.upgrade_modal = Some(UpgradeModalState { err, required_tier });
                self.dirty = true;
                None
            }

            Action::FlashHint { message } => {
                self.flash_hint_short(message);
                None
            }

            Action::PrefillInput { text } => {
                match &self.current_view {
                    ViewId::Dashboard => {
                        self.dashboard.prefill_input(text);
                    }
                    ViewId::SessionDetail(_) => {
                        if let Some(ref mut detail) = self.session_detail {
                            detail.prefill_input(text);
                        }
                    }
                    _ => {}
                }
                self.dirty = true;
                None
            }

            _ => None,
        }
    }
}
