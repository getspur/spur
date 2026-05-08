//! Theme system scaffolding for palette, token, and loader support.

pub mod compat;
pub mod loader;
pub mod palette;
mod resolver;
pub mod runtime;
pub mod tokens;

pub use loader::{load_built_in, load_theme_from_str, Theme, ThemeError};
pub use resolver::resolve_token;
pub use runtime::{
    list_available_themes, load_runtime_theme, AvailableThemes, ThemeLoadOutcome,
    BUILT_IN_THEME_NAMES,
};

/// Terminal color-rendering capability that drives `resolve_token`'s
/// truecolor-vs-ANSI choice.
///
/// Selected once at startup (capability detection or `tui.color_depth`
/// override) and threaded through every render site that pulls colors
/// from the active `Theme`. The `dark` / `light` / `high-contrast`
/// built-ins ship with both a 24-bit `rgb` and an `ansi` per palette
/// entry, so either depth produces a usable rendering. Custom themes
/// loaded from YAML inherit `ansi` from their parent (or the `dark`
/// built-in) at load time — see `theme/loader.rs` and the spec's
/// "Loader / resolver contract" subsection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorDepth {
    /// 24-bit truecolor terminal — render the `rgb` palette entry.
    Truecolor,
    /// 16-color ANSI terminal — render the `ansi` palette entry.
    Ansi16,
}

/// Process-wide `&'static Theme` of the built-in `dark` theme. Use this
/// for `ViewContext` fixtures (unit and integration tests) and migration
/// callsites that don't yet receive an `Arc<Theme>`. Production code
/// reads from the active `Theme` carried by `ViewContext.theme`.
pub fn fallback_theme() -> &'static Theme {
    use std::sync::OnceLock;
    static DARK: OnceLock<Theme> = OnceLock::new();
    DARK.get_or_init(|| load_built_in("dark").expect("dark built-in must load"))
}

#[cfg(test)]
mod tests {
    mod fidelity;
}
