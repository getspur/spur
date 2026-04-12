use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::time::Duration;

use crate::app::{App, Panel};

/// Poll for terminal keyboard events with the given tick rate.
/// Returns `true` if an event was processed, `false` on timeout.
pub fn handle_terminal_events(app: &mut App, tick_rate: Duration) -> anyhow::Result<bool> {
    if event::poll(tick_rate)? {
        if let Event::Key(key) = event::read()? {
            // crossterm 0.28 fires Press and Release; only handle Press
            if key.kind == KeyEventKind::Press {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        app.should_quit = true;
                    }
                    KeyCode::Tab => {
                        app.selected_panel = match app.selected_panel {
                            Panel::Agents => Panel::Log,
                            Panel::Log => Panel::Agents,
                        };
                    }
                    KeyCode::Up => {
                        app.log_scroll = app.log_scroll.saturating_sub(1);
                    }
                    KeyCode::Down => {
                        app.log_scroll = app.log_scroll.saturating_add(1);
                    }
                    _ => {}
                }
            }
        }
        return Ok(true);
    }
    Ok(false)
}
