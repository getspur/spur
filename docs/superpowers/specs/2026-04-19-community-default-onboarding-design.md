# SPUR Community-Default Onboarding — Design Spec

> **Posture:** sibling to the in-flight licensing hardening spec ([`2026-04-19-licensing-hardening-design.md`](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-19-licensing-hardening-design.md:1)). Hardening is a **verify-then-harden** effort that *closes* scope around the existing single-provider seam. This spec is a **smallest-viable onboarding fix** that *adds* a Community-tier provider without touching any of the primitives the hardening spec is freezing. The two specs land in parallel; their files do not overlap except for a 5-line change in [`crates/spur-license/src/licenseseat.rs:31-46`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:31).

**Source of truth for licensing primitives:** [`crates/spur-license/src/lib.rs`](/Volumes/Projects/spur/crates/spur-license/src/lib.rs:1), [`crates/spur-license/src/provider.rs`](/Volumes/Projects/spur/crates/spur-license/src/provider.rs:1), [`crates/spur-license/src/licenseseat.rs`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:1).

---

## Goals

- Replace the silent `ConfigError` dead-end on a fresh install with a working **Community** tier so first-run users get a tool that does something useful before they're asked to authenticate.
- Collapse the activation flow from three distinct verbs (set env vars + `spur auth login` + `spur watch`) to **one** verb (`spur watch`, with an inline paste prompt on first run).
- Establish a **typed, signed, embedded `PolicyDocument`** as the source of truth for what each tier (`community`, `pro`, `team`, `enterprise`) includes, so the Community feature list can change per release without touching call sites.
- **Establish Day-1 runtime feature-flag (FF) capability** — kill switches, gradual rollouts, tier targeting — so risky autonomous-agent rollouts can be safely managed without shipping a follow-up release. Implemented as a **custom local evaluator** over the same signed `PolicyDocument`, NOT as an OpenFeature/flagd adoption.
- Give prospective Pro evaluators a frictionless taste path via a **public, rate-limited, server-issued demo key** — no per-user trial enrollment, no email collection, no anti-abuse infrastructure.
- Replace the verbose 5-line `spur auth status` output with a single-line summary appropriate for first-time users; preserve the `--format json` schema unchanged.

## Non-goals

- 7-day per-user trial. Deferred with explicit revisit trigger (see Deferred section). With Day-1 flags now in scope, trial collapses to a flag config (~1 day) when revisited, not a code change.
- Remote signed policy refresh from a CDN. Deferred (V2 in the V1/V2/V3 roadmap below). Local signed overlay (`~/.spur/policy-overlay.json`) IS in scope as the V1 hot-swap mechanism for both entitlements and flags.
- Quota / rate-limit primitive (counters, windows). Deferred (V3).
- **OpenFeature / Flipt / Unleash adoption.** Day-1 FF capability is in scope (above), but via a custom local evaluator. The standard's overhead (pre-1.0 SDK churn, JsonLogic review pain, 10–15 transitive crates, two artifacts to sign) exceeds today's value of vendor neutrality. Migration to OpenFeature later is bounded: implement a `FeatureProvider` over our local evaluator (~200 LoC) when ecosystem benefits actually matter (e.g., when telemetry lands and we want PostHog experimentation).
- Conversion telemetry. Deferred to its own spec; opt-in design.
- Multi-provider rollout (self-hosted, enterprise tenants). Already deferred by the hardening spec.
- Typed-state-machine refactor of `LicenseState`. Already deferred by the hardening spec.

---

## Architecture (with real-code mapping)

### Diagram 1 — Component layers (deltas highlighted)

```mermaid
flowchart TB
  subgraph spur_license["spur-license crate"]
    SL["SpurLicense<br/>(facade, Clone — unchanged)"]
    LP["LicenseProvider trait<br/>(unchanged)"]

    subgraph existing_providers["existing providers"]
      LSP["LicenseSeatProvider<br/>(real adapter)"]
      DP["DisabledProvider<br/>(reused for partial-env case only)"]
    end

    subgraph new_providers["NEW"]
      CP["CommunityProvider<br/>(NEW; uses PolicyResolver pinned to 'community')"]
    end

    subgraph new_policy["NEW policy module"]
      PD["PolicyDocument<br/>(SignedPolicy wrapper;<br/>tier_policies + flags)"]
      PR["PolicyResolver (G1)<br/>tier entitlements"]
      FE["FlagEvaluator (G2)<br/>NEW: kill switch + rollout + tier targeting"]
      FK["FeatureKey newtype<br/>(typed const registry)"]
      GH["feature_enabled(license, flags, key)<br/>NEW: FLOOR ∧ GATE gating contract"]
      PD --owned by--> PR
      PD --owned by--> FE
      GH --consults--> PR
      GH --consults--> FE
    end

    SL --Arc<dyn>--> LP
    LP --impl--> LSP
    LP --impl--> DP
    LP --impl--> CP
    CP --consults--> PR
    LSP --consults as fallback--> PR
  end

  EmbeddedJSON["resources/default_policy.json<br/>(signed, include_bytes!, build.rs verified;<br/>carries G1 tiers AND G2 flags)"]
  EmbeddedJSON --LazyLock--> PD

  OverlayJSON["~/.spur/policy-overlay.json<br/>(NEW: optional signed overlay,<br/>fail-closed; supersedes embedded)"]
  OverlayJSON -.optional.-> PD

  InstallID["~/.spur/install-id<br/>(NEW: anonymous UUID for rollout bucketing)"]
  InstallID --used by--> FE

  Cargo["spur-license/Cargo.toml<br/>+ ed25519-dalek = '2'<br/>+ uuid = '1'"]
```

**Double-evaluation — Diagram 1 → code mapping**

| Node | Real-code citation (existing or target) | Status |
|---|---|---|
| `SpurLicense` facade | [`crates/spur-license/src/lib.rs:180-240`](/Volumes/Projects/spur/crates/spur-license/src/lib.rs:180) | unchanged |
| `LicenseProvider` trait | [`crates/spur-license/src/provider.rs:22-41`](/Volumes/Projects/spur/crates/spur-license/src/provider.rs:22) | unchanged |
| `LicenseSeatProvider` | [`crates/spur-license/src/licenseseat.rs:48-286`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:48) | +20 LoC (resolver fallback in `has_entitlement`) |
| `DisabledProvider` | [`crates/spur-license/src/licenseseat.rs:288-344`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:288) | unchanged; reused only for partial-env case |
| `CommunityProvider` | new file `crates/spur-license/src/community.rs` | NEW |
| `PolicyDocument` / `PolicyResolver` / `FeatureKey` | new module `crates/spur-license/src/policy/` (`mod.rs`, `feature_key.rs`) | NEW |
| `FlagSpec` / `FlagEvaluator` / `InstallId` (G2) | new file `crates/spur-license/src/policy/flags.rs` | NEW |
| `feature_enabled(license, flags, key)` gating helper | exposed from `crates/spur-license/src/lib.rs` | NEW |
| `spur flags list` subcommand | new file `crates/spur-cli/src/commands/flags.rs` | NEW |
| `default_policy.json` (entitlements + flags) | new file `crates/spur-license/resources/default_policy.json` | NEW |
| `~/.spur/policy-overlay.json` (signed runtime overlay) | runtime artifact | NEW |
| `~/.spur/install-id` (anonymous UUID for bucketing) | runtime artifact | NEW |
| `build.rs` policy-signature compile-time check | new file `crates/spur-license/build.rs` | NEW |
| `from_env_or_disabled` dispatch update | [`crates/spur-license/src/licenseseat.rs:31-46`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:31) | 5-LoC match-arm change |

### Diagram 2 — Provider dispatch (Community vs Disabled vs Seat)

```mermaid
flowchart LR
  Start["from_env_or_disabled()"]
  Start --> Q1{"SPUR_LICENSESEAT_API_KEY<br/>set?"}
  Q1 --no--> Q2{"SPUR_LICENSESEAT_PRODUCT_SLUG<br/>set?"}
  Q1 --yes--> Q3{"SPUR_LICENSESEAT_PRODUCT_SLUG<br/>set?"}

  Q2 --no--> CP["CommunityProvider<br/>(absent = 'didn't ask for paid')"]
  Q2 --yes--> DP1["DisabledProvider<br/>(partial = 'asked but failed')"]

  Q3 --no--> DP2["DisabledProvider<br/>(partial = 'asked but failed')"]
  Q3 --yes--> LSP["LicenseSeatProvider<br/>(full configuration)"]
```

**Rule: absence of BOTH env vars = Community (silent, no error). Presence of ONE = ConfigError (loud, diagnostic).** This preserves the existing "your CI is misconfigured" diagnostic for users who *intended* to use a paid license, while removing the friction for users who never asked.

### Diagram 3 — Cold-start sequence with Community default (TUI path)

```mermaid
sequenceDiagram
  autonumber
  participant CLI as spur-cli Watch
  participant SL as SpurLicense::from_env_or_disabled
  participant CP as CommunityProvider
  participant PR as PolicyResolver (embedded)
  participant Core as spur-core::license_runtime
  participant TUI as spur_tui::app::run_tui_with_license

  CLI->>SL: from_env_or_disabled()
  SL->>CP: new(PolicyResolver::embedded(), tier="community")
  CP->>PR: load embedded signed policy (LazyLock)
  PR-->>CP: PolicyDocument verified
  CP-->>SL: Arc<dyn LicenseProvider>
  SL-->>CLI: SpurLicense { provider: CommunityProvider }
  CLI->>CLI: maybe_prompt_first_run() if TTY && !marker
  Note over CLI: "Continue on Community? (paste key to upgrade now, Enter to continue)"
  CLI->>Core: to_event_state(license.current_state())
  Core-->>CLI: initial_license_state: LicenseStateEvent { tier: Community, features: [...] }
  CLI->>TUI: run_tui_with_license(..., initial_license_state)
  TUI-->>TUI: badge renders "Community — free tier"
```

**Verification finding for hardening invariant #1 (single emission seam):** `CommunityProvider` constructs an unused `tokio::sync::broadcast::Sender` only to satisfy the `subscribe()` trait method; it never sends. The funnel sees exactly one initial snapshot via the same `to_event_state` path used by every other provider. **No new emission seam introduced.**

---

## The PolicyDocument schema (irreversible-by-design — must be right in V1)

### Why this section is non-negotiable

Once published binaries deserialize a particular schema, every future binary must continue to accept it. The hardening spec exists to *protect* primitives; this schema is itself a new primitive. Forward-compat patterns are not optional polish — they are the only mitigation for the otherwise-permanent commitment.

### Schema (Rust types)

```rust
// crates/spur-license/src/policy/mod.rs

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The wire format. Always wrapped in SignedPolicy on disk and over the wire.
/// Carries TWO orthogonal namespaces: `tier_policies` (G1 — entitlements) and
/// `flags` (G2 — runtime toggles). They share the document because they share
/// the signing/distribution flow, NOT because they are the same concept.
/// Code-side, `PolicyResolver` handles G1 and `FlagEvaluator` handles G2; the
/// `feature_enabled(license, flags, key)` helper combines them via the
/// FLOOR ∧ GATE rule.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PolicyDocument {
    /// Major schema version. Code refuses load if `schema_version > CODE_SUPPORTED_MAJOR`.
    pub schema_version: u32,

    /// Monotonic; newer issued_at wins when comparing embedded vs overlay/cached.
    pub issued_at: DateTime<Utc>,

    /// Optional expiry. Embedded baseline has None; overlays have Some.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,

    /// G1 — Per-tier entitlement policy. Tier names: "community", "pro", "team", "enterprise".
    /// Unknown tiers in the document are ignored at lookup time (fail-closed).
    pub tier_policies: BTreeMap<String, TierPolicy>,

    /// G2 — Runtime feature flags. Empty map is valid (FF capability optional).
    /// Unknown flag keys at lookup time return `false` (fail-closed).
    #[serde(default)]
    pub flags: BTreeMap<String, FlagSpec>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TierPolicy {
    /// Canonical entitlement set for this tier.
    pub features: BTreeSet<String>,

    /// Quotas — empty in V1; reserved for V3.
    #[serde(default)]
    pub quotas: BTreeMap<String, serde_json::Value>,

    /// Display labels, marketing copy, badge color, etc. Non-functional.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

/// G2 — runtime flag specification. Intentionally minimal in V1 (kill switch +
/// rollout + tier targeting). Extensions (variants, segments, dependencies)
/// flow into `extensions` until they earn typed fields with a schema_version
/// minor bump.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct FlagSpec {
    /// Master switch. Default `true` so existing flag keys are "on" by default,
    /// matching the "ship behavior, then add a kill switch" pattern. A "dark"
    /// flag awaiting rollout sets `enabled: false` explicitly.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Percentage rollout 0.0..=100.0. None means 100% (always on if `enabled`).
    /// Determinism: stable bucket = SipHash(install_id || flag_key) % 100,
    /// then bucket < rollout_percent → on.
    #[serde(default)]
    pub rollout_percent: Option<f32>,

    /// Restrict to users on these license tiers (matched against
    /// `LicenseState::plan.label_lowercase()`). None means all tiers.
    /// Example: `["pro", "team", "enterprise"]` for a Pro-only beta.
    #[serde(default)]
    pub tier_filter: Option<Vec<String>>,

    /// Human description for PR review and `spur flags list` introspection.
    #[serde(default)]
    pub description: Option<String>,

    /// Forward-compat catch-all. Unknown fields land here so V1 binaries can
    /// still load V1.5 policies that introduce variants/segments/etc.
    #[serde(default, flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

fn default_true() -> bool { true }

/// Wrapper that carries the signature. The payload is canonical JSON of
/// PolicyDocument so signature verification is independent of serde
/// formatting choices.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SignedPolicy {
    pub payload: String,        // canonical JSON of PolicyDocument
    pub signature: String,      // base64-encoded Ed25519 signature
    pub key_id: String,         // for rotation
}
```

### Forward-compat rules (binding)

1. **`schema_version: u32`.** Code defines `pub const CODE_SUPPORTED_MAJOR: u32 = 1`. Code REFUSES to load any policy where `schema_version > CODE_SUPPORTED_MAJOR` and falls back to embedded. Older minor schemas (within the same major) deserialize via `#[serde(default)]`.
2. **`#[serde(default)]` on every optional field.** Old binaries accept new policies that omit fields they don't know about.
3. **`BTreeMap<String, serde_json::Value>` for `quotas`.** V1 ignores quota contents entirely (no schema for quotas yet); V3 introduces a typed schema. Using `Value` means V1 binaries don't fail on V3 quotas — they just don't enforce them.
4. **`#[serde(other)]` on enums.** When V1 introduces enums (none today; V3 adds `WindowSpec` and `ExceedAction`), unit variants get an `Unknown` carrier so unknown variants degrade gracefully.
5. **`#[serde(flatten)] extensions: BTreeMap<String, Value>`** on `FlagSpec`. Future flag features (variants, segments, dependencies) deserialize into `extensions` on V1 binaries; V1.5 binaries that know those fields read them directly. No data loss; no parse failure.
6. **Asymmetric defaults**:
   - Unknown FEATURE in `has_entitlement(unknown_key)` → **fail-closed** (return `false`). Caller asked about something not policied; safe default is deny.
   - Unknown FLAG in `is_enabled(unknown_key, _)` → **fail-closed** (return `false`). Same reasoning. A missing flag spec means "feature is gated off" until policy is updated.
   - Unknown ACTION on a quota (V3) → **fail-open** (allow). Policy author bug; wrong to punish user.

### Signature trust model

```rust
// crates/spur-license/src/policy/trust.rs

use ed25519_dalek::VerifyingKey;
use std::collections::BTreeMap;

/// Embedded trust map. Compiled into the binary. Multi-key from V1 to enable
/// rotation: ship a new binary that adds the new key BEFORE retiring the old
/// key on the issuance side; ship a later binary that removes the old key.
pub fn trusted_keys() -> &'static BTreeMap<&'static str, VerifyingKey> {
    static KEYS: std::sync::OnceLock<BTreeMap<&'static str, VerifyingKey>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let mut m = BTreeMap::new();
        m.insert(
            "spur-policy-2026-04",
            VerifyingKey::from_bytes(&include_bytes!("../../resources/keys/spur-policy-2026-04.pub")
                .as_slice().try_into().expect("32-byte key")).expect("valid key"),
        );
        m
    })
}
```

### Trust matrix

| Source of truth | Authority | Tamper-resistance |
|---|---|---|
| Community features (V1) | Embedded signed `default_policy.json` | Ed25519 verified at compile-time (build.rs) AND runtime |
| Community features (V2, future) | Cached signed overlay from CDN | Ed25519 verified on load; fall back to embedded if invalid/expired |
| Pro features (server-known) | LicenseSeat `active_entitlements` response | Server-authenticated |
| Pro features (server-not-yet-aware) | Resolver fallback to PolicyDocument's `pro` tier | Same as Community |
| Quotas (V3, local counters) | `~/.spur/usage.json` | Honor system; documented |

### `FeatureKey` newtype (typed const registry)

```rust
// crates/spur-license/src/policy/feature_key.rs

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FeatureKey(&'static str);

impl FeatureKey {
    pub const CHAT: Self                 = Self("chat");
    pub const CODE_EDIT: Self            = Self("code_edit");
    pub const WATCH_LOOP: Self           = Self("watch_loop");
    pub const ADVANCED_AGENTS: Self      = Self("advanced_agents");
    pub const TEAM_SHARING: Self         = Self("team_sharing");
    pub const CLOUD_SYNC: Self           = Self("cloud_sync");
    // … grows over time; one place to grep for the canonical list

    pub const fn as_str(&self) -> &'static str { self.0 }
}
```

Callers use `license.has_entitlement(FeatureKey::ADVANCED_AGENTS.as_str())`. Adding a feature = adding a const. Typo-safe, grep-discoverable. The underlying `has_entitlement(&str)` signature is preserved so the policy can also answer for unknown keys (fail-closed).

> **Note on the initial contents of `FeatureKey` and the Community/Pro split in `default_policy.json`:** these depend on product input. The spec commits to the *mechanism*; the *contents* are filled in during execution from the rough first cut you provide as pre-work. The implementation plan will treat the contents as a parameter.

---

## The Day-1 FF capability — `FlagEvaluator` and the gating contract

### Why custom over OpenFeature

After multi-round analysis (see commit history of this spec), the design space is `{defer, OpenFeature+flagd, custom-over-PolicyDocument}`. **Custom (K3) is strictly better than OpenFeature+flagd (K1) for spur today** on cost, maintenance burden, review ergonomics, single-signing-flow operations, and bus factor. K1's only durable advantage is vendor neutrality — a **hypothetical future** benefit (matters when we adopt a SaaS for collaborative flag UIs / experimentation stats engines, neither of which spur has near-term need for). Migration K3 → K1 later is bounded: implement a custom OpenFeature `FeatureProvider` over our local `FlagEvaluator` (~200 LoC), swap the wrapper. That migration cost is the SAME cost K1 forces today; we defer it to when there's an actual reason.

### `FlagEvaluator` (~150 LoC, sync, deterministic)

```rust
// crates/spur-license/src/policy/flags.rs

use crate::LicenseState;
use crate::policy::{FlagSpec, PolicyDocument};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Stable per-machine anonymous identifier. Generated on first run, persisted
/// at `~/.spur/install-id`. NOT correlated with user identity. Used solely
/// as the bucket-stable input for percentage rollouts.
#[derive(Clone, Debug)]
pub struct InstallId(pub String);

pub struct FlagEvaluator {
    document: Arc<PolicyDocument>,
    install_id: InstallId,
}

impl FlagEvaluator {
    pub fn new(document: Arc<PolicyDocument>, install_id: InstallId) -> Self {
        Self { document, install_id }
    }

    /// Returns true if the flag is enabled for THIS install + license combination.
    /// Unknown flag → false (fail-closed).
    pub fn is_enabled(&self, flag_key: &str, license: &LicenseState) -> bool {
        let Some(spec) = self.document.flags.get(flag_key) else {
            return false; // unknown flag = off
        };
        if !spec.enabled { return false; }

        if let Some(filter) = &spec.tier_filter {
            let tier = license.plan.label().to_ascii_lowercase();
            if !filter.iter().any(|t| t.to_ascii_lowercase() == tier) {
                return false;
            }
        }

        if let Some(pct) = spec.rollout_percent {
            let bucket = self.bucket(flag_key);
            if (bucket as f32) >= pct { return false; }
        }

        true
    }

    /// Stable bucket 0..100 from (install_id, flag_key). SipHash via std's
    /// DefaultHasher — not cryptographic, but stable across processes on the
    /// same install for the same flag key. Sufficient for rollout determinism.
    fn bucket(&self, flag_key: &str) -> u32 {
        let mut h = std::hash::DefaultHasher::new();
        self.install_id.0.hash(&mut h);
        flag_key.hash(&mut h);
        (h.finish() % 100) as u32
    }

    /// Introspection for `spur flags list`. Returns evaluation + spec.
    pub fn explain(&self, flag_key: &str, license: &LicenseState) -> FlagExplanation { /* … */ }
}
```

### The gating contract — FLOOR ∧ GATE

**Single helper, used at every gating callsite:**

```rust
// crates/spur-license/src/lib.rs (or a new gating.rs module)

/// Returns true iff the user is BOTH contractually allowed (license entitlement)
/// AND operationally enabled (flag evaluation) to use `key`.
///
/// FLOOR: `license.has_entitlement` — does the user's tier include this feature?
/// GATE:  `flags.is_enabled` — is the rollout open to this user right now?
///
/// Both must be true. Conjunction makes the system safe-by-default:
/// a misconfigured flag cannot grant entitlements you don't have;
/// a misconfigured license cannot expose features that aren't safe to
/// expose yet.
pub fn feature_enabled(
    license: &SpurLicense,
    flags: &FlagEvaluator,
    key: FeatureKey,
) -> bool {
    license.has_entitlement(key.as_str())
        && flags.is_enabled(key.as_str(), &license.current_state())
}
```

This is **THE callsite contract** for gating. Every new gated feature uses it. Reviewers see both checks together. Reversing this rule = workspace refactor — committed as **invariant #9** below.

### `InstallId` lifecycle

- Generate on first run: `uuid::Uuid::new_v4().to_string()` written to `~/.spur/install-id`. Single line, no schema.
- Same machine, same install: stable across sessions.
- Reset by `rm ~/.spur/install-id` — user gets a new bucket on next start. Documented as a "reset rollout" mechanism for power users debugging gradual-rollout edge cases.
- NOT correlated with LicenseSeat machine fingerprint, license key, or any user identity. Anonymous bucket-stability only.

### Local signed overlay — `~/.spur/policy-overlay.json`

Hot-swap path WITHOUT shipping a new release. Same `SignedPolicy` wrapper, same Ed25519 trust map.

- Loaded at startup if present; signature-verified; fail-closed on bad signature (overlay ignored, embedded baseline used; warning logged).
- Loaded once per process; no file-watch in V1. Hot-reload is a future affordance; for now, restart `spur watch` to pick up overlay changes. (Acceptable for V1 since `spur watch` is the long-running surface and is restart-cheap.)
- Distribution: ops can drop a signed overlay onto user machines via any channel (auto-updater, support Slack, doc instructions). Overlay's `expires_at` SHOULD be set (e.g., 30 days) so a stale overlay doesn't outlive its intent.
- Embedded baseline always present as floor; overlay only ADDS or modifies; cannot remove tiers/features.

### Initial flag inventory (V1, all default ON — behavior-neutral commit)

Spec commits to the mechanism; product input refines the list during Phase 0. Suggested starting set:

| Flag key | Default | Purpose |
|---|---|---|
| `kill_advanced_planner` | `true` | Kill switch on the new agent planner. Flip `false` to fall back to the previous planner. |
| `enable_browser_tool` | `true` | Gradual ramp candidate. Add `rollout_percent: 50.0` to expose to half of installs. |
| `enable_compaction_v2` | `true` | Kill switch on the V2 compaction logic. |
| `enable_telemetry` | `false` | OFF until the telemetry spec lands; flip ON to begin opt-in capture. |

All four exist as `pub const` in the `FeatureKey` registry alongside the entitlement keys. Same namespace; one place to grep.

### `spur flags list` subcommand

CLI introspection, ~30 LoC:

```
$ spur flags list
FLAG                       ENABLED  ROLLOUT  TIER FILTER     RESULT
kill_advanced_planner      true     —        —               on
enable_browser_tool        true     50%      —               off (bucket 73)
enable_compaction_v2       true     —        —               on
enable_telemetry           false    —        —               off
```

`--verbose` prints spec details (description, extensions). Useful for debugging "why didn't I see feature X?" support questions.

---

## The five coordinated changes

### 1. `CommunityProvider` (B2′)

```rust
// crates/spur-license/src/community.rs

use crate::policy::PolicyResolver;
use crate::provider::{LicenseProvider, RefreshPolicy};
use crate::{LicenseError, LicenseEvent, LicenseState, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct CommunityProvider {
    resolver: Arc<PolicyResolver>,
    state: LicenseState,                 // computed once at construction
    events_tx: broadcast::Sender<LicenseEvent>,  // never sent on; trait satisfaction only
}

impl CommunityProvider {
    pub fn new(resolver: Arc<PolicyResolver>) -> Self {
        let features = resolver.tier_features("community");
        let state = LicenseState::active_community(features);
        let (events_tx, _) = broadcast::channel(1);
        Self { resolver, state, events_tx }
    }
}

#[async_trait]
impl LicenseProvider for CommunityProvider {
    fn current_state(&self) -> LicenseState { self.state.clone() }
    fn subscribe(&self) -> broadcast::Receiver<LicenseEvent> { self.events_tx.subscribe() }
    fn refresh_policy(&self) -> RefreshPolicy { RefreshPolicy::default() }
    fn requires_heartbeat(&self) -> bool { false }
    fn has_entitlement(&self, feature: &str) -> bool {
        self.resolver.has_entitlement(feature)
    }
    async fn activate(&self, _key: &str) -> Result<LicenseState> {
        Err(LicenseError::NotConfigured(
            "Community tier; configure SPUR_LICENSESEAT_* env vars to activate a paid license".into()
        ))
    }
    async fn validate(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
    async fn heartbeat(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
    async fn deactivate(&self) -> Result<LicenseState> { Ok(self.state.clone()) }
}
```

A new `LicenseState::active_community(features: BTreeSet<String>) -> LicenseState` constructor is added alongside `active_cached()` and `active_validated()` in [`crates/spur-license/src/lib.rs:116-140`](/Volumes/Projects/spur/crates/spur-license/src/lib.rs:116). It sets `status: Active`, `plan: Plan::Community`, `subject_kind: SubjectKind::User`, `binding_mode: BindingMode::Unknown`, `offline_ok: true`, `status_text: "Community tier"`.

The dispatch in `from_env_or_disabled` ([`licenseseat.rs:31-46`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:31)) gains one match arm:

```rust
pub fn from_env_or_disabled() -> Arc<dyn LicenseProvider> {
    match (
        std::env::var(LICENSESEAT_API_KEY_ENV),
        std::env::var(LICENSESEAT_PRODUCT_SLUG_ENV),
    ) {
        (Ok(api_key), Ok(product_slug)) => Arc::new(LicenseSeatProvider::new(api_key, product_slug)),
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => {
            Arc::new(CommunityProvider::new(PolicyResolver::embedded()))   // CHANGED
        }
        _ => Arc::new(DisabledProvider::new("incomplete licensing environment configuration")),
    }
}
```

`LicenseSeatProvider::has_entitlement` ([`licenseseat.rs:180-182`](/Volumes/Projects/spur/crates/spur-license/src/licenseseat.rs:180)) gains a fallback:

```rust
fn has_entitlement(&self, feature: &str) -> bool {
    if self.sdk.has_entitlement(feature) { return true; }
    // Fallback: policy file knows about Pro-tier features the LicenseSeat
    // catalog hasn't been updated with yet. Server-asserted entitlements
    // override; policy is the FLOOR.
    self.resolver.tier_for_plan(self.current_snapshot().plan).has_entitlement(feature)
}
```

### 2. Single-verb activation (B6) — and the upgrade-path dependency

**Dependency to resolve first.** Both B6's paste-to-upgrade and B9's demo key call `SpurLicense::activate(key)`. Today, `activate()` only succeeds when `LicenseSeatProvider` is the underlying provider, which today requires `SPUR_LICENSESEAT_API_KEY` + `SPUR_LICENSESEAT_PRODUCT_SLUG` env vars set. **Without resolving this, B6/B9 do not actually work for users in the Community-default state** — exactly the users we want to convert.

Two implementation options for the plan to choose between (recorded as Open Question 1 below):

- **Option A (recommended) — bake in the LicenseSeat publishable credentials at compile time.** Per LicenseSeat docs ([crates.io/crates/licenseseat](https://crates.io/crates/licenseseat)), `pk_*` keys are explicitly designed for client embedding (analogous to a Stripe publishable key). The product slug is not a secret. Bake both via `option_env!`:
  ```rust
  // crates/spur-license/src/build_constants.rs
  pub const BAKED_LICENSESEAT_PUBLISHABLE_KEY: Option<&str> =
      option_env!("SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY");
  pub const BAKED_LICENSESEAT_PRODUCT_SLUG: Option<&str> =
      option_env!("SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG");
  ```
  Set in release CI from non-secret build vars. `option_env!` returns `None` for dev builds without them, so local dev still compiles. The dispatch in `from_env_or_disabled` becomes:
  - Runtime env vars present (override) → `LicenseSeatProvider` from env.
  - Runtime env vars absent AND baked-in present → `LicenseSeatProvider` from baked, BUT if no cached license, the FACADE exposes `CommunityProvider` for the current state. `activate()` is delegated to the always-available LicenseSeatProvider so paste-to-upgrade works. On successful activation, the facade switches the exposed provider to `LicenseSeatProvider`.
  - Both absent → `CommunityProvider` only; `activate()` returns `NotConfigured` with the doc link.

- **Option B (simpler, friction-ful) — require env vars for upgrade.** Spec doesn't bake credentials. B6's prompt becomes: `Continue on Community? (Enter to continue. To upgrade, set SPUR_LICENSESEAT_API_KEY and SPUR_LICENSESEAT_PRODUCT_SLUG and run 'spur auth login --key …')`. B9's demo key requires the same env-var setup. This honestly admits the dependency but defeats the "single-verb activation" goal.

**The spec recommends Option A.** It is the only option that delivers the goals as stated. The implementation plan will commit to A unless ops indicates the publishable key cannot be in-source for non-technical reasons (in which case Option B is the fallback and the goals are partially met).

**Implementation contract for the prompt itself (Option A assumed):**

In `spur-cli/src/main.rs` Watch path, after `SpurLicense::from_env_or_disabled()` and before `Orchestrator::new`:

```rust
maybe_prompt_first_run(&license).await?;
```

- Skip if `!std::io::stdin().is_terminal()` (CI safe).
- Skip if `~/.spur/onboarded` marker exists.
- Skip if `license.current_state().status` is anything other than the Community-default state (user already has a configured/cached license).
- Print: `spur is running on the Community tier (free). Paste a license key to unlock Pro now, or press Enter to continue.`
- If user pastes a key: call `license.activate(key)`. Under Option A this delegates to the baked-in LicenseSeat path. On success, persist marker AND print success line; the next process launch comes up as `LicenseSeatProvider`. On failure, print error and continue with Community.
- If user presses Enter: persist marker. Continue with Community.

The marker file `~/.spur/onboarded` contains a JSON line `{"version": 1, "first_run_at": "<RFC3339>"}` so future versions can decide whether to re-prompt for any reason.

### 3. Public demo key (B9)

**Zero code change beyond what B6/Option A already requires.** Tenant-side configuration on LicenseSeat:
- Issue a long-lived license key (e.g., `DEMO-SPUR-2026-Q2`) bound to the Pro plan with all entitlements.
- Apply rate limits (e.g., 1000 activations/day across all machines).
- Set wall-clock expiry (e.g., 2026-07-01T00:00:00Z).
- Document in `docs/onboarding/try-pro.md`: "Try Pro features through 2026-07-01 by running `spur auth login --key DEMO-SPUR-2026-Q2`. No signup required."

Rotation cadence: monthly. New key, new doc revision. Old key continues to work until its expiry.

**Note:** B9 requires Option A for B6 to be useful — without baked-in publishable credentials, users would also need to set env vars before the demo key works, which defeats its frictionless premise.

### 4. Day-1 FF capability (`FlagEvaluator` + `feature_enabled` + `spur flags list`)

Detailed design above in the **"The Day-1 FF capability"** section. Concrete deltas:

- New module `crates/spur-license/src/policy/flags.rs` (~150 LoC) with `FlagEvaluator`, `InstallId`, `FlagExplanation`.
- Extend `PolicyDocument` with `flags: BTreeMap<String, FlagSpec>` (already in the schema section above).
- New helper `feature_enabled(license, flags, key)` exposed from `spur-license::lib`.
- `~/.spur/install-id` UUID lifecycle (~30 LoC).
- `~/.spur/policy-overlay.json` signed-overlay loader (~80 LoC; reuses `SignedPolicy` verification).
- `spur flags list` CLI subcommand in `spur-cli/src/commands/flags.rs` (~30 LoC).
- 4 placeholder flags in `default_policy.json` (`kill_advanced_planner`, `enable_browser_tool`, `enable_compaction_v2`, `enable_telemetry`), all default-ON for behavior-neutral V1.
- Tests: evaluator returns `false` for unknown flag (fail-closed); rollout determinism (same install_id + flag_key → same bucket across processes); tier_filter respects `LicenseState::plan`; overlay supersedes embedded when newer & signed; bad-signature overlay falls back to embedded.

**Placement (E3 from analysis):** lives in `crates/spur-license/src/policy/flags.rs` — same crate as `PolicyResolver`, separate module. Two reasons: (a) shares the `PolicyDocument`, signing infrastructure, and `FeatureKey` registry without crate-boundary friction; (b) physical separation (`policy/mod.rs` for G1 entitlements, `policy/flags.rs` for G2 toggles) preserves the conceptual G1/G2 distinction without splitting the crate. A future `spur-flags` crate split is bounded if the surface grows (just move the module + re-export).

### 5. CLI output redesign

Replace the 5-line `print_state` in [`crates/spur-cli/src/commands/auth.rs:108-118`](/Volumes/Projects/spur/crates/spur-cli/src/commands/auth.rs:108) with a single-line summary keyed on tier and status:

| Status / Plan | Output |
|---|---|
| `Active` / `Community` | `spur Community — free tier  ⓘ run 'spur auth login --key …' to unlock Pro` |
| `Active` / `Pro` (or higher) | `spur Pro — active  ✓ all features unlocked` |
| `Active` / `Pro` with `expires_at` | `spur Pro — active until 2027-04-19  ✓ all features unlocked` |
| `Degraded` | `spur Pro — degraded (network)  ⚠ cached license still valid offline` |
| `Invalid` | `spur — license invalid  ✗ {status_text}` |
| `ConfigError` | `spur — config error  ✗ {status_text}` |
| `Inactive` | `spur — inactive  ⓘ run 'spur auth login --key …' to activate` |

`--format json` continues to emit the stable `LicenseStateEvent` schema (Phase 3 H5 of hardening — already planned).

---

## Invariants preserved (verified against hardening spec)

1. **Single-seam emission** ([hardening spec](/Volumes/Projects/spur/docs/superpowers/specs/2026-04-19-licensing-hardening-design.md:394) invariant #1). `CommunityProvider`'s `events_tx` is constructed but never sent on. Initial state flows through the same `to_event_state` path via the orchestrator. ✅
2. **Cached state is authoritative** (invariant #2). Network errors cannot transition Community → Invalid because Community has no network calls. Trivially preserved. ✅
3. **Monotonic seq ordering** (invariant #3). Unchanged; `FunnelHandle` still stamps. ✅
4. **Cold-start first frame latency** (invariant #4). `CommunityProvider::new` is synchronous; embedded policy is loaded via `LazyLock` on first access; TUI seeds from `to_event_state` exactly as before. ✅
5. **No secrets in status output** (invariant #5). The single-line summary contains tier and (for errors) `status_text` from the provider — same surface as today. JSON format unchanged. ✅

**Additional invariants this spec introduces:**

6. **Embedded policy compile-time validity.** `build.rs` runs `verify(SignedPolicy)` on the embedded `default_policy.json`. CI cannot ship a binary with an unsigned, malformed, or future-versioned policy.
7. **Policy schema forward-compat.** Tests assert that a v1 binary deserializes a hypothetical v1.5 policy (extra fields ignored), AND that a v1 binary refuses a v2 policy (`schema_version` check fires) and falls back to embedded.
8. **Asymmetric default safety.** Tests assert `unknown_feature → false` (fail-closed), `unknown_flag → false` (fail-closed), and `unknown_quota_action → allow` (fail-open) at the resolver/evaluator layer.
9. **FLOOR ∧ GATE gating contract.** Every gated feature uses `feature_enabled(license, flags, key)` — license entitlement is FLOOR, flag is GATE, both must be true. Tests assert: an entitled user with a flag at `enabled: false` sees the feature off; a non-entitled user with a flag at `enabled: true` also sees it off; only the conjunction exposes. Code-search invariant in CI (`grep`-based deny-list against direct `has_entitlement` calls in caller crates) — direct calls are allowed only inside the gating helper.
10. **Rollout determinism.** Tests assert that `FlagEvaluator::is_enabled` for a fixed `(install_id, flag_key, FlagSpec)` returns the SAME boolean across process restarts. Property test: for any `install_id`, the empirical bucket distribution across 10000 random flag_keys is uniform within tolerance.
11. **Overlay safety.** Tests assert: a signed overlay with `issued_at > embedded.issued_at` and valid signature wins; a tampered overlay is silently ignored (warning logged) and embedded is used; an expired overlay is ignored.

---

## Phases / execution order

### Phase 0 — Pre-work (you, in parallel with spec review)

- Generate Ed25519 keypair (`spur-policy-2026-04` key id). Private key in vault; 32-byte public key bytes file at `crates/spur-license/resources/keys/spur-policy-2026-04.pub`.
- Provide a rough first cut at the Community vs Pro feature list. Even partial is enough to draft `default_policy.json`.
- Confirm LicenseSeat tenant supports a long-lived rate-limited public demo key. (B9.)
- **Decide Option A vs Option B** (Open Question 1). Confirm whether the LicenseSeat publishable key + product slug are approved for in-source / in-binary embedding. If yes (Option A), provide the values for the build-time env vars `SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY` and `SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG` to be set in release CI.

### Phase 1 — Foundation (~0.5 day)

- New module `crates/spur-license/src/policy/` with `mod.rs`, `feature_key.rs`, `trust.rs`.
- `PolicyDocument`, `TierPolicy`, `SignedPolicy`, `PolicyResolver`, `FeatureKey` all defined per the schema section above.
- `crates/spur-license/build.rs` that loads `resources/default_policy.json`, verifies the signature against the embedded public key, and panics at compile-time if invalid.
- `crates/spur-license/resources/default_policy.json` (signed offline at first commit; signing script in `crates/spur-license/scripts/sign-policy.sh`).
- `crates/spur-license/Cargo.toml`: add `ed25519-dalek = "2"` and `base64 = "0.22"` to `[dependencies]`; the `[build-dependencies]` mirror.
- Tests: schema deserialization including unknown variants; signature happy path + tamper detection; `PolicyResolver::has_entitlement` for known/unknown features; forward-compat (extra-field ignored, future-version refused).

### Phase 2 — CommunityProvider (~0.5 day)

- New file `crates/spur-license/src/community.rs` per the spec above.
- `LicenseState::active_community(features)` constructor in `lib.rs`.
- `from_env_or_disabled` match-arm change in `licenseseat.rs`.
- `LicenseSeatProvider::has_entitlement` resolver fallback in `licenseseat.rs`.
- Tests: `CommunityProvider` returns correct entitlements; `LicenseSeatProvider` consults resolver for unknown SDK keys; existing `FakeProvider` tests still pass.

### Phase 3 — Day-1 FF capability (~0.5 day)

Implements change #4 per the spec sections above. Concrete deliverables:

- `crates/spur-license/src/policy/flags.rs` (`FlagEvaluator`, `InstallId`, `FlagExplanation`).
- Extend `PolicyDocument` with `flags: BTreeMap<String, FlagSpec>`.
- `feature_enabled(license, flags, key)` exposed from `spur-license::lib`.
- `~/.spur/install-id` UUID lifecycle, `~/.spur/policy-overlay.json` signed-overlay loader.
- 4 placeholder flags in `default_policy.json`.
- Add `uuid = { version = "1", features = ["v4"] }` to `spur-license/Cargo.toml`.
- Tests for invariants #9–#11 (FLOOR ∧ GATE, rollout determinism, overlay safety).

### Phase 4 — CLI onboarding + flags subcommand + output (~0.5 day)

- `maybe_prompt_first_run` in `spur-cli/src/main.rs` per the contract above.
- `~/.spur/onboarded` marker schema documented in a 10-line module comment.
- `spur flags list` subcommand in `spur-cli/src/commands/flags.rs` (~30 LoC + tests).
- `print_state` redesign in `auth.rs`.
- Tests: TTY-skip behavior, marker creation, prompt flow with valid key, prompt flow with Enter, output format for each (status, plan) pair, `spur flags list` rendering.

### Phase 5 — Docs + tenant config (hours)

- `docs/onboarding/try-pro.md` documenting the demo key.
- `docs/onboarding/community-tier.md` with the auto-derivable feature matrix from `default_policy.json` metadata.
- `docs/contributing/flag-conventions.md` documenting: when to add a flag, how to add one, the FLOOR ∧ GATE rule, the four initial flags' purposes, the overlay rotation policy.
- README onboarding section updated.
- Tenant-side: configure rate-limited demo key.

---

## Exit criteria

- All five phases land; CI green (`cargo check --workspace`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --all --check`).
- `default_policy.json` signature verified at compile time AND at runtime in tests.
- A fresh `cargo install --path crates/spur-cli` (no env vars) followed by `spur watch` runs the tool with `Plan::Community`; `spur auth status` shows the Community single-line summary.
- A `spur auth login --key DEMO-SPUR-2026-Q2` against the configured tenant transitions to Pro.
- `spur flags list` prints the 4 placeholder flags with correct `RESULT` columns for the current install + license combination.
- Hardening spec's invariants #1–#5 pass their existing tests unchanged.
- New invariants #6–#11 covered by tests.
- README, onboarding docs, and `flag-conventions.md` updated.

---

## Deferred items with explicit revisit triggers

| Deferred item | Trigger to revisit | Rough scope |
|---|---|---|
| 7-day per-user trial | 3+ months of conversion telemetry shows the Community → Pro funnel is bottlenecked **specifically** on lack of Pro evaluation, AND the demo key (B9) hasn't already addressed it | Sibling spec; ~5–8 engineer-days plus product/legal review |
| Remote signed policy refresh (V2) | Product wants to change Community feature list **without** shipping a release | Sibling spec; ~1 engineer-day plus CDN setup |
| Quotas (V3) | A real product gate where boolean entitlement isn't expressive enough (e.g., per-day token caps) | Sibling spec; ~2 engineer-days plus product-policy decisions |
| OpenFeature / Flipt adoption (vendor-neutral standard) | spur adopts a SaaS for collaborative flag management OR experimentation stats engine (e.g., PostHog) AND wants the standardized integration surface. Day-1 capability is already provided by the local `FlagEvaluator`; this trigger is purely about adopting the standard. | Sibling spec; ~1 engineer-day to implement a custom `FeatureProvider` over the local `FlagEvaluator`, swap the wrapper, keep policy distribution unchanged |
| Conversion telemetry | After this lands; product wants data on the Community → Pro funnel | Separate spec; opt-in design; minimum-surface |
| Constructor/background-start split (`SpurLicense::new` + `start_background`) | Already deferred by hardening spec | Unchanged |
| `parking_lot::RwLock` migration | Already deferred by hardening spec | Unchanged |
| Typed state-machine refactor of `LicenseState` | Already deferred by hardening spec | Unchanged |
| Multi-provider (self-hosted, enterprise tenants) | Already deferred by hardening spec | Unchanged |

---

## Open questions to resolve during execution

1. **Option A vs Option B for the upgrade-path dependency.** The spec recommends Option A (bake LicenseSeat publishable key + product slug at compile time via `option_env!`). Confirm during Phase 0 whether the LicenseSeat publishable key is approved for in-source / in-binary embedding. If not, fall back to Option B and accept that B6/B9 require env vars (partial goal achievement).
2. **`SpurLicense` facade composition under Option A.** Two implementation shapes for the facade switching between CommunityProvider and LicenseSeatProvider on activation: (a) facade owns both providers, exposes one based on cache state; (b) facade owns a single `Arc<RwLock<Arc<dyn LicenseProvider>>>` that gets swapped on activation. (b) is simpler and aligns with the existing `Arc<dyn>` shape but adds one indirection. Decide during Phase 2 implementation.
3. **Initial Community feature list.** Pre-work item from product. Spec commits to mechanism, not contents.
4. **Demo key naming convention.** Suggest `DEMO-SPUR-YYYY-QQ` (e.g., `DEMO-SPUR-2026-Q2`). Confirm or propose alternative during Phase 4.
5. **`spur auth login` interactive paste mode (no `--key` flag).** Out-of-scope for this spec but a natural future complement to B6. Tracked as a separate small follow-up if not addressed here.
6. **Telemetry trigger threshold.** "3+ months of data" is the trigger; the actual conversion threshold that warrants revisiting trial should be set when telemetry lands.
