#[derive(Debug, Clone)]
pub struct Limits {
    pub max_peer_message_size: usize,
    pub max_pending_mailbox_depth: usize,
    pub max_messages_per_source_delegation: usize,
    pub max_fanout_per_message: usize,
    pub drain_quiet_window_ms: u64,
    pub drain_max_total_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_peer_message_size: 2_048,
            max_pending_mailbox_depth: 8,
            max_messages_per_source_delegation: 32,
            max_fanout_per_message: 4,
            drain_quiet_window_ms: 2_000,
            drain_max_total_ms: 10_000,
        }
    }
}

/// Returns the aggregate peer-context budget in chars for the given target
/// context window size.
pub fn aggregate_budget_for_context_window(window_chars: u64) -> u64 {
    let pct = if window_chars < 64_000 {
        10
    } else if window_chars < 128_000 {
        7
    } else {
        5
    };
    window_chars * pct / 100
}

/// Returns the effective per-message size cap given a configured cap and the
/// target's aggregate budget. Bounded by `aggregate / max_depth`.
pub fn effective_max_message_size(
    configured_cap: usize,
    aggregate_budget: u64,
    max_depth: usize,
) -> usize {
    let derived = (aggregate_budget / max_depth.max(1) as u64) as usize;
    configured_cap.min(derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_10pct_under_64k() {
        assert_eq!(aggregate_budget_for_context_window(32_000), 3_200);
    }

    #[test]
    fn budget_is_7pct_at_64k_to_128k() {
        assert_eq!(aggregate_budget_for_context_window(64_000), 4_480);
        assert_eq!(aggregate_budget_for_context_window(127_999), 8_959);
    }

    #[test]
    fn budget_is_5pct_at_or_above_128k() {
        assert_eq!(aggregate_budget_for_context_window(128_000), 6_400);
        assert_eq!(aggregate_budget_for_context_window(200_000), 10_000);
    }

    #[test]
    fn effective_max_message_size_is_min_of_configured_and_derived() {
        // 32k window → 3200 budget, 8 depth → 400 derived. Config 2048.
        assert_eq!(effective_max_message_size(2_048, 3_200, 8), 400);
        // 200k window → 10000 budget, 8 depth → 1250 derived. Config 2048 → 1250.
        assert_eq!(effective_max_message_size(2_048, 10_000, 8), 1_250);
        // 200k window → 10000 budget, 1 depth → 10000. Config 2048 → 2048.
        assert_eq!(effective_max_message_size(2_048, 10_000, 1), 2_048);
    }
}
