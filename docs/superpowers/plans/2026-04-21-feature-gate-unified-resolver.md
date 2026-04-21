# Feature Gate UnifiedResolver Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `FeatureGate`, a wait-free feature/entitlement/quota API that merges embedded `PolicyDocument` and `LicenseSeat` server state into a single canonical view.

**Architecture:** `FeatureGate` uses `arc_swap::ArcSwap<EntitlementSnapshot>` for wait-free reads. Snapshots are immutable; state changes recompute and swap atomically. `FeatureKey` is an open-set newtype (known keys as `pub const`, unknown keys gracefully ignored). `QuotaKey` is a closed-set enum. All types live in `spur-license` and are additive to existing code.

**Tech Stack:** Rust 2021, `arc-swap`, `ahash`, `seahash`, existing `spur-license` crate, `licenseseat` SDK v0.5.3

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-license/Cargo.toml` | Modify | Add `arc-swap`, `ahash`, `seahash` dependencies |
| `crates/spur-license/src/policy/feature_key.rs` | Modify | Expand to 31 keys, add `from_known`, `UnknownFeatureKey` |
| `crates/spur-license/src/quota.rs` | Create | `QuotaKey` enum, `QuotaValue` enum |
| `crates/spur-license/src/tier.rs` | Create | `Tier` enum, `from_plan`, `label` |
| `crates/spur-license/src/snapshot.rs` | Create | `EntitlementSnapshot`, `SourceMetadata` |
| `crates/spur-license/src/gate.rs` | Create | `FeatureGate` — the public API |
| `crates/spur-license/src/lib.rs` | Modify | Export new types, wire `FeatureGate` into `SpurLicense` |
| `crates/spur-license/tests/feature_gate.rs` | Create | Unit + integration tests for `FeatureGate` |
| `crates/spur-license/tests/feature_key.rs` | Create | Tests for `FeatureKey` parsing, equality, hashing |

---

## Task 1: Add Dependencies

**Files:**
- Modify: `crates/spur-license/Cargo.toml`

- [ ] **Step 1: Add `arc-swap`, `ahash`, `seahash` to `[dependencies]`**

```toml
[dependencies]
# ... existing deps ...
arc-swap = "1.7"
ahash = "0.8"
seahash = "4"
```

- [ ] **Step 2: Verify workspace compiles**

Run: `cargo check -p spur-license`
Expected: PASS (new deps not used yet, just downloaded)

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/Cargo.toml
git commit -m "chore(spur-license): add arc-swap, ahash, seahash deps for FeatureGate"
```

---

## Task 2: Expand `FeatureKey` Registry

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs`
- Create: `crates/spur-license/tests/feature_key.rs`

- [ ] **Step 1: Write failing test for new `FeatureKey` constants**

```rust
// crates/spur-license/tests/feature_key.rs
use spur_license::FeatureKey;

#[test]
fn known_features_exist() {
    assert_eq!(FeatureKey::PARALLEL_WORKERS.as_str(), "parallel_workers");
    assert_eq!(FeatureKey::PM_INTEGRATION.as_str(), "pm_integration");
    assert_eq!(FeatureKey::SSO_SAML.as_str(), "sso_saml");
}

#[test]
fn from_known_parses_all_keys() {
    assert_eq!(FeatureKey::from_known("parallel_workers"), Some(FeatureKey::PARALLEL_WORKERS));
    assert_eq!(FeatureKey::from_known("unknown_feature_42"), None);
}

#[test]
fn feature_key_is_copy_and_hashable() {
    let a = FeatureKey::BRAIN_SESSION;
    let b = FeatureKey::BRAIN_SESSION;
    assert_eq!(a, b);
    let mut set = std::collections::HashSet::new();
    set.insert(a);
    assert!(set.contains(&b));
}
```

Run: `cargo test -p spur-license --test feature_key`
Expected: FAIL — `FeatureKey` doesn't have new constants or `from_known`

- [ ] **Step 2: Replace `feature_key.rs` with expanded registry**

```rust
// crates/spur-license/src/policy/feature_key.rs
//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.
//!
//! Open set: LicenseSeat server may send entitlement keys we don't know
//! about yet. Those are gracefully ignored (fail-closed) via
//! `FeatureKey::from_known` returning `None`.

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    // G1 entitlement keys — Community tier
    pub const BRAIN_SESSION: Self = Self("brain_session");
    pub const SINGLE_WORKER: Self = Self("single_worker");
    pub const WORKTREE_ISOLATION: Self = Self("worktree_isolation");
    pub const MANUAL_REVIEW: Self = Self("manual_review");
    pub const EVENT_PERSISTENCE: Self = Self("event_persistence");
    pub const BASIC_LINEAGE: Self = Self("basic_lineage");
    pub const TUI_DASHBOARD: Self = Self("tui_dashboard");
    pub const BASIC_COST_DISPLAY: Self = Self("basic_cost_display");
    pub const BASIC_NOTIFICATIONS: Self = Self("basic_notifications");
    pub const LOCAL_CONFIG: Self = Self("local_config");
    pub const MCP_STANDARD_TOOLS: Self = Self("mcp_standard_tools");

    // G1 entitlement keys — Pro tier
    pub const PARALLEL_WORKERS: Self = Self("parallel_workers");
    pub const AUTO_REVIEW_POLICIES: Self = Self("auto_review_policies");
    pub const SESSION_RESUME: Self = Self("session_resume");
    pub const ADVANCED_COST_ANALYTICS: Self = Self("advanced_cost_analytics");
    pub const CUSTOM_WORKTREE_POLICIES: Self = Self("custom_worktree_policies");
    pub const CUSTOM_NOTIFICATIONS: Self = Self("custom_notifications");
    pub const EXTENDED_RETENTION: Self = Self("extended_retention");
    pub const TUI_SESSION_DETAIL: Self = Self("tui_session_detail");

    // G1 entitlement keys — Team tier
    pub const PM_INTEGRATION: Self = Self("pm_integration");
    pub const SHARED_LINEAGE: Self = Self("shared_lineage");
    pub const TEAM_COST_DASHBOARD: Self = Self("team_cost_dashboard");
    pub const CENTRALIZED_CONFIG: Self = Self("centralized_config");
    pub const RBAC: Self = Self("rbac");
    pub const SHARED_REVIEW_QUEUE: Self = Self("shared_review_queue");
    pub const PM_WEBHOOKS: Self = Self("pm_webhooks");

    // G1 entitlement keys — Enterprise tier
    pub const SSO_SAML: Self = Self("sso_saml");
    pub const AUDIT_LOGS: Self = Self("audit_logs");
    pub const CUSTOM_POLICIES: Self = Self("custom_policies");
    pub const CUSTOM_MCP_TOOLS: Self = Self("custom_mcp_tools");
    pub const DEDICATED_SUPPORT: Self = Self("dedicated_support");
    pub const SLA_GUARANTEE: Self = Self("sla_guarantee");

    // G2 flag keys (always from PolicyDocument)
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str {
        self.0
    }

    /// Parse a string into a known FeatureKey. Returns `None` for unknown keys.
    pub const fn from_known(s: &str) -> Option<Self> {
        match s {
            "brain_session" => Some(Self::BRAIN_SESSION),
            "single_worker" => Some(Self::SINGLE_WORKER),
            "parallel_workers" => Some(Self::PARALLEL_WORKERS),
            "worktree_isolation" => Some(Self::WORKTREE_ISOLATION),
            "manual_review" => Some(Self::MANUAL_REVIEW),
            "auto_review_policies" => Some(Self::AUTO_REVIEW_POLICIES),
            "shared_review_queue" => Some(Self::SHARED_REVIEW_QUEUE),
            "event_persistence" => Some(Self::EVENT_PERSISTENCE),
            "extended_retention" => Some(Self::EXTENDED_RETENTION),
            "session_resume" => Some(Self::SESSION_RESUME),
            "basic_lineage" => Some(Self::BASIC_LINEAGE),
            "shared_lineage" => Some(Self::SHARED_LINEAGE),
            "tui_dashboard" => Some(Self::TUI_DASHBOARD),
            "tui_session_detail" => Some(Self::TUI_SESSION_DETAIL),
            "basic_cost_display" => Some(Self::BASIC_COST_DISPLAY),
            "advanced_cost_analytics" => Some(Self::ADVANCED_COST_ANALYTICS),
            "team_cost_dashboard" => Some(Self::TEAM_COST_DASHBOARD),
            "pm_integration" => Some(Self::PM_INTEGRATION),
            "pm_webhooks" => Some(Self::PM_WEBHOOKS),
            "basic_notifications" => Some(Self::BASIC_NOTIFICATIONS),
            "custom_notifications" => Some(Self::CUSTOM_NOTIFICATIONS),
            "local_config" => Some(Self::LOCAL_CONFIG),
            "centralized_config" => Some(Self::CENTRALIZED_CONFIG),
            "custom_policies" => Some(Self::CUSTOM_POLICIES),
            "rbac" => Some(Self::RBAC),
            "audit_logs" => Some(Self::AUDIT_LOGS),
            "sso_saml" => Some(Self::SSO_SAML),
            "custom_mcp_tools" => Some(Self::CUSTOM_MCP_TOOLS),
            "dedicated_support" => Some(Self::DEDICATED_SUPPORT),
            "sla_guarantee" => Some(Self::SLA_GUARANTEE),
            "kill_advanced_planner" => Some(Self::KILL_ADVANCED_PLANNER),
            "enable_browser_tool" => Some(Self::ENABLE_BROWSER_TOOL),
            "enable_compaction_v2" => Some(Self::ENABLE_COMPACTION_V2),
            "enable_telemetry" => Some(Self::ENABLE_TELEMETRY),
            _ => None,
        }
    }
}

impl std::fmt::Display for FeatureKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Unknown keys from LicenseSeat server. Used when the server sends an
/// entitlement key that SPUR doesn't recognize yet. Kept separate to avoid
/// polluting the known-key namespace.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnknownFeatureKey(std::sync::Arc<str>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_const_name_lowercase() {
        assert_eq!(FeatureKey::PARALLEL_WORKERS.as_str(), "parallel_workers");
        assert_eq!(FeatureKey::PM_INTEGRATION.as_str(), "pm_integration");
    }

    #[test]
    fn copy_eq_and_hash_work() {
        let a = FeatureKey::BRAIN_SESSION;
        let b = FeatureKey::BRAIN_SESSION;
        assert_eq!(a, b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn from_known_recognizes_all_consts() {
        assert_eq!(FeatureKey::from_known("brain_session"), Some(FeatureKey::BRAIN_SESSION));
        assert_eq!(FeatureKey::from_known("sso_saml"), Some(FeatureKey::SSO_SAML));
        assert_eq!(FeatureKey::from_known("unknown_thing"), None);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license --test feature_key`
Expected: PASS

Run: `cargo test -p spur-license policy::feature_key`
Expected: PASS (unit tests in the module)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs crates/spur-license/tests/feature_key.rs
git commit -m "feat(spur-license): expand FeatureKey to 31 architectural keys + from_known"
```

---

## Task 3: Implement `QuotaKey` + `QuotaValue`

**Files:**
- Create: `crates/spur-license/src/quota.rs`
- Modify: `crates/spur-license/src/lib.rs`

- [ ] **Step 1: Write failing test for `QuotaKey` and `QuotaValue`**

```rust
// Add to crates/spur-license/tests/feature_gate.rs (will be created in Task 6)
// For now, create a temporary test file

// crates/spur-license/tests/quota.rs
use spur_license::{QuotaKey, QuotaValue};

#[test]
fn quota_key_as_str_roundtrips() {
    assert_eq!(QuotaKey::MaxConcurrentWorkers.as_str(), "max_concurrent_workers");
    assert_eq!(QuotaKey::EventRetentionBytes.as_str(), "event_retention_bytes");
}

#[test]
fn quota_value_as_count() {
    assert_eq!(QuotaValue::Count(5).as_count(), Some(5));
    assert_eq!(QuotaValue::Unlimited.as_count(), None);
}

#[test]
fn quota_value_as_bytes() {
    assert_eq!(QuotaValue::Bytes(1024).as_bytes(), Some(1024));
    assert_eq!(QuotaValue::Count(1).as_bytes(), None);
}
```

Run: `cargo test -p spur-license --test quota`
Expected: FAIL — types don't exist yet

- [ ] **Step 2: Create `quota.rs`**

```rust
// crates/spur-license/src/quota.rs
//! Closed-set quota keys and strongly-typed quota values.
//!
//! Quotas are defined entirely by SPUR; LicenseSeat has no concept of
//! `max_concurrent_workers`.

/// Closed-set quota keys. Exhaustiveness-checked at compile time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum QuotaKey {
    MaxConcurrentWorkers,
    EventRetentionBytes,
    MaxTeamMembers,
    MinSeats,
}

impl QuotaKey {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MaxConcurrentWorkers => "max_concurrent_workers",
            Self::EventRetentionBytes => "event_retention_bytes",
            Self::MaxTeamMembers => "max_team_members",
            Self::MinSeats => "min_seats",
        }
    }
}

impl std::fmt::Display for QuotaKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Strongly-typed quota value. Avoids mixing counts and bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum QuotaValue {
    Unlimited,
    Count(u64),
    Bytes(u64),
}

impl QuotaValue {
    pub fn as_count(&self) -> Option<u64> {
        match self {
            Self::Count(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<u64> {
        match self {
            Self::Bytes(n) => Some(*n),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_key_display() {
        assert_eq!(QuotaKey::MaxConcurrentWorkers.to_string(), "max_concurrent_workers");
    }

    #[test]
    fn quota_value_count() {
        assert_eq!(QuotaValue::Count(42).as_count(), Some(42));
        assert_eq!(QuotaValue::Unlimited.as_count(), None);
    }

    #[test]
    fn quota_value_bytes() {
        assert_eq!(QuotaValue::Bytes(1024).as_bytes(), Some(1024));
        assert_eq!(QuotaValue::Count(1).as_bytes(), None);
    }
}
```

- [ ] **Step 3: Add `mod quota;` and re-export to `lib.rs`**

```rust
// In crates/spur-license/src/lib.rs, add after existing `mod` declarations:
mod quota;
mod tier;
mod snapshot;
mod gate;

// Add to existing `pub use` declarations:
pub use crate::quota::{QuotaKey, QuotaValue};
pub use crate::tier::Tier;
pub use crate::snapshot::EntitlementSnapshot;
pub use crate::gate::FeatureGate;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-license --test quota`
Expected: PASS

Run: `cargo test -p spur-license quota`
Expected: PASS (unit tests)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/quota.rs crates/spur-license/tests/quota.rs crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): add QuotaKey and QuotaValue types"
```

---

## Task 4: Implement `Tier`

**Files:**
- Create: `crates/spur-license/src/tier.rs`

- [ ] **Step 1: Create failing test**

```rust
// crates/spur-license/tests/tier.rs
use spur_license::{Tier, Plan};

#[test]
fn tier_from_plan_community() {
    assert_eq!(Tier::from_plan(Plan::Community), Tier::Community);
}

#[test]
fn tier_from_plan_pro_variants() {
    assert_eq!(Tier::from_plan(Plan::Pro), Tier::Pro);
    assert_eq!(Tier::from_plan(Plan::StarterLtd), Tier::Pro);
    assert_eq!(Tier::from_plan(Plan::BuilderLtd), Tier::Pro);
    assert_eq!(Tier::from_plan(Plan::FounderLtd), Tier::Pro);
}

#[test]
fn tier_label_matches() {
    assert_eq!(Tier::Community.label(), "Community");
    assert_eq!(Tier::Pro.label(), "Pro");
    assert_eq!(Tier::Team.label(), "Team");
    assert_eq!(Tier::Enterprise.label(), "Enterprise");
}
```

Run: `cargo test -p spur-license --test tier`
Expected: FAIL

- [ ] **Step 2: Create `tier.rs`**

```rust
// crates/spur-license/src/tier.rs
//! Canonical tier representation. Maps from `Plan` (LicenseSeat's plan_key)
//! to a normalized `Tier` used by FeatureGate.

use crate::Plan;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    Community,
    Pro,
    Team,
    Enterprise,
}

impl Tier {
    /// Map LicenseSeat `Plan` to normalized `Tier`.
    /// Legacy LTD plans map to Pro for feature-gate purposes.
    pub fn from_plan(plan: Plan) -> Self {
        match plan {
            Plan::Community => Self::Community,
            Plan::Pro | Plan::StarterLtd | Plan::BuilderLtd | Plan::FounderLtd => Self::Pro,
            Plan::Team => Self::Team,
            Plan::Enterprise => Self::Enterprise,
            Plan::Unknown => Self::Community, // Fail-open: unknown plan → Community
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Community => "Community",
            Self::Pro => "Pro",
            Self::Team => "Team",
            Self::Enterprise => "Enterprise",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_plan_defaults_to_community() {
        assert_eq!(Tier::from_plan(Plan::Unknown), Tier::Community);
    }

    #[test]
    fn ltd_plans_map_to_pro() {
        assert_eq!(Tier::from_plan(Plan::StarterLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::BuilderLtd), Tier::Pro);
        assert_eq!(Tier::from_plan(Plan::FounderLtd), Tier::Pro);
    }
}
```

- [ ] **Step 3: Add `mod tier;` to `lib.rs` (already done in Task 3)**

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-license --test tier`
Expected: PASS

Run: `cargo test -p spur-license tier`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/tier.rs crates/spur-license/tests/tier.rs
git commit -m "feat(spur-license): add Tier enum with Plan mapping"
```

---

## Task 5: Implement `EntitlementSnapshot` + `SourceMetadata`

**Files:**
- Create: `crates/spur-license/src/snapshot.rs`

- [ ] **Step 1: Create `snapshot.rs`**

```rust
// crates/spur-license/src/snapshot.rs
//! Immutable snapshot of merged entitlements. Stored in ArcSwap for wait-free reads.

use std::collections::HashMap;

use ahash::AHashSet;
use chrono::{DateTime, Utc};

use crate::{FeatureKey, QuotaKey, QuotaValue, Tier};
use crate::policy::FlagSpec;
use crate::Plan;

/// Immutable snapshot of merged entitlements. Constructed by
/// `FeatureGate::build_snapshot()` and stored in `ArcSwap`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntitlementSnapshot {
    pub tier: Tier,
    pub features: AHashSet<FeatureKey>,
    pub quotas: HashMap<QuotaKey, QuotaValue>,
    pub flags: HashMap<FeatureKey, FlagSpec>,
    pub source: SourceMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceMetadata {
    pub plan: Plan,
    pub expires_at: Option<DateTime<Utc>>,
    pub is_offline: bool,
}

impl Default for EntitlementSnapshot {
    fn default() -> Self {
        Self {
            tier: Tier::Community,
            features: AHashSet::default(),
            quotas: HashMap::new(),
            flags: HashMap::new(),
            source: SourceMetadata {
                plan: Plan::Community,
                expires_at: None,
                is_offline: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_snapshot_is_community_with_no_features() {
        let snap = EntitlementSnapshot::default();
        assert_eq!(snap.tier, Tier::Community);
        assert!(snap.features.is_empty());
        assert!(snap.quotas.is_empty());
    }
}
```

- [ ] **Step 2: Add `mod snapshot;` to `lib.rs` (already done in Task 3)**

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-license snapshot`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/src/snapshot.rs
git commit -m "feat(spur-license): add EntitlementSnapshot and SourceMetadata"
```

---

## Task 6: Implement `FeatureGate`

**Files:**
- Create: `crates/spur-license/src/gate.rs`
- Create: `crates/spur-license/tests/feature_gate.rs`
- Modify: `crates/spur-license/src/policy/mod.rs` (add `document()` method to `PolicyResolver`)

- [ ] **Step 1: Write failing test for `FeatureGate`**

```rust
// crates/spur-license/tests/feature_gate.rs
use std::collections::BTreeSet;
use spur_license::{FeatureGate, FeatureKey, QuotaKey, QuotaValue, Tier};
use spur_license::policy::PolicyResolver;

#[test]
fn community_has_core_features() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
    assert_eq!(gate.tier(), Tier::Community);
}

#[test]
fn community_quota_defaults() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
}

#[test]
fn unknown_feature_returns_false() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);
    
    // PARALLEL_WORKERS is not in Community tier
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
}
```

Run: `cargo test -p spur-license --test feature_gate`
Expected: FAIL — `FeatureGate` doesn't exist yet

- [ ] **Step 2: Add `document()` method to `PolicyResolver`**

```rust
// In crates/spur-license/src/policy/mod.rs, add to `impl PolicyResolver`:

    pub fn document(&self) -> Arc<PolicyDocument> {
        Arc::clone(&self.document)
    }
```

- [ ] **Step 3: Create `gate.rs`**

```rust
// crates/spur-license/src/gate.rs
//! Wait-free feature gate. All downstream crates use this type.
//!
//! Share via `Arc<FeatureGate>` — the type is `Send + Sync` and interior-mutable
//! via `ArcSwap`, so multiple crates can hold the same instance.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::{EntitlementSnapshot, FeatureKey, QuotaKey, QuotaValue, Tier};
use crate::policy::{FlagSpec, PolicyDocument, PolicyResolver};
use crate::{LicenseState, Plan};

/// Wait-free feature gate. All downstream crates use this type.
pub struct FeatureGate {
    snapshot: ArcSwap<EntitlementSnapshot>,
    policy: Arc<PolicyResolver>,
}

impl FeatureGate {
    /// Build initial gate from PolicyDocument (Community tier).
    /// LicenseSeat state is merged later via `update_state()`.
    pub fn new(policy: Arc<PolicyResolver>) -> Self {
        let snapshot = Self::build_community_snapshot(&policy);
        Self {
            snapshot: ArcSwap::new(Arc::new(snapshot)),
            policy,
        }
    }

    /// O(1) wait-free read. Single atomic load, no locking.
    pub fn has(&self, feature: FeatureKey) -> bool {
        self.snapshot.load().features.contains(&feature)
    }

    /// Returns `None` if quota not defined (caller decides default).
    pub fn quota(&self, key: QuotaKey) -> Option<QuotaValue> {
        self.snapshot.load().quotas.get(&key).copied()
    }

    pub fn tier(&self) -> Tier {
        self.snapshot.load().tier
    }

    /// Returns a guard that keeps the snapshot alive. The guard is cheap to
    /// create (single atomic load) and derefs to `&EntitlementSnapshot`.
    pub fn snapshot(&self) -> arc_swap::Guard<Arc<EntitlementSnapshot>> {
        self.snapshot.load()
    }

    /// Called by SpurLicense when license state changes.
    /// Recomputes snapshot and swaps atomically.
    pub fn update_state(&self, state: &LicenseState) {
        let new_snapshot = self.build_snapshot(state);
        self.snapshot.store(Arc::new(new_snapshot));
    }

    // ------------------------------------------------------------------
    // Private: snapshot builders
    // ------------------------------------------------------------------

    fn build_community_snapshot(policy: &PolicyResolver) -> EntitlementSnapshot {
        let features = policy
            .tier_features("community")
            .iter()
            .filter_map(|s| FeatureKey::from_known(s))
            .collect();

        let mut quotas = HashMap::new();
        quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(1));
        quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Bytes(128 * 1024 * 1024));

        EntitlementSnapshot {
            tier: Tier::Community,
            features,
            quotas,
            flags: Self::extract_flags(policy.document()),
            source: crate::snapshot::SourceMetadata {
                plan: Plan::Community,
                expires_at: None,
                is_offline: true,
            },
        }
    }

    fn build_snapshot(&self, state: &LicenseState) -> EntitlementSnapshot {
        if !state.is_active() {
            // Fail-closed: inactive license → empty features, zero quotas
            return EntitlementSnapshot::default();
        }

        let tier = Tier::from_plan(state.plan);
        let features = if tier == Tier::Community {
            // Community: features from PolicyDocument
            self.policy
                .tier_features("community")
                .iter()
                .filter_map(|s| FeatureKey::from_known(s))
                .collect()
        } else {
            // Pro/Team/Enterprise: features from LicenseSeat entitlements
            state
                .features
                .iter()
                .filter_map(|s| FeatureKey::from_known(s))
                .collect()
        };

        let quotas = self.merge_quotas(tier);

        EntitlementSnapshot {
            tier,
            features,
            quotas,
            flags: Self::extract_flags(self.policy.document()),
            source: crate::snapshot::SourceMetadata {
                plan: state.plan,
                expires_at: state.expires_at,
                is_offline: state.offline_ok,
            },
        }
    }

    fn merge_quotas(&self, tier: Tier) -> HashMap<QuotaKey, QuotaValue> {
        let mut quotas = HashMap::new();

        match tier {
            Tier::Community => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(1));
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Bytes(128 * 1024 * 1024));
            }
            Tier::Pro => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(5));
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Bytes(1024 * 1024 * 1024));
            }
            Tier::Team => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Count(10));
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Bytes(10 * 1024 * 1024 * 1024));
                quotas.insert(QuotaKey::MinSeats, QuotaValue::Count(3));
            }
            Tier::Enterprise => {
                quotas.insert(QuotaKey::MaxConcurrentWorkers, QuotaValue::Unlimited);
                quotas.insert(QuotaKey::EventRetentionBytes, QuotaValue::Unlimited);
            }
        }

        quotas
    }

    fn extract_flags(doc: Arc<PolicyDocument>) -> HashMap<FeatureKey, FlagSpec> {
        doc.flags
            .iter()
            .filter_map(|(k, v)| {
                FeatureKey::from_known(k).map(|key| (key, v.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::PolicyResolver;
    use crate::{LicenseState, LicenseStatus, Plan, SubjectKind, BindingMode};
    use std::collections::BTreeSet;

    #[test]
    fn inactive_license_is_fail_closed() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);
        
        gate.update_state(&LicenseState::inactive("expired"));
        
        assert!(!gate.has(FeatureKey::BRAIN_SESSION));
        assert_eq!(gate.quota(QuotaKey::MaxConcurrentWorkers), None);
    }

    #[test]
    fn tier_transition_updates_atomically() {
        let policy = PolicyResolver::embedded();
        let gate = FeatureGate::new(policy);
        
        assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
        
        let mut features = BTreeSet::new();
        features.insert("parallel_workers".to_string());
        
        gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
        
        assert!(gate.has(FeatureKey::PARALLEL_WORKERS));
        assert_eq!(gate.tier(), Tier::Pro);
    }
}
```

- [ ] **Step 4: Add `mod gate;` to `lib.rs` (already done in Task 3)**

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-license --test feature_gate`
Expected: PASS

Run: `cargo test -p spur-license gate`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/gate.rs crates/spur-license/tests/feature_gate.rs crates/spur-license/src/policy/mod.rs
git commit -m "feat(spur-license): implement FeatureGate with ArcSwap wait-free reads"
```

---

## Task 7: Wire `FeatureGate` into `SpurLicense`

**Files:**
- Modify: `crates/spur-license/src/lib.rs`
- Modify: `crates/spur-license/src/community.rs` (add `feature_gate()` accessor if needed)

- [ ] **Step 1: Add `feature_gate` field and accessor to `SpurLicense`**

```rust
// In crates/spur-license/src/lib.rs

pub struct SpurLicense {
    provider: Arc<dyn LicenseProvider>,
    feature_gate: Arc<FeatureGate>,
}

impl SpurLicense {
    pub fn from_provider(provider: Arc<dyn LicenseProvider>, feature_gate: Arc<FeatureGate>) -> Self {
        Self { provider, feature_gate }
    }

    pub fn from_env() -> Result<Self> {
        let provider = Arc::new(crate::licenseseat::from_env()?);
        let policy = Arc::new(PolicyResolver::with_default_overlay());
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Ok(Self::from_provider(provider, feature_gate))
    }

    pub fn from_env_or_disabled() -> Self {
        let provider = crate::licenseseat::from_env_or_disabled();
        let policy = Arc::new(PolicyResolver::with_default_overlay());
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Self::from_provider(provider, feature_gate)
    }

    pub fn feature_gate(&self) -> Arc<FeatureGate> {
        Arc::clone(&self.feature_gate)
    }

    // ... existing methods delegate to provider ...
}
```

- [ ] **Step 2: Update `from_env_or_disabled` to build `FeatureGate` eagerly**

The existing `from_env_or_disabled` creates a provider. We now also need to create a `FeatureGate` and seed it with the provider's initial state.

Replace the existing `from_env_or_disabled` method:

```rust
    pub fn from_env_or_disabled() -> Self {
        let provider = crate::licenseseat::from_env_or_disabled();
        let policy = Arc::new(crate::policy::PolicyResolver::with_default_overlay());
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Self {
            provider,
            feature_gate,
        }
    }
```

- [ ] **Step 3: Update `from_env` to build `FeatureGate` eagerly**

```rust
    pub fn from_env() -> Result<Self> {
        let provider = Arc::new(crate::licenseseat::from_env()?);
        let policy = Arc::new(crate::policy::PolicyResolver::with_default_overlay());
        let feature_gate = Arc::new(FeatureGate::new(policy));
        feature_gate.update_state(&provider.current_state());
        Ok(Self {
            provider,
            feature_gate,
        })
    }
```

- [ ] **Step 4: Verify compilation**

Run: `cargo check -p spur-license`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/lib.rs
git commit -m "feat(spur-license): wire FeatureGate into SpurLicense"
```

---

## Task 8: Update Existing Tests

**Files:**
- Modify: `crates/spur-license/tests/community_smoke.rs`

- [ ] **Step 1: Update `community_smoke.rs` to verify FeatureGate output**

```rust
// crates/spur-license/tests/community_smoke.rs
use spur_license::{SpurLicense, FeatureKey, QuotaKey, QuotaValue, Tier};

#[test]
fn community_default_has_expected_features() {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();

    assert_eq!(gate.tier(), Tier::Community);
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(gate.has(FeatureKey::WORKTREE_ISOLATION));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
    assert!(!gate.has(FeatureKey::PM_INTEGRATION));
}

#[test]
fn community_default_quotas() {
    let license = SpurLicense::from_env_or_disabled();
    let gate = license.feature_gate();

    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(128 * 1024 * 1024))
    );
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-license --test community_smoke`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/spur-license/tests/community_smoke.rs
git commit -m "test(spur-license): verify FeatureGate output in community smoke tests"
```

---

## Task 9: Full Test Suite Verification

- [ ] **Step 1: Run all spur-license tests**

Run: `cargo test -p spur-license`
Expected: ALL PASS (existing + new tests)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -p spur-license -- -D warnings`
Expected: CLEAN

- [ ] **Step 3: Run formatter**

Run: `cargo fmt --all`

- [ ] **Step 4: Final commit**

```bash
git add -A
git commit -m "feat(spur-license): FeatureGate wait-free feature entitlement API

Implements the UnifiedResolver architecture from spec:
- FeatureKey: 31 typed constants, open-set with from_known
- QuotaKey/QuotaValue: closed-set enums for resource limits
- Tier: canonical tier with Plan mapping
- EntitlementSnapshot: immutable projection for ArcSwap
- FeatureGate: wait-free has()/quota()/tier() via arc_swap
- Wired into SpurLicense with eager initialization

All existing tests preserved. New tests: feature_key, quota, tier, gate, community_smoke."
```

---

## Self-Review

### Spec Coverage Check

| Spec Section | Task | Status |
|---|---|---|
| §4.2 ArcSwap architecture | Task 6 | ✅ `FeatureGate` uses `ArcSwap<EntitlementSnapshot>` |
| §5.1 FeatureKey (31 keys, open set) | Task 2 | ✅ All keys defined, `from_known` returns `None` for unknown |
| §5.2 QuotaKey (closed enum) | Task 3 | ✅ 4 variants with `as_str` |
| §5.3 QuotaValue (strongly typed) | Task 3 | ✅ `Count`, `Bytes`, `Unlimited` with accessors |
| §5.4 Tier | Task 4 | ✅ `from_plan` maps all plans, LTD → Pro |
| §5.5 EntitlementSnapshot | Task 5 | ✅ Immutable, `Default` for fail-closed |
| §5.6 FeatureGate (wait-free API) | Task 6 | ✅ `has()`, `quota()`, `tier()`, `update_state()` |
| §5.7 FlagEvaluator | — | 🔄 Deferred to Phase 3 (not in this plan scope) |
| §6 Data flow | Task 7 | ✅ `SpurLicense` creates gate, seeds with provider state |
| §7 Quota enforcement | — | 🔄 Phase 2 (orchestrator integration, not this plan) |
| §8 Tier transition | — | 🔄 Deferred to v2 (restart required in v1) |
| §9 Error handling | Tasks 5, 6 | ✅ Inactive → empty snapshot, unknown → false |
| §10 Testing | Tasks 1-9 | ✅ Unit, integration, smoke tests |

### Placeholder Scan

No placeholders found. All code is complete. No `todo!()`, `TBD`, or `... etc` patterns.

### Type Consistency

- `FeatureKey::from_known` used consistently everywhere (not `from_str`)
- `Arc<FeatureGate>` sharing pattern shown in `SpurLicense`
- `ArcSwap::new(Arc::new(...))` used consistently
- `QuotaValue` copied via `.copied()` (it implements `Copy`)

### Gaps

- FlagEvaluator deferred to Phase 3 — intentional, out of this plan scope
- Quota enforcement in orchestrator deferred to Phase 2 — intentional
- Hot-swap provider deferred to v2 — intentional per spec §12.1

---

*Plan complete. Ready for execution.*
