# Feature Gate Phase 4 + 3b Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add tier quota definitions to the signed `default_policy.json` (Phase 4) and surface a compact runtime flag indicator in the TUI StatusBar (Phase 3b).

**Architecture:** Phase 4 leverages the existing `merge_quotas()` overlay mechanism in `gate.rs` — the code already reads policy quotas; we only need to populate them in the JSON and re-sign. Phase 3b adds `spur-license` as a TUI dependency, computes a flag summary from `FeatureGate` at app startup, and threads it through `ViewContext` → `StatusBarProps` to render a compact `F:3/4` pill in the StatusBar right-aligned metric area.

**Tech Stack:** Rust 2021, ratatui, serde_json, jq, openssl (policy signing)

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `crates/spur-license/resources/default_policy.json` | Modify | Add `quotas` objects to all 4 tier policies |
| `crates/spur-license/scripts/sign-policy.sh` | Execute | Re-sign edited policy with Ed25519 |
| `crates/spur-license/tests/policy_quota_overlay.rs` | Create | Verify policy quotas override hardcoded defaults |
| `crates/spur-tui/Cargo.toml` | Modify | Add `spur-license` workspace dependency |
| `crates/spur-tui/src/app.rs` | Modify | Compute and store `flag_summary`; pass into `ViewContext` |
| `crates/spur-tui/src/views/mod.rs` | Modify | Add `flag_summary` field to `ViewContext` |
| `crates/spur-tui/src/components/status_bar.rs` | Modify | Add `flag_summary` to `StatusBarProps`; render it |
| `crates/spur-tui/src/views/dashboard.rs` | Modify | Pass `flag_summary` into `StatusBarProps` (2 sites) |
| `crates/spur-tui/src/views/session_detail.rs` | Modify | Pass `flag_summary` into `StatusBarProps` (1 site) and `ViewContext` (3 sites) |
| `crates/spur-tui/src/views/session_picker.rs` | Modify | Pass `flag_summary` into `StatusBarProps` (3 sites) and `ViewContext` (1 site) |
| `crates/spur-tui/src/lib.rs` | Modify | Pass `flag_summary` into `ViewContext` (1 site) |
| `crates/spur-tui/tests/status_bar_flag_summary.rs` | Create | Assert flag summary renders with correct styling |

---

## Phase 4: Policy Quota Overlay

### Task 1: Add quota definitions to default_policy.json

**Files:**
- Modify: `crates/spur-license/resources/default_policy.json`

The current file is a signed wrapper. Extract the payload, edit it, then save as raw JSON (the signing script will re-wrap).

- [ ] **Step 1: Extract and pretty-print payload**

Run:
```bash
cd /Volumes/Projects/spur
cat crates/spur-license/resources/default_policy.json | jq -r '.payload' | python3 -m json.tool > /tmp/policy_pretty.json
```

- [ ] **Step 2: Edit each tier policy to add `quotas`**

Open `/tmp/policy_pretty.json`. Inside each tier under `tier_policies`, add a `"quotas"` object alongside `"features"` and `"metadata"`. Use **explicit tagged objects** for every value (no bare scalars):

```json
"community": {
  "features": [ ... ],
  "quotas": {
    "max_concurrent_workers": {"count": 1},
    "event_retention_bytes": {"bytes": 134217728}
  },
  "metadata": { ... }
}
```

```json
"pro": {
  "features": [ ... ],
  "quotas": {
    "max_concurrent_workers": {"count": 5},
    "event_retention_bytes": {"bytes": 1073741824}
  },
  "metadata": { ... }
}
```

```json
"team": {
  "features": [ ... ],
  "quotas": {
    "max_concurrent_workers": {"count": 10},
    "event_retention_bytes": {"bytes": 10737418240},
    "min_seats": {"count": 3}
  },
  "metadata": { ... }
}
```

```json
"enterprise": {
  "features": [ ... ],
  "quotas": {
    "max_concurrent_workers": "unlimited",
    "event_retention_bytes": "unlimited"
  },
  "metadata": { ... }
}
```

- [ ] **Step 3: Save raw JSON back to resources**

```bash
cp /tmp/policy_pretty.json crates/spur-license/resources/default_policy.json
```

Verify the file is valid JSON:
```bash
python3 -m json.tool crates/spur-license/resources/default_policy.json > /dev/null
```

- [ ] **Step 4: Commit the raw JSON edit**

```bash
git add crates/spur-license/resources/default_policy.json
git commit -m "feat(spur-license): add tier quotas to default_policy.json"
```

---

### Task 2: Re-sign the policy document

**Files:**
- Execute: `crates/spur-license/scripts/sign-policy.sh`

- [ ] **Step 1: Verify signing key is available**

```bash
if [[ -z "${SPUR_POLICY_SIGNING_KEY:-}" ]]; then echo "MISSING"; else echo "OK"; fi
```

Expected: `OK`. If `MISSING`, stop Phase 4 here and skip to Phase 3b. The hardcoded defaults remain functional.

- [ ] **Step 2: Run the signing script**

```bash
cd /Volumes/Projects/spur
bash crates/spur-license/scripts/sign-policy.sh
```

Expected output: `Signed crates/spur-license/resources/default_policy.json with key_id=spur-policy-2026-04`

- [ ] **Step 3: Verify wrapper structure**

```bash
cat crates/spur-license/resources/default_policy.json | jq -e '.payload and .signature and .key_id'
```

Expected: `true`

- [ ] **Step 4: Commit the signed policy**

```bash
git add crates/spur-license/resources/default_policy.json
git commit -m "feat(spur-license): re-sign policy with tier quotas"
```

---

### Task 3: Write policy quota overlay integration test

**Files:**
- Create: `crates/spur-license/tests/policy_quota_overlay.rs`

This test verifies that `merge_quotas()` picks up policy-defined quotas and that they override (or match) the hardcoded defaults.

- [ ] **Step 1: Write the test file**

```rust
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, QuotaKey, QuotaValue, Tier};

#[test]
fn community_quota_from_policy_overlay() {
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    assert_eq!(gate.tier(), Tier::Community);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(1))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(134_217_728))
    );
}

#[test]
fn pro_quota_from_policy_overlay() {
    use std::collections::BTreeSet;
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    let mut features = BTreeSet::new();
    features.insert("parallel_workers".to_string());
    let pro_state = spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        features,
    );
    gate.update_state(&pro_state);

    assert_eq!(gate.tier(), Tier::Pro);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(5))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(1_073_741_824))
    );
}

#[test]
fn team_quota_from_policy_overlay() {
    use std::collections::BTreeSet;
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    let mut features = BTreeSet::new();
    features.insert("pm_integration".to_string());
    let team_state = spur_license::LicenseState::active_validated(
        spur_license::Plan::Team,
        features,
    );
    gate.update_state(&team_state);

    assert_eq!(gate.tier(), Tier::Team);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Count(10))
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Bytes(10_737_418_240))
    );
    assert_eq!(
        gate.quota(QuotaKey::MinSeats),
        Some(QuotaValue::Count(3))
    );
}

#[test]
fn enterprise_quota_unlimited_from_policy_overlay() {
    use std::collections::BTreeSet;
    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    let mut features = BTreeSet::new();
    features.insert("sso_saml".to_string());
    let ent_state = spur_license::LicenseState::active_validated(
        spur_license::Plan::Enterprise,
        features,
    );
    gate.update_state(&ent_state);

    assert_eq!(gate.tier(), Tier::Enterprise);
    assert_eq!(
        gate.quota(QuotaKey::MaxConcurrentWorkers),
        Some(QuotaValue::Unlimited)
    );
    assert_eq!(
        gate.quota(QuotaKey::EventRetentionBytes),
        Some(QuotaValue::Unlimited)
    );
}
```

- [ ] **Step 2: Run the new test**

```bash
cd /Volumes/Projects/spur
cargo test -p spur-license --test policy_quota_overlay
```

Expected: 4 tests pass.

- [ ] **Step 3: Run all spur-license tests to ensure no regression**

```bash
cargo test -p spur-license
```

Expected: all tests pass (currently 61+).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-license/tests/policy_quota_overlay.rs
git commit -m "test(spur-license): verify policy quota overlay for all tiers"
```

---

## Phase 3b: TUI StatusBar Flag Indicator

### Task 4: Add spur-license dependency to spur-tui

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`

- [ ] **Step 1: Add workspace dependency**

Add to the `[dependencies]` section of `crates/spur-tui/Cargo.toml`:

```toml
spur-license = { workspace = true }
```

Place it after `spur-pm` to keep workspace crate dependencies grouped.

- [ ] **Step 2: Verify the crate builds**

```bash
cargo check -p spur-tui
```

Expected: clean compile (no new errors).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/Cargo.toml
git commit -m "build(spur-tui): add spur-license workspace dependency"
```

---

### Task 5: Compute flag summary in App and thread through ViewContext

**Files:**
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/mod.rs`

- [ ] **Step 1: Add flag_summary field to App**

In `crates/spur-tui/src/app.rs`, find the `App` struct fields (around line 165) and add:

```rust
    flag_summary: Option<(usize, usize)>, // (active_count, total_count)
```

Initialize it to `None` in `build_with_license_state` (around line 279, after `license_badge: None`):

```rust
            flag_summary: None,
```

- [ ] **Step 2: Add helper to compute flag summary from FeatureGate**

At the bottom of `app.rs` (or near `license_badge_from_state`), add:

```rust
fn compute_flag_summary() -> Option<(usize, usize)> {
    use spur_license::policy::PolicyResolver;
    use spur_license::{FeatureGate, FeatureKey};

    let policy = PolicyResolver::embedded();
    let gate = FeatureGate::new(policy);

    let flags = [
        FeatureKey::KILL_ADVANCED_PLANNER,
        FeatureKey::ENABLE_BROWSER_TOOL,
        FeatureKey::ENABLE_COMPACTION_V2,
        FeatureKey::ENABLE_TELEMETRY,
    ];

    let total = flags.len();
    let active = flags
        .iter()
        .filter(|&&k| gate.is_flag_enabled(k).unwrap_or(false))
        .count();

    Some((active, total))
}
```

- [ ] **Step 3: Call helper during App initialization**

In `build_with_license_state`, after `app.license_badge = license_badge_from_state(...)` (line 289), add:

```rust
        app.flag_summary = compute_flag_summary();
```

- [ ] **Step 4: Add flag_summary to ViewContext**

In `crates/spur-tui/src/views/mod.rs`, add to `ViewContext` (around line 86):

```rust
    pub flag_summary: Option<(usize, usize)>,
```

Update `test_ctx` (around line 98):

```rust
            flag_summary: None,
```

- [ ] **Step 5: Update all ViewContext construction sites in app.rs**

Find the three `ViewContext {` blocks in `app.rs` and add `flag_summary: self.flag_summary,`:

1. Around line 668-671:
```rust
                let ctx = crate::views::ViewContext {
                    lineage: &self.lineage,
                    brain_status: &self.brain_status,
                    license_badge: self.license_badge.as_ref(),
                    flag_summary: self.flag_summary,
                };
```

2. Around line 1145-1148:
```rust
        let ctx = crate::views::ViewContext {
            lineage: &self.lineage,
            brain_status: &self.brain_status,
            license_badge: self.license_badge.as_ref(),
            flag_summary: self.flag_summary,
        };
```

3. Around line 2052-2055:
```rust
        let ctx = crate::views::ViewContext {
            lineage: &self.lineage,
            brain_status: &self.brain_status,
            license_badge: self.license_badge.as_ref(),
            flag_summary: self.flag_summary,
        };
```

- [ ] **Step 6: Verify compilation**

```bash
cargo check -p spur-tui
```

Expected: clean compile. Fix any lifetime or type errors.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/app.rs crates/spur-tui/src/views/mod.rs crates/spur-tui/Cargo.toml
git commit -m "feat(spur-tui): compute flag summary and thread through ViewContext"
```

---

### Task 6: Thread flag_summary through all views into StatusBarProps

**Files:**
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/views/session_picker.rs`
- Modify: `crates/spur-tui/src/lib.rs`
- Modify: `crates/spur-tui/src/components/status_bar.rs`

- [ ] **Step 1: Add flag_summary to StatusBarProps**

In `crates/spur-tui/src/components/status_bar.rs`, add to `StatusBarProps` (around line 84, after `license_badge`):

```rust
    /// Compact flag snapshot: (active_count, total_count). None if unavailable.
    pub flag_summary: Option<(usize, usize)>,
```

- [ ] **Step 2: Update dashboard.rs StatusBarProps constructions**

In `crates/spur-tui/src/views/dashboard.rs`, find the two `StatusBarProps {` blocks.

1. Around line 425 (empty state path):

```rust
                StatusBarProps {
                    view: &ViewId::Dashboard,
                    running,
                    pending_review,
                    total_cost,
                    elapsed: &elapsed,
                    current_mode: None,
                    context_used: None,
                    context_size: None,
                    stream_in_flight: false,
                    issue_count: self.tracked_issues.len(),
                    alert_summary: self.alert_summary,
                    license_badge,
                    flag_summary: ctx.flag_summary,
                },
```

2. Around line 525 (non-empty state path):

```rust
            StatusBarProps {
                view: &ViewId::Dashboard,
                running,
                pending_review,
                total_cost,
                elapsed: &elapsed,
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                issue_count: self.tracked_issues.len(),
                alert_summary: self.alert_summary,
                license_badge,
                flag_summary: ctx.flag_summary,
            },
```

- [ ] **Step 3: Update session_detail.rs**

In `crates/spur-tui/src/views/session_detail.rs`:

1. Find the `StatusBarProps {` block (around line 1761) and add `flag_summary: ctx.flag_summary,`.

2. Find the three `ViewContext {` blocks (around lines 1920, 2079, 2513) and add `flag_summary: None,` to the two test-only contexts, and `flag_summary: ctx.flag_summary,` to the production context (if it passes through). If the production context is constructed inline from another `ctx`, just pass `flag_summary: ctx.flag_summary`.

Wait — check the actual code. The `session_detail.rs` contexts:
- Line ~1920: `crate::views::ViewContext { lineage, brain_status, license_badge: None }` — this is likely a test helper. Add `flag_summary: None,`.
- Line ~2079: similar. Add `flag_summary: None,`.
- Line ~2513: `let ctx = crate::views::ViewContext { lineage: ..., brain_status: ..., license_badge: None }` — test. Add `flag_summary: None,`.

Actually, if `session_detail.rs` delegates to a sub-render that receives `ctx: &ViewContext` directly, it may not need to construct a new `ViewContext`. Check the code carefully. The `StatusBarProps` construction only needs `ctx.flag_summary`.

- [ ] **Step 4: Update session_picker.rs**

In `crates/spur-tui/src/views/session_picker.rs`:

1. Find the `ViewContext {` block (around line 1090) and add `flag_summary: None,`.

2. Find the three `StatusBarProps {` blocks (around lines 409, 710, 770) and add `flag_summary: ctx.flag_summary,` to each.

- [ ] **Step 5: Update lib.rs ViewContext construction**

In `crates/spur-tui/src/lib.rs`, find the `ViewContext {` block (around line 31) and add `flag_summary: None,`.

- [ ] **Step 6: Verify compilation**

```bash
cargo check -p spur-tui
```

Fix any remaining compilation errors (likely missing struct fields in `ViewContext` or `StatusBarProps`).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs crates/spur-tui/src/views/dashboard.rs crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/views/session_picker.rs crates/spur-tui/src/lib.rs
git commit -m "feat(spur-tui): thread flag_summary through views into StatusBarProps"
```

---

### Task 7: Render flag summary in StatusBar

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs`

- [ ] **Step 1: Add rendering logic for flag_summary**

In `StatusBar::render`, in the `right_spans` building block (around line 141, after the license badge span), add:

```rust
        if let Some((active, total)) = props.flag_summary {
            let flag_style = if active == total {
                Style::default().fg(Color::Green)
            } else if active == 0 {
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };
            right_spans.push(Span::styled(format!("F:{active}/{total}"), flag_style));
            right_spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
```

Insert this block **after** the license badge block (around line 144) and **before** the running/review spans.

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p spur-tui
```

- [ ] **Step 3: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs
git commit -m "feat(spur-tui): render compact flag summary in StatusBar"
```

---

### Task 8: Add StatusBar render test for flag summary

**Files:**
- Create: `crates/spur-tui/tests/status_bar_flag_summary.rs`

- [ ] **Step 1: Write the test**

```rust
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;
use spur_tui::components::status_bar::{LicenseBadge, LicenseBadgeTone, StatusBar, StatusBarProps};
use spur_tui::action::ViewId;

#[test]
fn status_bar_renders_flag_summary() {
    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| {
        let area = Rect::new(0, 0, 80, 1);
        StatusBar::render(
            frame,
            area,
            StatusBarProps {
                view: &ViewId::Dashboard,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0s",
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                issue_count: 0,
                alert_summary: None,
                license_badge: Some(&LicenseBadge::new("community", LicenseBadgeTone::Neutral)),
                flag_summary: Some((3, 4)),
            },
        );
    }).unwrap();

    let buffer = terminal.backend().buffer().clone();
    let text = buffer.content.iter().map(|c| c.symbol()).collect::<String>();
    assert!(text.contains("F:3/4"), "expected flag summary 'F:3/4' in status bar, got: {}", text);
}

#[test]
fn status_bar_omits_flag_summary_when_none() {
    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal.draw(|frame| {
        let area = Rect::new(0, 0, 80, 1);
        StatusBar::render(
            frame,
            area,
            StatusBarProps {
                view: &ViewId::Dashboard,
                running: 0,
                pending_review: 0,
                total_cost: 0.0,
                elapsed: "0s",
                current_mode: None,
                context_used: None,
                context_size: None,
                stream_in_flight: false,
                issue_count: 0,
                alert_summary: None,
                license_badge: None,
                flag_summary: None,
            },
        );
    }).unwrap();

    let buffer = terminal.backend().buffer().clone();
    let text = buffer.content.iter().map(|c| c.symbol()).collect::<String>();
    assert!(!text.contains("F:"), "expected no flag summary when None, got: {}", text);
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p spur-tui --test status_bar_flag_summary
```

Expected: 2 tests pass.

- [ ] **Step 3: Run full spur-tui test suite**

```bash
cargo test -p spur-tui
```

Expected: all tests pass.

- [ ] **Step 4: Run clippy**

```bash
cargo clippy -p spur-tui --no-deps -- -D warnings
```

Expected: clean (no warnings).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/tests/status_bar_flag_summary.rs
git commit -m "test(spur-tui): verify StatusBar flag summary rendering"
```

---

## Verification Checklist

After all tasks are complete, run these final checks:

- [ ] `cargo test -p spur-license` passes
- [ ] `cargo test -p spur-tui` passes
- [ ] `cargo clippy -p spur-license --no-deps -- -D warnings` is clean
- [ ] `cargo clippy -p spur-tui --no-deps -- -D warnings` is clean
- [ ] `cargo build -p spur-cli` succeeds (CLI depends on both crates)
- [ ] `default_policy.json` is a valid SignedPolicy wrapper (has `.payload`, `.signature`, `.key_id`)

---

## Spec Coverage Self-Review

| Spec Requirement | Implementing Task |
|---|---|
| Add `quotas` to `TierPolicy` in `default_policy.json` | Task 1 |
| Re-sign policy after JSON edit | Task 2 |
| Policy overlay mechanism works (code already exists) | Task 3 (integration test verifies) |
| TUI flag status panel (compact, read-only) | Tasks 5-7 (StatusBar integration) |
| Show 4 G2 flags + state | Task 5 (computes from `FeatureGate::is_flag_enabled()`) |
| Wait-free reads preserved | No code changes to hot path — `FeatureGate` already exists |
| No breaking changes | All changes are additive |

**No placeholders remain.** Every step contains exact code, file paths, and commands.
