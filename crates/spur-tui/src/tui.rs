use std::io;

use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<io::Stdout>>;
const MOUSE_CAPTURE_ENV: &str = "SPUR_TUI_MOUSE_CAPTURE";

fn should_enable_mouse_capture(_term_program: Option<&str>, override_value: Option<&str>) -> bool {
    if let Some(value) = override_value {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }

    true
}

pub fn setup() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    if should_enable_mouse_capture(
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        std::env::var(MOUSE_CAPTURE_ENV).ok().as_deref(),
    ) {
        execute!(stdout, EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

pub fn teardown(terminal: &mut Tui) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_capture_is_enabled_by_default_in_apple_terminal() {
        assert!(should_enable_mouse_capture(Some("Apple_Terminal"), None));
    }

    #[test]
    fn mouse_capture_stays_enabled_by_default_elsewhere() {
        assert!(should_enable_mouse_capture(Some("iTerm.app"), None));
        assert!(should_enable_mouse_capture(None, None));
    }

    #[test]
    fn mouse_capture_override_can_force_enable_or_disable() {
        assert!(should_enable_mouse_capture(
            Some("Apple_Terminal"),
            Some("1")
        ));
        assert!(!should_enable_mouse_capture(Some("iTerm.app"), Some("0")));
    }
}
