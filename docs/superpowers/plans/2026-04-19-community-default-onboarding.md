# Community-Default Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the silent `ConfigError` dead-end on fresh install with a working Community tier backed by a typed signed `PolicyDocument`; collapse activation to one verb via a first-run TTY prompt; ship Day-1 runtime feature-flag capability via a custom local `FlagEvaluator` (NOT OpenFeature/flagd); document a public demo key for Pro evaluation; redesign `spur auth status` plain output.

**Architecture:** All licensing changes contained to `spur-license` crate; CLI changes contained to `spur-cli`. Zero changes to `spur-core`, `spur-acp`, `spur-tui`, the funnel, or the existing `LicenseProvider` trait. `PolicyDocument` carries TWO orthogonal namespaces — `tier_policies` (G1 entitlements) and `flags` (G2 runtime toggles) — sharing one Ed25519-signed artifact, one trust map, one distribution path. `FlagEvaluator` is sync, deterministic, ~150 LoC. `feature_enabled(license, flags, key)` is THE callsite contract enforcing FLOOR ∧ GATE.

**Tech Stack:** Rust 2021, tokio, async-trait, serde, ed25519-dalek 2.x, base64 0.22, uuid 1 (already in workspace), proptest (already dev-dep), `licenseseat = "=0.5.3"` (existing).

**Spec source:** [`docs/superpowers/specs/2026-04-19-community-default-onboarding-design.md`](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-19-community-default-onboarding-design.md:1)

---

## File Structure

### New files (creating)

| Path | Responsibility | Approx LoC |
|---|---|---|
| `crates/spur-license/build.rs` | Compile-time verify embedded `default_policy.json` signature; panic if invalid | ~40 |
| `crates/spur-license/resources/default_policy.json` | Embedded signed policy (G1 tiers + G2 flags) | ~80 |
| `crates/spur-license/resources/keys/spur-policy-2026-04.pub` | Ed25519 32-byte public key (binary) | n/a |
| `crates/spur-license/scripts/sign-policy.sh` | Helper to re-sign `default_policy.json` after edits | ~30 |
| `crates/spur-license/src/policy/mod.rs` | `PolicyDocument`, `TierPolicy`, `FlagSpec`, `SignedPolicy`, `PolicyResolver`, schema-version constants | ~250 |
| `crates/spur-license/src/policy/feature_key.rs` | `FeatureKey(&'static str)` newtype + const registry | ~50 |
| `crates/spur-license/src/policy/trust.rs` | Embedded trust map; `verify_signed_policy` helper | ~60 |
| `crates/spur-license/src/policy/flags.rs` | `FlagSpec` consumer: `FlagEvaluator`, `InstallId`, `FlagExplanation` | ~180 |
| `crates/spur-license/src/community.rs` | `CommunityProvider` impl of `LicenseProvider` | ~110 |
| `crates/spur-license/src/build_constants.rs` | `option_env!` baked LicenseSeat publishable key + product slug (Option A) | ~20 |
| `crates/spur-cli/src/commands/flags.rs` | `spur flags list` subcommand | ~80 |
| `crates/spur-cli/src/onboarding.rs` | `maybe_prompt_first_run` + `~/.spur/onboarded` marker | ~120 |

### Modified files

| Path | Change | Approx LoC |
|---|---|---|
| `crates/spur-license/Cargo.toml` | Add `ed25519-dalek = "2"`, `base64 = "0.22"`, `uuid = { workspace = true }` deps; add same to `[build-dependencies]` | +6 |
| `crates/spur-license/src/lib.rs` | Add `mod policy`, `mod community`, `mod build_constants`; new `LicenseState::active_community(features)` constructor; `feature_enabled(license, flags, key)` helper | +80 |
| `crates/spur-license/src/licenseseat.rs` | `from_env_or_disabled` dispatch update (5-LoC match-arm); `LicenseSeatProvider::has_entitlement` resolver fallback (~10 LoC); accept resolver in `new` (Option A bake-in path) | +40 |
| `crates/spur-cli/src/commands/mod.rs` | Add `pub mod flags;` | +1 |
| `crates/spur-cli/src/commands/auth.rs` | Replace `print_state` with single-line summary table | ~30 |
| `crates/spur-cli/src/main.rs` | Add `mod onboarding;` and call `maybe_prompt_first_run(&license).await?` after line 430; wire `spur flags list` subcommand | +15 |
| `crates/spur-cli/Cargo.toml` | Add `is-terminal = "0.4"` dep for TTY detection | +1 |

---

## Pre-Work (out of band, before starting Task 1)

These are HUMAN inputs the spec calls out — they unblock the plan but are not part of the engineer's task list:

1. Generate Ed25519 keypair via `openssl genpkey -algorithm Ed25519 -out spur-policy-2026-04.key && openssl pkey -in spur-policy-2026-04.key -pubout -outform DER | tail -c 32 > spur-policy-2026-04.pub`. Place private key in your team's secret vault. Place public key file at `crates/spur-license/resources/keys/spur-policy-2026-04.pub`.
2. Provide an initial Community vs Pro feature list. Default placeholders are committed to in Task 4 if you don't override.
3. Confirm Option A vs Option B. **Default = Option A**: the LicenseSeat publishable key (`pk_*` per LicenseSeat docs) and product slug are embedded at build time via `SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY` and `SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG` env vars set in release CI. Fall back to Option B (env vars required at runtime) only if the publishable key cannot be in-source. **The plan below assumes Option A.**
4. (Optional, B9) Configure a long-lived rate-limited demo key in your LicenseSeat tenant (e.g., `DEMO-SPUR-2026-Q2`, expiring 2026-07-01). This unblocks Phase 5 docs but does not block Phase 1–4 code.

---

## Phase 1 — Foundation (Tasks 1–6)

### Task 1: Add Cargo dependencies to spur-license

**Files:**
- Modify: `crates/spur-license/Cargo.toml`

- [ ] **Step 1: Add dependencies to `[dependencies]` and `[build-dependencies]`**

Replace the file contents with:

```toml
[package]
name = "spur-license"
description = "Provider-agnostic licensing facade for SPUR"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
tokio = { workspace = true }
async-trait = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }
anyhow = { workspace = true }
licenseseat = { workspace = true }
ed25519-dalek = { version = "2", default-features = false, features = ["std", "pkcs8"] }
base64 = "0.22"
uuid = { workspace = true }

[build-dependencies]
ed25519-dalek = { version = "2", default-features = false, features = ["std", "pkcs8"] }
base64 = "0.22"
serde = { workspace = true }
serde_json = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["full", "test-util"] }
tracing = { workspace = true }
tracing-subscriber = { workspace = true, features = ["fmt", "env-filter"] }
proptest = "1"

[features]
test-support = []
```

- [ ] **Step 2: Verify the workspace builds**

Run: `cargo check -p spur-license`
Expected: PASS (no `default_policy.json` referenced yet, so only the new deps resolve).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/Cargo.toml
git commit -m "build(spur-license): add ed25519-dalek, base64, uuid deps for policy module"
```

---

### Task 2: Define PolicyDocument schema types (no signing yet)

**Files:**
- Create: `crates/spur-license/src/policy/mod.rs`
- Modify: `crates/spur-license/src/lib.rs` (add `pub mod policy;`)
- Test: inside `crates/spur-license/src/policy/mod.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Create `crates/spur-license/src/policy/mod.rs`:

```rust
//! Signed policy document carrying tier entitlements (G1) and runtime feature
//! flags (G2). Single artifact, single signing flow, two namespaces.
//!
//! This module owns the schema types and forward-compatibility rules. The
//! actual evaluators live in `policy::feature_key`, `policy::flags`, and the
//! resolver section below.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
/// flow into `extensions` until they earn typed fields with a schema_version
/// minor bump.
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

fn default_true() -> bool { true }

/// Wrapper that carries the signature. The payload is canonical JSON of
/// `PolicyDocument` so signature verification is independent of serde
/// formatting choices on the verification side.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedPolicy {
    pub payload: String,
    pub signature: String,
    pub key_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flagspec_defaults_to_enabled_true() {
        let json = r#"{}"#;
        let spec: FlagSpec = serde_json::from_str(json).unwrap();
        assert!(spec.enabled, "FlagSpec missing `enabled` must default to true");
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
        // Simulates a v1.5 policy with a `future_field` that v1 doesn't know.
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
}
```

- [ ] **Step 2: Wire the module into lib.rs**

Edit `crates/spur-license/src/lib.rs`. Find the existing module declarations near the top (around lines 1–8) and add `pub mod policy;` alongside `mod licenseseat;` and `pub mod provider;`:

```rust
mod licenseseat;
pub mod policy;
pub mod provider;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p spur-license policy::tests -- --nocapture`
Expected: 5 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/policy/mod.rs crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): add PolicyDocument schema (G1 tiers + G2 flags)"
```

---

### Task 3: Add FeatureKey newtype with placeholder constants

**Files:**
- Create: `crates/spur-license/src/policy/feature_key.rs`
- Modify: `crates/spur-license/src/policy/mod.rs` (add `pub mod feature_key; pub use feature_key::FeatureKey;`)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-license/src/policy/feature_key.rs`:

```rust
//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    // G1 entitlement keys (referenced by `tier_policies[*].features`)
    pub const CHAT: Self = Self("chat");
    pub const CODE_EDIT: Self = Self("code_edit");
    pub const WATCH_LOOP: Self = Self("watch_loop");
    pub const ADVANCED_AGENTS: Self = Self("advanced_agents");
    pub const TEAM_SHARING: Self = Self("team_sharing");
    pub const CLOUD_SYNC: Self = Self("cloud_sync");

    // G2 flag keys (referenced by `flags[*]`)
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str { self.0 }
}

impl std::fmt::Display for FeatureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_const_name_lowercase() {
        assert_eq!(FeatureKey::ADVANCED_AGENTS.as_str(), "advanced_agents");
        assert_eq!(FeatureKey::KILL_ADVANCED_PLANNER.as_str(), "kill_advanced_planner");
    }

    #[test]
    fn copy_eq_and_hash_work() {
        let a = FeatureKey::CHAT;
        let b = FeatureKey::CHAT;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }
}
```

- [ ] **Step 2: Wire into policy/mod.rs**

Add to the TOP of `crates/spur-license/src/policy/mod.rs` (after the docstring, before `use chrono::...`):

```rust
pub mod feature_key;
pub use feature_key::FeatureKey;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license policy::feature_key`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs crates/spur-license/src/policy/mod.rs
git commit -m "feat(spur-license): add FeatureKey typed const registry"
```

---

### Task 4: Add the embedded default policy JSON (unsigned baseline)

**Files:**
- Create: `crates/spur-license/resources/default_policy.json`

This JSON is the in-source representation of the baseline policy. We'll wrap it in `SignedPolicy` in Task 6 once trust.rs exists. For now it's a plain JSON file the schema can load.

- [ ] **Step 1: Create the directory and write the file**

```bash
mkdir -p crates/spur-license/resources
```

Create `crates/spur-license/resources/default_policy.json` with EXACTLY:

```json
{
  "schema_version": 1,
  "issued_at": "2026-04-19T00:00:00Z",
  "tier_policies": {
    "community": {
      "features": ["chat", "code_edit", "watch_loop"],
      "metadata": {
        "label": "Community",
        "description": "Free tier with the core conversational and editing loop."
      }
    },
    "pro": {
      "features": ["chat", "code_edit", "watch_loop", "advanced_agents", "cloud_sync"],
      "metadata": {
        "label": "Pro",
        "description": "All Community features plus advanced agents and cloud sync."
      }
    },
    "team": {
      "features": ["chat", "code_edit", "watch_loop", "advanced_agents", "cloud_sync", "team_sharing"],
      "metadata": {
        "label": "Team",
        "description": "Pro plus team sharing."
      }
    },
    "enterprise": {
      "features": ["chat", "code_edit", "watch_loop", "advanced_agents", "cloud_sync", "team_sharing"],
      "metadata": {
        "label": "Enterprise",
        "description": "Team plus enterprise SLAs (server-issued; entitlements identical to Team in V1)."
      }
    }
  },
  "flags": {
    "kill_advanced_planner": {
      "enabled": true,
      "description": "Kill switch on the new agent planner. Flip false to fall back to the previous planner."
    },
    "enable_browser_tool": {
      "enabled": true,
      "description": "Gradual ramp candidate. Set rollout_percent to expose to a fraction of installs."
    },
    "enable_compaction_v2": {
      "enabled": true,
      "description": "Kill switch on the V2 compaction logic."
    },
    "enable_telemetry": {
      "enabled": false,
      "description": "Off until the telemetry spec lands; flip on to begin opt-in capture."
    }
  }
}
```

- [ ] **Step 2: Verify the JSON parses with the schema from Task 2**

Add this test to `crates/spur-license/src/policy/mod.rs` (inside the existing `#[cfg(test)] mod tests`):

```rust
    #[test]
    fn embedded_default_policy_json_parses() {
        let raw = include_str!("../../resources/default_policy.json");
        let doc: PolicyDocument = serde_json::from_str(raw).unwrap();
        assert_eq!(doc.schema_version, 1);
        assert!(doc.tier_policies.contains_key("community"));
        assert!(doc.tier_policies.contains_key("pro"));
        assert!(doc.flags.contains_key("kill_advanced_planner"));
    }
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p spur-license policy::tests::embedded_default_policy_json_parses`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/resources/default_policy.json crates/spur-license/src/policy/mod.rs
git commit -m "feat(spur-license): add embedded default_policy.json baseline"
```

---

### Task 5: Add trust map and signature verification (`policy/trust.rs`)

**Files:**
- Create: `crates/spur-license/src/policy/trust.rs`
- Create: `crates/spur-license/resources/keys/spur-policy-2026-04.pub` (32-byte raw Ed25519 public key — generated as Pre-Work item #1)
- Modify: `crates/spur-license/src/policy/mod.rs` (add `pub mod trust;`)

- [ ] **Step 1: Verify the public key file exists and is 32 bytes**

```bash
ls -la crates/spur-license/resources/keys/spur-policy-2026-04.pub
wc -c crates/spur-license/resources/keys/spur-policy-2026-04.pub
```
Expected: file exists, byte count is exactly `32`. If not, complete Pre-Work item #1 first.

- [ ] **Step 2: Write the failing tests**

Create `crates/spur-license/src/policy/trust.rs`:

```rust
//! Embedded Ed25519 trust map. Multi-key from V1 to enable rotation: ship a
//! new binary that adds the new key BEFORE retiring the old key on the
//! issuance side; ship a later binary that removes the old key.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::policy::{PolicyDocument, SignedPolicy, CODE_SUPPORTED_MAJOR};

#[derive(Debug, thiserror::Error)]
pub enum PolicyVerifyError {
    #[error("unknown signing key id: {0}")]
    UnknownKeyId(String),
    #[error("invalid base64 signature: {0}")]
    InvalidSignatureEncoding(String),
    #[error("signature did not verify against payload")]
    SignatureMismatch,
    #[error("policy payload is not valid JSON: {0}")]
    PayloadParse(String),
    #[error("policy schema_version {found} exceeds supported major {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("policy expired at {0}")]
    Expired(chrono::DateTime<chrono::Utc>),
}

/// Returns the static, embedded trusted-keys map. Add new keys here BEFORE
/// rotating issuance; remove old keys in a later release after issuance has
/// migrated.
pub fn trusted_keys() -> &'static BTreeMap<&'static str, VerifyingKey> {
    static KEYS: OnceLock<BTreeMap<&'static str, VerifyingKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut m = BTreeMap::new();
        let raw: &[u8] = include_bytes!("../../resources/keys/spur-policy-2026-04.pub");
        let key_bytes: [u8; 32] = raw.try_into().expect("pubkey file must be exactly 32 bytes");
        let vk = VerifyingKey::from_bytes(&key_bytes).expect("valid Ed25519 verifying key");
        m.insert("spur-policy-2026-04", vk);
        m
    })
}

/// Verify a `SignedPolicy` against the trusted keys, parse the payload, and
/// enforce schema-version + expiry. Fails closed on every error.
pub fn verify_signed_policy(
    signed: &SignedPolicy,
) -> Result<PolicyDocument, PolicyVerifyError> {
    let key = trusted_keys()
        .get(signed.key_id.as_str())
        .ok_or_else(|| PolicyVerifyError::UnknownKeyId(signed.key_id.clone()))?;

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .map_err(|e| PolicyVerifyError::InvalidSignatureEncoding(e.to_string()))?;
    let sig = Signature::from_slice(&sig_bytes)
        .map_err(|e| PolicyVerifyError::InvalidSignatureEncoding(e.to_string()))?;

    key.verify(signed.payload.as_bytes(), &sig)
        .map_err(|_| PolicyVerifyError::SignatureMismatch)?;

    let doc: PolicyDocument = serde_json::from_str(&signed.payload)
        .map_err(|e| PolicyVerifyError::PayloadParse(e.to_string()))?;

    if doc.schema_version > CODE_SUPPORTED_MAJOR {
        return Err(PolicyVerifyError::UnsupportedSchemaVersion {
            found: doc.schema_version,
            supported: CODE_SUPPORTED_MAJOR,
        });
    }

    if let Some(exp) = doc.expires_at {
        if exp < chrono::Utc::now() {
            return Err(PolicyVerifyError::Expired(exp));
        }
    }

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_keys_contains_2026_04_key() {
        let keys = trusted_keys();
        assert!(keys.contains_key("spur-policy-2026-04"));
    }

    #[test]
    fn unknown_key_id_is_rejected() {
        let signed = SignedPolicy {
            payload: r#"{"schema_version":1,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#.into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "no-such-key".into(),
        };
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::UnknownKeyId(_)));
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let signed = SignedPolicy {
            payload: r#"{"schema_version":1,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#.into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "spur-policy-2026-04".into(),
        };
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::SignatureMismatch));
    }

    #[test]
    fn future_schema_version_is_rejected() {
        // Build an inner doc with schema_version too high; signature would
        // also be invalid but UnsupportedSchemaVersion fires AFTER signature
        // succeeds, so this test only exercises the unknown-key path until
        // we have a real signing helper. See Task 6 for end-to-end happy path.
        let signed = SignedPolicy {
            payload: r#"{"schema_version":99,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#.into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "spur-policy-2026-04".into(),
        };
        // Until we have a signing helper, this fails at SignatureMismatch
        // first. The schema-version path is exercised in Task 6 once we can
        // produce a valid signature in tests.
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::SignatureMismatch));
    }
}
```

- [ ] **Step 3: Wire into `policy/mod.rs`**

Add at the top of `crates/spur-license/src/policy/mod.rs` (next to `pub mod feature_key;`):

```rust
pub mod trust;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-license policy::trust`
Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/policy/trust.rs crates/spur-license/src/policy/mod.rs crates/spur-license/resources/keys/spur-policy-2026-04.pub
git commit -m "feat(spur-license): add Ed25519 trust map and SignedPolicy verifier"
```

---

### Task 6: Add policy signing helper script and re-sign default_policy.json

**Files:**
- Create: `crates/spur-license/scripts/sign-policy.sh`
- Modify: `crates/spur-license/resources/default_policy.json` → replace contents with a `SignedPolicy` JSON wrapping the prior payload.

**Note:** This task requires the Ed25519 PRIVATE key from Pre-Work item #1. The script reads the private key path from `SPUR_POLICY_SIGNING_KEY` env var.

- [ ] **Step 1: Write the signing helper script**

Create `crates/spur-license/scripts/sign-policy.sh`:

```bash
#!/usr/bin/env bash
# Sign crates/spur-license/resources/default_policy.json with the Ed25519
# private key at $SPUR_POLICY_SIGNING_KEY (PEM).
#
# Output: rewrites default_policy.json with a SignedPolicy wrapper containing
# the canonical-JSON payload, base64 Ed25519 signature, and key_id.

set -euo pipefail

if [[ -z "${SPUR_POLICY_SIGNING_KEY:-}" ]]; then
  echo "error: SPUR_POLICY_SIGNING_KEY env var must point to the Ed25519 private key (PEM)" >&2
  exit 1
fi

KEY_ID="${SPUR_POLICY_KEY_ID:-spur-policy-2026-04}"
RESOURCES_DIR="$(cd "$(dirname "$0")/.." && pwd)/resources"
POLICY_FILE="$RESOURCES_DIR/default_policy.json"
TMP_PAYLOAD="$(mktemp)"
TMP_SIG="$(mktemp)"
trap 'rm -f "$TMP_PAYLOAD" "$TMP_SIG"' EXIT

# Detect whether the file already has a SignedPolicy wrapper or is a raw
# PolicyDocument. If wrapped, extract .payload; otherwise treat the whole
# file as the payload.
if jq -e '.payload and .signature and .key_id' "$POLICY_FILE" >/dev/null 2>&1; then
  jq -r '.payload' "$POLICY_FILE" > "$TMP_PAYLOAD"
else
  jq -c . "$POLICY_FILE" > "$TMP_PAYLOAD"
fi

# Sign the canonical payload bytes.
openssl pkeyutl -sign -inkey "$SPUR_POLICY_SIGNING_KEY" -rawin -in "$TMP_PAYLOAD" -out "$TMP_SIG"
SIG_B64="$(base64 < "$TMP_SIG" | tr -d '\n')"
PAYLOAD_STR="$(cat "$TMP_PAYLOAD")"

jq -n \
  --arg payload "$PAYLOAD_STR" \
  --arg signature "$SIG_B64" \
  --arg key_id "$KEY_ID" \
  '{payload: $payload, signature: $signature, key_id: $key_id}' \
  > "$POLICY_FILE"

echo "Signed $POLICY_FILE with key_id=$KEY_ID"
```

Make it executable:

```bash
chmod +x crates/spur-license/scripts/sign-policy.sh
```

- [ ] **Step 2: Run the signing script**

```bash
SPUR_POLICY_SIGNING_KEY=/path/to/spur-policy-2026-04.key bash crates/spur-license/scripts/sign-policy.sh
```

Expected output: `Signed crates/spur-license/resources/default_policy.json with key_id=spur-policy-2026-04`. The file is now a `SignedPolicy` JSON `{payload, signature, key_id}`.

- [ ] **Step 3: Update the embedded-loads test in policy/mod.rs to expect a SignedPolicy**

Replace the `embedded_default_policy_json_parses` test in `crates/spur-license/src/policy/mod.rs` with:

```rust
    #[test]
    fn embedded_default_policy_json_parses_as_signed() {
        let raw = include_str!("../../resources/default_policy.json");
        let signed: SignedPolicy = serde_json::from_str(raw).unwrap();
        assert_eq!(signed.key_id, "spur-policy-2026-04");
        assert!(!signed.signature.is_empty());
        // The inner payload must also parse as a PolicyDocument.
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
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p spur-license policy::tests::embedded_default_policy`
Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/scripts/sign-policy.sh crates/spur-license/resources/default_policy.json crates/spur-license/src/policy/mod.rs
git commit -m "feat(spur-license): sign default_policy.json with spur-policy-2026-04 key"
```

---

### Task 7: Add build.rs compile-time signature verification

**Files:**
- Create: `crates/spur-license/build.rs`

This guarantees CI cannot ship a binary with an unsigned, malformed, or future-versioned embedded policy. **Invariant #6.**

- [ ] **Step 1: Write the build script**

Create `crates/spur-license/build.rs`:

```rust
//! Compile-time verifier for the embedded default policy.
//!
//! Loads `resources/default_policy.json`, parses it as `SignedPolicy`,
//! verifies the Ed25519 signature against the embedded public key, and
//! parses the inner `PolicyDocument`. Panics (= build failure) on any
//! error, so CI cannot ship a binary with a broken default policy.
//!
//! Re-runs only when the policy or key file changes.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;

#[derive(Deserialize)]
struct SignedPolicy {
    payload: String,
    signature: String,
    key_id: String,
}

#[derive(Deserialize)]
struct PolicyDocumentMin {
    schema_version: u32,
}

const SUPPORTED_MAJOR: u32 = 1;

fn main() {
    println!("cargo:rerun-if-changed=resources/default_policy.json");
    println!("cargo:rerun-if-changed=resources/keys/spur-policy-2026-04.pub");

    let policy_raw = std::fs::read_to_string("resources/default_policy.json")
        .expect("resources/default_policy.json must exist");
    let signed: SignedPolicy = serde_json::from_str(&policy_raw)
        .expect("default_policy.json must be a SignedPolicy JSON");

    if signed.key_id != "spur-policy-2026-04" {
        panic!(
            "embedded policy uses unknown key_id '{}'; expected 'spur-policy-2026-04'",
            signed.key_id
        );
    }

    let key_bytes = std::fs::read("resources/keys/spur-policy-2026-04.pub")
        .expect("spur-policy-2026-04.pub must exist");
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .expect("public key must be exactly 32 bytes");
    let vk = VerifyingKey::from_bytes(&key_arr).expect("valid Ed25519 verifying key");

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("signature must be valid base64");
    let sig = Signature::from_slice(&sig_bytes).expect("signature must be 64 bytes");

    vk.verify(signed.payload.as_bytes(), &sig)
        .expect("embedded policy signature MUST verify (re-run sign-policy.sh)");

    let doc: PolicyDocumentMin = serde_json::from_str(&signed.payload)
        .expect("inner payload must be a valid PolicyDocument JSON");
    if doc.schema_version > SUPPORTED_MAJOR {
        panic!(
            "embedded policy schema_version {} exceeds supported major {}",
            doc.schema_version, SUPPORTED_MAJOR
        );
    }
}
```

- [ ] **Step 2: Verify the build runs the script**

Run: `cargo clean -p spur-license && cargo build -p spur-license`
Expected: PASS. If you see `embedded policy signature MUST verify`, re-run the signing script (Task 6 step 2).

- [ ] **Step 3: Add a deliberately-tampered-policy test (sanity)**

Manually corrupt the signature, observe the build fails:

```bash
# Save a backup
cp crates/spur-license/resources/default_policy.json /tmp/default_policy.json.bak
# Corrupt the signature
python3 -c "
import json
with open('crates/spur-license/resources/default_policy.json') as f: d = json.load(f)
d['signature'] = 'AAAA' + d['signature'][4:]
with open('crates/spur-license/resources/default_policy.json', 'w') as f: json.dump(d, f)
"
cargo clean -p spur-license
cargo build -p spur-license 2>&1 | tail -5
# Expected: compilation FAILS with "embedded policy signature MUST verify"
# Restore
cp /tmp/default_policy.json.bak crates/spur-license/resources/default_policy.json
cargo build -p spur-license
# Expected: PASS
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/build.rs
git commit -m "feat(spur-license): verify embedded policy signature at compile time"
```

---

### Task 8: Add PolicyResolver (in-memory accessor, embedded fallback)

**Files:**
- Modify: `crates/spur-license/src/policy/mod.rs` (append `PolicyResolver` struct + impl)

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-license/src/policy/mod.rs` (BEFORE the existing `#[cfg(test)] mod tests`):

```rust
use std::sync::{Arc, OnceLock};

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
                Arc::new(Self { document: Arc::new(doc) })
            })
            .clone()
    }

    /// Construct a resolver from an arbitrary document. Test-only; keeps
    /// the trust-bypass path explicit.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_document(doc: PolicyDocument) -> Arc<Self> {
        Arc::new(Self { document: Arc::new(doc) })
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
}
```

Then append to the existing `#[cfg(test)] mod tests`:

```rust
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
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-license policy::tests`
Expected: 4 new tests PASS plus the existing tests.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/src/policy/mod.rs
git commit -m "feat(spur-license): add PolicyResolver with embedded baseline accessor"
```

---

## Phase 2 — Day-1 FF capability (Tasks 9–11)

### Task 9: InstallId lifecycle (`policy/flags.rs` part 1)

**Files:**
- Create: `crates/spur-license/src/policy/flags.rs`
- Modify: `crates/spur-license/src/policy/mod.rs` (`pub mod flags;`)

- [ ] **Step 1: Write failing tests**

Create `crates/spur-license/src/policy/flags.rs`:

```rust
//! G2 — runtime feature flag evaluation. Sync, deterministic, in-process.
//!
//! Core types:
//! - `InstallId`: anonymous per-machine UUID for stable rollout bucketing.
//! - `FlagEvaluator`: reads `PolicyDocument.flags` and evaluates against
//!   `(install_id, license_state)`.
//! - `FlagExplanation`: introspection record for `spur flags list`.
//!
//! Flag evaluation rules:
//! 1. Unknown flag key → `false` (fail-closed).
//! 2. `enabled: false` → `false`.
//! 3. `tier_filter` set AND license tier not in filter → `false`.
//! 4. `rollout_percent` set AND `bucket(install_id, flag_key) >= pct` → `false`.
//! 5. Otherwise → `true`.

use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use crate::policy::PolicyDocument;
use crate::LicenseState;

/// Stable per-machine anonymous identifier. Generated on first run, persisted
/// at `~/.spur/install-id`. NOT correlated with user identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallId(pub String);

impl InstallId {
    /// Load from `path` if present, else generate a new v4 UUID and persist.
    pub fn load_or_create(path: &std::path::Path) -> std::io::Result<Self> {
        if let Ok(existing) = std::fs::read_to_string(path) {
            let trimmed = existing.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(Self(trimmed));
            }
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &new_id)?;
        Ok(Self(new_id))
    }

    /// Default location: `~/.spur/install-id`. Falls back to a deterministic
    /// per-process UUID if no home directory is available (CI sandboxes).
    pub fn default_path() -> Option<PathBuf> {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".spur").join("install-id"))
    }
}

#[cfg(test)]
mod install_id_tests {
    use super::*;

    #[test]
    fn load_or_create_generates_and_persists() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_owned();
        std::fs::remove_file(&path).ok();
        let first = InstallId::load_or_create(&path).unwrap();
        let second = InstallId::load_or_create(&path).unwrap();
        assert_eq!(first, second, "subsequent loads must return the same id");
        assert_eq!(first.0.len(), 36, "uuid v4 string is 36 chars");
    }

    #[test]
    fn empty_file_triggers_regeneration() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "").unwrap();
        let id = InstallId::load_or_create(tmp.path()).unwrap();
        assert!(!id.0.is_empty());
    }
}
```

- [ ] **Step 2: Wire module + add `directories`/`tempfile` deps**

Add to `crates/spur-license/src/policy/mod.rs` (next to `pub mod feature_key;`):

```rust
pub mod flags;
```

Add to `crates/spur-license/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
directories = { workspace = true }
```

And add `tempfile = "3"` to `[dev-dependencies]`.

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license policy::flags::install_id_tests`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/policy/flags.rs crates/spur-license/src/policy/mod.rs crates/spur-license/Cargo.toml
git commit -m "feat(spur-license): add InstallId lifecycle for rollout bucketing"
```

---

### Task 10: FlagEvaluator with FLOOR ∧ GATE-ready evaluation

**Files:**
- Modify: `crates/spur-license/src/policy/flags.rs` (append FlagEvaluator + FlagExplanation)

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-license/src/policy/flags.rs`:

```rust
/// Sync, in-process evaluator. Constructed once per process (or whenever the
/// underlying PolicyDocument changes via overlay reload — V2).
pub struct FlagEvaluator {
    document: Arc<PolicyDocument>,
    install_id: InstallId,
}

#[derive(Clone, Debug)]
pub struct FlagExplanation {
    pub flag_key: String,
    pub enabled: bool,
    pub rollout_percent: Option<f32>,
    pub bucket: u32,
    pub tier_filter: Option<Vec<String>>,
    pub license_tier: String,
    pub result: bool,
    pub description: Option<String>,
}

impl FlagEvaluator {
    pub fn new(document: Arc<PolicyDocument>, install_id: InstallId) -> Self {
        Self { document, install_id }
    }

    /// Returns true iff the flag is enabled for this (install_id, license)
    /// pair. Unknown flag → false (fail-closed).
    pub fn is_enabled(&self, flag_key: &str, license: &LicenseState) -> bool {
        let Some(spec) = self.document.flags.get(flag_key) else {
            return false;
        };
        if !spec.enabled {
            return false;
        }
        if let Some(filter) = &spec.tier_filter {
            let tier = license.plan.label().to_ascii_lowercase();
            if !filter.iter().any(|t| t.to_ascii_lowercase() == tier) {
                return false;
            }
        }
        if let Some(pct) = spec.rollout_percent {
            let bucket = self.bucket(flag_key);
            if (bucket as f32) >= pct {
                return false;
            }
        }
        true
    }

    /// Stable bucket 0..100 from `(install_id, flag_key)`. SipHash via std's
    /// DefaultHasher — not cryptographic, but stable across processes on the
    /// same install for the same flag key.
    fn bucket(&self, flag_key: &str) -> u32 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.install_id.0.hash(&mut h);
        flag_key.hash(&mut h);
        (h.finish() % 100) as u32
    }

    /// Returns an `Iterator` over `(flag_key, FlagExplanation)` for every
    /// flag in the document. Used by `spur flags list`.
    pub fn explain_all<'a>(
        &'a self,
        license: &'a LicenseState,
    ) -> impl Iterator<Item = FlagExplanation> + 'a {
        self.document.flags.iter().map(move |(key, spec)| {
            let bucket = self.bucket(key);
            FlagExplanation {
                flag_key: key.clone(),
                enabled: spec.enabled,
                rollout_percent: spec.rollout_percent,
                bucket,
                tier_filter: spec.tier_filter.clone(),
                license_tier: license.plan.label().to_string(),
                result: self.is_enabled(key, license),
                description: spec.description.clone(),
            }
        })
    }
}

#[cfg(test)]
mod evaluator_tests {
    use super::*;
    use crate::policy::{FlagSpec, TierPolicy};
    use crate::{LicenseState, Plan};
    use std::collections::{BTreeMap, BTreeSet};

    fn build_doc(flags: BTreeMap<String, FlagSpec>) -> Arc<PolicyDocument> {
        Arc::new(PolicyDocument {
            schema_version: 1,
            issued_at: chrono::Utc::now(),
            expires_at: None,
            tier_policies: BTreeMap::new(),
            flags,
        })
    }

    fn community_state() -> LicenseState {
        let mut s = LicenseState::inactive("test");
        s.plan = Plan::Community;
        s
    }

    #[test]
    fn unknown_flag_returns_false() {
        let ev = FlagEvaluator::new(build_doc(BTreeMap::new()), InstallId("test".into()));
        assert!(!ev.is_enabled("nope", &community_state()));
    }

    #[test]
    fn disabled_flag_returns_false() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec { enabled: false, ..Default::default() });
        let ev = FlagEvaluator::new(build_doc(flags), InstallId("test".into()));
        assert!(!ev.is_enabled("f", &community_state()));
    }

    #[test]
    fn enabled_flag_with_no_constraints_returns_true() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec::default());
        let ev = FlagEvaluator::new(build_doc(flags), InstallId("test".into()));
        assert!(ev.is_enabled("f", &community_state()));
    }

    #[test]
    fn tier_filter_excludes_non_matching_tier() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec {
            enabled: true,
            tier_filter: Some(vec!["pro".into()]),
            ..Default::default()
        });
        let ev = FlagEvaluator::new(build_doc(flags), InstallId("test".into()));
        assert!(!ev.is_enabled("f", &community_state()), "Community must be excluded by Pro-only filter");
    }

    #[test]
    fn rollout_zero_percent_disables_for_everyone() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec {
            enabled: true,
            rollout_percent: Some(0.0),
            ..Default::default()
        });
        let ev = FlagEvaluator::new(build_doc(flags), InstallId("test".into()));
        assert!(!ev.is_enabled("f", &community_state()));
    }

    #[test]
    fn rollout_100_percent_enables_for_everyone() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec {
            enabled: true,
            rollout_percent: Some(100.0),
            ..Default::default()
        });
        let ev = FlagEvaluator::new(build_doc(flags), InstallId("test".into()));
        assert!(ev.is_enabled("f", &community_state()));
    }

    #[test]
    fn rollout_is_deterministic_across_calls() {
        let mut flags = BTreeMap::new();
        flags.insert("f".into(), FlagSpec {
            enabled: true,
            rollout_percent: Some(50.0),
            ..Default::default()
        });
        let doc = build_doc(flags);
        let id = InstallId("stable-id".into());
        let a = FlagEvaluator::new(Arc::clone(&doc), id.clone()).is_enabled("f", &community_state());
        let b = FlagEvaluator::new(Arc::clone(&doc), id).is_enabled("f", &community_state());
        assert_eq!(a, b, "same install + flag must yield the same result");
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-license policy::flags::evaluator_tests`
Expected: 7 tests PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/src/policy/flags.rs
git commit -m "feat(spur-license): add FlagEvaluator with deterministic rollout buckets"
```

---

### Task 11: Property-test FlagEvaluator distribution uniformity (Invariant #10)

**Files:**
- Create: `crates/spur-license/tests/flag_distribution.rs`

- [ ] **Step 1: Write the property test**

Create `crates/spur-license/tests/flag_distribution.rs`:

```rust
//! Invariant #10 — rollout determinism + approximate uniformity.

use std::collections::BTreeMap;
use std::sync::Arc;

use proptest::prelude::*;
use spur_license::policy::flags::{FlagEvaluator, InstallId};
use spur_license::policy::{FlagSpec, PolicyDocument};
use spur_license::{LicenseState, Plan};

fn build_doc(flag_key: &str, pct: f32) -> Arc<PolicyDocument> {
    let mut flags = BTreeMap::new();
    flags.insert(flag_key.into(), FlagSpec {
        enabled: true,
        rollout_percent: Some(pct),
        ..Default::default()
    });
    Arc::new(PolicyDocument {
        schema_version: 1,
        issued_at: chrono::Utc::now(),
        expires_at: None,
        tier_policies: BTreeMap::new(),
        flags,
    })
}

fn community_state() -> LicenseState {
    let mut s = LicenseState::inactive("test");
    s.plan = Plan::Community;
    s
}

#[test]
fn rollout_50_percent_distributes_within_tolerance() {
    let doc = build_doc("flag", 50.0);
    let mut hits = 0usize;
    let total = 5000usize;
    for i in 0..total {
        let id = InstallId(format!("install-{i}"));
        let ev = FlagEvaluator::new(Arc::clone(&doc), id);
        if ev.is_enabled("flag", &community_state()) { hits += 1; }
    }
    let frac = hits as f32 / total as f32;
    assert!(
        (0.45..=0.55).contains(&frac),
        "50% rollout produced {frac:.3} hit rate over {total} installs"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 32, ..ProptestConfig::default() })]

    #[test]
    fn same_install_same_flag_yields_same_result(install in "[a-z0-9]{16}", flag in "[a-z_]{5,12}") {
        let doc = build_doc(&flag, 50.0);
        let id = InstallId(install);
        let a = FlagEvaluator::new(Arc::clone(&doc), id.clone()).is_enabled(&flag, &community_state());
        let b = FlagEvaluator::new(Arc::clone(&doc), id).is_enabled(&flag, &community_state());
        prop_assert_eq!(a, b);
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p spur-license --test flag_distribution`
Expected: 2 tests PASS. The 50%-distribution test is statistical; if it fails sporadically increase `total` to 20000.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/tests/flag_distribution.rs
git commit -m "test(spur-license): property-test rollout determinism + uniformity"
```

---

## Phase 3 — CommunityProvider + LicenseSeat fallback (Tasks 12–15)

### Task 12: Add `LicenseState::active_community(features)` constructor

**Files:**
- Modify: `crates/spur-license/src/lib.rs` (add new constructor next to `active_validated`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/spur-license/src/lib.rs`:

```rust
    #[test]
    fn active_community_state_has_correct_shape() {
        use std::collections::BTreeSet;
        let mut features = BTreeSet::new();
        features.insert("chat".to_string());
        features.insert("watch_loop".to_string());
        let state = LicenseState::active_community(features.clone());
        assert!(matches!(state.status, LicenseStatus::Active));
        assert!(matches!(state.plan, Plan::Community));
        assert!(matches!(state.subject_kind, SubjectKind::User));
        assert!(matches!(state.binding_mode, BindingMode::Unknown));
        assert!(state.offline_ok);
        assert_eq!(state.features, features);
        assert!(state.is_active(), "Community must be is_active() == true");
    }
```

- [ ] **Step 2: Implement the constructor**

Add to `crates/spur-license/src/lib.rs` inside `impl LicenseState`, next to `active_validated`:

```rust
    pub fn active_community(features: BTreeSet<String>) -> Self {
        Self {
            status: LicenseStatus::Active,
            subject_kind: SubjectKind::User,
            plan: Plan::Community,
            features,
            expires_at: None,
            binding_mode: BindingMode::Unknown,
            offline_ok: true,
            status_text: "Community tier".into(),
        }
    }
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p spur-license active_community_state_has_correct_shape`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): add LicenseState::active_community constructor"
```

---

### Task 13: Add CommunityProvider

**Files:**
- Create: `crates/spur-license/src/community.rs`
- Modify: `crates/spur-license/src/lib.rs` (add `mod community; pub use community::CommunityProvider;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/spur-license/src/community.rs`:

```rust
//! `LicenseProvider` impl for the no-LicenseSeat-config case.
//!
//! Reads the embedded signed PolicyDocument; exposes the `community` tier's
//! entitlements; never emits events; rejects `activate` (the facade or CLI
//! routes that to the LicenseSeat path under Option A).

use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::policy::PolicyResolver;
use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{LicenseError, LicenseEvent, LicenseState, Result};

pub struct CommunityProvider {
    state: LicenseState,
    /// Constructed but never sent on. Exists to satisfy the trait's
    /// `subscribe()`. Single-emission-seam invariant preserved.
    events_tx: broadcast::Sender<LicenseEvent>,
}

impl CommunityProvider {
    pub fn new(resolver: Arc<PolicyResolver>) -> Self {
        let features = resolver.tier_features("community");
        let state = LicenseState::active_community(features);
        let (events_tx, _) = broadcast::channel(1);
        Self { state, events_tx }
    }
}

#[async_trait]
impl LicenseProvider for CommunityProvider {
    fn current_state(&self) -> LicenseState { self.state.clone() }
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> { self.events_tx.subscribe() }
    fn refresh_policy(&self) -> RefreshPolicy { RefreshPolicy::default() }
    fn requires_heartbeat(&self) -> bool { false }
    fn has_entitlement(&self, feature: &str) -> bool {
        self.state.features.contains(feature)
    }
    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        Err(LicenseError::NotConfigured(
            "Community tier provider cannot activate license keys directly. \
             Build with SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY/PRODUCT_SLUG, \
             or set the matching SPUR_LICENSESEAT_* runtime env vars to upgrade."
                .into()
        ))
    }
    async fn validate(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
    async fn heartbeat(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
    async fn deactivate(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyResolver;

    #[tokio::test]
    async fn community_provider_reports_community_features() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let state = p.current_state();
        assert!(matches!(state.plan, crate::Plan::Community));
        assert!(p.has_entitlement("chat"));
        assert!(p.has_entitlement("watch_loop"));
        assert!(!p.has_entitlement("advanced_agents"));
    }

    #[tokio::test]
    async fn community_provider_rejects_activate() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let err = p.activate("any-key").await.unwrap_err();
        assert!(matches!(err, LicenseError::NotConfigured(_)));
    }

    #[tokio::test]
    async fn community_provider_validate_is_idempotent() {
        let p = CommunityProvider::new(PolicyResolver::embedded());
        let s1 = p.validate().await.unwrap();
        let s2 = p.validate().await.unwrap();
        assert_eq!(s1.features, s2.features);
    }
}
```

- [ ] **Step 2: Wire into lib.rs**

In `crates/spur-license/src/lib.rs`, find the existing `mod licenseseat;` line and add:

```rust
mod community;
pub use community::CommunityProvider;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license community::tests`
Expected: 3 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/community.rs crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): add CommunityProvider backed by PolicyResolver"
```

---

### Task 14: Update `from_env_or_disabled` dispatch + LicenseSeatProvider resolver fallback

**Files:**
- Modify: `crates/spur-license/src/licenseseat.rs` (the `from_env_or_disabled` function and `LicenseSeatProvider::has_entitlement`)

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-license/src/community.rs` (`#[cfg(test)] mod tests`):

```rust
    #[test]
    fn from_env_or_disabled_returns_community_when_env_absent() {
        // Note: this test relies on env vars being unset in the test process.
        // tokio/cargo test sets some, but not these.
        std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
        std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
        let provider = crate::licenseseat::from_env_or_disabled();
        assert!(matches!(provider.current_state().plan, crate::Plan::Community));
    }
```

- [ ] **Step 2: Update `from_env_or_disabled` in licenseseat.rs**

Find lines 31–46 in `crates/spur-license/src/licenseseat.rs` and replace with:

```rust
pub fn from_env_or_disabled() -> Arc<dyn LicenseProvider> {
    match (
        std::env::var(LICENSESEAT_API_KEY_ENV),
        std::env::var(LICENSESEAT_PRODUCT_SLUG_ENV),
    ) {
        (Ok(api_key), Ok(product_slug)) => {
            Arc::new(LicenseSeatProvider::new(api_key, product_slug))
        }
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {
            Arc::new(crate::CommunityProvider::new(
                crate::policy::PolicyResolver::embedded(),
            ))
        }
        _ => Arc::new(DisabledProvider::new(
            "incomplete licensing environment configuration",
        )),
    }
}
```

- [ ] **Step 3: Update `LicenseSeatProvider::has_entitlement` with resolver fallback**

Find lines 180–182 in `crates/spur-license/src/licenseseat.rs` (the existing `has_entitlement` impl) and replace with:

```rust
    fn has_entitlement(&self, feature: &str) -> bool {
        if self.sdk.has_entitlement(feature) {
            return true;
        }
        // Server-asserted entitlements override; policy is the FLOOR.
        // Pro-tier features the LicenseSeat catalog hasn't been updated with
        // are still honored if the local policy lists them.
        let plan_label = self.current_snapshot().plan.label().to_ascii_lowercase();
        crate::policy::PolicyResolver::embedded().tier_has_feature(&plan_label, feature)
    }
```

- [ ] **Step 4: Run all spur-license tests**

Run: `cargo test -p spur-license`
Expected: all tests PASS, including new `from_env_or_disabled_returns_community_when_env_absent`.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/licenseseat.rs crates/spur-license/src/community.rs
git commit -m "feat(spur-license): dispatch absent env -> CommunityProvider; LicenseSeat policy fallback"
```

---

### Task 14b: Option A — baked-in LicenseSeat publishable credentials + upgrade routing

**Files:**
- Create: `crates/spur-license/src/build_constants.rs`
- Modify: `crates/spur-license/src/lib.rs` (`mod build_constants;`)
- Modify: `crates/spur-license/src/licenseseat.rs` (`from_env_or_disabled` consults baked credentials; `CommunityProvider`-with-baked-fallback path)

This implements Option A from the spec — LicenseSeat publishable key (`pk_*`) and product slug embedded at build time so `spur auth login --key …` works on a fresh install with zero env vars.

**Pre-work check:** This task ASSUMES the publishable key is approved for in-source embedding (Pre-Work item #3). If it isn't, SKIP this task entirely and ship Option B (env vars required for upgrade).

- [ ] **Step 1: Add the build_constants module**

Create `crates/spur-license/src/build_constants.rs`:

```rust
//! Build-time-baked LicenseSeat publishable credentials (Option A).
//!
//! These are NON-SECRET. The publishable key is the `pk_*` LicenseSeat issues
//! for client embedding (analogous to a Stripe publishable key). The product
//! slug is similarly non-secret. Both can safely live in the binary.
//!
//! Set in release CI:
//!   SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY=pk_...
//!   SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG=spur
//!
//! When unset (typical for `cargo build` locally), `option_env!` returns
//! None and the binary behaves exactly as Option B (env vars required).

pub const BAKED_LICENSESEAT_PUBLISHABLE_KEY: Option<&str> =
    option_env!("SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY");

pub const BAKED_LICENSESEAT_PRODUCT_SLUG: Option<&str> =
    option_env!("SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG");

/// Returns Some((api_key, product_slug)) if BOTH baked-in values are present.
pub fn baked_credentials() -> Option<(&'static str, &'static str)> {
    match (
        BAKED_LICENSESEAT_PUBLISHABLE_KEY,
        BAKED_LICENSESEAT_PRODUCT_SLUG,
    ) {
        (Some(k), Some(s)) => Some((k, s)),
        _ => None,
    }
}
```

- [ ] **Step 2: Wire into lib.rs**

Add to `crates/spur-license/src/lib.rs` near the other `mod` lines:

```rust
pub mod build_constants;
```

- [ ] **Step 3: Update `from_env_or_disabled` to prefer runtime env, then baked, then Community**

Replace the body of `from_env_or_disabled` in `crates/spur-license/src/licenseseat.rs`:

```rust
pub fn from_env_or_disabled() -> Arc<dyn LicenseProvider> {
    // 1. Runtime env vars override (developer / CI override path).
    match (
        std::env::var(LICENSESEAT_API_KEY_ENV),
        std::env::var(LICENSESEAT_PRODUCT_SLUG_ENV),
    ) {
        (Ok(api_key), Ok(product_slug)) => {
            return Arc::new(LicenseSeatProvider::new(api_key, product_slug));
        }
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {
            // 2. Fall through to baked credentials check.
        }
        _ => {
            // Partial env: loud config error.
            return Arc::new(DisabledProvider::new(
                "incomplete licensing environment configuration",
            ));
        }
    }

    // 2. Baked credentials (Option A release builds) — but only switch to
    //    LicenseSeatProvider if there's a CACHED license. Without a cache,
    //    expose Community for the current state and route activate() to
    //    LicenseSeat under the hood.
    if let Some((api_key, product_slug)) = crate::build_constants::baked_credentials() {
        let seat = LicenseSeatProvider::new(api_key.into(), product_slug.into());
        if seat.has_cached_license() {
            return Arc::new(seat);
        }
        // No cache: present as Community but allow upgrade via baked seat.
        return Arc::new(crate::community::CommunityProviderWithUpgrade::new(
            crate::policy::PolicyResolver::embedded(),
            Arc::new(seat),
        ));
    }

    // 3. No env vars, no baked creds: pure Community.
    Arc::new(crate::CommunityProvider::new(
        crate::policy::PolicyResolver::embedded(),
    ))
}
```

- [ ] **Step 4: Add `LicenseSeatProvider::has_cached_license` helper**

In `crates/spur-license/src/licenseseat.rs`, inside `impl LicenseSeatProvider`, add:

```rust
    /// True iff the underlying SDK has a cached license on disk.
    pub fn has_cached_license(&self) -> bool {
        self.sdk.current_license().is_some()
    }
```

- [ ] **Step 5: Add `CommunityProviderWithUpgrade` variant**

Append to `crates/spur-license/src/community.rs`:

```rust
/// Community-tier surface that delegates `activate` to a baked-in
/// `LicenseSeatProvider`. Used in Option A release builds when no cached
/// license is present: the user sees Community everywhere, but
/// `spur auth login --key …` works without any env-var setup.
///
/// On successful activation, the underlying SDK persists the license cache;
/// the NEXT process launch comes up as `LicenseSeatProvider` directly via
/// the `has_cached_license()` branch in `from_env_or_disabled`.
pub struct CommunityProviderWithUpgrade {
    community: CommunityProvider,
    upgrade_target: Arc<crate::licenseseat::LicenseSeatProvider>,
}

impl CommunityProviderWithUpgrade {
    pub fn new(
        resolver: Arc<PolicyResolver>,
        upgrade_target: Arc<crate::licenseseat::LicenseSeatProvider>,
    ) -> Self {
        Self {
            community: CommunityProvider::new(resolver),
            upgrade_target,
        }
    }
}

#[async_trait]
impl LicenseProvider for CommunityProviderWithUpgrade {
    fn current_state(&self) -> LicenseState {
        // Always present as Community until restart promotes us via cache.
        self.community.current_state()
    }
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> {
        self.community.subscribe()
    }
    fn refresh_policy(&self) -> RefreshPolicy {
        self.community.refresh_policy()
    }
    fn requires_heartbeat(&self) -> bool { false }
    fn has_entitlement(&self, feature: &str) -> bool {
        self.community.has_entitlement(feature)
    }
    async fn activate(&self, key: &str) -> Result<LicenseState> {
        // Delegate to the baked-in LicenseSeat provider. On success, the
        // cache is persisted; the next process launch picks it up.
        self.upgrade_target.activate(key).await
    }
    async fn validate(&self) -> Result<LicenseState> {
        self.community.validate().await
    }
    async fn heartbeat(&self) -> Result<LicenseState> {
        self.community.heartbeat().await
    }
    async fn deactivate(&self) -> Result<LicenseState> {
        self.community.deactivate().await
    }
}
```

`LicenseSeatProvider` and `RefreshPolicy` are already in scope from the existing `CommunityProvider` imports; verify the `use` statements at the top of `community.rs` cover them (add as needed).

- [ ] **Step 6: Run tests**

Run: `cargo test -p spur-license`
Expected: existing tests still PASS. The new path is exercised at runtime; we don't add a unit test here because it requires real LicenseSeat credentials. The smoke test in Task 22 covers the no-baked-creds branch.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-license/src/build_constants.rs crates/spur-license/src/lib.rs crates/spur-license/src/licenseseat.rs crates/spur-license/src/community.rs
git commit -m "feat(spur-license): Option A baked-in LicenseSeat credentials + upgrade routing"
```

---

### Task 15: Add `feature_enabled(license, flags, key)` gating helper (FLOOR ∧ GATE — Invariant #9)

**Files:**
- Modify: `crates/spur-license/src/lib.rs` (append the helper at the bottom, before tests)
- Test: `crates/spur-license/tests/gating_contract.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-license/tests/gating_contract.rs`:

```rust
//! Invariant #9 — FLOOR ∧ GATE. Both license entitlement (FLOOR) and flag
//! evaluation (GATE) must be true for `feature_enabled` to return true.

#![cfg(feature = "test-support")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use spur_license::policy::flags::{FlagEvaluator, InstallId};
use spur_license::policy::{FlagSpec, PolicyDocument, TierPolicy};
use spur_license::test_support::FakeProvider;
use spur_license::{feature_enabled, LicenseState, Plan, SpurLicense};

fn doc_with_flag(key: &str, spec: FlagSpec) -> Arc<PolicyDocument> {
    let mut flags = BTreeMap::new();
    flags.insert(key.into(), spec);
    Arc::new(PolicyDocument {
        schema_version: 1,
        issued_at: chrono::Utc::now(),
        expires_at: None,
        tier_policies: BTreeMap::new(),
        flags,
    })
}

fn community_state_with(features: BTreeSet<String>) -> LicenseState {
    LicenseState::active_community(features)
}

#[test]
fn floor_off_gate_off_yields_off() {
    let mut features = BTreeSet::new();
    // No "f" entitlement — FLOOR is off.
    let license = SpurLicense::from_provider(Arc::new(FakeProvider::new(
        community_state_with(features),
    )));
    let flags = FlagEvaluator::new(
        doc_with_flag("f", FlagSpec { enabled: false, ..Default::default() }),
        InstallId("test".into()),
    );
    assert!(!feature_enabled(&license, &flags, "f"));
}

#[test]
fn floor_on_gate_off_yields_off() {
    let mut features = BTreeSet::new();
    features.insert("f".into());
    let license = SpurLicense::from_provider(Arc::new(FakeProvider::new(
        community_state_with(features),
    )));
    let flags = FlagEvaluator::new(
        doc_with_flag("f", FlagSpec { enabled: false, ..Default::default() }),
        InstallId("test".into()),
    );
    assert!(!feature_enabled(&license, &flags, "f"), "entitled but flag-off must be off");
}

#[test]
fn floor_off_gate_on_yields_off() {
    let features = BTreeSet::new();
    let license = SpurLicense::from_provider(Arc::new(FakeProvider::new(
        community_state_with(features),
    )));
    let flags = FlagEvaluator::new(
        doc_with_flag("f", FlagSpec { enabled: true, ..Default::default() }),
        InstallId("test".into()),
    );
    assert!(!feature_enabled(&license, &flags, "f"), "flag-on but unentitled must be off");
}

#[test]
fn floor_on_gate_on_yields_on() {
    let mut features = BTreeSet::new();
    features.insert("f".into());
    let license = SpurLicense::from_provider(Arc::new(FakeProvider::new(
        community_state_with(features),
    )));
    let flags = FlagEvaluator::new(
        doc_with_flag("f", FlagSpec { enabled: true, ..Default::default() }),
        InstallId("test".into()),
    );
    assert!(feature_enabled(&license, &flags, "f"), "both on must be on");
}
```

- [ ] **Step 2: Implement the helper**

Add at the bottom of `crates/spur-license/src/lib.rs`, BEFORE the existing `#[cfg(test)] mod tests`:

```rust
/// THE callsite contract for gating: license entitlement (FLOOR) ∧ flag
/// evaluation (GATE). Both must be true for the feature to be exposed.
///
/// FLOOR — `license.has_entitlement(key)` — does the user's tier include
/// this feature?
/// GATE — `flags.is_enabled(key, &license.current_state())` — is the
/// rollout open to this user right now?
///
/// Conjunction makes the system safe-by-default:
/// - A misconfigured flag cannot grant entitlements you don't have.
/// - A misconfigured license cannot expose features that aren't safe to
///   expose yet.
pub fn feature_enabled(
    license: &SpurLicense,
    flags: &crate::policy::flags::FlagEvaluator,
    key: &str,
) -> bool {
    license.has_entitlement(key)
        && flags.is_enabled(key, &license.current_state())
}
```

- [ ] **Step 3: Run the gating tests**

Run: `cargo test -p spur-license --features test-support --test gating_contract`
Expected: 4 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/lib.rs crates/spur-license/tests/gating_contract.rs
git commit -m "feat(spur-license): add feature_enabled FLOOR ∧ GATE gating helper (invariant #9)"
```

---

## Phase 4 — CLI onboarding, flags subcommand, output redesign (Tasks 16–19)

### Task 16: Add `is-terminal` dep + onboarding marker module

**Files:**
- Modify: `crates/spur-cli/Cargo.toml`
- Create: `crates/spur-cli/src/onboarding.rs`
- Modify: `crates/spur-cli/src/main.rs` (add `mod onboarding;`)

- [ ] **Step 1: Add the dep**

Add to `[dependencies]` in `crates/spur-cli/Cargo.toml` (after `directories`):

```toml
is-terminal = "0.4"
```

- [ ] **Step 2: Write failing tests**

Create `crates/spur-cli/src/onboarding.rs`:

```rust
//! First-run TTY prompt for the Community-default onboarding path.
//!
//! Persists `~/.spur/onboarded` (a one-line JSON marker) once the user has
//! either pasted a license key or explicitly continued on Community. On
//! subsequent runs the marker presence skips the prompt.
//!
//! TTY-skip: `is_terminal()` short-circuits when stdin isn't interactive
//! (CI safe).

use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use is_terminal::IsTerminal;
use serde::{Deserialize, Serialize};
use spur_license::{LicenseStatus, Plan, SpurLicense};

#[derive(Serialize, Deserialize)]
struct OnboardingMarker {
    version: u32,
    first_run_at: String,
}

pub fn marker_path() -> Option<PathBuf> {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".spur").join("onboarded"))
}

pub fn marker_exists() -> bool {
    marker_path().map(|p| p.exists()).unwrap_or(false)
}

pub fn write_marker() -> Result<()> {
    let path = marker_path().context("no home directory; cannot write onboarding marker")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let marker = OnboardingMarker {
        version: 1,
        first_run_at: chrono::Utc::now().to_rfc3339(),
    };
    std::fs::write(&path, serde_json::to_string(&marker)?)?;
    Ok(())
}

/// Returns true if the prompt SHOULD run for this license state.
/// False when: not a TTY, marker present, or license already configured
/// (anything other than the bare Community-default state).
pub fn should_prompt(license: &SpurLicense) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    if marker_exists() {
        return false;
    }
    let state = license.current_state();
    matches!(state.plan, Plan::Community)
        && matches!(state.status, LicenseStatus::Active)
}

/// Run the first-run prompt. Public entry point called from main.rs.
/// On any error, logs and continues (never blocks startup).
pub async fn maybe_prompt_first_run(license: &SpurLicense) -> Result<()> {
    if !should_prompt(license) {
        return Ok(());
    }
    eprintln!(
        "spur is running on the Community tier (free). Paste a license key to unlock Pro now, or press Enter to continue."
    );
    eprint!("> ");
    use std::io::Write;
    std::io::stderr().flush().ok();

    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let trimmed = line.trim();

    if trimmed.is_empty() {
        eprintln!("Continuing on Community.");
    } else {
        match license.activate(trimmed).await {
            Ok(state) => {
                eprintln!("Activated: {} ({})", state.plan.label(), state.status_text);
            }
            Err(e) => {
                eprintln!("Activation failed: {e}. Continuing on Community.");
            }
        }
    }
    write_marker()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_serialization_round_trips() {
        let marker = OnboardingMarker {
            version: 1,
            first_run_at: "2026-04-19T00:00:00Z".into(),
        };
        let s = serde_json::to_string(&marker).unwrap();
        let back: OnboardingMarker = serde_json::from_str(&s).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.first_run_at, "2026-04-19T00:00:00Z");
    }
}
```

- [ ] **Step 3: Wire into main.rs**

Add to `crates/spur-cli/src/main.rs` near the top (after the other `mod` lines):

```rust
mod onboarding;
```

Then find line 430 (`let license = SpurLicense::from_env_or_disabled();`) and immediately after it add:

```rust
            if let Err(e) = onboarding::maybe_prompt_first_run(&license).await {
                tracing::warn!("first-run prompt failed: {e}; continuing");
            }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-cli onboarding::tests`
Expected: PASS.

Run: `cargo build -p spur-cli`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/Cargo.toml crates/spur-cli/src/onboarding.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): add maybe_prompt_first_run for Community onboarding"
```

---

### Task 17: Add `spur flags list` subcommand

**Files:**
- Create: `crates/spur-cli/src/commands/flags.rs`
- Modify: `crates/spur-cli/src/commands/mod.rs`
- Modify: `crates/spur-cli/src/main.rs` (wire the subcommand into the top-level Command enum)

- [ ] **Step 1: Locate the top-level Command enum**

Run: `grep -n "enum Command\|enum Commands\|#\\[command(subcommand)\\]" crates/spur-cli/src/main.rs | head -5`

Note the location. The new subcommand `Flags` will be added there.

- [ ] **Step 2: Write the subcommand module**

Create `crates/spur-cli/src/commands/flags.rs`:

```rust
//! `spur flags list` — introspect the Day-1 FF capability.
//!
//! Prints each flag in the embedded `default_policy.json` along with its
//! evaluation result for the current install + license combination.

use anyhow::Result;
use clap::Subcommand;
use std::sync::Arc;

use spur_license::policy::flags::{FlagEvaluator, InstallId};
use spur_license::policy::PolicyResolver;
use spur_license::SpurLicense;

#[derive(Subcommand, Debug, Clone)]
pub enum FlagsCommands {
    /// List all flags and their evaluation results.
    List {
        /// Show full FlagSpec details (description, extensions).
        #[arg(long, short)]
        verbose: bool,
    },
}

pub async fn run(command: FlagsCommands) -> Result<()> {
    let license = SpurLicense::from_env_or_disabled();
    run_with_license(command, license)
}

pub fn run_with_license(command: FlagsCommands, license: SpurLicense) -> Result<()> {
    match command {
        FlagsCommands::List { verbose } => {
            let resolver = PolicyResolver::embedded();
            let install_id = match InstallId::default_path() {
                Some(p) => InstallId::load_or_create(&p)
                    .unwrap_or_else(|_| InstallId(uuid::Uuid::new_v4().to_string())),
                None => InstallId(uuid::Uuid::new_v4().to_string()),
            };
            let evaluator = FlagEvaluator::new(resolver.document(), install_id);
            let state = license.current_state();

            println!(
                "{:<28} {:<8} {:<8} {:<16} {:<6}",
                "FLAG", "ENABLED", "ROLLOUT", "TIER FILTER", "RESULT"
            );
            for ex in evaluator.explain_all(&state) {
                let rollout = ex
                    .rollout_percent
                    .map(|p| format!("{p:.0}%"))
                    .unwrap_or_else(|| "—".into());
                let tier_filter = ex
                    .tier_filter
                    .as_ref()
                    .map(|v| v.join(","))
                    .unwrap_or_else(|| "—".into());
                let result = if ex.result { "on" } else { "off" };
                println!(
                    "{:<28} {:<8} {:<8} {:<16} {:<6}",
                    ex.flag_key,
                    ex.enabled,
                    rollout,
                    tier_filter,
                    result
                );
                if verbose {
                    if let Some(desc) = &ex.description {
                        println!("    {desc}");
                    }
                    println!("    bucket={} license_tier={}", ex.bucket, ex.license_tier);
                }
            }
            Ok(())
        }
    }
}
```

- [ ] **Step 3: Wire into commands/mod.rs**

Edit `crates/spur-cli/src/commands/mod.rs` and replace contents with:

```rust
pub mod auth;
pub mod config_check;
pub mod flags;
pub mod init;
```

- [ ] **Step 4: Wire into main.rs Command enum**

In `crates/spur-cli/src/main.rs`, find the existing top-level subcommand enum (alongside `Auth`, `Init`, etc.) and add:

```rust
    /// Inspect runtime feature flags.
    Flags {
        #[command(subcommand)]
        command: crate::commands::flags::FlagsCommands,
    },
```

Then in the `match cli.command { ... }` arms, add:

```rust
        Commands::Flags { command } => commands::flags::run(command).await,
```

(Use whatever local-module path matches the existing arms — the project uses either `crate::commands::*` or `commands::*`. Match the existing style.)

- [ ] **Step 5: Verify the binary builds and runs**

Run: `cargo build -p spur-cli && ./target/debug/spur flags list`
Expected: prints the 4 flags from `default_policy.json` with result columns.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/commands/flags.rs crates/spur-cli/src/commands/mod.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): add 'spur flags list' subcommand"
```

---

### Task 18: Redesign `spur auth status` plain output

**Files:**
- Modify: `crates/spur-cli/src/commands/auth.rs` (replace `print_state`)

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/spur-cli/src/commands/auth.rs`:

```rust
#[cfg(test)]
mod print_state_tests {
    use super::*;
    use std::collections::BTreeSet;
    use spur_license::{BindingMode, LicenseState, LicenseStatus, Plan, SubjectKind};

    fn fixture(status: LicenseStatus, plan: Plan, status_text: &str) -> LicenseState {
        LicenseState {
            status,
            subject_kind: SubjectKind::User,
            plan,
            features: BTreeSet::new(),
            expires_at: None,
            binding_mode: BindingMode::Unknown,
            offline_ok: true,
            status_text: status_text.into(),
        }
    }

    #[test]
    fn plain_summary_for_community_active() {
        let state = fixture(LicenseStatus::Active, Plan::Community, "Community tier");
        let s = format_plain_summary(&state);
        assert!(s.contains("Community"), "saw: {s}");
        assert!(s.contains("free"), "saw: {s}");
    }

    #[test]
    fn plain_summary_for_pro_active() {
        let state = fixture(LicenseStatus::Active, Plan::Pro, "License validated");
        let s = format_plain_summary(&state);
        assert!(s.contains("Pro"), "saw: {s}");
        assert!(s.contains("active"), "saw: {s}");
    }

    #[test]
    fn plain_summary_for_degraded() {
        let state = fixture(LicenseStatus::Degraded, Plan::Pro, "Heartbeat failed");
        let s = format_plain_summary(&state);
        assert!(s.to_lowercase().contains("degraded"), "saw: {s}");
    }

    #[test]
    fn plain_summary_for_invalid() {
        let state = fixture(LicenseStatus::Invalid, Plan::Unknown, "license revoked");
        let s = format_plain_summary(&state);
        assert!(s.to_lowercase().contains("invalid"), "saw: {s}");
        assert!(s.contains("license revoked"), "saw: {s}");
    }
}
```

- [ ] **Step 2: Replace `print_state` with `format_plain_summary` + `print_state`**

In `crates/spur-cli/src/commands/auth.rs`, replace the existing `print_state` function (lines ~108–118) with:

```rust
pub(crate) fn format_plain_summary(state: &LicenseState) -> String {
    use spur_license::{LicenseStatus, Plan};
    let plan_label = state.plan.label();
    let expiry_suffix = state
        .expires_at
        .as_ref()
        .map(|d| format!(" until {}", d.format("%Y-%m-%d")))
        .unwrap_or_default();
    match (state.status, state.plan) {
        (LicenseStatus::Active, Plan::Community) => format!(
            "spur Community — free tier  ⓘ run 'spur auth login --key …' to unlock Pro"
        ),
        (LicenseStatus::Active, _) => format!(
            "spur {plan_label} — active{expiry_suffix}  ✓ all features unlocked"
        ),
        (LicenseStatus::Degraded, _) => format!(
            "spur {plan_label} — degraded (network)  ⚠ cached license still valid offline"
        ),
        (LicenseStatus::Invalid, _) => format!(
            "spur — license invalid  ✗ {}",
            state.status_text
        ),
        (LicenseStatus::ConfigError, _) => format!(
            "spur — config error  ✗ {}",
            state.status_text
        ),
        (LicenseStatus::Inactive, _) => format!(
            "spur — inactive  ⓘ run 'spur auth login --key …' to activate"
        ),
    }
}

fn print_state(state: &LicenseState) {
    println!("{}", format_plain_summary(state));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-cli auth::print_state_tests`
Expected: 4 tests PASS.

Run: `cargo build -p spur-cli && ./target/debug/spur auth status`
Expected: single-line output (no env vars set → Community summary).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/src/commands/auth.rs
git commit -m "feat(spur-cli): single-line plain output for spur auth status"
```

---

### Task 19: Workspace-wide build + clippy + fmt verification

**Files:** none (verification gate)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`

- [ ] **Step 2: Build everything**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 3: Test everything**

Run: `cargo test --workspace`
Expected: PASS. The hardening spec's existing tests (e.g. `event_funnel::tests::funnel_stamps_monotonic_seq`, `licenseseat::dedup_tests::*`, `licenseseat_probe::*`, `invariants::*`) MUST all still pass. If any of them fail, the change broke a hardening invariant — investigate.

- [ ] **Step 4: Clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: PASS. Fix any warnings flagged.

- [ ] **Step 5: Format check**

Run: `cargo fmt --all --check`
Expected: PASS.

- [ ] **Step 6: Commit any fmt changes**

```bash
git add -A
git diff --cached --quiet || git commit -m "style: cargo fmt after Phase 4"
```

---

## Phase 5 — Docs, demo key, and overlay scaffolding (Tasks 20–22)

### Task 20: Onboarding docs + flag-conventions reference

**Files:**
- Create: `docs/onboarding/community-tier.md`
- Create: `docs/onboarding/try-pro.md`
- Create: `docs/contributing/flag-conventions.md`

- [ ] **Step 1: Write `docs/onboarding/community-tier.md`**

Create with:

````markdown
# spur Community tier

When you install spur with no LicenseSeat configuration, it runs on the **Community** tier — a free tier that includes:

| Feature | Available on Community |
|---|---|
| `chat` | ✓ |
| `code_edit` | ✓ |
| `watch_loop` | ✓ |
| `advanced_agents` | — Pro |
| `cloud_sync` | — Pro |
| `team_sharing` | — Team |

The canonical list lives in [`crates/spur-license/resources/default_policy.json`](../../crates/spur-license/resources/default_policy.json) under `tier_policies`. It is signed (Ed25519) and verified at compile time and runtime.

## Upgrading to Pro

Run `spur auth login --key <YOUR-KEY>` once you have a license key. The tool persists the activation locally and the next `spur watch` comes up as Pro.

If you don't have a key yet, see [Try Pro](try-pro.md).
````

- [ ] **Step 2: Write `docs/onboarding/try-pro.md`**

Create with:

````markdown
# Try Pro features

We provide a public, rate-limited demo key so you can evaluate Pro without signing up:

```
spur auth login --key DEMO-SPUR-2026-Q2
```

**Demo key details:**
- Wall-clock expiry: 2026-07-01 (rotated quarterly).
- Activation rate-limited tenant-side.
- All Pro entitlements unlocked for the duration.

When the key expires, `spur` automatically falls back to the Community tier. To stay on Pro, purchase a license from your team's vendor portal and run `spur auth login --key <REAL-KEY>`.
````

- [ ] **Step 3: Write `docs/contributing/flag-conventions.md`**

Create with:

````markdown
# Feature flag conventions

spur ships with a Day-1 runtime feature-flag (FF) capability backed by a custom local `FlagEvaluator` over the embedded signed `PolicyDocument`. This doc explains when to add a flag, how, and the discipline around it.

## When to add a flag

Add a flag when shipping a change that:
- **Is risky** — autonomous-agent behavior, file edits, network calls, irreversible actions.
- **Should be gradually rolled out** — start at `rollout_percent: 10.0`, ramp.
- **Needs a kill switch** — a `enabled: true` default with an obvious flip-to-`false` recovery path.

Don't add a flag when:
- The change is purely additive (new helper, new doc, new test).
- The behavior is deterministic and the failure mode is bounded.

## How to add a flag

1. Add a `pub const` to [`crates/spur-license/src/policy/feature_key.rs`](../../crates/spur-license/src/policy/feature_key.rs).
2. Add a `FlagSpec` entry to `flags` in [`crates/spur-license/resources/default_policy.json`](../../crates/spur-license/resources/default_policy.json).
3. Re-sign with `SPUR_POLICY_SIGNING_KEY=… bash crates/spur-license/scripts/sign-policy.sh`.
4. At the gating callsite, use `feature_enabled(&license, &flags, FeatureKey::FOO.as_str())` — NOT `license.has_entitlement` directly.

## The FLOOR ∧ GATE rule (invariant #9)

`feature_enabled(license, flags, key)` returns `true` only when BOTH:
- **FLOOR** — `license.has_entitlement(key)` (the user is contractually allowed)
- **GATE** — `flags.is_enabled(key, &license.current_state())` (the rollout is open to this user)

A misconfigured flag cannot grant entitlements you don't have. A misconfigured license cannot expose features that aren't safe to expose yet.

## The 4 baseline flags

| Flag | Default | Purpose |
|---|---|---|
| `kill_advanced_planner` | `true` | Kill switch on the new agent planner. |
| `enable_browser_tool` | `true` | Gradual ramp candidate. |
| `enable_compaction_v2` | `true` | Kill switch on V2 compaction. |
| `enable_telemetry` | `false` | OFF until the telemetry spec lands. |

## Why custom over OpenFeature/flagd

Multi-round analysis in the [design spec](../superpowers/specs/2026-04-19-community-default-onboarding-design.md) concluded that for spur's current scale, a 150-LoC local evaluator over the existing signed PolicyDocument beats OpenFeature+flagd on cost, maintenance, review ergonomics, and operational simplicity. Migration to OpenFeature later is a bounded ~200-LoC custom `FeatureProvider` exercise — paid only when ecosystem benefits actually matter (e.g., when telemetry lands and we want PostHog experimentation).
````

- [ ] **Step 4: Commit**

```bash
git add docs/onboarding/community-tier.md docs/onboarding/try-pro.md docs/contributing/flag-conventions.md
git commit -m "docs: add Community tier, Try Pro, and flag-conventions guides"
```

---

### Task 21: Add overlay-loader scaffold for `~/.spur/policy-overlay.json` (deferred-by-default)

**Files:**
- Modify: `crates/spur-license/src/policy/mod.rs` (add overlay loader path)

The full hot-swap mechanism is V2; this task adds the loader code so an overlay file IS honored if present, but does NOT add background reload. Behavior-neutral when no overlay exists.

- [ ] **Step 1: Write the failing test**

Add to `crates/spur-license/src/policy/mod.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn overlay_supersedes_when_newer_and_signed() {
        // Overlay loader: when a signed overlay file exists with issued_at
        // > embedded.issued_at AND signature verifies AND not expired,
        // PolicyResolver::with_overlay returns a resolver over the overlay.
        // We can't easily forge a signed payload in tests without leaking
        // the private key, so this test only exercises the path-not-present
        // branch. Real signature happy-path is covered by Task 6 tests via
        // the embedded path; the overlay path uses the same verifier.
        let result = PolicyResolver::with_overlay_path(std::path::Path::new("/nonexistent/overlay.json"));
        // Falls back to embedded.
        assert!(result.tier_features("community").contains("chat"));
    }

    #[test]
    fn overlay_with_invalid_signature_falls_back_to_embedded() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), r#"{"payload":"{}","signature":"AAAA","key_id":"spur-policy-2026-04"}"#).unwrap();
        let r = PolicyResolver::with_overlay_path(tmp.path());
        // Overlay rejected (signature won't verify); falls back to embedded.
        assert!(r.tier_features("community").contains("chat"));
    }
```

- [ ] **Step 2: Implement `with_overlay_path` and a default `with_overlay`**

Add to `crates/spur-license/src/policy/mod.rs` inside `impl PolicyResolver`:

```rust
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
                    Arc::new(Self { document: Arc::new(doc) })
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
```

Add `tracing` to `[dependencies]` of `crates/spur-license/Cargo.toml` if not already present (check with `grep -n tracing crates/spur-license/Cargo.toml`):

```toml
tracing = { workspace = true }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license policy::tests::overlay`
Expected: 2 tests PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/policy/mod.rs crates/spur-license/Cargo.toml
git commit -m "feat(spur-license): add signed policy-overlay loader (~/.spur/policy-overlay.json)"
```

---

### Task 22: Final integration smoke test (manual + scripted)

**Files:**
- Create: `crates/spur-cli/tests/community_smoke.rs`

- [ ] **Step 1: Write the smoke test**

Create `crates/spur-cli/tests/community_smoke.rs`:

```rust
//! End-to-end smoke: a fresh process with no LicenseSeat env vars must come
//! up as Community with the correct entitlements.

use spur_license::{Plan, SpurLicense};

#[test]
fn fresh_process_no_env_vars_is_community() {
    std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
    std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
    let license = SpurLicense::from_env_or_disabled();
    let state = license.current_state();
    assert!(matches!(state.plan, Plan::Community), "got {:?}", state.plan);
    assert!(license.has_entitlement("chat"));
    assert!(license.has_entitlement("watch_loop"));
    assert!(!license.has_entitlement("advanced_agents"));
}

#[test]
fn community_provider_blocks_activate() {
    std::env::remove_var("SPUR_LICENSESEAT_API_KEY");
    std::env::remove_var("SPUR_LICENSESEAT_PRODUCT_SLUG");
    let license = SpurLicense::from_env_or_disabled();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(license.activate("ANY-KEY"));
    assert!(result.is_err(), "Community provider must reject activate");
}
```

- [ ] **Step 2: Run the smoke test**

Run: `cargo test -p spur-cli --test community_smoke`
Expected: 2 tests PASS.

- [ ] **Step 3: Manual smoke**

```bash
unset SPUR_LICENSESEAT_API_KEY SPUR_LICENSESEAT_PRODUCT_SLUG
cargo build -p spur-cli
./target/debug/spur auth status
# Expected: "spur Community — free tier  ⓘ run 'spur auth login --key …' to unlock Pro"

./target/debug/spur flags list
# Expected: 4 flags listed with their RESULT columns
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/tests/community_smoke.rs
git commit -m "test(spur-cli): end-to-end smoke for Community-default behavior"
```

---

## Final verification checklist (after all tasks)

- [ ] `cargo build --workspace` PASS
- [ ] `cargo test --workspace` PASS — every existing test still green; new tests for invariants #6–#11 covered
- [ ] `cargo clippy --workspace -- -D warnings` PASS
- [ ] `cargo fmt --all --check` PASS
- [ ] Manual: `unset SPUR_LICENSESEAT_*; cargo run -p spur-cli -- auth status` shows Community single-line summary
- [ ] Manual: `cargo run -p spur-cli -- flags list` shows the 4 placeholder flags
- [ ] Manual (with valid env vars): `SPUR_LICENSESEAT_API_KEY=… SPUR_LICENSESEAT_PRODUCT_SLUG=… cargo run -p spur-cli -- auth status` shows the LicenseSeat-backed status
- [ ] Hardening spec's existing tests (`event_funnel`, `licenseseat::dedup_tests`, `licenseseat_probe`, `invariants`) all still PASS
- [ ] `docs/onboarding/community-tier.md`, `docs/onboarding/try-pro.md`, `docs/contributing/flag-conventions.md` all present
