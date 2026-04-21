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
