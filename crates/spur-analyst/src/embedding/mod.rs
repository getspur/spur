mod in_process;
mod mode;
mod model_cache;
mod runtime;

#[cfg(all(test, feature = "embed"))]
pub(crate) use in_process::embed_with_ready_model;
#[cfg(feature = "embed")]
pub(crate) use in_process::load_embed_model;
#[cfg(all(test, feature = "embed"))]
pub(crate) use in_process::set_embed_query_disabled_for_test;
#[cfg(test)]
pub(crate) use mode::set_analyst_embed_mode_for_test;
#[cfg(test)]
pub(crate) use mode::AnalystEmbedMode;
#[cfg(feature = "embed")]
pub(crate) use model_cache::embed_model_cell;
#[cfg(all(test, feature = "embed"))]
pub(crate) use model_cache::EmbedModelCell;
#[cfg(test)]
pub(crate) use runtime::reset_auto_sidecar_probe_for_test;
pub use runtime::{warm_embed_model, EmbeddingRuntime};
