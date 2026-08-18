//! Idempotent solve-result cache keyed by a stable request fingerprint.
//!
//! Timeouts, `include_smt`, `persist`, session fields, and `use_cache` are
//! excluded from the key so cache hits remain model-equivalent. Session-bound
//! solves never consult the cache.

use std::{
    collections::{hash_map::DefaultHasher, HashMap, VecDeque},
    hash::{Hash, Hasher},
    sync::Mutex,
};

use serde::Serialize;
use serde_json::Value;

use crate::types::SolveConstraintsResponse;

/// Maximum number of distinct request fingerprints retained.
pub const MAX_CACHE_ENTRIES: usize = 128;

/// Process-wide solve cache (FIFO eviction past capacity).
#[derive(Debug, Default)]
pub struct SolveCache {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    order: VecDeque<String>,
    entries: HashMap<String, SolveConstraintsResponse>,
}

impl SolveCache {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Looks up a previously computed response for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<SolveConstraintsResponse> {
        let guard = self.inner.lock().ok()?;
        guard.entries.get(key).cloned()
    }

    /// Inserts `response` under `key`, evicting oldest entries past capacity.
    pub fn insert(&self, key: String, response: SolveConstraintsResponse) {
        let Ok(mut guard) = self.inner.lock() else {
            return;
        };
        if guard.entries.contains_key(&key) {
            guard.entries.insert(key, response);
            return;
        }
        while guard.order.len() >= MAX_CACHE_ENTRIES {
            if let Some(old) = guard.order.pop_front() {
                guard.entries.remove(&old);
            } else {
                break;
            }
        }
        guard.order.push_back(key.clone());
        guard.entries.insert(key, response);
    }
}

/// Stable fingerprint of the model-relevant request fields.
///
/// # Errors
///
/// Returns an error string when serialization fails.
pub fn fingerprint_request<T: Serialize>(request: &T) -> Result<String, String> {
    let value = serde_json::to_value(request).map_err(|error| error.to_string())?;
    let stripped = strip_ephemeral_fields(value);
    let canonical = serde_json::to_vec(&stripped).map_err(|error| error.to_string())?;
    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    Ok(format!("fp:{:016x}", hasher.finish()))
}

fn strip_ephemeral_fields(mut value: Value) -> Value {
    if let Value::Object(map) = &mut value {
        for key in [
            "timeout_ms",
            "persist",
            "include_smt",
            "session_id",
            "session_op",
            "use_cache",
        ] {
            map.remove(key);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{fingerprint_request, SolveCache, MAX_CACHE_ENTRIES};
    use crate::types::{
        ConstraintExpr, ObjectivePriority, SessionOp, SolveConstraintsRequest,
        SolveConstraintsResponse, SolveStatus, Variable, DEFAULT_MAX_SOLUTIONS, DEFAULT_TIMEOUT_MS,
    };

    fn sample_request(timeout_ms: u64) -> SolveConstraintsRequest {
        SolveConstraintsRequest {
            vars: vec![Variable::Int {
                name: "x".to_owned(),
            }],
            constraints: vec![ConstraintExpr::Bool { value: true }.into()],
            objectives: vec![],
            objective_priority: ObjectivePriority::Lex,
            max_solutions: DEFAULT_MAX_SOLUTIONS,
            timeout_ms,
            persist: false,
            include_smt: false,
            use_cache: true,
            session_id: None,
            session_op: SessionOp::None,
        }
    }

    #[test]
    fn fingerprint_ignores_timeout_and_persist() {
        let a = sample_request(1_000);
        let mut b = sample_request(60_000);
        b.persist = true;
        b.include_smt = true;
        assert_eq!(
            fingerprint_request(&a).unwrap(),
            fingerprint_request(&b).unwrap()
        );
    }

    #[test]
    fn cache_evicts_oldest_when_full() {
        let cache = SolveCache::new();
        for index in 0..MAX_CACHE_ENTRIES + 5 {
            let key = format!("k{index}");
            cache.insert(
                key,
                SolveConstraintsResponse {
                    status: SolveStatus::Unsat,
                    model: None,
                    duration_ms: 1,
                    solve_id: None,
                    reason: None,
                    smt: None,
                    unsat_core: None,
                    cached: false,
                    session_id: None,
                    optimization: None,
                    solver_version: None,
                },
            );
        }
        assert!(cache.get("k0").is_none());
        assert!(cache.get(&format!("k{}", MAX_CACHE_ENTRIES + 4)).is_some());
        let _ = DEFAULT_TIMEOUT_MS;
    }
}
