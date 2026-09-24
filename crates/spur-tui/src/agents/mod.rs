//! Config-driven dispatch hooks. Types declared in `spur-acp::config`
//! (strongly-typed deserialize); behavior implemented here where it has
//! access to `AgentConnection`, `AvailableCommand`, and `SessionDetailView`.
//!
//! | Hook ID                 | Kind                     | Where implemented    |
//! |-------------------------|--------------------------|----------------------|
//! | prompt_text             | DispatchKind             | entry_builder + submit_shell |
//! | vendor_exec             | DispatchKind             | entry_builder + submit_shell + orchestrator |
//! | raw_rest                | ArgsTemplateKind         | submit_shell |
//! | json_path_list          | IngestParserKind         | ingest::run_ingest_hook |
//! | acp_available_command   | ItemSchemaKind           | ingest::run_ingest_hook |
//! | system_note             | ResponseRenderKind       | session_detail::render_response |
//!
//! The pure entry builders moved to `spur-commands` (spec 2026-09-23
//! §3.3/§5 C2); they are re-exported so `crate::agents::build_entry`
//! call sites compile unchanged.

pub mod ingest;

pub use ingest::run_ingest_hook;
pub use spur_commands::entry_builder::{build_entry, build_static_entry};
