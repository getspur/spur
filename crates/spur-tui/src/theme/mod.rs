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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorDepth {
    Truecolor,
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
