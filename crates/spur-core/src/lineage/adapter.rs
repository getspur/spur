//! Fold legacy `SpurEvent` variants (BrainSpawned, WorkerSpawned,
//! DelegationRequested/Completed, SessionCompleted, CostUpdate) into the
//! projection. Implemented in Task 5.

use spur_acp::SpurEvent;

use super::projection::ExecutorLineage;

pub fn apply_legacy(_lineage: &mut ExecutorLineage, _event: &SpurEvent) {
    // implemented in Task 5
}
