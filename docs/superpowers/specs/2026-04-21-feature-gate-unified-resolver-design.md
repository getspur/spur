# Feature Gate UnifiedResolver Design

> Technical specification for SPUR's feature-based tier licensing system.
> Design methodology: MCTS simulation with first-principles reasoning across 7
> architectural sections. Approved 2026-04-21.

## 1. Problem Statement

SPUR has two sources of licensing truth:

1. **Embedded `PolicyDocument`** (`spur-license/resources/default_policy.json`): Defines
   Community tier entitlements, runtime feature flags (G2), and quotas. Ed25519-signed,
   verified at compile time. Always available offline.

2. **LicenseSeat server response** (`licenseseat` SDK v0.5.3): Defines Pro/Team/Enterprise
   entitlements via `active_entitlements: Vec<Entitlement>`. Requires network + valid
   license key. Authoritative when active.

Today, downstream crates (`spur-core`, `spur-tui`, `spur-pm`) must understand both
sources. This creates coupling, inconsistency, and race conditions. We need a single,
tier-agnostic API that merges both sources into one canonical view.

## 2. Goals

| Goal | Metric |
|---|---|
| Single API for all feature/entitlement checks | `FeatureGate::has(feature)` used by all crates |
| Wait-free reads on hot paths | `has()` and `quota()` are O(1), no locking, no allocation |
| Tier transition without restart (where feasible) | Community → Pro activation updates gate atomically |
| Fail-closed for security | Unknown feature → `false`; missing quota → `0` |
| Fail-open for availability | LicenseSeat down → Community tier (not crash) |
| Zero breaking changes to existing `LicenseProvider` trait | New types are additive |

## 3. Non-Goals

- Hot-swap provider in `spur watch` without restart (deferred to v2 — see §9.1)
- Real-time quota enforcement across distributed team members (enterprise, out of scope)
- Custom policy authoring UI (admin panel is LicenseSeat's domain)
- Per-feature kill switch that disables already-running delegations (graceful degradation only)

## 4. Architecture

### 4.1 High-Level Diagram

```text
┌─────────────────────────────────────────────────────────────────────┐
│                        Downstream Crates                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐           │
│  │spur-core │  │ spur-tui │  │ spur-pm  │  │ spur-cli │           │
│  │Orchestr. │  │  App     │  │ Adapters │  │  auth    │           │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘           │
│       │             │             │             │                   │
│       └─────────────┴──────┬──────┴─────────────┘                   │
│                            │                                        │
│                    ┌───────▼────────┐                               │
│                    │  FeatureGate   │                               │
│                    │  (wait-free)   │                               │
│                    └───────┬────────┘                               │
└────────────────────────────┼────────────────────────────────────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
       ┌──────▼──────┐ ┌────▼─────┐ ┌─────▼──────┐
       │  ArcSwap    │ │ ArcSwap  │ │ ArcSwap    │
       │  <Snapshot> │ │<Snapshot>│ │<Snapshot>  │
       └──────┬──────┘ └────┬─────┘ └─────┬──────┘
              │             │             │
       ┌──────▼──────┐ ┌────▼─────┐ ┌─────▼──────┐
       │  PolicyDoc  │ │ License  │ │  Computed  │
       │  (embedded) │ │  Seat    │ │   Quotas   │
       └─────────────┘ └──────────┘ └────────────┘
```

### 4.2 Key Decision: ArcSwap over RwLock

Read/write ratio for feature gates is approximately **10,000:1** (hot-path reads
during delegation dispatch, TUI rendering, event sink writes; writes only on
license activation, validation, or policy overlay update).

| Mechanism | Read Cost | Write Cost | Lock Contention | Async-Safe |
|---|---|---|---|---|
| `std::sync::RwLock` | Lock acquire + release | Lock + recompute | High in hot path | Risk: guard held across await = deadlock |
| `arc_swap::ArcSwap` | 1 atomic load (wait-free) | New `Arc` + atomic swap | None | Yes: no locks, no lifetimes |
| Direct delegation | Vtable dispatch + SDK internal lock | N/A | Medium: hits SDK lock every time | Yes, but slower |

**Winner: `arc_swap::ArcSwap<EntitlementSnapshot>`**

Immutable snapshot semantics eliminate an entire class of race conditions. The
borrow checker becomes an ally, not an obstacle.

## 5. Component Design

### 5.1 `FeatureKey` — Open-Set Newtype

Features are an **open set** from LicenseSeat's perspective (admin can create new
entitlements without code changes). A closed enum would drop unknown keys silently.

```rust
/// Typed feature key. Open set: server may send keys we don't know about.
/// Known keys are `pub const` for IDE autocompletion and typo safety.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    // G1: Entitlements (from PolicyDocument OR LicenseSeat)
    pub const BRAIN_SESSION: Self = Self("brain_session");
    pub const SINGLE_WORKER: Self = Self("single_worker");
    pub const PARALLEL_WORKERS: Self = Self("parallel_workers");
    pub const WORKTREE_ISOLATION: Self = Self("worktree_isolation");
    pub const MANUAL_REVIEW: Self = Self("manual_review");
    pub const AUTO_REVIEW_POLICIES: Self = Self("auto_review_policies");
    pub const SHARED_REVIEW_QUEUE: Self = Self("shared_review_queue");
    pub const EVENT_PERSISTENCE: Self = Self("event_persistence");
    pub const EXTENDED_RETENTION: Self = Self("extended_retention");
    pub const SESSION_RESUME: Self = Self("session_resume");
    pub const BASIC_LINEAGE: Self = Self("basic_lineage");
    pub const SHARED_LINEAGE: Self = Self("shared_lineage");
    pub const TUI_DASHBOARD: Self = Self("tui_dashboard");
    pub const TUI_SESSION_DETAIL: Self = Self("tui_session_detail");
    pub const BASIC_COST_DISPLAY: Self = Self("basic_cost_display");
    pub const ADVANCED_COST_ANALYTICS: Self = Self("advanced_cost_analytics");
    pub const TEAM_COST_DASHBOARD: Self = Self("team_cost_dashboard");
    pub const PM_INTEGRATION: Self = Self("pm_integration");
    pub const PM_WEBHOOKS: Self = Self("pm_webhooks");
    pub const BASIC_NOTIFICATIONS: Self = Self("basic_notifications");
    pub const CUSTOM_NOTIFICATIONS: Self = Self("custom_notifications");
    pub const LOCAL_CONFIG: Self = Self("local_config");
    pub const CENTRALIZED_CONFIG: Self = Self("centralized_config");
    pub const CUSTOM_POLICIES: Self = Self("custom_policies");
    pub const RBAC: Self = Self("rbac");
    pub const AUDIT_LOGS: Self = Self("audit_logs");
    pub const SSO_SAML: Self = Self("sso_saml");
    pub const CUSTOM_MCP_TOOLS: Self = Self("custom_mcp_tools");
    pub const DEDICATED_SUPPORT: Self = Self("dedicated_support");
    pub const SLA_GUARANTEE: Self = Self("sla_guarantee");

    // G2: Runtime flags (always from PolicyDocument)
    pub const KILL_ADVANCED_PLANNER: Self = Self("kill_advanced_planner");
    pub const ENABLE_BROWSER_TOOL: Self = Self("enable_browser_tool");
    pub const ENABLE_COMPACTION_V2: Self = Self("enable_compaction_v2");
    pub const ENABLE_TELEMETRY: Self = Self("enable_telemetry");

    pub const fn as_str(&self) -> &'static str {
        self.0
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
///
/// `FeatureGate::has_unknown()` checks these separately; unknown keys are
/// ignored (fail-closed) unless explicitly handled.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnknownFeatureKey(Arc<str>);
```

### 5.2 `QuotaKey` — Closed-Set Enum

Quotas are a **closed set** defined entirely by SPUR. LicenseSeat has no concept
of `max_concurrent_workers`. An enum gives us exhaustiveness checking.

```rust
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
```

### 5.3 `QuotaValue` — Strongly Typed

```rust
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
```

### 5.4 `Tier` — Canonical Tier Representation

```rust
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    Community,
    Pro,
    Team,
    Enterprise,
}

impl Tier {
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
```

### 5.5 `EntitlementSnapshot` — Immutable Projection

The single source of truth for all read operations. Once constructed, never
mutated. Replaced atomically via `ArcSwap`.

```rust
/// Immutable snapshot of merged entitlements. Constructed by
/// `UnifiedResolver::build_snapshot()` and stored in `ArcSwap`.
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
```

### 5.6 `FeatureGate` — Public API

```rust
/// Wait-free feature gate. All downstream crates use this type.
///
/// Share via `Arc<FeatureGate>` — the type is `Send + Sync` and interior-mutable
/// via `ArcSwap`, so multiple crates can hold the same instance.
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
            flags: Self::extract_flags(policy.document()), // Always from policy
            source: SourceMetadata {
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
                .filter_map(|s| FeatureKey::from_str(s))
                .collect()
        } else {
            // Pro/Team/Enterprise: features from LicenseSeat entitlements
            state
                .features
                .iter()
                .filter_map(|s| FeatureKey::from_known(s))
                .collect()
        };

        let quotas = self.merge_quotas(tier, state);

        EntitlementSnapshot {
            tier,
            features,
            quotas,
            flags: Self::extract_flags(self.policy.document()), // Always from policy
            source: SourceMetadata {
                plan: state.plan,
                expires_at: state.expires_at,
                is_offline: state.offline_ok,
            },
        }
    }

    fn merge_quotas(&self, tier: Tier, _state: &LicenseState) -> HashMap<QuotaKey, QuotaValue> {
        let mut quotas = HashMap::new();

        // Base quotas from PolicyDocument
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

impl FeatureKey {
    /// Parse a string into a known FeatureKey. Returns `None` for unknown keys.
    ///
    /// NOTE: Not named `from_str` to avoid conflicting with `std::str::FromStr`.
    pub const fn from_known(s: &str) -> Option<Self> {
        // Match against all known consts. In practice, a static phf map would
        // be faster, but for <50 keys a match is fine and const-friendly.
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
```

### 5.7 `FlagEvaluator` — G2 Flag Evaluation

Runtime flags (G2) are always from `PolicyDocument`, never from LicenseSeat.
They support rollout percentages and tier filtering.

```rust
/// Stable per-install identifier. Generated once on first run, persisted in
/// ~/.spur/install_id. Used for deterministic rollout hashing.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InstallId(uuid::Uuid);

impl InstallId {
    pub fn load_or_create() -> Self {
        let path = directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().join(".spur").join("install_id"));
        
        if let Some(ref p) = path {
            if let Ok(s) = std::fs::read_to_string(p) {
                if let Ok(uuid) = s.trim().parse::<uuid::Uuid>() {
                    return Self(uuid);
                }
            }
        }
        
        let new_id = Self(uuid::Uuid::new_v4());
        if let Some(ref p) = path {
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(p, new_id.0.to_string());
        }
        new_id
    }
}

pub struct FlagEvaluator {
    install_id: InstallId,
}

impl FlagEvaluator {
    pub fn new(install_id: InstallId) -> Self {
        Self { install_id }
    }

    /// Evaluate whether a flag is enabled for the given tier.
    /// Deterministic: same (install_id, flag_key) always yields same result.
    pub fn evaluate(&self, key: FeatureKey, flag: &FlagSpec, tier: Tier) -> bool {
        // 1. Kill switch
        if !flag.enabled {
            return false;
        }

        // 2. Tier filter
        if let Some(ref tiers) = flag.tier_filter {
            let tier_str = tier.label().to_lowercase();
            if !tiers.iter().any(|t| t == &tier_str) {
                return false;
            }
        }

        // 3. Rollout percentage (deterministic hash)
        if let Some(pct) = flag.rollout_percent {
            let hash = seahash::hash(
                format!("{}:{}", self.install_id.0, key.as_str()).as_bytes()
            );
            let normalized = (hash % 100) as f32;
            return normalized < pct;
        }

        true
    }
}
```

## 6. Data Flow

### 6.1 Initialization Flow

```rust
// In spur-cli/src/main.rs or spur-core bootstrap
let policy = PolicyResolver::with_default_overlay();
let feature_gate = FeatureGate::new(Arc::new(policy));

// If LicenseSeat env vars present, create provider and merge state
let license = SpurLicense::from_env_or_disabled();
feature_gate.update_state(&license.current_state());

// Pass feature_gate to all downstream components
let orchestrator = Orchestrator::new(Arc::new(feature_gate));
```

### 6.2 Update Flow (State Change)

```text
LicenseSeatProvider emits LicenseEvent
        │
        ▼
SpurLicense receives event via subscribe()
        │
        ▼
SpurLicense calls feature_gate.update_state(&provider.current_state())
        │
        ▼
FeatureGate recomputes EntitlementSnapshot
        │
        ▼
ArcSwap::store(new_snapshot) — atomic, wait-free
        │
        ▼
All subsequent has()/quota() calls see new state immediately
```

### 6.3 Read Flow (Hot Path)

```rust
// In spur-core/src/orchestrator.rs (delegation dispatch)
let max_workers = self.feature_gate
    .quota(QuotaKey::MaxConcurrentWorkers)
    .and_then(|v| v.as_count())
    .unwrap_or(1) as usize;

// In spur-tui/src/app.rs (rendering)
if self.feature_gate.has(FeatureKey::ADVANCED_COST_ANALYTICS) {
    self.render_cost_breakdown();
}
```

Both calls are **wait-free**: a single atomic load of an `Arc` pointer, then
field access on the immutable snapshot.

## 7. Quota Enforcement Integration

### 7.1 Enforcement Points

| Quota | Enforcement Point | Current Code |
|---|---|---|
| `max_concurrent_workers` | Semaphore in `Orchestrator` | `orchestrator.rs: delegation_semaphore` |
| `event_retention_bytes` | Rotation in `EventSink` | `event_sink.rs: rotate_if_needed()` |
| `max_team_members` | Team config validation | `spur-pm: PmAdapter::validate_team_config()` |
| `min_seats` | LicenseSeat checkout | Server-side on activation |

### 7.2 Dynamic Semaphore Pattern

When quota increases (Community → Pro), the old semaphore's permits remain valid
for in-flight delegations. New acquisitions use the new semaphore:

```rust
impl Orchestrator {
    pub fn on_license_changed(&mut self) {
        let max = self.feature_gate
            .quota(QuotaKey::MaxConcurrentWorkers)
            .and_then(|v| v.as_count())
            .unwrap_or(1) as usize;

        // Replace semaphore. Old permits remain valid.
        self.delegation_semaphore = Arc::new(Semaphore::new(max));
    }
}
```

When quota decreases (Pro expires → Community), existing delegations keep their
permits. New delegations are blocked until the count drops below the new limit.
This is the correct semantic: we never kill running work.

## 8. Tier Transition

### 8.1 CLI Path (`spur auth login`)

1. User runs `spur auth login --key <KEY>`
2. CLI creates `SpurLicense::from_env_or_disabled()` (CommunityProvider initially)
3. Calls `license.activate(key)` — CommunityProvider returns `Err(NotConfigured)`
4. CLI detects `NotConfigured`, checks for `SPUR_LICENSESEAT_API_KEY` env var
5. If missing, prompts user or suggests `export SPUR_LICENSESEAT_API_KEY=...`
6. Once env is set, creates new `LicenseSeatProvider`, calls `activate(key)`
7. On success: `feature_gate.update_state(&state)`
8. Shows success message: "Activated Pro. Run `spur watch` to use Pro features."

**Restart required for `spur watch` in v1** (see §9.1 for deferred hot-swap).

### 8.2 State Diagram

```text
                    ┌─────────────────┐
                    │   No License    │
                    │   (Community)   │
                    └────────┬────────┘
                             │
              spur auth login│
                             ▼
                    ┌─────────────────┐
         ┌─────────│  Activating...  │◄────────┐
         │         └────────┬────────┘         │
         │ activation fails │                  │
         │                  │ activation ok    │
         ▼                  ▼                  │
┌─────────────────┐  ┌─────────────────┐       │
│   Community     │  │  Pro/Team/Ent   │───────┘
│   (unchanged)   │  │   (active)      │ deactivate
└─────────────────┘  └─────────────────┘
```

## 9. Error Handling

| Scenario | Behavior |
|---|---|
| PolicyDocument signature invalid at compile | `build.rs` panics. Binary cannot be built with bad policy. |
| PolicyDocument signature invalid at runtime | Impossible if build.rs passed. If tampered: log error, fall back to hardcoded minimal Community set. |
| LicenseSeat server down | LicenseSeat SDK falls back to offline validation. If offline fails: `LicenseStatus::Degraded`. `FeatureGate` uses cached features if `offline_ok`. If not, falls back to Community. |
| Unknown feature key queried | `FeatureGate::has()` returns `false` (fail-closed). |
| Missing quota key | `FeatureGate::quota()` returns `None`. Caller provides default. |
| License expired | `LicenseStatus::Invalid`. `FeatureGate` returns empty snapshot (all features denied). TUI shows "License expired — reverting to Community" banner. |
| Partial env vars (only API key, no product slug) | `ConfigError`. Routed to `DisabledProvider` (not Community). User must fix config. |

## 10. Testing Strategy

### 10.1 Unit Tests (in `spur-license`)

```rust
#[test]
fn community_has_core_features() {
    let gate = FeatureGate::for_test(Tier::Community, &[]);
    assert!(gate.has(FeatureKey::BRAIN_SESSION));
    assert!(gate.has(FeatureKey::SINGLE_WORKER));
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));
}

#[test]
fn pro_has_parallel_workers() {
    let gate = FeatureGate::for_test(
        Tier::Pro,
        &["parallel_workers", "auto_review_policies"]
    );
    assert!(gate.has(FeatureKey::PARALLEL_WORKERS));
    assert!(!gate.has(FeatureKey::PM_INTEGRATION)); // Team-only
}

#[test]
fn inactive_license_is_fail_closed() {
    let gate = FeatureGate::for_test_inactive();
    assert!(!gate.has(FeatureKey::BRAIN_SESSION));
    assert_eq!(gate.quota(QuotaKey::MaxConcurrentWorkers), None);
}

#[test]
fn tier_transition_updates_atomically() {
    let gate = FeatureGate::for_test(Tier::Community, &[]);
    assert!(!gate.has(FeatureKey::PARALLEL_WORKERS));

    gate.update_state(&LicenseState::active_validated(
        Plan::Pro,
        ["parallel_workers"].iter().map(|s| s.to_string()).collect()
    ));

    assert!(gate.has(FeatureKey::PARALLEL_WORKERS));
}
```

### 10.2 Property Tests (proptest)

```rust
proptest! {
    #[test]
    fn feature_check_never_panics(
        tier in prop::sample::select(&[Tier::Community, Tier::Pro, Tier::Team, Tier::Enterprise]),
        feature_key in "[a-z_]{1,40}"
    ) {
        let gate = FeatureGate::for_test(tier, &[]);
        let key = FeatureKey::from_known(&feature_key)
            .unwrap_or(FeatureKey::BRAIN_SESSION); // Any valid key if unknown
        let _ = gate.has(key); // Must not panic
    }

    #[test]
    fn quota_is_non_negative(
        tier in prop::sample::select(&[Tier::Community, Tier::Pro, Tier::Team, Tier::Enterprise])
    ) {
        let gate = FeatureGate::for_test(tier, &[]);
        if let Some(QuotaValue::Count(n)) = gate.quota(QuotaKey::MaxConcurrentWorkers) {
            prop_assert!(n > 0);
        }
    }
}
```

### 10.3 Integration Tests

- `community_smoke.rs` (exists): Verify CommunityProvider returns correct features
- `pro_activation.rs` (new): Test `activate()` with demo key → verify Pro features
- `tier_transition.rs` (new): Community → Pro → deactivate → Community
- `quota_enforcement.rs` (new): Test semaphore limits and retention rotation
- `flag_evaluator.rs` (new): Deterministic rollout, tier filtering, kill switch

## 11. Dependencies

| Crate | Version | Purpose |
|---|---|---|
| `arc-swap` | ^1.7 | Wait-free atomic pointer swap for `FeatureGate` |
| `ahash` | ^0.8 | Faster `HashSet`/`HashMap` for hot-path feature lookups |
| `seahash` | ^4 | Deterministic hashing for rollout percentage (no crypto needed) |
| `uuid` | workspace | `InstallId` generation |

Add to `crates/spur-license/Cargo.toml`:
```toml
[dependencies]
arc-swap = "1.7"
ahash = "0.8"
seahash = "4"
```

## 12. Deferred Work

### 12.1 Hot-Swap Provider in `spur watch` (v2)

In v1, activation requires restarting `spur watch`. In v2, we will:

1. Add `shutdown()` method to `LicenseSeatProvider` (or use `tokio_util::TaskTracker`)
2. Use `ArcSwap<dyn LicenseProvider>` in `SpurLicense` for atomic provider swap
3. Signal `Orchestrator` to reconfigure (new semaphore, new feature gate)
4. Migrate in-flight delegations without killing them

### 12.2 Day-1 FF Capability (Phase 2 from onboarding spec)

`FlagEvaluator`, `InstallId`, `spur flags list` CLI command, and
`feature_enabled` helper. Deferred because it requires:
- Stable `InstallId` persistence
- CLI flag introspection commands
- TUI flag status display

### 12.3 Option A Baked Credentials

`CommunityProviderWithUpgrade` that delegates `activate()` to a baked-in
`LicenseSeatProvider`. Requires `SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY`
env var in CI. Deferred until CI credentials are available.

## 13. Migration Path

### Phase 1: `FeatureGate` + `FeatureKey` (this spec)
- Add `arc-swap`, `ahash`, `seahash` to `spur-license/Cargo.toml`
- Implement `FeatureKey`, `QuotaKey`, `QuotaValue`, `Tier`
- Implement `EntitlementSnapshot`, `FeatureGate`
- Update `SpurLicense` to own `FeatureGate`
- Wire `FeatureGate` into `Orchestrator` (quota reads only)

### Phase 2: Quota Enforcement
- Replace static semaphore with dynamic `FeatureGate` quota read
- Add retention rotation based on `EventRetentionBytes` quota
- Add PM adapter gating

### Phase 3: Flag System
- Implement `InstallId` persistence
- Implement `FlagEvaluator`
- Add `spur flags list` CLI command
- Add TUI flag status panel

### Phase 4: PolicyDocument Update
- Update `default_policy.json` with full 31-feature schema
- Re-sign with `spur-policy-2026-04` key
- Update `community-tier.md` docs

## 14. Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `arc-swap` adds complexity vs `RwLock` | Low | Low | Well-established crate (500k+ downloads), simple API |
| LicenseSeat SDK changes entitlement model | Medium | High | `FeatureKey` is open-set; unknown keys are silently ignored. We adapt by adding new consts. |
| Quota enforcement performance | Low | Medium | Feature gate reads are O(1) atomic loads. Enforcement logic (semaphore) is already in hot path. |
| Community tier too generous | Low | High | MCTS analysis confirmed core value proposition must be free. Conversion is driven by scale friction, not feature absence. |

## 15. Appendix: Feature-to-Crate Mapping

| FeatureKey | Crate | Enforcement Point |
|---|---|---|
| `brain_session` | `spur-core` | `BrainSessionManager::spawn()` |
| `single_worker` | `spur-core` | `Orchestrator::dispatch_delegation()` (always allowed) |
| `parallel_workers` | `spur-core` | `Orchestrator::delegation_semaphore` |
| `worktree_isolation` | `spur-worktree` | `WorktreeManager::create()` (always allowed) |
| `manual_review` | `spur-core` | `ReviewSink::request_review()` (always allowed) |
| `auto_review_policies` | `spur-core` | `ReviewSink::auto_approve()` |
| `event_persistence` | `spur-core` | `EventSink::write()` (always allowed) |
| `extended_retention` | `spur-core` | `EventSink::rotate_if_needed()` |
| `session_resume` | `spur-core` | `Orchestrator::resume_session()` |
| `basic_lineage` | `spur-tui` | `LineageProjection::render()` (always allowed) |
| `shared_lineage` | `spur-tui` | `LineageProjection::team_view()` |
| `tui_dashboard` | `spur-tui` | `DashboardView::render()` (always allowed) |
| `tui_session_detail` | `spur-tui` | `SessionDetailView::render()` |
| `basic_cost_display` | `spur-tui` | `CostWidget::render()` (always allowed) |
| `advanced_cost_analytics` | `spur-cost` | `CostTracker::export_breakdown()` |
| `team_cost_dashboard` | `spur-tui` | `TeamCostView::render()` |
| `pm_integration` | `spur-pm` | `PmAdapter::configure()` |
| `pm_webhooks` | `spur-pm` | `WebhookHandler::register()` |
| `basic_notifications` | `spur-core` | `NotificationPump::send()` (always allowed) |
| `custom_notifications` | `spur-core` | `NotificationPump::configure_webhook()` |
| `local_config` | `spur-cli` | `Config::load_local()` (always allowed) |
| `centralized_config` | `spur-cli` | `Config::load_team()` |
| `custom_policies` | `spur-license` | `PolicyResolver::with_overlay_path()` |
| `rbac` | `spur-core` | `McpServer::authorize_tool_call()` |
| `audit_logs` | `spur-core` | `EventSink::export_audit()` |
| `sso_saml` | `spur-cli` | `AuthCommand::sso_login()` |
| `custom_mcp_tools` | `spur-mcp` | `McpServer::register_custom_tool()` |
| `dedicated_support` | — | Documentation/SLA only (no code gate) |
| `sla_guarantee` | — | Documentation/SLA only (no code gate) |

---

*Approved: 2026-04-21*
*Next step: Implementation plan via `writing-plans` skill*
