//! Theme system scaffolding for palette, token, and loader support.

pub mod loader;
pub mod palette;
mod resolver;
pub mod tokens;

pub use resolver::resolve_token;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorDepth {
    Truecolor,
    Ansi16,
}
