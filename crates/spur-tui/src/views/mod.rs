pub mod dashboard;
#[cfg(feature = "analytics")]
pub mod insights;
pub mod issue_browser;
#[cfg(feature = "markdown")]
pub mod mermaid_viewer;
pub mod plan_inspector;
pub mod session_detail;
pub mod session_picker;

#[cfg(not(feature = "analytics"))]
pub mod insights {
    use crossterm::event::KeyEvent;
    use ratatui::{layout::Rect, Frame};
    use spur_acp::SpurEvent;

    use crate::action::Action;

    use super::{View, ViewContext};

    pub struct InsightsView;

    impl InsightsView {
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for InsightsView {
        fn default() -> Self {
            Self::new()
        }
    }

    impl View for InsightsView {
        fn handle_key(&mut self, _key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
            None
        }

        fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &ViewContext) {}

        fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
            use ratatui::widgets::Paragraph;

            let p = Paragraph::new(
                "Analytics feature disabled — rebuild with --features spur-tui/analytics",
            );
            frame.render_widget(p, area);
        }

        fn tick(&mut self) {}
    }
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use spur_acp::SpurEvent;

use crate::action::Action;
use crate::components::status_bar::{HintOverride, LicenseBadge};
use crate::components::tombstone::Tombstone;

// ── macOS Option-key normalisation ────────────────────────────────────
//
// macOS terminals emit a Unicode character (e.g. `∑` for Option-W on
// US-QWERTY) instead of an Alt escape sequence when the "Use Option as
// Meta key" setting is off — which is the default on Terminal.app,
// iTerm2, and most other macOS terminals.
//
// The table below maps the US-QWERTY Option-letter characters back to
// `Alt+<ascii>` so that SPUR's keybindings work out-of-the-box on macOS
// without requiring users to change their terminal settings.
//
// This is intentionally NOT gated behind `cfg(target_os = "macos")`
// because it must also work when a macOS terminal SSHs into a Linux
// host running SPUR — the terminal still sends Unicode, but the binary
// is compiled for Linux.  The false-positive risk (a user intentionally
// typing `∑` or `µ` in a TUI) is negligible.

/// Normalise a macOS Option-key Unicode character to `Alt+<ascii>`.
/// Returns the key unchanged on non-macOS or when no mapping applies.
pub(crate) fn normalize_macos_option(key: KeyEvent) -> KeyEvent {
    if let KeyCode::Char(ch) = key.code {
        if let Some(ascii) = macos_option_char(ch) {
            return KeyEvent::new(KeyCode::Char(ascii), key.modifiers | KeyModifiers::ALT);
        }
    }
    key
}

/// US-QWERTY Option-letter → ASCII mapping.  Covers most of the
/// alphabet; dead-key letters (e → ´, n → ˜, u → ¨) are excluded
/// because macOS does not emit a standalone character for those — it
/// waits for a second keystroke to compose an accented letter.
fn macos_option_char(ch: char) -> Option<char> {
    match ch {
        'å' => Some('a'),
        '∫' => Some('b'),
        'ç' => Some('c'),
        '∂' => Some('d'),
        // 'e' → dead-key (´), skip
        'ƒ' => Some('f'),
        '©' => Some('g'),
        '˙' => Some('h'),
        'ˆ' => Some('i'),
        '∆' => Some('j'),
        '˚' => Some('k'),
        '¬' => Some('l'),
        'µ' => Some('m'),
        // 'n' → dead-key (˜), skip
        'ø' => Some('o'),
        'π' => Some('p'),
        'œ' => Some('q'),
        '®' => Some('r'),
        'ß' => Some('s'),
        '†' => Some('t'),
        // 'u' → dead-key (¨), skip
        '√' => Some('v'),
        '∑' => Some('w'),
        '≈' => Some('x'),
        '¥' => Some('y'),
        'Ω' => Some('z'),
        // Option+digit on macOS US-QWERTY: ¡™£¢∞§¶•ªº.
        // Only 1..4 are wired to global shortcuts today; the rest are
        // reserved so future Alt+digit bindings work without a revisit.
        '¡' => Some('1'),
        '™' => Some('2'),
        '£' => Some('3'),
        '¢' => Some('4'),
        '∞' => Some('5'),
        '§' => Some('6'),
        '¶' => Some('7'),
        '•' => Some('8'),
        'ª' => Some('9'),
        'º' => Some('0'),
        _ => None,
    }
}

/// Shared read-only context passed from App to every View method.
/// Eliminates the `render_with_lineage` / `handle_key_with_lineage`
/// bypass pattern — views access lineage and brain status through this
/// struct instead of extra parameters outside the trait.
pub struct ViewContext<'a> {
    pub lineage: &'a spur_core::lineage::projection::ExecutorLineage,
    pub plan_projection: &'a spur_core::PlanProjectionStore,
    pub synopsis: &'a spur_core::SessionSynopsisProjection,
    pub brain_status: &'a crate::app::BrainStatus,
    pub license_badge: Option<&'a LicenseBadge>,
    pub flag_summary: Option<(usize, usize)>,
    pub tombstone: Option<&'a Tombstone>,
    pub transient_hint_override: Option<HintOverride<'a>>,
}

/// Test-only default context backed by empty lineage and idle status.
#[cfg(test)]
static TEST_BRAIN_STATUS: crate::app::BrainStatus = crate::app::BrainStatus::Idle;
#[cfg(test)]
static TEST_PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
    std::sync::OnceLock::new();
#[cfg(test)]
static TEST_SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
    std::sync::OnceLock::new();

#[cfg(test)]
impl ViewContext<'_> {
    /// Cheap context for unit tests that don't exercise lineage or brain
    /// status. Backed by a static idle status and the provided lineage ref.
    pub fn test_ctx(lineage: &spur_core::lineage::projection::ExecutorLineage) -> ViewContext<'_> {
        ViewContext {
            lineage,
            plan_projection: TEST_PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: TEST_SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &TEST_BRAIN_STATUS,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
        }
    }
}

/// Trait for top-level views (Dashboard, Session Detail, etc.).
pub trait View {
    /// Handle a keyboard event. Return an Action if the view wants the app to do something.
    fn handle_key(&mut self, key: KeyEvent, ctx: &ViewContext) -> Option<Action>;
    /// Process an orchestrator event, updating internal state.
    fn handle_spur_event(&mut self, event: &SpurEvent, ctx: &ViewContext);
    /// Render the view into the given frame area.
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &ViewContext);
    /// Called on each tick (for spinner animations, batched text flush, etc.).
    fn tick(&mut self);
}
