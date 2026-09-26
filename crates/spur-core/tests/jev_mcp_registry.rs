use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};

use spur_acp::config::ContextServiceConfig;
use spur_core::mcp::delegation::DelegationMcpDeps;
use spur_core::mcp::plan::PlanMcpDeps;
use spur_core::mcp::signals::SignalMcpDeps;
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};

const SOLVER_TOOLS: &[&str] = &[
    "solve_rule_spec",
    "solve_rules",
    "solve_constraint_spec",
    "solve_constraint_check",
    "solve_constraints",
    "solve_smt",
    "get_solve_result",
];

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct JevFlagGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl JevFlagGuard {
    fn acquire() -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            previous: std::env::var("SPUR_JEV_ENABLED").ok(),
            _lock: lock,
        }
    }

    fn set(&self, value: Option<&str>) {
        let _held_lock = &self._lock;
        match value {
            Some(value) => std::env::set_var("SPUR_JEV_ENABLED", value),
            None => std::env::remove_var("SPUR_JEV_ENABLED"),
        }
    }
}

impl Drop for JevFlagGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("SPUR_JEV_ENABLED", value),
            None => std::env::remove_var("SPUR_JEV_ENABLED"),
        }
    }
}

fn feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_owned()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

fn brain_tool_names() -> Vec<String> {
    let registry = spur_core::mcp::brain_tool_registry(
        DelegationMcpDeps::catalog_only(),
        PlanMcpDeps::catalog_only(),
        SignalMcpDeps {
            pm_service: None,
            event_sink: None,
            feature_gate: feature_gate(),
        },
        &ContextServiceConfig {
            url: String::new(),
            ..ContextServiceConfig::default()
        },
    )
    .expect("brain registry");
    registry
        .list_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect()
}

fn solver_tool_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| SOLVER_TOOLS.contains(&name.as_str()) || name.as_str() == "jev_compile")
        .cloned()
        .collect()
}

#[test]
fn jev_compile_is_flagged_brain_only_solver_tool() {
    let flag = JevFlagGuard::acquire();

    flag.set(None);
    let disabled = solver_tool_names(&brain_tool_names());
    assert_eq!(disabled, SOLVER_TOOLS, "flag-off solver catalog drifted");

    flag.set(Some("1"));
    let enabled = solver_tool_names(&brain_tool_names());
    assert_eq!(enabled.len(), SOLVER_TOOLS.len() + 1);
    assert_eq!(enabled.last().map(String::as_str), Some("jev_compile"));

    let worker_names: Vec<String> = spur_core::mcp::worker_tools_list()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert!(
        worker_names.iter().all(|name| name != "jev_compile"),
        "jev_compile must never be exposed to workers"
    );
}
