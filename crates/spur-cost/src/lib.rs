#![expect(
    clippy::doc_markdown,
    reason = "legacy cost docs contain product and field identifiers that need a dedicated markdown pass"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy table presenter builds output strings with format! append patterns"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy reports iterate hash maps where stable ordering is not part of the API"
)]
#![expect(
    clippy::ref_option,
    reason = "legacy ingestion helpers accept borrowed options at existing call sites"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy cost code predates the current to_owned style lint"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy ingestion/report modules import extension traits by name for readability"
)]
#![expect(
    clippy::use_self,
    reason = "legacy presenter tree code spells type names explicitly"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy presenter formatting uses pre-inline format argument style"
)]
#![expect(
    clippy::unnecessary_literal_bound,
    reason = "ingester trait implementations keep the original &str signature style"
)]
#![expect(
    clippy::useless_let_if_seq,
    reason = "legacy Codex ingestion keeps fallback state mutation explicit"
)]

pub mod db;
pub mod estimator;
pub mod ingest;
pub mod presenter;
pub mod pricing;
pub mod reporter;
pub mod reports;
pub mod tracker;

pub use db::{
    CostSummary, DelegationRecord, ModelCostSummary, ProjectCostSummary, SessionRecord,
    TokenSummary,
};
pub use ingest::{IngestionPipeline, TokenEvent};
pub use pricing::{
    calculate_cost, calculate_cost_for_model, ModelPricing, PricingRegistry, TieredPricing,
    TokenUsage,
};
pub use tracker::CostTracker;
