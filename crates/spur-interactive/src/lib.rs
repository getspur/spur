#![expect(
    clippy::clone_on_ref_ptr,
    reason = "legacy interactive bridge code still uses method-call clone syntax for Arc values"
)]
#![expect(
    clippy::large_futures,
    reason = "legacy interactive host awaits large orchestrator futures directly"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy shutdown path uses match to keep timeout handling visually explicit"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy modules import extension traits by name"
)]
#![expect(
    clippy::use_self,
    reason = "legacy trait forwarding code spells concrete trait names"
)]

pub mod data_loop;
pub mod host;

pub use host::{
    validate_frontend_command, DataQuery, InteractiveFrontendHandle, InteractiveFrontendHost,
    ReviewSubmission,
};
