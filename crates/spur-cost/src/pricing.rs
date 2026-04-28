//! Model pricing registry for token-based cost calculation.
//!
//! Inspired by LiteLLM's pricing schema (ccusage reference implementation).
//! Each model has per-token rates for input, output, cache creation, and cache read.
//! Supports tiered pricing for large-context models (e.g., Claude 1M context window).

use std::collections::HashMap;

const KIMI_INPUT_PRICE_PER_MTOK: f64 = 0.60;
const KIMI_OUTPUT_PRICE_PER_MTOK: f64 = 2.50;
const KIMI_CACHE_READ_PRICE_PER_MTOK: f64 = 0.15;
const KIMI_CACHE_CREATE_PRICE_PER_MTOK: f64 = 0.0;

// ─── Types ────────────────────────────────────────────────────────────

/// Per-model pricing rates in USD per token.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    /// Cost per input token (USD).
    pub input_cost_per_token: f64,
    /// Cost per output token (USD).
    pub output_cost_per_token: f64,
    /// Cost per cache-creation input token (USD).
    pub cache_creation_input_token_cost: f64,
    /// Cost per cache-read input token (USD).
    pub cache_read_input_token_cost: f64,
    /// Optional tiered pricing for tokens above a threshold (e.g., 200k).
    pub tiered: Option<TieredPricing>,
}

/// Tiered pricing for large-context models.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TieredPricing {
    /// Token threshold above which tiered rates apply.
    pub threshold: u64,
    /// Cost per input token above the threshold (USD).
    pub input_cost_per_token_above: f64,
    /// Cost per output token above the threshold (USD).
    pub output_cost_per_token_above: f64,
    /// Cost per cache-creation input token above the threshold (USD).
    pub cache_creation_input_token_cost_above: f64,
    /// Cost per cache-read input token above the threshold (USD).
    pub cache_read_input_token_cost_above: f64,
}

/// Token usage breakdown for a single request or session.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenUsage {
    /// Input (prompt) tokens.
    pub input_tokens: u64,
    /// Output (generated) tokens. Includes reasoning tokens if the provider
    /// bundles them; do not add reasoning tokens separately.
    pub output_tokens: u64,
    /// Cache-creation input tokens.
    pub cache_creation_input_tokens: u64,
    /// Cache-read input tokens.
    pub cache_read_input_tokens: u64,
}

// ─── Built-in Pricing Registry ────────────────────────────────────────

/// A registry of known model prices, embeddable for offline use.
#[derive(Debug, Clone)]
pub struct PricingRegistry {
    models: HashMap<String, ModelPricing>,
    aliases: HashMap<String, String>,
}

impl Default for PricingRegistry {
    fn default() -> Self {
        Self::with_builtin_prices()
    }
}

impl PricingRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            models: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    /// Create a registry pre-loaded with a curated set of known model prices.
    ///
    /// Prices are sourced from LiteLLM and rounded to the precision used by
    /// major providers (April 2026). Update these as provider pricing changes.
    pub fn with_builtin_prices() -> Self {
        let mut reg = Self::new();

        // ─── Anthropic / Claude ─────────────────────────────────────
        reg.insert(
            "claude-opus-4",
            ModelPricing {
                input_cost_per_token: 15.0 / 1_000_000.0,
                output_cost_per_token: 75.0 / 1_000_000.0,
                cache_creation_input_token_cost: 18.75 / 1_000_000.0,
                cache_read_input_token_cost: 1.50 / 1_000_000.0,
                tiered: Some(TieredPricing {
                    threshold: 200_000,
                    input_cost_per_token_above: 30.0 / 1_000_000.0,
                    output_cost_per_token_above: 150.0 / 1_000_000.0,
                    cache_creation_input_token_cost_above: 37.50 / 1_000_000.0,
                    cache_read_input_token_cost_above: 3.00 / 1_000_000.0,
                }),
            },
        );
        reg.insert(
            "claude-sonnet-4",
            ModelPricing {
                input_cost_per_token: 3.0 / 1_000_000.0,
                output_cost_per_token: 15.0 / 1_000_000.0,
                cache_creation_input_token_cost: 3.75 / 1_000_000.0,
                cache_read_input_token_cost: 0.30 / 1_000_000.0,
                tiered: Some(TieredPricing {
                    threshold: 200_000,
                    input_cost_per_token_above: 6.0 / 1_000_000.0,
                    output_cost_per_token_above: 22.50 / 1_000_000.0,
                    cache_creation_input_token_cost_above: 7.50 / 1_000_000.0,
                    cache_read_input_token_cost_above: 0.60 / 1_000_000.0,
                }),
            },
        );
        reg.insert(
            "claude-haiku-4",
            ModelPricing {
                input_cost_per_token: 0.50 / 1_000_000.0,
                output_cost_per_token: 2.50 / 1_000_000.0,
                cache_creation_input_token_cost: 0.625 / 1_000_000.0,
                cache_read_input_token_cost: 0.05 / 1_000_000.0,
                tiered: None,
            },
        );
        // TODO: verify 4.5 pricing differs from 4
        reg.insert(
            "claude-opus-4-5",
            ModelPricing {
                input_cost_per_token: 15.0 / 1_000_000.0,
                output_cost_per_token: 75.0 / 1_000_000.0,
                cache_creation_input_token_cost: 18.75 / 1_000_000.0,
                cache_read_input_token_cost: 1.50 / 1_000_000.0,
                tiered: Some(TieredPricing {
                    threshold: 200_000,
                    input_cost_per_token_above: 30.0 / 1_000_000.0,
                    output_cost_per_token_above: 150.0 / 1_000_000.0,
                    cache_creation_input_token_cost_above: 37.50 / 1_000_000.0,
                    cache_read_input_token_cost_above: 3.00 / 1_000_000.0,
                }),
            },
        );
        reg.insert(
            "claude-sonnet-4-5",
            ModelPricing {
                input_cost_per_token: 3.0 / 1_000_000.0,
                output_cost_per_token: 15.0 / 1_000_000.0,
                cache_creation_input_token_cost: 3.75 / 1_000_000.0,
                cache_read_input_token_cost: 0.30 / 1_000_000.0,
                tiered: Some(TieredPricing {
                    threshold: 200_000,
                    input_cost_per_token_above: 6.0 / 1_000_000.0,
                    output_cost_per_token_above: 22.50 / 1_000_000.0,
                    cache_creation_input_token_cost_above: 7.50 / 1_000_000.0,
                    cache_read_input_token_cost_above: 0.60 / 1_000_000.0,
                }),
            },
        );
        reg.insert(
            "claude-haiku-4-5",
            ModelPricing {
                input_cost_per_token: 0.50 / 1_000_000.0,
                output_cost_per_token: 2.50 / 1_000_000.0,
                cache_creation_input_token_cost: 0.625 / 1_000_000.0,
                cache_read_input_token_cost: 0.05 / 1_000_000.0,
                tiered: None,
            },
        );

        // ─── OpenAI / GPT ───────────────────────────────────────────
        reg.insert(
            "gpt-5",
            ModelPricing {
                input_cost_per_token: 1.25 / 1_000_000.0,
                output_cost_per_token: 10.0 / 1_000_000.0,
                cache_creation_input_token_cost: 1.25 / 1_000_000.0,
                cache_read_input_token_cost: 0.125 / 1_000_000.0,
                tiered: None,
            },
        );
        reg.insert(
            "gpt-5-codex",
            ModelPricing {
                input_cost_per_token: 1.25 / 1_000_000.0,
                output_cost_per_token: 10.0 / 1_000_000.0,
                cache_creation_input_token_cost: 1.25 / 1_000_000.0,
                cache_read_input_token_cost: 0.125 / 1_000_000.0,
                tiered: None,
            },
        );
        reg.insert(
            "gpt-4o",
            ModelPricing {
                input_cost_per_token: 2.50 / 1_000_000.0,
                output_cost_per_token: 10.0 / 1_000_000.0,
                cache_creation_input_token_cost: 2.50 / 1_000_000.0,
                cache_read_input_token_cost: 1.25 / 1_000_000.0,
                tiered: None,
            },
        );
        reg.insert(
            "gpt-4o-mini",
            ModelPricing {
                input_cost_per_token: 0.15 / 1_000_000.0,
                output_cost_per_token: 0.60 / 1_000_000.0,
                cache_creation_input_token_cost: 0.15 / 1_000_000.0,
                cache_read_input_token_cost: 0.075 / 1_000_000.0,
                tiered: None,
            },
        );

        // ─── Google / Gemini ────────────────────────────────────────
        reg.insert(
            "gemini-2.5-pro",
            ModelPricing {
                input_cost_per_token: 1.25 / 1_000_000.0,
                output_cost_per_token: 10.0 / 1_000_000.0,
                cache_creation_input_token_cost: 1.25 / 1_000_000.0,
                cache_read_input_token_cost: 0.125 / 1_000_000.0,
                tiered: None,
            },
        );
        reg.insert(
            "gemini-2.5-flash",
            ModelPricing {
                input_cost_per_token: 0.15 / 1_000_000.0,
                output_cost_per_token: 0.60 / 1_000_000.0,
                cache_creation_input_token_cost: 0.15 / 1_000_000.0,
                cache_read_input_token_cost: 0.075 / 1_000_000.0,
                tiered: None,
            },
        );

        // ─── Moonshot AI / Kimi ─────────────────────────────────────
        reg.insert(
            "kimi-for-coding",
            ModelPricing {
                input_cost_per_token: KIMI_INPUT_PRICE_PER_MTOK / 1_000_000.0,
                output_cost_per_token: KIMI_OUTPUT_PRICE_PER_MTOK / 1_000_000.0,
                cache_creation_input_token_cost: KIMI_CACHE_CREATE_PRICE_PER_MTOK / 1_000_000.0,
                cache_read_input_token_cost: KIMI_CACHE_READ_PRICE_PER_MTOK / 1_000_000.0,
                tiered: None,
            },
        );

        // ─── Aliases ────────────────────────────────────────────────
        // Codex aliases
        reg.alias("gpt-5.3-codex", "gpt-5-codex");
        // Claude aliases (common shorthand → canonical name)
        reg.alias("claude-opus", "claude-opus-4");
        reg.alias("claude-sonnet", "claude-sonnet-4");
        reg.alias("claude-haiku", "claude-haiku-4");

        reg
    }

    /// Insert or overwrite pricing for a model.
    pub fn insert(&mut self, model: &str, pricing: ModelPricing) {
        self.models.insert(model.to_lowercase(), pricing);
    }

    /// Register a model alias.
    pub fn alias(&mut self, alias: &str, canonical: &str) {
        self.aliases
            .insert(alias.to_lowercase(), canonical.to_lowercase());
    }

    /// Look up pricing for a model name, resolving aliases.
    ///
    /// The lookup is case-insensitive. If the exact model is not found,
    /// registered aliases are tried. Unknown models return `None`.
    pub fn get(&self, model: &str) -> Option<&ModelPricing> {
        if model.is_empty() {
            return None;
        }
        let key = model.to_lowercase();

        if let Some(p) = self.models.get(&key) {
            return Some(p);
        }

        if let Some(canon) = self.aliases.get(&key) {
            if let Some(p) = self.models.get(canon) {
                return Some(p);
            }
        }

        // Longest-prefix match with `-` or `.` (or end-of-string) as the
        // boundary. Real-world model names use both: `gpt-5.4` (dot as a
        // version separator) and `claude-opus-4-6` (dash). Accepting either
        // keeps post-cutoff names like `gpt-5.4-mini` → `gpt-5` and
        // `claude-opus-4-7` → `claude-opus-4` without requiring every
        // variant to be registered as an alias.
        let mut candidates: Vec<(&String, &ModelPricing)> = self.models.iter().collect();
        candidates.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()).then(a.cmp(b)));
        for (k, v) in candidates {
            let kb = k.as_bytes();
            if key.as_bytes().starts_with(kb)
                && (key.len() == kb.len()
                    || matches!(key.as_bytes().get(kb.len()), Some(&b'-') | Some(&b'.')))
            {
                return Some(v);
            }
        }

        None
    }

    /// Return a copy of all registered canonical model names.
    pub fn known_models(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }

    /// Return all (alias, canonical) pairs.
    pub fn aliases(&self) -> Vec<(String, String)> {
        self.aliases
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

// ─── Cost Calculation ─────────────────────────────────────────────────

/// Calculate cost in USD from token usage and model pricing.
///
/// Applies tiered pricing when the model's token count exceeds the
/// configured threshold. Cache-read tokens are capped at input tokens
/// to avoid billing more cache reads than total input.
///
/// # Example
/// ```
/// use spur_cost::pricing::{calculate_cost, ModelPricing, TokenUsage};
///
/// let pricing = ModelPricing {
///     input_cost_per_token: 3.0 / 1_000_000.0,
///     output_cost_per_token: 15.0 / 1_000_000.0,
///     cache_creation_input_token_cost: 3.75 / 1_000_000.0,
///     cache_read_input_token_cost: 0.30 / 1_000_000.0,
///     tiered: None,
/// };
/// let usage = TokenUsage {
///     input_tokens: 1_000,
///     output_tokens: 500,
///     cache_creation_input_tokens: 0,
///     cache_read_input_tokens: 200,
/// };
/// let cost = calculate_cost(usage, &pricing);
/// assert!(cost > 0.0);
/// ```
pub fn calculate_cost(usage: TokenUsage, pricing: &ModelPricing) -> f64 {
    let input_cost = tiered_token_cost(
        usage.input_tokens,
        pricing.input_cost_per_token,
        pricing
            .tiered
            .as_ref()
            .map(|t| (t.threshold, t.input_cost_per_token_above)),
    );

    let output_cost = tiered_token_cost(
        usage.output_tokens,
        pricing.output_cost_per_token,
        pricing
            .tiered
            .as_ref()
            .map(|t| (t.threshold, t.output_cost_per_token_above)),
    );

    let cache_creation_cost = tiered_token_cost(
        usage.cache_creation_input_tokens,
        pricing.cache_creation_input_token_cost,
        pricing
            .tiered
            .as_ref()
            .map(|t| (t.threshold, t.cache_creation_input_token_cost_above)),
    );

    // Cap cache-read tokens at input tokens to avoid over-billing
    let cache_read_tokens = usage.cache_read_input_tokens.min(usage.input_tokens);
    let cache_read_cost = tiered_token_cost(
        cache_read_tokens,
        pricing.cache_read_input_token_cost,
        pricing
            .tiered
            .as_ref()
            .map(|t| (t.threshold, t.cache_read_input_token_cost_above)),
    );

    input_cost + output_cost + cache_creation_cost + cache_read_cost
}

/// Look up a model in the registry and calculate its cost.
///
/// Returns `None` if the model is not found in the registry.
pub fn calculate_cost_for_model(
    usage: TokenUsage,
    model: &str,
    registry: &PricingRegistry,
) -> Option<f64> {
    registry
        .get(model)
        .map(|pricing| calculate_cost(usage, pricing))
}

// ─── Helpers ──────────────────────────────────────────────────────────

fn tiered_token_cost(tokens: u64, base_price: f64, tiered: Option<(u64, f64)>) -> f64 {
    if tokens == 0 || base_price <= 0.0 {
        return 0.0;
    }

    let Some((threshold, tiered_price)) = tiered else {
        return tokens as f64 * base_price;
    };

    if tokens <= threshold || tiered_price <= 0.0 {
        return tokens as f64 * base_price;
    }

    let below = threshold as f64 * base_price;
    let above = (tokens - threshold) as f64 * tiered_price;
    below + above
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sonnet_pricing() -> ModelPricing {
        ModelPricing {
            input_cost_per_token: 3.0 / 1_000_000.0,
            output_cost_per_token: 15.0 / 1_000_000.0,
            cache_creation_input_token_cost: 3.75 / 1_000_000.0,
            cache_read_input_token_cost: 0.30 / 1_000_000.0,
            tiered: Some(TieredPricing {
                threshold: 200_000,
                input_cost_per_token_above: 6.0 / 1_000_000.0,
                output_cost_per_token_above: 22.50 / 1_000_000.0,
                cache_creation_input_token_cost_above: 7.50 / 1_000_000.0,
                cache_read_input_token_cost_above: 0.60 / 1_000_000.0,
            }),
        }
    }

    #[test]
    fn test_basic_cost_calculation() {
        let pricing = ModelPricing {
            input_cost_per_token: 1.25e-6,
            output_cost_per_token: 10.0e-6,
            cache_creation_input_token_cost: 1.25e-6,
            cache_read_input_token_cost: 0.125e-6,
            tiered: None,
        };
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 200,
        };
        let cost = calculate_cost(usage, &pricing);
        let expected = 1_000.0 * 1.25e-6 + 500.0 * 10.0e-6 + 200.0 * 0.125e-6;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn test_tiered_pricing_above_threshold() {
        let pricing = sonnet_pricing();
        let usage = TokenUsage {
            input_tokens: 300_000,
            output_tokens: 250_000,
            cache_creation_input_tokens: 300_000,
            cache_read_input_tokens: 250_000,
        };
        let cost = calculate_cost(usage, &pricing);

        let expected_input = 200_000.0 * 3.0e-6 + 100_000.0 * 6.0e-6;
        let expected_output = 200_000.0 * 15.0e-6 + 50_000.0 * 22.50e-6;
        let expected_cache_creation = 200_000.0 * 3.75e-6 + 100_000.0 * 7.50e-6;
        let expected_cache_read = 200_000.0 * 0.30e-6 + 50_000.0 * 0.60e-6;
        let expected =
            expected_input + expected_output + expected_cache_creation + expected_cache_read;

        assert!(
            (cost - expected).abs() < 1e-9,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn test_tiered_pricing_at_boundary() {
        let pricing = sonnet_pricing();
        let usage = TokenUsage {
            input_tokens: 200_000,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = calculate_cost(usage, &pricing);
        let expected = 200_000.0 * 3.0e-6;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn test_tiered_pricing_one_token_above() {
        let pricing = sonnet_pricing();
        let usage = TokenUsage {
            input_tokens: 200_001,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = calculate_cost(usage, &pricing);
        let expected = 200_000.0 * 3.0e-6 + 1.0 * 6.0e-6;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn test_zero_tokens_returns_zero() {
        let pricing = sonnet_pricing();
        let usage = TokenUsage::default();
        assert_eq!(calculate_cost(usage, &pricing), 0.0);
    }

    #[test]
    fn test_cache_read_capped_at_input() {
        let pricing = ModelPricing {
            input_cost_per_token: 1.0e-6,
            output_cost_per_token: 0.0,
            cache_creation_input_token_cost: 0.0,
            cache_read_input_token_cost: 0.5e-6,
            tiered: None,
        };
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 500, // more than input
        };
        let cost = calculate_cost(usage, &pricing);
        // Should only bill 100 cache-read tokens, not 500
        let expected = 100.0 * 1.0e-6 + 100.0 * 0.5e-6;
        assert!(
            (cost - expected).abs() < 1e-12,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn test_registry_exact_lookup() {
        let reg = PricingRegistry::with_builtin_prices();
        assert!(reg.get("gpt-5").is_some());
        assert!(reg.get("claude-sonnet-4").is_some());
    }

    #[test]
    fn pricing_registry_includes_kimi_for_coding() {
        let pricing = PricingRegistry::with_builtin_prices();
        let entry = pricing.get("kimi-for-coding");
        assert!(entry.is_some(), "kimi-for-coding must be registered");
    }

    #[test]
    fn test_registry_alias_resolution() {
        let reg = PricingRegistry::with_builtin_prices();
        assert!(reg.get("claude-opus").is_some());
        assert!(reg.get("claude-sonnet").is_some());
    }

    #[test]
    fn test_registry_case_insensitive() {
        let reg = PricingRegistry::with_builtin_prices();
        assert!(reg.get("GPT-5").is_some());
        assert!(reg.get("Claude-Sonnet-4").is_some());
    }

    #[test]
    fn test_registry_longest_prefix_dash_or_dot_boundary() {
        let reg = PricingRegistry::with_builtin_prices();
        assert!(reg.get("claude-haiku-4-5-20251001").is_some());
        assert!(reg.get("gpt-5-codex-2026-preview").is_some());
        // Real post-cutoff names using dot as a version separator.
        assert!(reg.get("gpt-5.4").is_some(), "gpt-5.4 should match gpt-5");
        assert!(
            reg.get("gpt-5.4-mini").is_some(),
            "gpt-5.4-mini should match gpt-5"
        );
        assert!(reg.get("totally-unknown").is_none());
        assert!(reg.get("").is_none());

        let foo = ModelPricing {
            input_cost_per_token: 1.0,
            output_cost_per_token: 2.0,
            cache_creation_input_token_cost: 3.0,
            cache_read_input_token_cost: 4.0,
            tiered: None,
        };
        let foo_bar = ModelPricing {
            input_cost_per_token: 5.0,
            output_cost_per_token: 6.0,
            cache_creation_input_token_cost: 7.0,
            cache_read_input_token_cost: 8.0,
            tiered: None,
        };
        let mut r2 = PricingRegistry::new();
        r2.insert("foo", foo);
        r2.insert("foo-bar", foo_bar);
        assert_eq!(r2.get("foo-bar-baz"), r2.models.get("foo-bar"));

        let gpt_4 = ModelPricing {
            input_cost_per_token: 9.0,
            output_cost_per_token: 10.0,
            cache_creation_input_token_cost: 11.0,
            cache_read_input_token_cost: 12.0,
            tiered: None,
        };
        let mut r3 = PricingRegistry::new();
        r3.insert("gpt-4", gpt_4);
        assert!(
            r3.get("gpt-4o").is_none(),
            "gpt-4 must not swallow gpt-4o (different family — no dash/dot boundary)"
        );
        assert_eq!(r3.get("gpt-4-turbo"), r3.models.get("gpt-4"));
        assert_eq!(
            r3.get("gpt-4.1"),
            r3.models.get("gpt-4"),
            "dot boundary should admit gpt-4.1 as a gpt-4 variant"
        );
    }

    #[test]
    fn test_registry_unknown_model() {
        let reg = PricingRegistry::with_builtin_prices();
        assert!(reg.get("totally-unknown-model-xyz").is_none());
    }

    #[test]
    fn test_calculate_cost_for_model_found() {
        let reg = PricingRegistry::with_builtin_prices();
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = calculate_cost_for_model(usage, "gpt-5", &reg);
        assert!(cost.is_some());
        assert!(cost.unwrap() > 0.0);
    }

    #[test]
    fn test_calculate_cost_for_model_not_found() {
        let reg = PricingRegistry::with_builtin_prices();
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 500,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };
        let cost = calculate_cost_for_model(usage, "unknown-model", &reg);
        assert!(cost.is_none());
    }

    #[test]
    fn test_registry_aliases_exposed() {
        let reg = PricingRegistry::with_builtin_prices();
        let aliases = reg.aliases();
        assert!(!aliases.is_empty());
        // Verify known aliases are present
        let map: std::collections::HashMap<_, _> = aliases.iter().cloned().collect();
        assert!(map.contains_key("claude-opus"));
        assert_eq!(map.get("claude-opus").unwrap(), "claude-opus-4");
    }

    #[test]
    fn test_registry_all_aliases_resolve() {
        let reg = PricingRegistry::with_builtin_prices();
        for (alias, canonical) in reg.aliases() {
            assert!(
                reg.get(&canonical).is_some(),
                "alias '{}' points to unknown canonical model '{}'",
                alias,
                canonical
            );
        }
    }
}
