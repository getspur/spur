//! Config-driven dispatch hooks. Types declared in `spur-acp::config`
//! (strongly-typed deserialize); behavior implemented here where it has
//! access to `AgentConnection`, `AvailableCommand`, and `SessionDetailView`.
//!
//! Spec 1 ships seven hooks:
//!
//! | Hook ID                 | Kind                     | Where implemented    |
//! |-------------------------|--------------------------|----------------------|
//! | prompt_text             | DispatchKind             | entry_builder + submit_router |
//! | vendor_exec             | DispatchKind             | entry_builder + submit_router + orchestrator |
//! | raw_rest                | ArgsTemplateKind         | submit_router |
//! | json_path_list          | IngestParserKind         | ingest::run_ingest_hook |
//! | acp_available_command   | ItemSchemaKind           | ingest::run_ingest_hook |
//! | system_note             | ResponseRenderKind       | session_detail::render_response |
//! | files                   | MentionSourceKind        | (Spec 2; no behavior today) |

pub mod entry_builder;
pub mod ingest;

pub use entry_builder::build_entry;
pub use ingest::run_ingest_hook;
