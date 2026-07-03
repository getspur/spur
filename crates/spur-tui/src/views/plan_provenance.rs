use spur_acp::PlanLoopOriginEvent;

pub(crate) fn loop_origin_badge(origin: &PlanLoopOriginEvent) -> String {
    format!("⟳ gen {}", origin.generation)
}
