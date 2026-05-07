use super::loader::Theme;
use super::ColorDepth;
use ratatui::style::Color;

pub fn resolve_token(theme: &Theme, token: &str, depth: ColorDepth) -> Color {
    let palette_key = theme
        .tokens
        .0
        .get(token)
        .unwrap_or_else(|| panic!("theme `{}` does not define token `{}`", theme.name, token));

    let entry = theme
        .tokens
        .resolve(token, &theme.palette)
        .unwrap_or_else(|| {
            panic!(
                "theme `{}` token `{}` references unknown palette entry `{}`",
                theme.name, token, palette_key
            )
        });

    match depth {
        ColorDepth::Truecolor => entry.rgb,
        ColorDepth::Ansi16 => entry.ansi.unwrap_or_else(|| {
            panic!(
                "theme `{}` token `{}` palette entry `{}` has no ANSI fallback",
                theme.name, token, palette_key
            )
        }),
    }
}
