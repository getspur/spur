use std::collections::HashMap;
use std::time::Instant;

use crate::components::input_bar::InputBar;

use super::SessionDetailView;

#[cfg(any(test, debug_assertions))]
impl SessionDetailView {
    /// Test-only: read current InputBar text.
    #[doc(hidden)]
    pub fn input_bar_text_for_test(&self) -> String {
        self.input_bar.text()
    }

    /// Test-only: mutable InputBar access for seeding history in tests.
    #[doc(hidden)]
    pub fn input_bar_mut_for_test(&mut self) -> &mut InputBar {
        &mut self.input_bar
    }

    #[doc(hidden)]
    pub fn completion_active_for_test(&self) -> bool {
        self.completion.is_active()
    }

    #[doc(hidden)]
    pub fn cancel_hint_until_for_test(&self) -> Option<Instant> {
        self.cancel_hint_until
    }

    #[doc(hidden)]
    pub fn set_cancel_hint_until_for_test(&mut self, value: Option<Instant>) {
        self.cancel_hint_until = value;
    }

    #[doc(hidden)]
    pub fn set_stream_in_flight_for_test(&mut self, value: bool) {
        self.stream_in_flight = value;
    }

    /// Test-only: read tool_depth map.
    #[doc(hidden)]
    pub fn tool_depth_for_test(&self) -> &HashMap<String, u8> {
        &self.tool_depth
    }

    /// Test-only: mutable tool_depth map for seeding tests.
    #[doc(hidden)]
    pub fn tool_depth_for_test_mut(&mut self) -> &mut HashMap<String, u8> {
        &mut self.tool_depth
    }
}
