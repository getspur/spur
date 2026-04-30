// Plan C M0 (wave C.1) — parameterized invariants for the 8 cli_core_*
// keys gated at dispatch entry. Mirrors the existing `tests/pm_gate.rs`
// shape (community policy + FeatureGate + assertion).
//
// Note: CLI_CORE_LICENSE_ACTIVATE is intentionally absent from the
// invariant. Per the M0 plan Special Case, it remains in the typed
// registry but is not enforced at dispatch in M0; enforcement lands
// inside `auth::run` on the `Login` variant only (follow-up M0.5).

use spur_license::policy::PolicyResolver;
use spur_license::{require_feature, FeatureGate, FeatureKey, LicenseState, Plan};
use std::collections::BTreeSet;

const M0_GATED_KEYS: &[FeatureKey] = &[
    FeatureKey::CLI_CORE_INIT,
    FeatureKey::CLI_CORE_AGENTS,
    FeatureKey::CLI_CORE_RUN,
    FeatureKey::CLI_CORE_EXEC,
    FeatureKey::CLI_CORE_SESSIONS,
    FeatureKey::CLI_CORE_COST,
    FeatureKey::CLI_CORE_CONNECT,
    FeatureKey::CLI_CORE_TUI,
];

fn community_gate() -> FeatureGate {
    FeatureGate::new(PolicyResolver::embedded())
}

fn empty_pro_gate() -> FeatureGate {
    let g = FeatureGate::new(PolicyResolver::embedded());
    g.update_state(&LicenseState::active_validated(Plan::Pro, BTreeSet::new()));
    g
}

#[test]
fn embedded_community_policy_grants_all_8_m0_cli_core_keys() {
    let gate = community_gate();
    for &key in M0_GATED_KEYS {
        assert!(
            gate.has(key),
            "embedded community policy must grant {} so daily-driver Free \
             users are not blocked at CLI dispatch",
            key.as_str(),
        );
        assert!(
            require_feature(&gate, key).is_ok(),
            "require_feature must accept {} on community tier",
            key.as_str(),
        );
    }
}

#[test]
fn empty_pro_gate_still_inherits_all_8_m0_cli_core_keys_from_community() {
    // Pro/Team/Enterprise inherit the Community baseline via the policy's
    // `@inherit:community` directive. Even when the license-server JWT
    // returns zero entitlements (the `BTreeSet::new()` fixture below),
    // every CLI command gated on a community key — including
    // `cli_core_tui` — must still dispatch. Otherwise activating a Pro
    // license would *remove* daily-driver functionality that the Free
    // tier already had, which is the regression spotted by
    // `spur tui --brain claude-code` returning
    // `feature 'cli_core_tui' is not available on tier 'Pro'`.
    //
    // Tampered-policy denial is still exercised at the binary level by
    // `cli_core_gate_e2e` via `SPUR_LICENSE_TEST_STRIP_KEYS`; that path
    // strips keys *after* policy resolution, which is the correct shape
    // for adversarial tampering.
    let gate = empty_pro_gate();
    for &key in M0_GATED_KEYS {
        assert!(
            gate.has(key),
            "Pro tier must inherit {} from the Community baseline even \
             when the license JWT carries no entitlements",
            key.as_str(),
        );
        assert!(
            require_feature(&gate, key).is_ok(),
            "require_feature must accept {} on Pro tier via @inherit:community",
            key.as_str(),
        );
    }
}

#[test]
fn auth_login_is_gated_inside_auth_run_in_m0p5() {
    // Plan C M0.5 — `CLI_CORE_LICENSE_ACTIVATE` is now gated, but
    // not at the dispatch level. Enforcement lives inside
    // `crates/spur-cli/src/commands/auth.rs::login_inner` so that
    // Logout / Refresh / Status remain ungated and the brick-recovery
    // path stays open for tampered tiers.
    //
    // Registry assertion: community gate must still grant the key
    // so daily-driver Free users can run `spur auth login` against
    // a fresh community-policy install.
    let gate = community_gate();
    assert!(gate.has(FeatureKey::CLI_CORE_LICENSE_ACTIVATE));

    // The "denial returns FeatureGateError::Denied" invariant is
    // exercised at the binary boundary by
    // `cli_core_gate_e2e::spur_auth_login_exits_nonzero_*` and at
    // the in-process boundary by
    // `auth_fake_provider::login_blocked_by_empty_pro_gate_*`.
    // Not duplicated here because this file is a registry-level
    // invariant test; auth-flow specifics belong in those files.
}
