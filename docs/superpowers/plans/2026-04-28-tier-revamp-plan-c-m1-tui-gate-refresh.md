# Tier Revamp Plan C — M1: Live TUI Gate Refresh + Pro-Tier Demo

> **Status:** Open. Filed 2026-04-28 after L9-MCTS evaluation +
> grounding pass + codex architectural review. Promoted from
> "low EV" pre-Tier-2 to **critical path** post-Tier-2 because
> Tier 2 introduced `App::feature_gate` as a startup snapshot that
> blocks any Pro-tier gate from working with mid-session license
> changes (trial activation, login, tier upgrade).
>
> **For agentic workers:** Ships in 3 atomic implementation tasks;
> each delegated to a fresh worker and gated by reviewer panel
> before merge. The brain (orchestrator) judges reviewer output and
> decides accept / iterate.

**Goal:** Wire `App::feature_gate.update_state(&LicenseState)` into
the existing `LicenseUpdated` event handler so that the TUI's
feature gate reflects the current license state across the session
(trial activation, `spur auth login` from another terminal, server-
driven re-validation, heartbeat-driven status changes). Then
demonstrate end-to-end with a real Pro-tier gate site that
community users hit naturally in production (no debug strip env).

**Why this is the next move (per L9-MCTS verdict + grounding):**

1. **Hard precondition for Plan D (trial JWT).** Without M1.1,
   trial activation propagates to the broadcast channel but does
   NOT reach the TUI gate; users see denial despite holding a
   valid trial. Plan D would ship broken.
2. **Promoted from low-EV to critical-path** post-Tier-2: Tier 2
   documented `App::feature_gate` as a startup snapshot with M1
   wiring as the future fix. This is now the binding constraint.
3. **First real production validation of Tier 2's modal.** Today
   the modal only renders via the `SPUR_LICENSE_TEST_STRIP_KEYS`
   debug env. M1.3 puts a real Pro-only gate at `Action::ShowSessionCost`
   so community users naturally hit the modal — first real-world
   conversion-pressure surface.
4. **Cheap, surgical, well-scoped.** Per codex's grounding review:
   ~1.5–2 dev-days total across 3 atomic tasks.

**Tech Stack:** Rust 2021, tokio broadcast (existing), spur-acp
events (existing), `FeatureGate::update_state` (already exposed
at `gate.rs:64` via `ArcSwap`). No new deps.

---

## Spec grounding (verified by 3-agent parallel survey + codex)

### `FeatureGate::update_state` exists and is `&self`

`crates/spur-license/src/gate.rs:64` — `pub fn update_state(&self,
state: &LicenseState)`. Uses `ArcSwap` for atomic snapshot
replacement. **No `&mut self` access required** — the gate can be
refreshed even from a `&App` borrow if needed (though M1 calls it
from `&mut self` since `update_license_state` already takes
`&mut self`).

### `PolicyResolver::for_license(&LicenseState)` does NOT exist

The bridge from `LicenseState` to features is `FeatureGate::update_state`
internally (calls private `build_snapshot`). There is no separate
helper to "resolve features for a license state." We use the
gate-update path directly.

### Event flow already wired through to App

- `SpurEventBody::LicenseUpdated { state: LicenseStateEvent }` is
  emitted by `crates/spur-core/src/license_runtime.rs:113-117`
  (`emit_snapshot`) on every provider state change.
- TUI subscribes via the broadcast `Receiver<SpurEvent>` in
  `run_tui_with_license` (`crates/spur-tui/src/app.rs:2867`).
- Dispatched to `App::handle_spur_event` which matches
  `LicenseUpdated { state }` at `app.rs:1512` and calls
  `App::update_license_state(license_state)`.
- `App::update_license_state(&mut self, license_state: LicenseStateEvent)`
  at `app.rs:511-515` currently updates `self.license_state`,
  `self.license_badge`, `self.dirty=true`. **Does NOT touch
  `self.feature_gate`.** This is the M1.1 wiring point.

### `LicenseStateEvent` (spur-acp) vs `LicenseState` (spur-license)

`LicenseStateEvent` is defined at `crates/spur-acp/src/domain/events.rs:255-265`
with structurally similar fields to `crates/spur-license/src/lib.rs:101-110`'s
`LicenseState`. A **converter is required** to call
`feature_gate.update_state(&LicenseState)` from the
`LicenseStateEvent` handler. Field-level mapping:

| `LicenseStateEvent` (spur-acp) | `LicenseState` (spur-license) |
|---|---|
| `status: LicenseStatusEvent` | `status: LicenseStatus` |
| `subject_kind: LicenseSubjectKind` | `subject_kind: SubjectKind` |
| `plan: LicensePlan` | `plan: Plan` |
| `features: BTreeSet<String>` | `features: BTreeSet<String>` (same) |
| `expires_at: Option<DateTime<Utc>>` | `expires_at: Option<DateTime<Utc>>` (same) |
| `binding_mode: LicenseBindingMode` | `binding_mode: BindingMode` |
| `offline_ok: bool` | `offline_ok: bool` (same) |
| `status_text: String` | `status_text: String` (same) |

The enum variants must be 1:1 mapped (codex verified the round-trip
via `to_event_state()` in `license_runtime.rs:113-117`'s emit path
— that proves both directions exist; we use the inverse mapping).

### Existing TUI gate-check sites (1 today)

- `crates/spur-tui/src/app.rs:1697` — `Action::SendMessage` gates
  on `FeatureKey::CLI_CORE_EXEC` (community-tier; demo-only).

### Pro-only key + handler picked for M1.3

- **`COST_PRO_PER_PROJECT_TRACKING`** (`crates/spur-license/src/policy/feature_key.rs:117`)
  × **`Action::ShowSessionCost`** (`crates/spur-tui/src/app.rs:2084`).
- Cleanest synchronous match arm; no async, no partial state.
- Semantic match: community gets basic cost display
  (`cost_core_session_display` is free); Pro adds per-project
  tracking which is what this action surfaces.
- Modal copy writes itself: "Upgrade to Pro to track costs across
  projects."

### Architectural decision: Option A (pump update_state)

Rejected Option B (replace `App::feature_gate` with `Arc<FeatureGate>`
from `SpurLicense::feature_gate()`). **Codex verified the underlying
invariant is FALSE:** `SpurLicense::validate()` / `heartbeat()` do
NOT call `self.feature_gate.update_state(...)`; they only delegate
to the provider, which calls `replace_state()` — a method that
updates provider-local state and broadcasts events but never
touches `SpurLicense.feature_gate`. So the provider-side
`Arc<FeatureGate>` is initialized fresh ONCE at construction and
never refreshed afterward.

This staleness affects non-TUI consumers too (CLI PM construction
at `crates/spur-cli/src/main.rs:808, 828`). Filed as **M1.x
follow-up** (separate doc) — broader than M1's scope; should not
block M1.

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-tui/src/app.rs` | Modify | M1.1: Add `LicenseStateEvent → LicenseState` converter helper + call `self.feature_gate.update_state(&state)` in `update_license_state`. M1.3: Add gate-check at `Action::ShowSessionCost` arm in `process_action`. |
| `crates/spur-tui/tests/license_state_gate_refresh.rs` | Create | M1.2: Integration test asserting Pro `LicenseUpdated` event makes Pro-only `FeatureKey` pass through `App::feature_gate`; initial Community state denies. Pins the freshness contract against future regression. |

No new deps. Touches only spur-tui crate (and reads enums from
spur-license + spur-acp, which are already in the dep graph).

---

## Task M1.1: Live `App::feature_gate` refresh on `LicenseUpdated`

**Worker assignment:** claude-code (implementer). Reviewers: gemini
+ claude-code (2 gates in parallel). Final: codex review gate.

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (~30 LOC: converter helper +
  `update_state` call in `update_license_state`)

### Subtask 1.1a: Implement `LicenseStateEvent → LicenseState` converter

Add a private helper in `app.rs` (placement: near `update_license_state`
at line 511, OR in a small `license_state_event_to_state` free fn at
the top of the impl block):

```rust
/// Convert `spur_acp::events::LicenseStateEvent` (TUI broadcast
/// representation) into `spur_license::LicenseState` (resolver
/// input). The two types carry the same data with different enum
/// homes; this is the inverse of `spur_core::license_runtime::to_event_state`.
fn license_state_event_to_state(
    e: &spur_acp::events::LicenseStateEvent,
) -> spur_license::LicenseState {
    spur_license::LicenseState {
        status: match e.status {
            spur_acp::events::LicenseStatusEvent::Inactive => spur_license::LicenseStatus::Inactive,
            spur_acp::events::LicenseStatusEvent::Active => spur_license::LicenseStatus::Active,
            spur_acp::events::LicenseStatusEvent::Degraded => spur_license::LicenseStatus::Degraded,
            spur_acp::events::LicenseStatusEvent::Invalid => spur_license::LicenseStatus::Invalid,
            spur_acp::events::LicenseStatusEvent::ConfigError => spur_license::LicenseStatus::ConfigError,
        },
        subject_kind: match e.subject_kind {
            // 1:1 enum mapping per spur-acp / spur-license definitions
            spur_acp::events::LicenseSubjectKind::User => spur_license::SubjectKind::User,
            spur_acp::events::LicenseSubjectKind::Machine => spur_license::SubjectKind::Machine,
            spur_acp::events::LicenseSubjectKind::Unknown => spur_license::SubjectKind::Unknown,
            // ... all variants
        },
        plan: match e.plan {
            spur_acp::events::LicensePlan::Community => spur_license::Plan::Community,
            spur_acp::events::LicensePlan::Pro => spur_license::Plan::Pro,
            // ... all 8 variants per Plan::from_key in spur-license/src/lib.rs:73-84
        },
        features: e.features.clone(),
        expires_at: e.expires_at,
        binding_mode: match e.binding_mode {
            // 1:1 enum mapping
            spur_acp::events::LicenseBindingMode::Strict => spur_license::BindingMode::Strict,
            spur_acp::events::LicenseBindingMode::Loose => spur_license::BindingMode::Loose,
            // ... all variants
        },
        offline_ok: e.offline_ok,
        status_text: e.status_text.clone(),
    }
}
```

**Implementer note:** Verify all enum variant names by reading
`/Volumes/Projects/spur/crates/spur-acp/src/domain/events.rs` AND
`/Volumes/Projects/spur/crates/spur-license/src/lib.rs`. The
mapping above is illustrative; the worker must enumerate every
variant of every enum and ensure 1:1 mapping. If any spur-acp
variant has no spur-license counterpart (unlikely but possible —
spur-acp may have added newer event variants the license crate
doesn't have), document the fallback explicitly (e.g. unknown →
`LicenseStatus::Inactive` with a `tracing::warn!`).

**Defensiveness:** if any conversion fails (e.g. unknown plan
string), the converter should log and fall back to a safe default
(Community plan, Inactive status). It must NOT panic — the gate
refresh path runs in the TUI event loop and must be robust to
upstream schema drift.

### Subtask 1.1b: Wire `update_state` call into `update_license_state`

In `crates/spur-tui/src/app.rs::update_license_state` (line 511-515),
add a call to `self.feature_gate.update_state(&state)` after the
existing field updates:

```rust
fn update_license_state(&mut self, license_state: spur_acp::events::LicenseStateEvent) {
    // M1.1 — refresh the feature gate before updating UI state so
    // the next `process_action` invocation sees the new entitlements.
    let resolved = license_state_event_to_state(&license_state);
    self.feature_gate.update_state(&resolved);

    self.license_badge = license_badge_from_state(&license_state);
    self.license_state = license_state;
    self.dirty = true;
}
```

**Why update gate BEFORE setting `license_state`:** if a downstream
handler relies on both `license_state` AND `feature_gate`, the
ordering ensures gate is fresh first. Both end-states are the same;
just defensive.

**Why `&self.feature_gate` (not `&mut`):** `FeatureGate::update_state`
takes `&self` (uses `ArcSwap` internally — atomic, lock-free). So
`self.feature_gate.update_state(&resolved)` works even though we're
inside `&mut self` here.

### Acceptance for M1.1

- [ ] `license_state_event_to_state` helper defined; covers ALL
      enum variants of all 4 mapped enums (`status`, `subject_kind`,
      `plan`, `binding_mode`). Verified by exhaustive `match` (no
      `_ => fallback` arm without comment).
- [ ] `update_license_state` calls `self.feature_gate.update_state(&resolved)`
      before updating other fields.
- [ ] No new deps in `Cargo.toml`.
- [ ] Workspace builds clean: `cargo build -p spur-tui`.
- [ ] Existing `license_status_render` tests still pass.
- [ ] No clippy warnings in touched code.
- [ ] No fmt diff in touched code.

---

## Task M1.2: Freshness regression guard test

**Worker assignment:** claude-code (implementer). Reviewers: gemini
+ claude-code (2 gates in parallel).

**Files:**
- Create: `crates/spur-tui/tests/license_state_gate_refresh.rs`
  (~80 LOC integration test)

### Subtask 1.2a: Integration test pinning the freshness contract

Create `crates/spur-tui/tests/license_state_gate_refresh.rs`:

```rust
//! Plan C M1 — pin the contract that `App::update_license_state`
//! refreshes `App::feature_gate`. Without this, a future change
//! that drops the `feature_gate.update_state(...)` call from
//! `update_license_state` would silently regress every Pro-tier
//! gate site (including the M1.3 demo at `Action::ShowSessionCost`).
//!
//! Test shape: construct an `App` with initial Community state,
//! assert a Pro-only `FeatureKey` is denied. Push a Pro
//! `LicenseUpdated` event. Assert the same key is now granted.

#![cfg(unix)]

use spur_license::{require_feature, FeatureKey};
use spur_tui::test_helpers::App; // OR whatever the test surface is

#[test]
fn pro_license_update_grants_pro_only_key() {
    // 1. Construct App at Community baseline. Implementer must use
    //    whatever test-construction helper exists. If none, file a
    //    sub-task to add `App::new_for_tests(...)` (Tier 2 added a
    //    similar helper for `quit_shortcut_tests` — reuse or extend).
    let mut app = App::new_for_tests(/* community baseline state */);

    // 2. Assert COST_PRO_PER_PROJECT_TRACKING is denied initially.
    let denied = require_feature(
        &app.feature_gate(),
        FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
    );
    assert!(denied.is_err(), "community baseline must deny pro key");

    // 3. Simulate a Pro license-update event.
    let pro_event = build_pro_license_state_event(); // implementer-defined fixture
    app.update_license_state(pro_event);

    // 4. Assert the SAME key is now granted.
    let granted = require_feature(
        &app.feature_gate(),
        FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
    );
    assert!(
        granted.is_ok(),
        "Pro license-update must refresh the gate; if this fails, \
         the freshness wiring in update_license_state regressed",
    );
}
```

**Implementer note:** the test surface (`App::new_for_tests`,
`App::feature_gate()` accessor, `build_pro_license_state_event()`)
may not exist today. The implementer should:
1. Survey `crates/spur-tui/tests/` for existing test helpers (Tier
   2 added `quit_shortcut_tests` which uses `App::new_for_tests`).
2. Reuse the existing test surface; extend if needed.
3. If a `feature_gate()` accessor on App doesn't exist (the field
   may be private), add `pub(crate)` getter OR add a helper method
   `app.is_feature_granted(key) -> bool` that wraps `require_feature(&self.feature_gate, key)`.
4. The Pro fixture should produce a `LicenseStateEvent` with `plan:
   LicensePlan::Pro` and `features: { ... pro-tier features
   including cost_pro_per_project_tracking ... }`. Use the
   embedded policy + `tier_features("pro")` to build the feature
   set programmatically (no hardcoded list).

### Subtask 1.2b: Add test for "Community update keeps Pro denial"

Same pattern in reverse:

```rust
#[test]
fn community_license_update_after_pro_re_denies() {
    let mut app = App::new_for_tests(/* community baseline */);
    app.update_license_state(build_pro_license_state_event());
    // Now downgrade back to community (e.g. license expired).
    app.update_license_state(build_community_license_state_event());

    let denied = require_feature(
        &app.feature_gate(),
        FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
    );
    assert!(
        denied.is_err(),
        "Community downgrade must re-deny Pro key; gate must reflect \
         current state, not high-water-mark",
    );
}
```

This catches a stale-state bug where the gate retains Pro
entitlements after the user's license downgrades.

### Acceptance for M1.2

- [ ] At least 2 tests: pro-update grants, community-downgrade
      re-denies.
- [ ] Tests use `COST_PRO_PER_PROJECT_TRACKING` (the M1.3 demo key)
      so the test pins exactly the production-relevant case.
- [ ] No hardcoded feature lists in fixtures; build via
      `PolicyResolver::tier_features("pro")` to stay synchronized
      with policy.
- [ ] Tests fail under `git revert <M1.1 commit>` (red-green
      verified).
- [ ] No new deps.
- [ ] Build + clippy + fmt clean for the new test file.

---

## Task M1.3: Pro-tier demo gate at `Action::ShowSessionCost`

**Worker assignment:** claude-code (implementer). Reviewers: gemini
+ claude-code (2 gates in parallel).

**Files:**
- Modify: `crates/spur-tui/src/app.rs` (gate-check at the
  `Action::ShowSessionCost` arm)

### Subtask 1.3a: Add the gate-check

In `crates/spur-tui/src/app.rs::process_action`, locate the
`Action::ShowSessionCost` arm (line 2084). Add a gate-check at the
top, mirroring the existing `Action::SendMessage` pattern at line
1697:

```rust
Action::ShowSessionCost { /* fields */ } => {
    // M1.3 — Pro-tier demo gate. Gates the per-project cost
    // tracking display (`cost_pro_per_project_tracking`); community
    // users see the upgrade modal instead.
    if let Err(err) = spur_license::require_feature(
        &self.feature_gate,
        spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
    ) {
        let required_tier = spur_license::upgrade_cta::required_tier_for(
            spur_license::FeatureKey::COST_PRO_PER_PROJECT_TRACKING,
        );
        self.process_action(Action::ShowUpgradeModal { err, required_tier });
        return;
    }
    // ... existing handler logic ...
}
```

**Implementer note:** verify the exact arm shape (field names) by
reading the current `Action::ShowSessionCost` definition + handler.
The pattern is mechanical — the implementer just needs to insert
the early-return gate check.

### Subtask 1.3b: Update Tier 2 plan post-merge addendum

Mark M1.3's demo path complete in the Tier 2 plan addendum:

In `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier2-tui-upgrade-modal.md`,
under the post-merge addendum, append a 1-line note:
"M1.3 added a Pro-tier gate at `Action::ShowSessionCost ×
COST_PRO_PER_PROJECT_TRACKING`; community users now hit the
upgrade modal naturally without `SPUR_LICENSE_TEST_STRIP_KEYS`."

### Acceptance for M1.3

- [ ] Gate-check at `Action::ShowSessionCost` arm in `process_action`.
- [ ] Uses `COST_PRO_PER_PROJECT_TRACKING` (the M1.2-pinned key).
- [ ] On denial, dispatches `Action::ShowUpgradeModal` with
      `required_tier_for(key)` populated; modal renders the Pro
      tier in the "Required tier:" row.
- [ ] **Manual TUI verification:** under default community license
      (no DEV_PLAN, no STRIP_KEYS), running `cargo run -p spur-cli
      -- tui --brain claude-code` and triggering ShowSessionCost
      pops the upgrade modal showing "Required tier: Pro" /
      "Current tier: Community."
- [ ] **Manual TUI verification with Pro license:** under
      `SPUR_LICENSE_DEV_PLAN=pro`, the same action runs the
      handler normally (no modal).
- [ ] Existing `Action::SendMessage` gate at line 1697 still works
      (Tier 2 regression net).
- [ ] Build + clippy + fmt clean.

---

## Final sweep (judge-only, not delegated)

After M1.1, M1.2, M1.3 all pass per-task review and merge:

- [ ] `cargo build --workspace`
- [ ] `cargo test -p spur-license -p spur-tui -p spur-cli`
- [ ] `cargo clippy -p spur-license -p spur-tui -p spur-cli --tests -- -D warnings`
- [ ] `cargo fmt -p spur-license -p spur-tui -p spur-cli -- --check`
- [ ] **Manual end-to-end TUI verification** (per M1.3 acceptance):
      community baseline → modal pops; Pro DEV_PLAN → handler runs
      normally.
- [ ] Update Tier 2 plan post-merge addendum's "Open follow-ups"
      section to mark M1 closed.
- [ ] File the M1.x follow-up doc:
      `docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`
      (capture codex's finding that `SpurLicense.feature_gate` is
      not refreshed by `validate/heartbeat/activate/deactivate`,
      affecting CLI PM construction at `main.rs:808, 828`).
- [ ] Total commits expected: 3-4 (M1.1, M1.2, M1.3, possibly
      Cargo.lock if any).

## Acceptance criteria for M1 as a whole

- [ ] Every `LicenseUpdated` event reaching the TUI refreshes
      `App::feature_gate` before `update_license_state` returns.
- [ ] Pro-only `FeatureKey`s are denied under Community state and
      granted under Pro state by `App::feature_gate`, verified by
      integration tests.
- [ ] At least one production-reachable Pro-tier gate site exists
      in spur-tui that surfaces the upgrade modal for community
      users without debug env overrides.
- [ ] Tier 2's existing `Action::SendMessage` gate (`cli_core_exec`)
      continues to work (no regression).
- [ ] Tier 2's binary smoke (CTA shape under SPUR_FORCE_TTY) still
      passes.
- [ ] No new deps in any crate.
- [ ] No clippy warnings in touched code; no fmt regressions.

## Out of scope for M1 (deferred)

- **`SpurLicense::feature_gate()` staleness fix (M1.x).** Codex
  flagged that `SpurLicense::validate/heartbeat/activate/deactivate`
  do NOT refresh the provider's `Arc<FeatureGate>`. This affects
  CLI PM construction at `crates/spur-cli/src/main.rs:808, 828` and
  any other consumer of `license.feature_gate()`. **File as a
  separate follow-up doc; do NOT fold into M1** — broader scope,
  touches CLI not just TUI, requires CLI-side regression coverage.
- **ACP-layer gate spread (M1.4 originally).** Don't gate keys
  already enforced at spur-mcp (e.g. `pm_pro_beads_advanced` has
  25+ enforcement sites at `crates/spur-mcp/src/`). Defense-in-depth
  here would create double-error UX. Defer to a separate wave only
  if a gap surfaces.
- **Tier 3 / Plan D content** (trial JWT CTA copy, trial activation
  flow). M1 unblocks Plan D; doesn't ship it.
- **Per-key user-facing labels** (e.g. "Per-project cost tracking"
  vs `cost_pro_per_project_tracking`). Tier 3 polish.
- **Plan E.h Team v2 hygiene.** Now unblocked by Tier 2's Team
  policy embed. Independent track.
- **`Plan::Display` impl.** Tier 2 used `format!("{plan:?}")` and
  the cleanup commit switched to `Plan::label()`. No further
  refactor needed for M1.

## Self-review (pre-dispatch checklist)

- [x] **Architectural choice (Option A) verified.** Codex confirmed
      Option B's invariant is FALSE. Option A is the only option
      backed by existing behavior.
- [x] **Converter scope identified.** All 4 enums need 1:1 mapping
      (`status`, `subject_kind`, `plan`, `binding_mode`). Field-level
      types (BTreeSet<String>, Option<DateTime>, bool, String) are
      shared.
- [x] **No `&mut` borrow conflicts.** `FeatureGate::update_state`
      takes `&self`; can be called from `&mut self::update_license_state`
      without issue.
- [x] **Test isolation.** `App::new_for_tests` already exists per
      Tier 2's `quit_shortcut_tests`. Reuse.
- [x] **Demo key choice grounded.** `COST_PRO_PER_PROJECT_TRACKING`
      × `Action::ShowSessionCost` is the cleanest synchronous
      single-line match arm with semantically matched feature.
- [x] **No double-gating.** `pm_pro_beads_advanced` excluded
      (already MCP-enforced). `SubmitReview` excluded (key
      semantics don't match action).
- [x] **Contract tests both directions.** Pro-update grants,
      Community-downgrade re-denies.
- [x] **M1.x correctly deferred** (separate doc, separate wave;
      CLI regression too broad for M1).

---

## Note on `SpurLicense::feature_gate()` staleness (M1.x context)

Codex's grounding review found that `SpurLicense.feature_gate` is
initialized fresh at construction time (from `provider.current_state()`
at `crates/spur-license/src/lib.rs:213, 236, 247`) but is NEVER
refreshed by `SpurLicense::validate()`, `heartbeat()`, `activate()`,
or `deactivate()` — those methods only delegate to the provider,
which mutates provider-local state via `replace_state()` at
`crates/spur-license/src/licenseseat.rs:84` without touching
`SpurLicense.feature_gate`.

This means `license.feature_gate()` returns a stale `Arc<FeatureGate>`
to non-TUI consumers (e.g. CLI PM construction at
`crates/spur-cli/src/main.rs:808, 828`). After a `spur auth login`
or trial activation, those consumers continue checking against the
startup-time entitlement set.

This is a separate bug from M1's TUI-specific freshness gap. M1
does NOT fix it. The M1.x follow-up doc captures it explicitly so
it doesn't get forgotten.

## References

- Plan C Tier 2 (just shipped):
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier2-tui-upgrade-modal.md`
- Plan C Tier 1:
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier1-cli-denial-cta.md`
- M1.x follow-up (to be filed):
  `docs/superpowers/plans/2026-04-28-tier-revamp-m1x-followup-spurlicense-gate-refresh.md`
- `FeatureGate::update_state`: `crates/spur-license/src/gate.rs:64`
- `App::update_license_state`: `crates/spur-tui/src/app.rs:511-515`
- `App::feature_gate` (Tier 2): `crates/spur-tui/src/app.rs:229-234, 396-398`
- `SpurEventBody::LicenseUpdated`: `crates/spur-acp/src/domain/events.rs:255-265`
- `LicenseStateEvent → LicenseState` mapping inverse:
  `crates/spur-core/src/license_runtime.rs:113-117` (`to_event_state`)
- M1.3 demo target: `Action::ShowSessionCost` at
  `crates/spur-tui/src/app.rs:2084` × `COST_PRO_PER_PROJECT_TRACKING`
  at `crates/spur-license/src/policy/feature_key.rs:117`
