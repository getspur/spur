use spur_acp::CostTier;
use std::time::Duration;

use crate::pricing::{calculate_cost_for_model, PricingRegistry, TokenUsage};

/// Estimate cost in USD based on agent cost tier and session duration.
///
/// This is a coarse time-based heuristic kept for backward compatibility.
/// For accurate costs, prefer [`estimate_cost_from_tokens`].
pub fn estimate_cost(tier: CostTier, duration: Duration) -> f64 {
    let rate = match tier {
        CostTier::High => 0.008,   // ~$0.50/min (Claude Opus)
        CostTier::Medium => 0.003, // ~$0.18/min (Kiro/Sonnet)
        CostTier::Low => 0.001,    // ~$0.06/min (Codex/Gemini)
    };
    duration.as_secs_f64() * rate
}

/// Estimate cost from actual token usage and an optional model name.
///
/// If `model` is provided and found in the built-in pricing registry,
/// the cost is calculated from per-token rates. Otherwise falls back to
/// the time-based [`estimate_cost`] heuristic.
///
/// # Example
/// ```
/// use spur_acp::CostTier;
/// use std::time::Duration;
/// use spur_cost::estimator::estimate_cost_from_tokens;
/// use spur_cost::pricing::TokenUsage;
///
/// let usage = TokenUsage {
///     input_tokens: 1_000,
///     output_tokens: 500,
///     cache_creation_input_tokens: 0,
///     cache_read_input_tokens: 200,
/// };
/// let cost = estimate_cost_from_tokens(
///     CostTier::Medium,
///     Duration::from_secs(60),
///     usage,
///     Some("gpt-5"),
/// );
/// assert!(cost > 0.0);
/// ```
pub fn estimate_cost_from_tokens(
    tier: CostTier,
    duration: Duration,
    usage: TokenUsage,
    model: Option<&str>,
) -> f64 {
    // If we have a model name, try token-based calculation first
    if let Some(model_name) = model {
        let registry = PricingRegistry::with_builtin_prices();
        if let Some(cost) = calculate_cost_for_model(usage, model_name, &registry) {
            return cost;
        }
    }

    // Fall back to time-based estimation when tokens or model are unavailable
    estimate_cost(tier, duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_based_estimation() {
        let duration = Duration::from_secs(60);
        assert_eq!(estimate_cost(CostTier::High, duration), 60.0 * 0.008);
        assert_eq!(estimate_cost(CostTier::Medium, duration), 60.0 * 0.003);
        assert_eq!(estimate_cost(CostTier::Low, duration), 60.0 * 0.001);
    }

    #[test]
    fn test_token_based_override() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = estimate_cost_from_tokens(
            CostTier::Medium,
            Duration::from_secs(60),
            usage,
            Some("gpt-5"),
        );
        // Token-based should produce a different value than time-based
        let time_based = estimate_cost(CostTier::Medium, Duration::from_secs(60));
        assert_ne!(cost, time_based);
        // gpt-5: 1M input @ $1.25/M + 500K output @ $10/M = $1.25 + $5.00 = $6.25
        assert!((cost - 6.25).abs() < 0.01, "cost={cost}, expected ~6.25");
    }

    #[test]
    fn test_fallback_to_time_based() {
        let usage = TokenUsage::default();
        let cost = estimate_cost_from_tokens(
            CostTier::Medium,
            Duration::from_secs(60),
            usage,
            Some("unknown-model"),
        );
        assert_eq!(
            cost,
            estimate_cost(CostTier::Medium, Duration::from_secs(60))
        );
    }

    #[test]
    fn test_fallback_when_no_model() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = estimate_cost_from_tokens(CostTier::Low, Duration::from_secs(120), usage, None);
        assert_eq!(cost, estimate_cost(CostTier::Low, Duration::from_secs(120)));
    }
}
