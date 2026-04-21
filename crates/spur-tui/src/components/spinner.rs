//! Unified spinner frame sets and helpers for TUI animations.
//!
//! Centralises the various Braille/pulse/dot sequences that were previously
//! duplicated across `react_trace`, `inline_executor_card`, `agents_tree` and
//! `mod.rs`.

/// Classic Braille-pattern spinner (10 frames).
pub const BRAILLE: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Pulse spinner for streaming / data-flow states.
pub const PULSE: &[&str] = &["▸", "▹", "►", "▻"];

/// Slow dot crawl for indeterminate / connecting states.
pub const DOTS: &[&str] = &["   ", ".  ", ".. ", "..."];

/// Return the frame for `set` given a monotonically-increasing `tick`.
///
/// Divides by 2 so that at a typical 50–100 ms TUI tick the animation
/// advances at a comfortable ~2–5 Hz instead of a blur.
pub fn frame<'a>(set: &'a [&'a str], tick: u32) -> &'a str {
    set[(tick as usize / 2) % set.len()]
}
