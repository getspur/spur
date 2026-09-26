//! Stable rule-catalog data consumed by the Jev routing battery.

use serde::{Deserialize, Serialize};

/// A versioned, caller-supplied view of the solver rule catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    /// Version of the catalog language used to describe the rules.
    pub language_version: u32,
    /// Rule cards available for routing.
    pub rules: Vec<RuleCard>,
}

/// The catalog metadata needed to route one rule.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCard {
    /// Stable rule identifier.
    pub rule_id: String,
    /// Owning family identifier from catalog metadata.
    pub family: String,
    /// Short catalog summary presented to the routing model.
    pub summary: String,
}
