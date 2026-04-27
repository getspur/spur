//! Library target for spur-cli. Used by integration tests in tests/.
//! The production binary entrypoint stays in `src/main.rs`.

pub mod commands;

pub fn pm_service_gate_allows_construction(gate: &spur_license::FeatureGate) -> bool {
    gate.has(spur_license::FeatureKey::PM_CORE_BROWSE)
}
