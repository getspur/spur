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

/// Post-price a turn from observed tokens × published rates.
///
/// Returns `None` when `model` is missing or unknown. Does **not** invent a
/// duration/$tier number — LiteLLM / provider billing never use wall clock
/// as a token-cost substitute. Callers must persist and emit JSON `null`
/// (print `NaN`) when this returns `None`.
///
/// `tier` and `duration` are accepted for call-site compatibility and ignored.
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
/// assert!(cost.expect("gpt-5 is registered") > 0.0);
/// ```
pub fn estimate_cost_from_tokens(
    _tier: CostTier,
    _duration: Duration,
    usage: TokenUsage,
    model: Option<&str>,
) -> Option<f64> {
    let model_name = model.filter(|m| !m.is_empty())?;
    let registry = PricingRegistry::with_builtin_prices();
    calculate_cost_for_model(usage, model_name, &registry)
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
        )
        .expect("gpt-5 is registered");
        // Token-based should produce a different value than time-based
        let time_based = estimate_cost(CostTier::Medium, Duration::from_secs(60));
        assert_ne!(cost, time_based);
        // gpt-5: 1M input @ $1.25/M + 500K output @ $10/M = $1.25 + $5.00 = $6.25
        assert!((cost - 6.25).abs() < 0.01, "cost={cost}, expected ~6.25");
    }

    #[test]
    fn unknown_model_is_none_not_duration() {
        let usage = TokenUsage {
            input_tokens: 6_964,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 128,
        };
        let cost = estimate_cost_from_tokens(
            CostTier::Medium,
            Duration::from_secs(4),
            usage,
            Some("unknown-model"),
        );
        assert_eq!(cost, None);
        assert_ne!(
            4.0 * 0.003,
            0.0,
            "duration heuristic must not be used as a stand-in"
        );
    }

    #[test]
    fn missing_model_is_none_not_duration() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = estimate_cost_from_tokens(CostTier::Low, Duration::from_secs(120), usage, None);
        assert_eq!(cost, None);
    }

    #[test]
    fn grok_46_prices_uncached_input_plus_cache_read() {
        // docs.x.ai grok-4.6 <200k: $2 / $0.50 cached / $6 out per 1M.
        // input includes cache (15271 = 3623 uncached + 11648 cached).
        let usage = TokenUsage {
            input_tokens: 15_271,
            output_tokens: 32,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 11_648,
        };
        let cost = estimate_cost_from_tokens(
            CostTier::Medium,
            Duration::from_secs(4),
            usage,
            Some("grok-4.6-build"),
        )
        .expect("grok-4.6 must be in the registry");
        let expected =
            3_623.0 * 2.0 / 1_000_000.0 + 11_648.0 * 0.50 / 1_000_000.0 + 32.0 * 6.0 / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 1e-9,
            "cost={cost} expected={expected}"
        );
        assert!((cost - 0.013262).abs() < 1e-9, "cost={cost}");
    }

    #[test]
    fn glm_52_zai_plan_id_prices_from_tokens() {
        // Z.ai / OpenCode Zen GLM-5.2: $1.40 / $0.26 cached / $4.40 out per 1M.
        let usage = TokenUsage {
            input_tokens: 6_964,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 128,
        };
        let cost = estimate_cost_from_tokens(
            CostTier::Medium,
            Duration::from_secs(4),
            usage,
            Some("zai-coding-plan/glm-5.2"),
        )
        .expect("glm-5.2 must resolve through provider prefix");
        let expected =
            6_836.0 * 1.40 / 1_000_000.0 + 128.0 * 0.26 / 1_000_000.0 + 3.0 * 4.40 / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 1e-9,
            "cost={cost} expected={expected}"
        );
    }
}
