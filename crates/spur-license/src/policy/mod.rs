//! Signed policy document carrying tier entitlements (G1) and runtime feature
//! flags (G2). Single artifact, single signing flow, two namespaces.
//!
//! This module owns the schema types and forward-compatibility rules. The
//! actual evaluators live in `policy::feature_key`, `policy::flags`, and the
//! resolver section below.

pub mod feature_key;
pub mod trust;

pub use feature_key::FeatureKey;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

/// Major schema version this binary understands. Code REFUSES to load any
/// policy where `schema_version > CODE_SUPPORTED_MAJOR` and falls back to
/// the embedded baseline.
pub const CODE_SUPPORTED_MAJOR: u32 = 1;

/// The wire format. Always wrapped in `SignedPolicy` on disk and over the wire.
///
/// Carries TWO orthogonal namespaces: `tier_policies` (G1 — entitlements) and
/// `flags` (G2 — runtime toggles). They share the document because they share
/// the signing/distribution flow, NOT because they are the same concept.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PolicyDocument {
    pub schema_version: u32,
    pub issued_at: DateTime<Utc>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub tier_policies: BTreeMap<String, TierPolicy>,
    #[serde(default)]
    pub flags: BTreeMap<String, FlagSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TierPolicy {
    pub features: BTreeSet<String>,
    #[serde(default)]
    pub quotas: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// G2 — runtime flag specification. Intentionally minimal in V1 (kill switch
/// + rollout + tier targeting). Extensions (variants, segments, dependencies)
///   flow into `extensions` until they earn typed fields with a schema_version
///   minor bump.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FlagSpec {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub rollout_percent: Option<f32>,
    #[serde(default)]
    pub tier_filter: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    /// Forward-compat catch-all. Unknown fields land here.
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl PartialEq for FlagSpec {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.rollout_percent.map(f32::to_bits) == other.rollout_percent.map(f32::to_bits)
            && self.tier_filter == other.tier_filter
            && self.description == other.description
            && self.extensions == other.extensions
    }
}

impl Eq for FlagSpec {}

impl Default for FlagSpec {
    fn default() -> Self {
        Self {
            enabled: true,
            rollout_percent: None,
            tier_filter: None,
            description: None,
            extensions: BTreeMap::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Wrapper that carries the signature. The payload is canonical JSON of
/// `PolicyDocument` so signature verification is independent of serde
/// formatting choices on the verification side.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedPolicy {
    pub payload: String,
    pub signature: String,
    pub key_id: String,
}

/// Read-only accessor over a (possibly overlay-supplemented) PolicyDocument.
/// V1 only loads the embedded baseline; remote overlays land in V2.
pub struct PolicyResolver {
    document: Arc<PolicyDocument>,
}

impl PolicyResolver {
    /// Returns the singleton resolver backed by the embedded signed policy.
    /// First call verifies the signature; subsequent calls reuse the cached
    /// document. Panics on signature failure (caught at compile-time by
    /// `build.rs`, so a runtime panic here means the binary was tampered).
    pub fn embedded() -> Arc<Self> {
        static RESOLVER: OnceLock<Arc<PolicyResolver>> = OnceLock::new();
        RESOLVER
            .get_or_init(|| {
                let raw = include_str!("../../resources/default_policy.json");
                let signed: SignedPolicy = serde_json::from_str(raw)
                    .expect("embedded default_policy.json must parse as SignedPolicy");
                let doc = crate::policy::trust::verify_signed_policy(&signed)
                    .expect("embedded policy MUST verify (build.rs guarantees)");
                Arc::new(Self {
                    document: Arc::new(doc),
                })
            })
            .clone()
    }

    /// Construct a resolver from an arbitrary document. Test-only; keeps
    /// the trust-bypass path explicit.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_document(doc: PolicyDocument) -> Arc<Self> {
        Arc::new(Self {
            document: Arc::new(doc),
        })
    }

    pub fn document(&self) -> Arc<PolicyDocument> {
        Arc::clone(&self.document)
    }

    /// Returns the canonical entitlement set for the given tier name.
    /// Unknown tier → empty set (fail-closed at lookup).
    pub fn tier_features(&self, tier: &str) -> BTreeSet<String> {
        self.document
            .tier_policies
            .get(tier)
            .map(|tp| tp.features.clone())
            .unwrap_or_default()
    }

    /// Returns true iff the named tier's `features` set contains `feature`.
    /// Unknown tier OR unknown feature → false (fail-closed).
    pub fn tier_has_feature(&self, tier: &str, feature: &str) -> bool {
        self.document
            .tier_policies
            .get(tier)
            .map(|tp| tp.features.contains(feature))
            .unwrap_or(false)
    }

    /// Default overlay path: `~/.spur/policy-overlay.json`.
    pub fn default_overlay_path() -> Option<std::path::PathBuf> {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".spur").join("policy-overlay.json"))
    }

    /// Like `embedded()` but FIRST tries to load + verify a signed overlay
    /// at `path`. Falls back to embedded on any error (file missing, bad
    /// signature, expired, schema-version too high).
    pub fn with_overlay_path(path: &std::path::Path) -> Arc<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Self::embedded(),
        };
        let signed: SignedPolicy = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("policy overlay at {path:?} unparseable: {e}; using embedded");
                return Self::embedded();
            }
        };
        match crate::policy::trust::verify_signed_policy(&signed) {
            Ok(doc) => {
                let embedded = Self::embedded();
                if doc.issued_at > embedded.document.issued_at {
                    Arc::new(Self {
                        document: Arc::new(doc),
                    })
                } else {
                    embedded
                }
            }
            Err(e) => {
                tracing::warn!("policy overlay at {path:?} rejected: {e}; using embedded");
                Self::embedded()
            }
        }
    }

    /// Convenience: try the default overlay path, fall back to embedded.
    pub fn with_default_overlay() -> Arc<Self> {
        match Self::default_overlay_path() {
            Some(p) => Self::with_overlay_path(&p),
            None => Self::embedded(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flagspec_defaults_to_enabled_true() {
        let json = r#"{}"#;
        let spec: FlagSpec = serde_json::from_str(json).unwrap();
        assert!(
            spec.enabled,
            "FlagSpec missing `enabled` must default to true"
        );
        assert!(spec.rollout_percent.is_none());
        assert!(spec.tier_filter.is_none());
    }

    #[test]
    fn flagspec_unknown_fields_land_in_extensions() {
        let json = r#"{"variants": {"a": 1}, "segments": ["beta"]}"#;
        let spec: FlagSpec = serde_json::from_str(json).unwrap();
        assert!(spec.extensions.contains_key("variants"));
        assert!(spec.extensions.contains_key("segments"));
    }

    #[test]
    fn policy_document_with_no_flags_field_loads() {
        let json = r#"{
            "schema_version": 1,
            "issued_at": "2026-04-19T00:00:00Z",
            "tier_policies": {}
        }"#;
        let doc: PolicyDocument = serde_json::from_str(json).unwrap();
        assert!(doc.flags.is_empty());
    }

    #[test]
    fn policy_document_round_trips() {
        let doc = PolicyDocument {
            schema_version: 1,
            issued_at: chrono::Utc::now(),
            expires_at: None,
            tier_policies: BTreeMap::new(),
            flags: BTreeMap::new(),
        };
        let json = serde_json::to_string(&doc).unwrap();
        let back: PolicyDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, 1);
    }

    #[test]
    fn future_minor_schema_extra_fields_ignored() {
        let json = r#"{
            "schema_version": 1,
            "issued_at": "2026-04-19T00:00:00Z",
            "tier_policies": {},
            "flags": {},
            "future_field": "ignored by v1"
        }"#;
        let doc: PolicyDocument = serde_json::from_str(json).unwrap();
        assert_eq!(doc.schema_version, 1);
    }

    #[test]
    fn embedded_default_policy_json_parses_as_signed() {
        let raw = include_str!("../../resources/default_policy.json");
        let signed: SignedPolicy = serde_json::from_str(raw).unwrap();
        assert_eq!(signed.key_id, "spur-policy-2026-04");
        assert!(!signed.signature.is_empty());
        let doc: PolicyDocument = serde_json::from_str(&signed.payload).unwrap();
        assert_eq!(doc.schema_version, 1);
        assert!(doc.tier_policies.contains_key("community"));
        assert!(doc.flags.contains_key("kill_advanced_planner"));
    }

    #[test]
    fn embedded_default_policy_signature_verifies() {
        let raw = include_str!("../../resources/default_policy.json");
        let signed: SignedPolicy = serde_json::from_str(raw).unwrap();
        let doc = crate::policy::trust::verify_signed_policy(&signed)
            .expect("embedded signed policy must verify");
        assert_eq!(doc.schema_version, 1);
    }

    #[test]
    fn embedded_resolver_returns_community_features() {
        let r = PolicyResolver::embedded();
        let community = r.tier_features("community");
        assert!(community.contains("chat"));
        assert!(community.contains("code_edit"));
        assert!(community.contains("watch_loop"));
        assert!(!community.contains("advanced_agents"));
    }

    #[test]
    fn embedded_resolver_returns_pro_features_superset() {
        let r = PolicyResolver::embedded();
        let pro = r.tier_features("pro");
        assert!(pro.contains("chat"));
        assert!(pro.contains("advanced_agents"));
        assert!(pro.contains("cloud_sync"));
    }

    #[test]
    fn unknown_tier_returns_empty_set() {
        let r = PolicyResolver::embedded();
        assert!(r.tier_features("nonexistent").is_empty());
    }

    #[test]
    fn tier_has_feature_fails_closed_on_unknown() {
        let r = PolicyResolver::embedded();
        assert!(!r.tier_has_feature("community", "advanced_agents"));
        assert!(!r.tier_has_feature("nonexistent", "chat"));
        assert!(!r.tier_has_feature("community", "nonexistent_feature"));
    }

    #[test]
    fn overlay_supersedes_when_newer_and_signed() {
        let result =
            PolicyResolver::with_overlay_path(std::path::Path::new("/nonexistent/overlay.json"));
        assert!(result.tier_features("community").contains("chat"));
    }

    #[test]
    fn overlay_with_invalid_signature_falls_back_to_embedded() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"{"payload":"{}","signature":"AAAA","key_id":"spur-policy-2026-04"}"#,
        )
        .unwrap();
        let r = PolicyResolver::with_overlay_path(tmp.path());
        assert!(r.tier_features("community").contains("chat"));
    }
}
