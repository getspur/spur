//! Session (snapshot-backed) mention sources — the sud-m3 adapter contract.
//!
//! `worker`, `issue`, and `datasource` rows come from snapshots the app
//! pushes into the registry (agent registry, tracked issues, notebook
//! datasource bridge). Unlike the filesystem/code-graph sources, they have
//! no TUI-trait extras (no code payloads, no candidate hydration), so they
//! implement the neutral [`spur_mentions::MentionSource`] directly: the
//! engine builds them without a wrapper, TUI-only row state travels in the
//! sidecar each build writes (decoupling spec §5 Phase M3), and the
//! snapshot setters stamp a monotonically bumped data-revision token so
//! engine cache identity invalidates on replace without waiting out the
//! TTL.

use std::collections::HashMap;
use std::sync::Arc;

use spur_mentions::MentionSource as NeutralMentionSource;

use super::sidecar::TuiMentionSidecar;

/// Facade-facing surface of the session sources, implemented alongside the
/// neutral trait by [`super::WorkerMentionSource`],
/// [`super::IssueMentionSource`], and [`super::DatasourceMentionSource`].
///
/// Everything the engine needs comes from the neutral trait (`key`,
/// `source_token`, `build`); the methods here are what the TUI facade
/// reads when it adopts a snapshot the engine just built.
pub(crate) trait SessionMentionSource: NeutralMentionSource {
    /// TUI slot name for this source; equals [`NeutralMentionSource::key`].
    fn slot_name(&self) -> &'static str;

    /// Sidecar records written by the most recent build, keyed by row id.
    fn sidecar(&self) -> &TuiMentionSidecar;

    /// Adapter-owned datasource prompt hints from the most recent build
    /// (`None` for sources without datasource extras). Like code payloads,
    /// hints deliberately stay out of the sidecar mapping.
    fn datasource_hints(&self) -> Option<&HashMap<String, Arc<String>>>;
}
