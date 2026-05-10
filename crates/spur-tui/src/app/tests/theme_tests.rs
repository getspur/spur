#[cfg(test)]
mod theme_threading_tests {
    use super::super::super::*;
    use crate::theme::runtime::test_support::with_isolated_dirs;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn app_with_theme(theme: &str) -> App {
        let mut spur_config = spur_acp::SpurConfig::default();
        spur_config.tui.theme = theme.to_string();

        App::new_with_config(
            None,
            false,
            std::sync::Arc::new(spur_config),
            crate::landing::LandingDecision::ShowDashboard,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Boots `App` with `tui.theme = "light"` and confirms (a) construction
    /// does not panic even though no surface consumes the theme yet, and
    /// (b) the resolved theme is the requested one. This guards the cascade
    /// from regressing into a `dark`-only fallback path.
    ///
    /// Wrapped in `with_isolated_dirs` so a stray `~/.spur/themes/light.yaml`
    /// or `.spur/themes/light.yaml` in the developer's environment cannot
    /// shadow the built-in and break the assertion.
    #[test]
    fn light_theme_boots_without_panic() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "light".to_string();

            let app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            assert_eq!(app.theme.name, "light");
        });
    }

    #[test]
    fn unknown_theme_falls_back_to_dark_without_panic() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "definitely-not-a-theme".to_string();

            let app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            assert_eq!(app.theme.name, "dark");
        });
    }

    /// `/theme light` must atomically swap `App.theme` so the next render
    /// pulls tokens from the light palette. Verifies both the resolved
    /// `theme.name` and the tracked `active_theme_name` after dispatch.
    #[test]
    fn slash_theme_switch_swaps_app_theme_arc() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "dark".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "light".to_string(),
            });

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
            let hint = app
                .transient_hint_for_test()
                .expect("flash hint set on success");
            assert!(
                hint.text.contains("light"),
                "hint should mention switched theme, got `{}`",
                hint.text
            );
        });
    }

    /// `/theme definitely-not-a-theme` keeps the previous theme intact
    /// and surfaces an error via the transient-hint mechanism. The
    /// `Arc<Theme>` must NOT be replaced — verified via pointer equality.
    #[test]
    fn slash_theme_switch_unknown_keeps_previous_and_flashes_error() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "light".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );
            let prev_theme_ptr = std::sync::Arc::as_ptr(&app.theme);
            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "definitely-not-a-theme".to_string(),
            });

            assert_eq!(app.theme.name, "light", "theme must not change on failure");
            assert_eq!(
                app.active_theme_name, "light",
                "active_theme_name must not change on failure"
            );
            assert_eq!(
                std::sync::Arc::as_ptr(&app.theme),
                prev_theme_ptr,
                "Arc<Theme> must not be replaced on failed switch"
            );
            let hint = app
                .transient_hint_for_test()
                .expect("flash hint set on failure");
            assert!(
                hint.text.contains("not found"),
                "error hint should mention not-found, got `{}`",
                hint.text
            );
        });
    }

    #[test]
    fn bare_slash_theme_opens_theme_picker() {
        with_isolated_dirs(|_, _| {
            let mut spur_config = spur_acp::SpurConfig::default();
            spur_config.tui.theme = "dark".to_string();

            let mut app = App::new_with_config(
                None,
                false,
                std::sync::Arc::new(spur_config),
                crate::landing::LandingDecision::ShowDashboard,
            );

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });

            assert!(
                app.dashboard_for_test().completion_active(),
                "bare `/theme` should open the fuzzy theme picker"
            );
            assert!(
                app.transient_hint_for_test().is_none(),
                "bare `/theme` should not show the old theme-list flash"
            );
        });
    }

    #[test]
    fn theme_picker_accept_switches_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            app.handle_crossterm_event_for_test(key(KeyCode::Down));
            app.handle_crossterm_event_for_test(key(KeyCode::Enter));

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
        });
    }

    #[test]
    fn theme_picker_esc_cancels_without_changing_dashboard_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            assert!(app.dashboard_for_test().completion_active());

            app.handle_crossterm_event_for_test(key(KeyCode::Esc));

            assert!(!app.dashboard_for_test().completion_active());
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");
        });
    }

    #[test]
    fn slash_theme_reload_reloads_active_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("light");

            app.process_action(crate::action::Action::ThemeCommand {
                arg: "reload".to_string(),
            });

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
            let hint = app
                .transient_hint_for_test()
                .expect("reload should flash status");
            assert_eq!(hint.text, "theme reloaded: light");
        });
    }

    #[test]
    fn session_detail_theme_picker_accept_switches_theme() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");
            let session_id = spur_acp::SessionId("palette-test".into());
            app.session_detail = Some(
                crate::views::session_detail::SessionDetailView::new_for_palette_test(
                    crate::commands::CommandRegistry::default(),
                ),
            );
            app.current_view = ViewId::SessionDetail(session_id);

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });
            assert!(
                app.session_detail
                    .as_ref()
                    .is_some_and(|detail| detail.completion_active()),
                "bare `/theme` should open the session detail theme picker"
            );

            app.handle_crossterm_event_for_test(key(KeyCode::Down));
            app.handle_crossterm_event_for_test(key(KeyCode::Enter));

            assert_eq!(app.theme.name, "light");
            assert_eq!(app.active_theme_name, "light");
        });
    }

    #[test]
    fn bare_theme_in_unwired_view_flashes_theme_status() {
        with_isolated_dirs(|_, _| {
            let mut app = app_with_theme("dark");
            app.current_view = ViewId::SessionPicker;

            app.process_action(crate::action::Action::ThemeCommand { arg: String::new() });

            let hint = app
                .transient_hint_for_test()
                .expect("unwired views should show theme status");
            assert!(
                hint.text.contains("themes:"),
                "theme status flash should list themes, got `{}`",
                hint.text
            );
            assert!(
                hint.text.contains("* dark"),
                "active theme marker should include a space, got `{}`",
                hint.text
            );
            assert_eq!(app.theme.name, "dark");
            assert_eq!(app.active_theme_name, "dark");
        });
    }
}
