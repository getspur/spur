//! Shared helpers for palette_integration tests.
//!
//! Centralizes the thorny construction of an `App` with a pre-seeded
//! session_detail + command registry, so individual tests stay short.

use spur_tui::app::App;

/// Construct an `App` whose `session_detail` has a `CommandRegistry`
/// containing a single dynamic command `/{name}` with `description` for
/// agent handle `handle`. The session_detail is otherwise minimal.
///
/// Intended only for palette-integration tests.
pub fn app_with_seeded_session_and_dynamic_command(
    handle: &str,
    name: &str,
    description: &str,
) -> App {
    let mut app = App::new_for_palette_test();
    app.seed_session_detail_with_dynamic_command_for_test(handle, name, description);
    app
}
