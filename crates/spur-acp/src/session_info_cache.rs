//! `SessionInfoCache` — orchestrator-owned mirror of the agent's last
//! `SessionInfoUpdate` payload.
//!
//! Lives on `BrainSession` (`crates/spur-core/src/orchestrator.rs`)
//! parallel to `config_options` and `Arc<SpurAgentCaps>`. M9 hoist
//! (`docs/superpowers/plans/2026-04-27-m9-spur-acp-followups.md` §1)
//! moved this off `SessionDetailView` so the cached title survives the
//! view's destruction on navigation away from the session detail screen.
//!
//! Mirror of `agent_client_protocol::schema::SessionInfoUpdate` flattened
//! into plain `Option<String>` so consumers do not propagate the SDK's
//! `MaybeUndefined` distinction.

use agent_client_protocol::schema::SessionInfoUpdate;

/// Last-known `SessionInfoUpdate` payload for a session. `None` fields
/// mean "agent has never emitted this field" (or emitted it as JSON
/// null; spur folds both into `None`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionInfoCache {
    /// Human-readable session title.
    pub title: Option<String>,
    /// ISO 8601 timestamp of last activity.
    pub updated_at: Option<String>,
}

impl SessionInfoCache {
    /// Merge a `SessionInfoUpdate` payload into this cache using ACP
    /// `MaybeUndefined` semantics:
    ///
    /// * `Undefined` (field absent from the wire payload) — preserve the
    ///   existing cached value.
    /// * `Null` — clear the cached value (set to `None`).
    /// * `Value(v)` — overwrite the cached value with `Some(v)`.
    pub fn merge(&mut self, info: &SessionInfoUpdate) {
        if let Some(title) = info.title.as_opt_deref::<str>() {
            self.title = title.map(str::to_owned);
        }
        if let Some(updated_at) = info.updated_at.as_opt_deref::<str>() {
            self.updated_at = updated_at.map(str::to_owned);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SessionInfoCache;
    use agent_client_protocol::schema::SessionInfoUpdate;

    #[test]
    fn merge_value_sets_field() {
        let mut cache = SessionInfoCache::default();
        let payload = SessionInfoUpdate::new().title("First");
        cache.merge(&payload);
        assert_eq!(cache.title.as_deref(), Some("First"));
    }

    #[test]
    fn merge_undefined_preserves_existing() {
        let mut cache = SessionInfoCache {
            title: Some("First".to_string()),
            updated_at: None,
        };
        // SessionInfoUpdate::new() leaves both fields Undefined.
        let payload = SessionInfoUpdate::new();
        cache.merge(&payload);
        assert_eq!(
            cache.title.as_deref(),
            Some("First"),
            "Undefined must preserve existing value"
        );
    }

    #[test]
    fn merge_chained_updates_overwrites_value() {
        let mut cache = SessionInfoCache::default();
        cache.merge(&SessionInfoUpdate::new().title("First"));
        cache.merge(&SessionInfoUpdate::new().title("Second"));
        assert_eq!(cache.title.as_deref(), Some("Second"));
    }
}
