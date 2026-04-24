# Industry Research: Cost Calculation Patterns from `ccusage`

## Source
- Repository: https://github.com/ryoppippi/ccusage
- Cloned to: `/tmp/ccusage`

## What `ccusage` Does
`ccusage` is a mature TypeScript CLI tool that tracks token usage and costs across **multiple AI agents**:
- Claude Code
- Codex (OpenAI)
- Pi Agent
- OpenCode
- AMP

It reads each agent's JSONL log files, normalizes them to a common token-event format, and calculates costs using per-model pricing.

---

## Key Patterns to Apply to `spur-cost`

### 1. Token-Based Cost Calculation (Not Time-Based)

**Industry standard:** Cost is calculated from actual token consumption, not session duration.

```
cost = (input_tokens × input_cost_per_token)
     + (output_tokens × output_cost_per_token)
     + (cache_creation_tokens × cache_creation_cost_per_token)
     + (cache_read_tokens × cache_read_cost_per_token)
```

**Current `spur-cost` limitation:**
```rust
// Time-based (very imprecise)
duration.as_secs_f64() * rate  // rate from CostTier::High/Medium/Low
```

### 2. Four Token Types

`ccusage` tracks these distinct token counters:

| Token Type | Description | Billing Treatment |
|---|---|---|
| `input_tokens` | Prompt tokens sent to the model | Full input rate |
| `output_tokens` | Generated tokens (includes reasoning) | Full output rate |
| `cache_creation_input_tokens` | Tokens written to cache | Usually same as input, sometimes discounted |
| `cache_read_input_tokens` | Tokens read from cache | Heavily discounted (e.g., 90% off for Claude) |

**Note on reasoning tokens:** Codex reports `reasoning_output_tokens` separately, but they are **already included** in `output_tokens` for billing. Do not double-count.

### 3. Per-Model Pricing (Not Per-Tier)

`ccusage` fetches pricing from the LiteLLM open-source pricing database:
`https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`

Each model has its own rates:
- `claude-sonnet-4-20250514`: input $3/M, output $15/M
- `gpt-5`: input $1.25/M, output $10/M
- etc.

**Current `spur-cost` limitation:** Only 3 coarse tiers (High/Medium/Low) with hardcoded per-minute rates.

### 4. Tiered Pricing for Large Contexts

Some models (Claude 1M context window) charge different rates above a threshold:

```
if tokens > 200_000:
    cost = (200_000 × base_rate) + ((tokens - 200_000) × tiered_rate)
else:
    cost = tokens × base_rate
```

Gemini uses a 128k threshold (not yet implemented in `ccusage` calculations).

### 5. Model Aliases & Fallbacks

Different agents use different model naming. `ccusage` resolves aliases:
- Codex: `gpt-5-codex` → `gpt-5`
- OpenCode: `gemini-3-pro-high` → `gemini-3-pro-preview`
- Legacy Codex logs without model metadata → fallback to `gpt-5`

For SPUR, we should map agent configs to their underlying model names for accurate pricing.

### 6. Cost Calculation Modes

`ccusage` supports three modes:
- **`auto`**: Use pre-calculated cost from API if available; otherwise calculate from tokens
- **`calculate`**: Always calculate from token counts + model pricing
- **`display`**: Always use pre-calculated costs; show 0 if missing

This is useful because some APIs (Claude Code) report `costUSD` in their JSONL logs, while others (Codex) only report token counts.

### 7. Offline Pricing Cache

`ccusage` prefetches LiteLLM pricing at build time as a compile-time macro, so the tool works offline. It falls back to this cached data if the network fetch fails.

For SPUR, we can embed a curated set of known model prices directly in the Rust binary.

### 8. Provider-Specific Multipliers

Some providers offer speed tiers with cost multipliers:
- `speed: 'fast'` → multiply by `provider_specific_entry.fast` (e.g., 6× for Claude fast mode)

### 9. Deduplication

`ccusage` deduplicates entries using `message_id + request_id` hash to avoid double-counting when the same event appears in multiple files.

---

## Recommended Changes for `spur-cost`

1. **Add `ModelPricing` struct** with per-token rates (input, output, cache creation, cache read)
2. **Add `TokenUsage` struct** tracking the four token types
3. **Add `calculate_cost_from_tokens()`** using the industry-standard formula
4. **Add `PricingRegistry`** with embedded prices for known models (offline-capable)
5. **Add tiered pricing support** for 200k+ context models
6. **Add model alias resolution** for SPUR's worker agents
7. **Extend DB schema** with token columns on the `sessions` table
8. **Keep backward compatibility** with existing `CostTier` + duration estimation
9. **Add tests** for token cost calculation, tiered pricing, and alias resolution
