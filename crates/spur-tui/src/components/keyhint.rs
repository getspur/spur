use crossterm::event::{KeyCode, KeyModifiers};

pub fn format_key_hint(code: KeyCode, mods: KeyModifiers) -> String {
    let mut parts = Vec::new();
    let (ctrl, alt, shift) = platform_modifier_glyphs();

    if mods.contains(KeyModifiers::CONTROL) {
        parts.push(ctrl.to_string());
    }
    if mods.contains(KeyModifiers::ALT) {
        parts.push(alt.to_string());
    }
    if mods.contains(KeyModifiers::SHIFT) {
        parts.push(shift.to_string());
    }
    parts.push(format_keycode(code));

    parts.join("+")
}

#[cfg(target_os = "macos")]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("⌃", "⌥", "⇧")
}

#[cfg(not(target_os = "macos"))]
fn platform_modifier_glyphs() -> (&'static str, &'static str, &'static str) {
    ("Ctrl", "Alt", "Shift")
}

fn format_keycode(code: KeyCode) -> String {
    match code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Tab | KeyCode::BackTab => "Tab".to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::F(n @ 1..=12) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}
