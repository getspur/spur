use super::loader::Theme;
use super::ColorDepth;
use ratatui::style::Color;

/// Resolve a semantic token name to a concrete `ratatui::style::Color`,
/// honoring the active `ColorDepth`.
///
/// # Truecolor vs ANSI16
///
/// - `ColorDepth::Truecolor` returns the palette entry's `rgb` color.
/// - `ColorDepth::Ansi16` returns the palette entry's `ansi` color.
///
/// # Loader invariant
///
/// The resolver does **not** walk `extends:` chains at render time. The
/// loader (`load_theme_from_str` / `load_built_in`) collapses parent
/// fall-through into the materialized `Theme` so every palette entry
/// carries `Some(ansi)`. See `theme/loader.rs` and the spec's
/// "Loader / resolver contract" subsection.
///
/// # Panics
///
/// Programmer-error guards — none of these can fire on a theme that
/// flowed through the loader:
///
/// 1. The token is not bound in `theme.tokens` (missing token key).
/// 2. The token references a palette key not present in the palette.
/// 3. `depth == Ansi16` and the resolved palette entry has `ansi: None`.
///    Loaded themes always satisfy `Some(ansi)`; this only triggers when
///    a `Theme` is constructed in code (e.g. test fixtures with sparse
///    palettes) bypassing the loader.
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
