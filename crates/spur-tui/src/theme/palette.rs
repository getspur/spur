use ratatui::style::Color;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaletteEntry {
    pub rgb: Color,
    pub ansi: Option<Color>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    pub bg: PaletteEntry,
    pub bg_panel: PaletteEntry,
    pub bg_selection: PaletteEntry,
    pub bg_overlay: PaletteEntry,
    pub fg: PaletteEntry,
    pub fg_muted: PaletteEntry,
    pub fg_subtle: PaletteEntry,
    pub fg_on_accent: PaletteEntry,
    pub fg_on_success: PaletteEntry,
    pub fg_on_warning: PaletteEntry,
    pub fg_on_danger: PaletteEntry,
    pub fg_on_info: PaletteEntry,
    pub fg_on_overlay: PaletteEntry,
    pub border: PaletteEntry,
    pub border_focused: PaletteEntry,
    pub accent: PaletteEntry,
    pub accent_alt: PaletteEntry,
    pub success: PaletteEntry,
    pub warning: PaletteEntry,
    pub danger: PaletteEntry,
    pub info: PaletteEntry,
    pub highlight: PaletteEntry,
    pub diff_add: PaletteEntry,
    pub diff_del: PaletteEntry,
}

impl Palette {
    pub fn dark_default() -> Self {
        Self {
            bg: entry((0, 0, 0), Some(Color::Black)),
            bg_panel: entry((12, 12, 12), Some(Color::Black)),
            bg_selection: entry((0, 95, 135), Some(Color::Blue)),
            bg_overlay: entry((24, 24, 24), Some(Color::Black)),
            fg: entry((255, 255, 255), Some(Color::White)),
            fg_muted: entry((128, 128, 128), Some(Color::Gray)),
            fg_subtle: entry((96, 96, 96), Some(Color::DarkGray)),
            fg_on_accent: entry((0, 0, 0), Some(Color::Black)),
            fg_on_success: entry((0, 0, 0), Some(Color::Black)),
            fg_on_warning: entry((0, 0, 0), Some(Color::Black)),
            fg_on_danger: entry((255, 255, 255), Some(Color::White)),
            fg_on_info: entry((255, 255, 255), Some(Color::White)),
            fg_on_overlay: entry((255, 255, 255), Some(Color::White)),
            border: entry((96, 96, 96), Some(Color::DarkGray)),
            border_focused: entry((0, 255, 255), Some(Color::Cyan)),
            accent: entry((0, 255, 255), Some(Color::Cyan)),
            accent_alt: entry((255, 0, 255), Some(Color::Magenta)),
            success: entry((0, 255, 0), Some(Color::Green)),
            warning: entry((255, 255, 0), Some(Color::Yellow)),
            danger: entry((255, 0, 0), Some(Color::Red)),
            info: entry((0, 0, 255), Some(Color::Blue)),
            highlight: entry((255, 255, 128), Some(Color::LightYellow)),
            diff_add: entry((0, 255, 0), Some(Color::Green)),
            diff_del: entry((255, 0, 0), Some(Color::Red)),
        }
    }

    pub fn light_default() -> Self {
        Self {
            bg: entry((255, 255, 255), None),
            bg_panel: entry((243, 244, 246), None),
            bg_selection: entry((219, 234, 254), None),
            bg_overlay: entry((255, 255, 255), None),
            fg: entry((17, 24, 39), None),
            fg_muted: entry((75, 85, 99), None),
            fg_subtle: entry((156, 163, 175), None),
            fg_on_accent: entry((255, 255, 255), None),
            fg_on_success: entry((255, 255, 255), None),
            fg_on_warning: entry((17, 24, 39), None),
            fg_on_danger: entry((255, 255, 255), None),
            fg_on_info: entry((255, 255, 255), None),
            fg_on_overlay: entry((17, 24, 39), None),
            border: entry((209, 213, 219), None),
            border_focused: entry((37, 99, 235), None),
            accent: entry((37, 99, 235), None),
            accent_alt: entry((124, 58, 237), None),
            success: entry((21, 128, 61), None),
            warning: entry((202, 138, 4), None),
            danger: entry((220, 38, 38), None),
            info: entry((2, 132, 199), None),
            highlight: entry((254, 240, 138), None),
            diff_add: entry((21, 128, 61), None),
            diff_del: entry((220, 38, 38), None),
        }
    }

    pub fn high_contrast_default() -> Self {
        Self {
            bg: entry((0, 0, 0), Some(Color::Black)),
            bg_panel: entry((0, 0, 0), Some(Color::Black)),
            bg_selection: entry((0, 0, 255), Some(Color::Blue)),
            bg_overlay: entry((0, 0, 0), Some(Color::Black)),
            fg: entry((255, 255, 255), Some(Color::White)),
            fg_muted: entry((224, 224, 224), Some(Color::Gray)),
            fg_subtle: entry((160, 160, 160), Some(Color::DarkGray)),
            fg_on_accent: entry((0, 0, 0), Some(Color::Black)),
            fg_on_success: entry((0, 0, 0), Some(Color::Black)),
            fg_on_warning: entry((0, 0, 0), Some(Color::Black)),
            fg_on_danger: entry((255, 255, 255), Some(Color::White)),
            fg_on_info: entry((255, 255, 255), Some(Color::White)),
            fg_on_overlay: entry((255, 255, 255), Some(Color::White)),
            border: entry((160, 160, 160), Some(Color::DarkGray)),
            border_focused: entry((0, 255, 255), Some(Color::Cyan)),
            accent: entry((0, 255, 255), Some(Color::Cyan)),
            accent_alt: entry((255, 0, 255), Some(Color::Magenta)),
            success: entry((0, 255, 0), Some(Color::Green)),
            warning: entry((255, 255, 0), Some(Color::Yellow)),
            danger: entry((255, 0, 0), Some(Color::Red)),
            info: entry((0, 128, 255), Some(Color::Blue)),
            highlight: entry((255, 255, 0), Some(Color::LightYellow)),
            diff_add: entry((0, 255, 0), Some(Color::Green)),
            diff_del: entry((255, 0, 0), Some(Color::Red)),
        }
    }
}

fn entry((r, g, b): (u8, u8, u8), ansi: Option<Color>) -> PaletteEntry {
    PaletteEntry {
        rgb: Color::Rgb(r, g, b),
        ansi,
    }
}

#[cfg(test)]
mod tests {
    use super::{Palette, PaletteEntry};
    use ratatui::style::Color;

    fn entries(palette: &Palette) -> [&PaletteEntry; 24] {
        [
            &palette.bg,
            &palette.bg_panel,
            &palette.bg_selection,
            &palette.bg_overlay,
            &palette.fg,
            &palette.fg_muted,
            &palette.fg_subtle,
            &palette.fg_on_accent,
            &palette.fg_on_success,
            &palette.fg_on_warning,
            &palette.fg_on_danger,
            &palette.fg_on_info,
            &palette.fg_on_overlay,
            &palette.border,
            &palette.border_focused,
            &palette.accent,
            &palette.accent_alt,
            &palette.success,
            &palette.warning,
            &palette.danger,
            &palette.info,
            &palette.highlight,
            &palette.diff_add,
            &palette.diff_del,
        ]
    }

    #[test]
    fn dark_default_has_twenty_four_entries() {
        let palette = Palette::dark_default();
        let entries = entries(&palette);

        assert_eq!(entries.len(), 24);
        assert!(entries.iter().all(|entry| entry.ansi.is_some()));
        assert_eq!(palette.bg.ansi, Some(Color::Black));
    }

    #[test]
    fn light_default_has_twenty_four_entries() {
        let palette = Palette::light_default();

        assert_eq!(entries(&palette).len(), 24);
    }

    #[test]
    fn high_contrast_default_has_twenty_four_entries_with_full_ansi_map() {
        let palette = Palette::high_contrast_default();
        let entries = entries(&palette);

        assert_eq!(entries.len(), 24);
        assert!(entries.iter().all(|entry| entry.ansi.is_some()));
    }
}
