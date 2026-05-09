use crate::theme::{loader::load_built_in, resolve_token, ColorDepth};
use ratatui::style::Color;

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

#[test]
fn dark_tokens_match_current_literal_color_sites() {
    let dark = load_built_in("dark").expect("dark built-in loads");
    let cases: &[(&str, Color)] = &[
        // status_bar.rs:53 LicenseBadgeTone::Neutral uses Color::DarkGray.
        ("license_badge.neutral.fg", rgb(0x73, 0x73, 0x73)),
        // status_bar.rs:87 tombstone badges use Color::DarkGray.
        ("status_bar.tombstone.fg", rgb(0x73, 0x73, 0x73)),
        // status_bar.rs:354 issue counts use Color::Cyan.
        ("status_bar.issue_count.fg", rgb(0x38, 0xbd, 0xf8)),
        // status_bar.rs:356 metric separators use Color::DarkGray.
        ("status_bar.separator.fg", rgb(0x73, 0x73, 0x73)),
        // trace_format.rs:70 ToolFamily::Read uses Color::Cyan.
        ("tool.family.read", rgb(0x38, 0xbd, 0xf8)),
        // trace_format.rs:71 ToolFamily::Edit uses Color::Yellow.
        ("tool.family.edit", rgb(0xfb, 0xbf, 0x24)),
        // trace_format.rs:72 ToolFamily::Delete uses Color::Red.
        ("tool.family.delete", rgb(0xf8, 0x71, 0x71)),
        // trace_format.rs:73 ToolFamily::Move uses Color::Yellow.
        ("tool.family.move", rgb(0xfb, 0xbf, 0x24)),
        // trace_format.rs:74 ToolFamily::Search uses Color::Blue.
        ("tool.family.search", rgb(0x60, 0xa5, 0xfa)),
        // trace_format.rs:75 ToolFamily::Execute uses Color::Magenta.
        ("tool.family.bash", rgb(0xc0, 0x84, 0xfc)),
        // trace_format.rs:76 ToolFamily::Think uses Color::DarkGray.
        ("tool.family.thinking", rgb(0x73, 0x73, 0x73)),
        // trace_format.rs:77 ToolFamily::Fetch uses Color::Blue.
        ("tool.family.fetch", rgb(0x60, 0xa5, 0xfa)),
        // trace_format.rs:78 ToolFamily::SwitchMode uses Color::Cyan.
        ("tool.family.switch_mode", rgb(0x38, 0xbd, 0xf8)),
        // trace_format.rs:79 ToolFamily::Plan uses Color::Cyan.
        ("tool.family.task", rgb(0x38, 0xbd, 0xf8)),
        // trace_format.rs:80 ToolFamily::Mcp uses Color::DarkGray.
        ("tool.family.mcp", rgb(0x73, 0x73, 0x73)),
        // trace_format.rs:81 ToolFamily::Unknown uses Color::Yellow.
        ("tool.family.unknown", rgb(0xfb, 0xbf, 0x24)),
    ];

    for (token, expected_color) in cases {
        assert_eq!(
            resolve_token(&dark, token, ColorDepth::Truecolor),
            *expected_color,
            "{token}"
        );
    }
}
