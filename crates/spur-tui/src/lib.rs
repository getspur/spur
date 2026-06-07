#![expect(
    elided_lifetimes_in_paths,
    reason = "legacy TUI render function signatures omit explicit Frame lifetimes"
)]
#![expect(
    clippy::allow_attributes,
    reason = "legacy TUI code still contains localized allow attributes"
)]
#![expect(
    clippy::branches_sharing_code,
    reason = "legacy TUI wrapping/rendering code keeps branch-local structure for readability"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy TUI docs contain UI/component terms that are not consistently backticked yet"
)]
#![expect(
    clippy::bool_to_int_with_if,
    reason = "legacy TUI layout code uses explicit boolean branches for dimensions"
)]
#![expect(
    clippy::clone_on_ref_ptr,
    reason = "legacy TUI image/config code still uses method-call clone syntax for Arc values"
)]
#![expect(
    clippy::derive_partial_eq_without_eq,
    reason = "legacy TUI DTOs derive PartialEq without consistently deriving Eq"
)]
#![expect(
    clippy::equatable_if_let,
    reason = "legacy TUI input code uses if-let patterns for key checks"
)]
#![expect(
    clippy::elidable_lifetime_names,
    reason = "legacy TUI component impls spell explicit lifetimes"
)]
#![expect(
    clippy::enum_glob_use,
    reason = "legacy TUI event handlers use enum variant glob imports"
)]
#![expect(
    clippy::explicit_iter_loop,
    reason = "legacy TUI rendering loops sometimes call iter explicitly for clarity"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy TUI renderers append formatted strings directly"
)]
#![expect(
    clippy::future_not_send,
    reason = "legacy TUI futures capture non-Send UI state and run on the UI task"
)]
#![expect(
    clippy::ignored_unit_patterns,
    reason = "legacy TUI select branches use wildcard unit patterns"
)]
#![expect(
    clippy::implicit_clone,
    reason = "legacy TUI code sometimes clones strings through to_string"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy TUI worker stream rendering iterates hash maps in UI state updates"
)]
#![expect(
    clippy::manual_let_else,
    reason = "legacy TUI code still contains match-based early-return control flow"
)]
#![expect(
    clippy::manual_string_new,
    reason = "legacy TUI table construction uses empty string conversions"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy TUI theme parsing maps errors into domain errors without preserving all sources"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy TUI input matches keep duplicated arms for interaction readability"
)]
#![expect(
    clippy::match_wildcard_for_single_variants,
    reason = "legacy TUI state matches use wildcard arms for future-proofing"
)]
#![expect(
    clippy::missing_assert_message,
    reason = "legacy TUI debug assertions omit explicit messages"
)]
#![expect(
    clippy::needless_pass_by_ref_mut,
    reason = "legacy TUI component APIs keep mutable receiver/reference shapes for compatibility"
)]
#![expect(
    clippy::or_fun_call,
    reason = "legacy TUI action routing uses eager option fallbacks"
)]
#![expect(
    clippy::option_option,
    reason = "legacy TUI startup state uses nested Option to represent three states"
)]
#![expect(
    clippy::ref_patterns,
    reason = "legacy TUI action routing still uses explicit ref bindings"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy TUI option extraction uses and_then chains"
)]
#![expect(
    clippy::redundant_type_annotations,
    reason = "legacy TUI local annotations are kept for readability"
)]
#![expect(
    clippy::semicolon_if_nothing_returned,
    reason = "legacy TUI render branches omit semicolons in unit-returning expressions"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy TUI code has many &str to String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::string_add,
    reason = "legacy TUI truncation helpers use string concatenation"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy TUI formatting has not all moved to captured format args"
)]
#![expect(
    clippy::unnested_or_patterns,
    reason = "legacy TUI input matching keeps separate key alternatives for readability"
)]
#![expect(
    clippy::unused_async,
    reason = "legacy TUI async APIs preserve call-site compatibility"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy TUI modules import extension traits by name"
)]
#![expect(
    clippy::unnecessary_wraps,
    reason = "legacy TUI action helpers preserve Option return shapes used by callers"
)]
#![expect(
    clippy::unnecessary_literal_bound,
    reason = "legacy TUI query source traits keep receiver-tied string return signatures"
)]
#![expect(
    clippy::useless_let_if_seq,
    reason = "legacy TUI event handlers keep mutation flags explicit"
)]
#![expect(
    clippy::unused_self,
    reason = "legacy TUI methods keep receiver shape for API consistency"
)]
#![expect(
    clippy::use_self,
    reason = "legacy TUI code often spells concrete type names in impl bodies"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy TUI code uses match for nontrivial fallback branches"
)]

pub mod action;
pub mod agents;
pub mod app;
pub mod commands;
pub mod components;
pub mod input_history;
pub mod landing;
pub mod mentions;
pub mod notebook_daemon;
pub mod session_metadata;
pub mod theme;
pub mod tui;
pub mod views;
pub mod worker_streams;

pub use app::{run_tui, UserInput};

#[doc(hidden)]
pub mod test_support {
    //! Test-only helpers that poke at session state without spinning up a
    //! full `App`. Not a stable API; gated behind a hidden module so
    //! integration tests in `tests/` can exercise `apply_session_update`.

    use crate::{action::Action, views::session_detail::SessionDetailView};
    use spur_acp::{SessionId, SessionNotification, SpurEvent};

    /// Build a `ViewContext` backed by the given lineage and an idle brain
    /// status. Suitable for integration tests that don't exercise brain
    /// status rendering.
    pub fn test_view_ctx(
        lineage: &spur_core::lineage::projection::ExecutorLineage,
    ) -> crate::views::ViewContext<'_> {
        static IDLE: crate::app::BrainStatus = crate::app::BrainStatus::Idle;
        static PLAN_PROJECTION: std::sync::OnceLock<spur_core::PlanProjectionStore> =
            std::sync::OnceLock::new();
        static SYNOPSIS: std::sync::OnceLock<spur_core::SessionSynopsisProjection> =
            std::sync::OnceLock::new();
        crate::views::ViewContext {
            lineage,
            plan_projection: PLAN_PROJECTION.get_or_init(spur_core::PlanProjectionStore::new),
            synopsis: SYNOPSIS.get_or_init(spur_core::SessionSynopsisProjection::new),
            brain_status: &IDLE,
            license_badge: None,
            flag_summary: None,
            tombstone: None,
            transient_hint_override: None,
            notebook_ready: false,
            theme: crate::theme::fallback_theme(),
        }
    }

    /// Build a minimal `Arc<AgentConfig>` with all-default nested blocks,
    /// suitable for constructing `SessionDetailView` in integration tests
    /// that don't exercise any ingest/response bindings.
    pub fn default_agent_config(name: &str) -> std::sync::Arc<spur_acp::AgentConfig> {
        std::sync::Arc::new(spur_acp::AgentConfig::with_defaults(name))
    }

    /// Build a fresh `SessionDetailView` with placeholder identity fields so
    /// tests can apply session updates to it.
    pub fn new_session_state() -> SessionDetailView {
        SessionDetailView::new(
            SessionId("test".to_string()),
            "test-agent".to_string(),
            "brain".to_string(),
            std::path::PathBuf::from("."),
            default_agent_config("test-agent"),
            Vec::new(),
        )
    }

    /// Route a `SessionNotification`'s update through
    /// `app::apply_session_update` for assertions in tests.
    pub fn apply_notification(state: &mut SessionDetailView, notif: &SessionNotification) {
        crate::app::apply_session_update(state, &notif.update);
    }

    /// Build a fresh `App` with no user-input channel (actions emitted by
    /// `App::process_action` will silently drop). Intended for assertions
    /// that don't exercise the outbound channel.
    pub fn new_app() -> crate::app::App {
        crate::app::App::new(None, false)
    }

    /// Build a fresh `App` with a live user-input channel for UAT tests.
    pub fn app_with_user_input_tx() -> (
        crate::app::App,
        tokio::sync::mpsc::Receiver<crate::app::UserInput>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<crate::app::UserInput>(64);
        (crate::app::App::new(Some(tx), false), rx)
    }

    /// Dispatch a `SpurEvent` into the app exactly as the runtime loop would.
    pub fn push_event(app: &mut crate::app::App, ev: SpurEvent) {
        app.handle_spur_event(ev);
    }

    /// Query whether a feature key is granted by the App's current feature gate.
    pub fn feature_enabled(app: &crate::app::App, key: spur_license::FeatureKey) -> bool {
        app.feature_enabled_for_test(key)
    }

    /// Dispatch an `Action` through the app controller.
    pub fn process_action(app: &mut crate::app::App, action: Action) {
        app.process_action(action);
    }

    /// Borrow the current `SessionDetailView`, if one exists.
    pub fn session_detail(app: &crate::app::App) -> Option<&SessionDetailView> {
        app.session_detail_for_test()
    }

    /// Borrow the pending first user message, if one exists.
    pub fn pending_first_user_message(app: &crate::app::App) -> Option<&str> {
        app.pending_first_user_message_for_test()
    }
}
