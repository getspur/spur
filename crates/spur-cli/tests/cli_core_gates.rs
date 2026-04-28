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
fn empty_pro_gate_blocks_all_8_m0_cli_core_keys() {
    let gate = empty_pro_gate();
    for &key in M0_GATED_KEYS {
        let err = require_feature(&gate, key).expect_err(&format!(
            "tampered policy that strips {} must block dispatch",
            key.as_str(),
        ));
        // Verify the typed contract Plan D D.6 will rely on.
        // External crate + `#[non_exhaustive]` ⇒ pattern is refutable;
        // use `let ... else` form.
        let spur_license::FeatureGateError::Denied {
            key: returned_key, ..
        } = err
        else {
            panic!("expected Denied, got {err:?}");
        };
        assert_eq!(returned_key, key);
    }
}

#[test]
fn auth_arm_remains_ungated_in_m0() {
    // Documents the M0 Special Case: CLI_CORE_LICENSE_ACTIVATE is
    // not enforced at dispatch. If/when this changes (M0.5), this
    // test should be updated or deleted.
    //
    // The community gate still grants the key; the registry presence
    // does not imply runtime enforcement.
    let gate = community_gate();
    assert!(gate.has(FeatureKey::CLI_CORE_LICENSE_ACTIVATE));
}
