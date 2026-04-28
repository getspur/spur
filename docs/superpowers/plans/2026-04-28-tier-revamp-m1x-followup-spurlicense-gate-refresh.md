# Tier Revamp Follow-up M1.x — `SpurLicense::feature_gate()` staleness

**Status:** Open follow-up. Filed 2026-04-28 from codex's grounding
review of Plan C M1 scope.
**Priority:** Medium. Latent bug affecting non-TUI consumers
(`license.feature_gate()` returns stale `Arc<FeatureGate>`). M1's
TUI-specific fix does not address this.

## The gap

`SpurLicense.feature_gate` is initialized fresh at construction
time (from `provider.current_state()` at
`crates/spur-license/src/lib.rs:213, 236, 247`) but is **never
refreshed afterward**. The mutating methods on `SpurLicense`:

- `SpurLicense::validate()` (`lib.rs:282`)
- `SpurLicense::heartbeat()` (`lib.rs:286`)
- `SpurLicense::activate()`
- `SpurLicense::deactivate()`

…all delegate to the provider (`Arc<dyn LicenseProvider>`). The
provider mutates provider-local state via `replace_state()` at
`crates/spur-license/src/licenseseat.rs:84`, but that method
**never touches `SpurLicense.feature_gate`**.

Result: `license.feature_gate()` returns a stale `Arc<FeatureGate>`
to all non-TUI consumers. After a `spur auth login` or trial
activation, these consumers continue gate-checking against the
startup-time entitlement set, denying capabilities the user has
just legitimately acquired.

## Affected consumers

Confirmed call sites of `license.feature_gate()` outside spur-tui:

1. **CLI PM construction:** `crates/spur-cli/src/main.rs:808, 828`
   — used by orchestrator wiring. Stale gate here means a Pro user
   who activated mid-session continues to be treated as community
   for any CLI subcommand that re-uses the SpurLicense.
2. *(Any other consumers — survey before fixing)*

## Why this is NOT in M1

M1 fixes the TUI-specific gate (`App::feature_gate`) by pumping
`update_state` from the existing `LicenseUpdated` event handler
(`update_license_state`). The TUI has a clean event subscription
already wired through `run_tui_with_license`'s broadcast receiver.

The CLI / non-TUI consumers do NOT have an analogous event
subscription. Fixing them requires one of:

- **(α)** Make `SpurLicense::validate/heartbeat/activate/deactivate`
  call `self.feature_gate.update_state(&new_state)` after the
  provider returns successfully. Centralizes the refresh in the
  facade. **Recommended.**
- **(β)** Make `LicenseProvider::replace_state` (or similar) emit
  the gate update through a side channel. More invasive.
- **(γ)** Add a long-lived background task that polls
  `provider.current_state()` and pushes to a shared gate. Worst
  option (polling).

Per option (α), the fix is small (~4 method changes in `lib.rs`,
each adding `self.feature_gate.update_state(&new_state)` after the
provider call returns Ok). But it requires:

1. Establishing the regression-test surface (existing tests for
   SpurLicense don't currently assert gate freshness).
2. Auditing all `license.feature_gate()` call sites outside spur-tui
   to confirm they actually need fresh state (or if they only run
   at startup, the stale snapshot is harmless).
3. Verifying no panic / borrow / Send-Sync issues across the
   `&self` × `Arc<FeatureGate>` boundary.

## Proposed fix shape (α)

```rust
// crates/spur-license/src/lib.rs

impl SpurLicense {
    pub async fn validate(&self) -> Result<LicenseState, ProviderError> {
        let new_state = self.provider.validate().await?;
        self.feature_gate.update_state(&new_state);
        Ok(new_state)
    }

    pub async fn heartbeat(&self) -> Result<LicenseState, ProviderError> {
        let new_state = self.provider.heartbeat().await?;
        self.feature_gate.update_state(&new_state);
        Ok(new_state)
    }

    pub async fn activate(&self, key: &str) -> Result<LicenseState, ProviderError> {
        let new_state = self.provider.activate(key).await?;
        self.feature_gate.update_state(&new_state);
        Ok(new_state)
    }

    pub async fn deactivate(&self) -> Result<LicenseState, ProviderError> {
        let new_state = self.provider.deactivate().await?;
        self.feature_gate.update_state(&new_state);
        Ok(new_state)
    }
}
```

(Verify exact method signatures from `lib.rs:213-300` before
implementing — `replace_state` may already happen async, return
shapes may differ, etc.)

## Acceptance criteria

- [ ] `SpurLicense::validate/heartbeat/activate/deactivate` each
      refresh `self.feature_gate` after provider call succeeds.
- [ ] Unit test: construct `SpurLicense` at Community baseline,
      assert Pro key denied; mock provider to return Pro state;
      call `validate`; assert Pro key now granted.
- [ ] Existing tests pass without modification (no behavioral
      change for callers that don't query `feature_gate()` after
      mutation).
- [ ] No regression in CLI PM construction (`main.rs:808, 828` —
      may need a CLI-side smoke test asserting Pro entitlements
      take effect after `spur auth login`).
- [ ] Document the freshness contract in `SpurLicense`'s public
      doc-comment.

## Out of scope for this follow-up

- TUI-specific fix (already handled in M1).
- Provider-internal refactor (broker pattern, etc.).
- Adding a global subscription mechanism for gate updates.
- Making `FeatureGate` itself observable.

## When to land

After Plan C M1 ships. Bundle with:
- Plan D trial JWT impl (which will exercise the activate path
  heavily; staleness here would block trial activation in CLI
  contexts).
- OR a dedicated small commit if Plan D is delayed beyond a
  quarter.

## References

- Source of finding: codex grounding review for Plan C M1
  (`spur://continuation/fa24c352-1612-4942-8c6a-5ae9b38c32e7`).
- M1 plan:
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-m1-tui-gate-refresh.md`
- `SpurLicense` definition: `crates/spur-license/src/lib.rs:213-216`
- `SpurLicense::feature_gate()`: `crates/spur-license/src/lib.rs:254-256`
- `LicenseSeatProvider::replace_state`: `crates/spur-license/src/licenseseat.rs:84`
- Affected consumers: `crates/spur-cli/src/main.rs:808, 828`
