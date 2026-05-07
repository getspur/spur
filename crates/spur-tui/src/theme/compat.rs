//! Migration-only shim: returns the legacy `ratatui::style::Color` literals
//! currently hardcoded across views, so PR3/PR4 can mechanically swap
//! `Color::Cyan` for `theme_compat::current().tool_family_read()` without
//! mid-migration tinting drift.
//!
//! **This module will be deleted after the last surface migrates.** It
//! intentionally has no theme parameter — it is a frozen snapshot of the
//! pre-theme palette. Once every Color:: site is replaced with a real
//! `theme.token(...)` call, delete this file and remove the `compat` module
//! from `theme/mod.rs`.
//!
//! Do not extend this module with new helpers as views migrate. New code
//! should read from the active `Theme` carried on `ViewContext`.

use ratatui::style::Color;

/// Returns the singleton compat snapshot. Cheap (`Copy` on a unit struct).
pub fn current() -> CompatTheme {
    CompatTheme
}

/// Compat snapshot. Methods return literal `Color` values that match the
/// pre-theme TUI palette one-for-one. See `docs/.../tui-theme-system-design.md`
/// "Token taxonomy" for the canonical mapping these helpers reproduce.
#[derive(Clone, Copy, Debug)]
pub struct CompatTheme;

impl CompatTheme {
    // ── Tool families ─────────────────────────────────────────────────
    pub fn tool_family_thinking(self) -> Color {
        Color::Magenta
    }

    pub fn tool_family_edit(self) -> Color {
        Color::Yellow
    }

    pub fn tool_family_read(self) -> Color {
        Color::Cyan
    }

    pub fn tool_family_bash(self) -> Color {
        Color::Green
    }

    pub fn tool_family_task(self) -> Color {
        Color::Magenta
    }

    // ── Borders / chrome ──────────────────────────────────────────────
    pub fn border(self) -> Color {
        Color::DarkGray
    }

    pub fn border_focused(self) -> Color {
        Color::Cyan
    }

    pub fn spinner_fg(self) -> Color {
        Color::Cyan
    }

    pub fn status_bar_fg(self) -> Color {
        Color::White
    }

    pub fn status_bar_bg(self) -> Color {
        Color::Reset
    }

    // ── Status semantics ──────────────────────────────────────────────
    pub fn success(self) -> Color {
        Color::Green
    }

    pub fn warning(self) -> Color {
        Color::Yellow
    }

    pub fn danger(self) -> Color {
        Color::Red
    }

    pub fn info(self) -> Color {
        Color::Blue
    }

    pub fn highlight(self) -> Color {
        Color::LightYellow
    }

    // ── Text tones ────────────────────────────────────────────────────
    pub fn fg(self) -> Color {
        Color::White
    }

    pub fn fg_muted(self) -> Color {
        Color::Gray
    }

    pub fn fg_subtle(self) -> Color {
        Color::DarkGray
    }

    /// Black on warning / accent bg — current code embeds this assumption
    /// (`Color::Black` over `Color::Yellow`) in plan_pulse, input_bar, and
    /// session_detail. Surfaces a single rename target for PR3.
    pub fn fg_on_warning(self) -> Color {
        Color::Black
    }

    pub fn fg_on_success(self) -> Color {
        Color::Black
    }

    pub fn fg_on_danger(self) -> Color {
        Color::White
    }

    // ── Diff ──────────────────────────────────────────────────────────
    pub fn diff_add(self) -> Color {
        Color::Green
    }

    pub fn diff_del(self) -> Color {
        Color::Red
    }
}

#[cfg(test)]
mod tests {
    use super::current;
    use ratatui::style::Color;

    #[test]
    fn compat_theme_is_immutable_and_returns_legacy_literals() {
        let t = current();
        assert_eq!(t.tool_family_read(), Color::Cyan);
        assert_eq!(t.tool_family_edit(), Color::Yellow);
        assert_eq!(t.fg_on_warning(), Color::Black);
        assert_eq!(t.danger(), Color::Red);
    }
}
