//! Phase 0 scaffold gate (spec §2.3): the facade re-export paths resolve.
//!
//! `spur-utilities` holds re-exports only — `commands` (`spur-commands`) and
//! `mentions` (`spur-mentions`) — and the facade itself never enables the
//! `code` feature of `spur-mentions`.

#[test]
fn facade_reexports_commands_and_mentions() {
    // Both family members resolve through the facade's re-export paths.
    use spur_utilities::{commands as _, mentions as _};
}
