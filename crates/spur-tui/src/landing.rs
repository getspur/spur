/// Resolved landing decision for `spur tui` startup.
#[derive(Debug, Clone)]
pub enum LandingDecision {
    /// Resume the last active ACP session.
    AutoResume { acp_id: String, brain: String },
    /// Open the session picker.
    ShowPicker,
    /// Open the Dashboard empty state.
    ShowDashboard,
    /// Agents not configured; show setup nudge.
    SetupRequired,
}
