use crossterm::event::{KeyCode, KeyModifiers};
use spur_tui::components::keyhint::format_key_hint;

#[test]
#[cfg(target_os = "macos")]
fn macos_ctrl_renders_as_glyph() {
    assert_eq!(
        format_key_hint(KeyCode::Char('p'), KeyModifiers::CONTROL),
        "⌃+p"
    );
}

#[test]
#[cfg(not(target_os = "macos"))]
fn non_macos_ctrl_renders_as_word() {
    assert_eq!(
        format_key_hint(KeyCode::Char('p'), KeyModifiers::CONTROL),
        "Ctrl+p"
    );
}

#[test]
#[cfg(target_os = "macos")]
fn macos_multi_modifier_order_is_ctrl_alt_shift() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

    assert_eq!(format_key_hint(KeyCode::Char('P'), mods), "⌃+⇧+P");
}

#[test]
#[cfg(not(target_os = "macos"))]
fn non_macos_multi_modifier_order_is_ctrl_alt_shift() {
    let mods = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

    assert_eq!(format_key_hint(KeyCode::Char('P'), mods), "Ctrl+Shift+P");
}

#[test]
fn tab_renders_as_tab() {
    assert_eq!(format_key_hint(KeyCode::Tab, KeyModifiers::NONE), "Tab");
}

#[test]
fn up_renders_as_arrow() {
    assert_eq!(format_key_hint(KeyCode::Up, KeyModifiers::NONE), "↑");
}
