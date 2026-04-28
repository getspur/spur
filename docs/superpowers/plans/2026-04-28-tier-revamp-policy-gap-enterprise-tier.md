# Tier Revamp — Policy Gap: Embedded `enterprise` Tier Missing

**Status:** ✅ RESOLVED 2026-04-28 (commit `1dfe4557`).
The embedded signed policy now defines `community`, `pro`, `team`,
and `enterprise` tier blocks. Team and Enterprise are placeholders
that mirror Pro entitlements until product-spec'd feature deltas
land (Plan E.h for Team; future tier-design pass for Enterprise).
This doc is kept as the audit trail for the gap and the gap-fix
process; do not delete.

Date: 2026-04-28
Filed during: Plan C M0 (wave C.1) execution
Source: real-execution finding the M0 plan didn't anticipate

## Summary

The embedded signed `default_policy.json` defines tier policies for
`community` and `pro` only. The `Plan` enum in
`crates/spur-license/src/lib.rs:60-69` has variants for
`Community / StarterLtd / BuilderLtd / FounderLtd / Pro / Team /
Enterprise / Unknown`, and the dev-mode override
`SPUR_LICENSE_DEV_PLAN` (debug builds only,
`crates/spur-license/src/community.rs:36-44`) explicitly allows the
value `"enterprise"`. When that combination fires, the tier resolves
to `Plan::Enterprise` with **zero features** — every
`require_feature(...)` call denies, including baseline `cli_core_*`
keys.

## Reproduction

```bash
# In any debug build (cargo test, cargo run, debug spur binary):
export SPUR_LICENSE_DEV_PLAN=enterprise
cargo run -p spur-cli -- init
# → Error: feature `cli_core_init` is not available on tier `Enterprise`
```

This first surfaced as a Plan C M0 test breakage:
`crates/spur-cli/tests/init_ux.rs` ran tests in a shell with
`SPUR_LICENSE_DEV_PLAN=enterprise` already exported, the test runner
inherited that env, the spawned `spur init` child saw an empty
Enterprise tier, and the new `require_cli_gate(CLI_CORE_INIT)?` call
denied dispatch.

The M0 commit `5e67e1d2` worked around this by stripping
`SPUR_LICENSE_DEV_PLAN` in the `init_ux::spur()` helper and the new
`cli_core_gate_e2e.rs` test. That fix is correct *for those tests*
but does not address the underlying policy gap.

## Why this is a real bug, not a test-isolation curiosity

1. **Marketing copy and the Plan enum both promise an Enterprise
   tier exists.** The dev override is the only way today to preview
   what an Enterprise customer would experience locally; if the
   embedded policy has no `enterprise` block, the override is
   silently broken.

2. **The failure is silent at the policy layer** — `tier_features("enterprise")`
   returns `Err(_)` which `community.rs:28-30` swallows into an
   empty `BTreeSet`. No warning, no panic. The dev sees zero
   features and assumes "Enterprise was supposed to be more
   restrictive than Pro" or similar — wrong mental model.

3. **Plan E.h (Team v2 stubs)** will face the same issue when it
   adds `Team` tier definitions. Same root cause: enum variants
   exist without matching policy blocks.

## Proposed fixes (pick one or both)

**A. Fail loudly when an unknown tier is requested via dev override.**
   Touch: `crates/spur-license/src/community.rs:28-30` — replace
   `unwrap_or_else(|err| { tracing::warn!(...); empty })` with
   `unwrap_or_else(|err| { tracing::error!(...); panic!(...) })`
   *only* when `SPUR_LICENSE_DEV_PLAN` is set. Production paths that
   resolve "community" or "pro" still degrade gracefully on a
   genuinely malformed policy.

   **Pro:** any dev-machine misconfig surfaces immediately.
   **Con:** still won't actually let a dev preview Enterprise.

**B. Add `enterprise` (and `team`) tier blocks to the embedded policy.**
   Touch: `crates/spur-license/resources/default_policy.json` —
   under `tier_policies`, add `enterprise` and `team` blocks that
   inherit from `pro` and add their respective feature sets. Even
   if the feature set is just `["@inherit:pro"]` for now, the dev
   override resolves correctly and Plan E.h has somewhere to land
   Team v2 stubs.

   **Pro:** dev override works; Plan E.h has a target; mirrors what
   we'll need for production once Team/Enterprise activations exist.
   **Con:** changes the signed policy → re-sign required (use
   `scripts/sign-policy.sh`), and `build.rs` schema check must
   tolerate the new tier strings.

**Recommendation: B + A.** B is the right product-correct fix; A is
the right defense-in-depth complement so future adapter splits
(e.g., `team_internal` for organization-only feature variants)
fail-fast on naming drift instead of silently producing empty tiers.

## Out of scope here

- Defining Enterprise's feature delta vs Pro. That's product work,
  belongs in a future tier-design pass. For the policy-gap fix,
  `["@inherit:pro"]` is enough to make the dev override functional.
- Activation-server changes. Enterprise activation is a v2 problem
  parked alongside Team; the policy gap fix only addresses the
  client-side signed-policy shape.

## Severity

Low for production users (the dev override is debug-only and not
present in release builds — see `community.rs:43`
`#[cfg(not(debug_assertions))]`). Medium for SPUR contributors
(silently broken dev affordance + future drift risk in Plan E.h).

## References

- M0 finding: commit `5e67e1d2` test-isolation fix
- Source: `crates/spur-license/src/community.rs:36-44` (dev override)
- Source: `crates/spur-license/src/lib.rs:60-69` (Plan enum)
- Policy: `crates/spur-license/resources/default_policy.json`
  (`tier_policies` block — only `community` + `pro` defined)
- Adjacent: Plan E.h hygiene plan (Team v2 stubs) — same root cause
