use spur_acp::CostTier;
use std::time::Duration;

/// Estimate cost in USD based on agent cost tier and session duration.
pub fn estimate_cost(tier: CostTier, duration: Duration) -> f64 {
    let rate = match tier {
        CostTier::High => 0.008,   // ~$0.50/min (Claude Opus)
        CostTier::Medium => 0.003, // ~$0.18/min (Kiro/Sonnet)
        CostTier::Low => 0.001,    // ~$0.06/min (Codex/Gemini)
    };
    duration.as_secs_f64() * rate
}
