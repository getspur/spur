# Tier Revamp — Plan A: Registry Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 64 new `FeatureKey` constants (Wave 8 final, down from 135 across Waves 5+6+7+8 4-reviewer + L9-MCTS first-principles + second-order composition rationalization) and 1 new `QuotaKey` variant to `spur-license` (additive — old keys remain alongside new keys). No behavior change yet; this is the typed registry foundation that Plans B–E build on. **Wave 8 was a major restructure**: 15 over-decomposed families (compile-coupled APIs, all-or-nothing valid substates, producer/consumer chains where one half is meaningless without the other) collapsed into single umbrella keys; +4 drops + 5 vaporware deferrals — see spec §4.16 for the full Wave-8 backlog with code grounding.

**Architecture:** Pure additive changes to `crates/spur-license/src/policy/feature_key.rs` and `crates/spur-license/src/quota.rs`. New keys follow the `<crate>_<tier>_<capability>` naming convention from the spec. The existing `from_known()` parser is extended; the existing `FeatureKey` newtype + `bytes_eq()` const helper are reused unchanged. After this plan ships, the codebase has a dual registry (old + new keys); Plan B will rewrite the policy doc and migrate all existing call sites; Plan B's final task will remove the old keys.

**Tech Stack:** Rust 2021, `spur-license` crate, `cargo test --package spur-license`, no new dependencies.

**Spec reference:** `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md` §4 (full feature key registry, 64 keys total post-Wave-8) + §4.16 Wave-8 consolidation/drop/defer tables.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/spur-license/src/quota.rs` | Modify | Add `BrainFailoverChainDepth` variant to `QuotaKey` enum |
| `crates/spur-license/src/policy/feature_key.rs` | Modify | Net +64 new `pub const` declarations (Wave 8 final) grouped by crate prefix; extend `from_known()` parser; add new tests. Wave 8 implementation also REMOVES 38 prior-wave const+parser+test entries (15 family consolidations + 4 drops + 5 vaporware defers + 14 prior-wave consts being absorbed into umbrellas) — see spec §4.16. |

No new files. No changes to `gate.rs`, `licenseseat.rs`, `community.rs`, or `default_policy.json` (those are Plan B).

---

## Conventions for every task

- Naming: const name is `UPPER_SNAKE_CASE` of the underlying string (e.g., `CORE_CORE_BRAIN_SESSION` for `"core_core_brain_session"`)
- Each crate-group task: write failing test → confirm fail → add consts + parser arms → confirm pass → commit
- Test-first: every const is asserted via `from_known()` roundtrip in the same task that adds it
- Commit messages follow `feat(spur-license): registry add <crate-group> keys (<count>) for tier revamp Plan A` pattern
- Run `cargo test --package spur-license --lib feature_key` for fast targeted runs; `cargo build --workspace` at end of each task to catch wider breakage

---

## Task 1: Add `BrainFailoverChainDepth` variant to QuotaKey

**Files:**
- Modify: `crates/spur-license/src/quota.rs:4-9` (add enum variant)
- Modify: `crates/spur-license/src/quota.rs:13-19` (add as_str arm)
- Modify: `crates/spur-license/src/quota.rs:23-28` (add from_known arm)
- Modify: `crates/spur-license/src/quota.rs:66-77` (extend test)

- [ ] **Step 1: Write the failing test**

Add this test below the existing `quota_value_as_bytes` test in `crates/spur-license/src/quota.rs` (after line 89):

```rust
    #[test]
    fn quota_key_brain_failover_chain_depth_roundtrips() {
        assert_eq!(
            QuotaKey::BrainFailoverChainDepth.as_str(),
            "brain_failover_chain_depth"
        );
        assert_eq!(
            QuotaKey::from_known("brain_failover_chain_depth"),
            Some(QuotaKey::BrainFailoverChainDepth)
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --package spur-license --lib quota::tests::quota_key_brain_failover_chain_depth_roundtrips`

Expected: FAIL with `error[E0599]: no variant or associated item named 'BrainFailoverChainDepth' found for enum 'QuotaKey'`

- [ ] **Step 3: Add the enum variant**

In `crates/spur-license/src/quota.rs`, modify the `QuotaKey` enum at line 4-9:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuotaKey {
    MaxConcurrentWorkers,
    EventRetentionBytes,
    MaxTeamMembers,
    MinSeats,
    BrainFailoverChainDepth,
}
```

- [ ] **Step 4: Add the as_str arm**

In `crates/spur-license/src/quota.rs`, modify the `as_str` match (around line 13-19):

```rust
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::MaxConcurrentWorkers => "max_concurrent_workers",
            Self::EventRetentionBytes => "event_retention_bytes",
            Self::MaxTeamMembers => "max_team_members",
            Self::MinSeats => "min_seats",
            Self::BrainFailoverChainDepth => "brain_failover_chain_depth",
        }
    }
```

- [ ] **Step 5: Add the from_known arm**

In `crates/spur-license/src/quota.rs`, modify the `from_known` match (around line 22-29):

```rust
    pub fn from_known(s: &str) -> Option<Self> {
        match s {
            "max_concurrent_workers" => Some(Self::MaxConcurrentWorkers),
            "event_retention_bytes" => Some(Self::EventRetentionBytes),
            "max_team_members" => Some(Self::MaxTeamMembers),
            "min_seats" => Some(Self::MinSeats),
            "brain_failover_chain_depth" => Some(Self::BrainFailoverChainDepth),
            _ => None,
        }
    }
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --package spur-license --lib quota::tests`

Expected: PASS — all 4 quota tests including the new `quota_key_brain_failover_chain_depth_roundtrips`.

- [ ] **Step 7: Build workspace to catch unrelated breakage**

Run: `cargo build --workspace`

Expected: PASS (the new variant is unused but valid; no `non_exhaustive` warnings because no external matches yet).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-license/src/quota.rs
git commit -m "feat(spur-license): add BrainFailoverChainDepth quota variant for tier revamp"
```

---

## Task 2: Establish naming convention banner + helper test

This task adds a top-of-file banner documenting the new naming convention, plus a single integration test that asserts the total count of registered keys (which we'll bump as we add each crate group). This guards against accidental key removal.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs:1-7` (extend module banner)
- Modify: `crates/spur-license/src/policy/feature_key.rs:end-of-tests` (add count test)

- [ ] **Step 1: Write the failing test**

Add this test at the end of the `mod tests` block in `crates/spur-license/src/policy/feature_key.rs` (just before the final closing `}` of the test module):

```rust
    /// Guards against accidental removal of registered keys.
    /// Bump the expected count when adding new keys via dedicated tasks.
    #[test]
    fn registered_key_count_matches_expected() {
        const EXPECTED_TOTAL_KEYS: usize = 36;
        let mut count = 0usize;
        for s in &[
            // Community (11)
            "brain_session", "single_worker", "worktree_isolation", "manual_review",
            "event_persistence", "basic_lineage", "tui_dashboard", "basic_cost_display",
            "basic_notifications", "local_config", "mcp_standard_tools",
            // Pro (8)
            "parallel_workers", "auto_review_policies", "session_resume",
            "advanced_cost_analytics", "custom_worktree_policies", "custom_notifications",
            "extended_retention", "tui_session_detail",
            // Team (7)
            "pm_integration", "shared_lineage", "team_cost_dashboard", "centralized_config",
            "rbac", "shared_review_queue", "pm_webhooks",
            // Enterprise (6)
            "sso_saml", "audit_logs", "custom_policies", "custom_mcp_tools",
            "dedicated_support", "sla_guarantee",
            // G2 flags (4)
            "kill_advanced_planner", "enable_browser_tool", "enable_compaction_v2",
            "enable_telemetry",
        ] {
            assert!(
                FeatureKey::from_known(s).is_some(),
                "key {s:?} not parseable",
            );
            count += 1;
        }
        assert_eq!(count, EXPECTED_TOTAL_KEYS, "key count mismatch");
    }
```

- [ ] **Step 2: Run test to verify it passes (this is a sanity check, not red→green)**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::registered_key_count_matches_expected`

Expected: PASS — the existing 36 keys are all parseable.

- [ ] **Step 3: Update the module banner**

Replace the existing banner at the top of `crates/spur-license/src/policy/feature_key.rs` (lines 1-6) with this expanded version:

```rust
//! Typed const registry of feature keys. Unifies G1 (entitlement) and G2
//! (flag) namespaces into a single grep-discoverable list.
//!
//! Adding a feature = adding a `pub const` here. Underlying string is what
//! the policy file and LicenseSeat catalog speak; this newtype exists to
//! make callers typo-safe.
//!
//! ## Naming convention (post-2026-04-26 tier revamp)
//!
//! New keys follow `<crate>_<tier>_<capability>` where:
//! - `<crate>` ∈ {acp, core, mcp, tui, cli, pm, cost, worktree, license, bot,
//!   interactive, blob, ctx, skills, notif}
//! - `<tier>` ∈ {core (Free baseline), pro (Pro upsell), team (Team v2-deferred)}
//! - `<capability>` is a single atomic capability, lowercase snake_case
//!
//! Const name is UPPER_SNAKE_CASE of the underlying string. Grep
//! `pm_pro_*` to find every Pro PM gate. The legacy keys above (BRAIN_SESSION
//! etc.) remain during the v0 → v1 transition; Plan B removes them after
//! callers migrate.
//!
//! See `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
//! §4 for the full 64-key registry (Wave 8 final, post-second-order
//! composition rationalization).
```

- [ ] **Step 4: Build workspace**

Run: `cargo build --workspace`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "docs(spur-license): document tier-revamp naming convention + add count guard test"
```

---

## Task 3: Add spur-acp keys (11)

Adds: 2 transports + 7 vendor adapters + 2 session attach modes.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs` (add consts, parser arms, tests)

- [ ] **Step 1: Write the failing test**

Add this test inside the `mod tests` block, after `registered_key_count_matches_expected`:

```rust
    #[test]
    fn spur_acp_keys_registered() {
        for s in &[
            "acp_core_transport_stdio",
            "acp_core_transport_socket",
            "acp_core_adapter_claude_code",
            "acp_core_adapter_codex",
            "acp_core_adapter_gemini",
            "acp_core_adapter_kiro",
            "acp_core_adapter_cursor",
            "acp_core_adapter_opencode",
            "acp_core_adapter_kimi",
            "acp_core_session_attach_advisory_lock",
            "acp_core_session_attach_degraded_nolock",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::spur_acp_keys_registered`

Expected: FAIL — first key `"acp_core_transport_stdio"` returns `None`, assertion message includes `missing acp_core_transport_stdio`.

- [ ] **Step 3: Add the consts + parser arms**

In `crates/spur-license/src/policy/feature_key.rs`, add this block AFTER the existing `// --- G2 flag keys (4) ---` block (after line 73 in the existing file, before the `pub const fn as_str` declaration):

```rust
    // ============================================================
    // === Tier revamp v1 keys (post-2026-04-26) ==================
    // ============================================================

    // --- spur-acp (11) ---
    pub const ACP_CORE_TRANSPORT_STDIO: Self = Self("acp_core_transport_stdio");
    pub const ACP_CORE_TRANSPORT_SOCKET: Self = Self("acp_core_transport_socket");
    pub const ACP_CORE_ADAPTER_CLAUDE_CODE: Self = Self("acp_core_adapter_claude_code");
    pub const ACP_CORE_ADAPTER_CODEX: Self = Self("acp_core_adapter_codex");
    pub const ACP_CORE_ADAPTER_GEMINI: Self = Self("acp_core_adapter_gemini");
    pub const ACP_CORE_ADAPTER_KIRO: Self = Self("acp_core_adapter_kiro");
    pub const ACP_CORE_ADAPTER_CURSOR: Self = Self("acp_core_adapter_cursor");
    pub const ACP_CORE_ADAPTER_OPENCODE: Self = Self("acp_core_adapter_opencode");
    pub const ACP_CORE_ADAPTER_KIMI: Self = Self("acp_core_adapter_kimi");
    pub const ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK: Self = Self("acp_core_session_attach_advisory_lock");
    pub const ACP_CORE_SESSION_ATTACH_DEGRADED_NOLOCK: Self = Self("acp_core_session_attach_degraded_nolock");
```

Then extend the `from_known` const fn. Find the LAST `else if bytes_eq(b, b"enable_telemetry")` branch (the final G2 flag arm) and add new branches BEFORE the closing `} else { None }`. The existing tail looks like:

```rust
        } else if bytes_eq(b, b"enable_telemetry") {
            Some(Self::ENABLE_TELEMETRY)
        } else {
            None
        }
```

Replace the trailing `} else { None }` with this expanded chain (paste BEFORE the `} else { None }`):

```rust
        // ===== Tier revamp v1 keys =====
        // spur-acp
        } else if bytes_eq(b, b"acp_core_transport_stdio") {
            Some(Self::ACP_CORE_TRANSPORT_STDIO)
        } else if bytes_eq(b, b"acp_core_transport_socket") {
            Some(Self::ACP_CORE_TRANSPORT_SOCKET)
        } else if bytes_eq(b, b"acp_core_adapter_claude_code") {
            Some(Self::ACP_CORE_ADAPTER_CLAUDE_CODE)
        } else if bytes_eq(b, b"acp_core_adapter_codex") {
            Some(Self::ACP_CORE_ADAPTER_CODEX)
        } else if bytes_eq(b, b"acp_core_adapter_gemini") {
            Some(Self::ACP_CORE_ADAPTER_GEMINI)
        } else if bytes_eq(b, b"acp_core_adapter_kiro") {
            Some(Self::ACP_CORE_ADAPTER_KIRO)
        } else if bytes_eq(b, b"acp_core_adapter_cursor") {
            Some(Self::ACP_CORE_ADAPTER_CURSOR)
        } else if bytes_eq(b, b"acp_core_adapter_opencode") {
            Some(Self::ACP_CORE_ADAPTER_OPENCODE)
        } else if bytes_eq(b, b"acp_core_adapter_kimi") {
            Some(Self::ACP_CORE_ADAPTER_KIMI)
        } else if bytes_eq(b, b"acp_core_session_attach_advisory_lock") {
            Some(Self::ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK)
        } else if bytes_eq(b, b"acp_core_session_attach_degraded_nolock") {
            Some(Self::ACP_CORE_SESSION_ATTACH_DEGRADED_NOLOCK)
```

(Keep the trailing `} else { None }` intact at the end.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::spur_acp_keys_registered`

Expected: PASS.

- [ ] **Step 5: Build workspace**

Run: `cargo build --workspace`

Expected: PASS (consts unused — that's fine for an additive registry).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "feat(spur-license): registry add spur-acp keys (11) for tier revamp Plan A"
```

---

## Task 4: Add spur-core brain & scheduling keys (5)

Adds: brain_session, brain_scheduler, brain_failover_manual_keystroke, brain_failover_auto_pool, continuation_bridge.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests`:

```rust
    #[test]
    fn spur_core_brain_keys_registered() {
        for s in &[
            "core_core_brain_session",
            "core_core_brain_scheduler",
            "core_core_brain_failover_manual_keystroke",
            "core_pro_brain_failover_auto_pool",
            "core_core_continuation_bridge",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::spur_core_brain_keys_registered`

Expected: FAIL — first key `"core_core_brain_session"` returns `None`.

- [ ] **Step 3: Add the consts**

In `crates/spur-license/src/policy/feature_key.rs`, add this block AFTER the spur-acp consts from Task 3:

```rust
    // --- spur-core: brain & scheduling (5) ---
    pub const CORE_CORE_BRAIN_SESSION: Self = Self("core_core_brain_session");
    pub const CORE_CORE_BRAIN_SCHEDULER: Self = Self("core_core_brain_scheduler");
    pub const CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE: Self = Self("core_core_brain_failover_manual_keystroke");
    pub const CORE_PRO_BRAIN_FAILOVER_AUTO_POOL: Self = Self("core_pro_brain_failover_auto_pool");
    pub const CORE_CORE_CONTINUATION_BRIDGE: Self = Self("core_core_continuation_bridge");
```

Add these `from_known` arms after the spur-acp arms (before `} else { None }`):

```rust
        // spur-core: brain & scheduling
        } else if bytes_eq(b, b"core_core_brain_session") {
            Some(Self::CORE_CORE_BRAIN_SESSION)
        } else if bytes_eq(b, b"core_core_brain_scheduler") {
            Some(Self::CORE_CORE_BRAIN_SCHEDULER)
        } else if bytes_eq(b, b"core_core_brain_failover_manual_keystroke") {
            Some(Self::CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE)
        } else if bytes_eq(b, b"core_pro_brain_failover_auto_pool") {
            Some(Self::CORE_PRO_BRAIN_FAILOVER_AUTO_POOL)
        } else if bytes_eq(b, b"core_core_continuation_bridge") {
            Some(Self::CORE_CORE_CONTINUATION_BRIDGE)
```

- [ ] **Step 4: Run test**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::spur_core_brain_keys_registered`

Expected: PASS.

- [ ] **Step 5: Build**

Run: `cargo build --workspace`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "feat(spur-license): registry add spur-core brain & scheduling keys (5) for tier revamp Plan A"
```

---

## Task 5: Add spur-core workers & semaphore keys (3)

Adds: parallel_workers, cancellable_semaphore, worker_heartbeat_watchdog.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn spur_core_workers_keys_registered() {
        for s in &[
            "core_core_parallel_workers",
            "core_core_cancellable_semaphore",
            "core_pro_worker_heartbeat_watchdog",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::spur_core_workers_keys_registered`

- [ ] **Step 3: Add consts + parser arms**

Append after Task 4's consts:

```rust
    // --- spur-core: workers & semaphore (3) ---
    pub const CORE_CORE_PARALLEL_WORKERS: Self = Self("core_core_parallel_workers");
    pub const CORE_CORE_CANCELLABLE_SEMAPHORE: Self = Self("core_core_cancellable_semaphore");
    pub const CORE_PRO_WORKER_HEARTBEAT_WATCHDOG: Self = Self("core_pro_worker_heartbeat_watchdog");
```

Append parser arms:

```rust
        // spur-core: workers & semaphore
        } else if bytes_eq(b, b"core_core_parallel_workers") {
            Some(Self::CORE_CORE_PARALLEL_WORKERS)
        } else if bytes_eq(b, b"core_core_cancellable_semaphore") {
            Some(Self::CORE_CORE_CANCELLABLE_SEMAPHORE)
        } else if bytes_eq(b, b"core_pro_worker_heartbeat_watchdog") {
            Some(Self::CORE_PRO_WORKER_HEARTBEAT_WATCHDOG)
```

- [ ] **Step 4: Run test, expect PASS.**
- [ ] **Step 5: Build workspace, expect PASS.**
- [ ] **Step 6: Commit:**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "feat(spur-license): registry add spur-core workers keys (3) for tier revamp Plan A"
```

---

## Task 6: Add spur-core event pipeline keys (5)

Adds: event_funnel_broadcast, event_sink_ndjson_128mb, executor_lineage_projection, notification_pump, broadcast_lagged_recovery.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs`

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn spur_core_event_pipeline_keys_registered() {
        for s in &[
            "core_core_event_funnel_broadcast",
            "core_core_event_sink_ndjson_128mb",
            "core_core_executor_lineage_projection",
            "core_core_notification_pump",
            "core_pro_broadcast_lagged_recovery",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**

- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: event pipeline (5) ---
    pub const CORE_CORE_EVENT_FUNNEL_BROADCAST: Self = Self("core_core_event_funnel_broadcast");
    pub const CORE_CORE_EVENT_SINK_NDJSON_128MB: Self = Self("core_core_event_sink_ndjson_128mb");
    pub const CORE_CORE_EXECUTOR_LINEAGE_PROJECTION: Self = Self("core_core_executor_lineage_projection");
    pub const CORE_CORE_NOTIFICATION_PUMP: Self = Self("core_core_notification_pump");
    pub const CORE_PRO_BROADCAST_LAGGED_RECOVERY: Self = Self("core_pro_broadcast_lagged_recovery");
```

Add parser arms:

```rust
        // spur-core: event pipeline
        } else if bytes_eq(b, b"core_core_event_funnel_broadcast") {
            Some(Self::CORE_CORE_EVENT_FUNNEL_BROADCAST)
        } else if bytes_eq(b, b"core_core_event_sink_ndjson_128mb") {
            Some(Self::CORE_CORE_EVENT_SINK_NDJSON_128MB)
        } else if bytes_eq(b, b"core_core_executor_lineage_projection") {
            Some(Self::CORE_CORE_EXECUTOR_LINEAGE_PROJECTION)
        } else if bytes_eq(b, b"core_core_notification_pump") {
            Some(Self::CORE_CORE_NOTIFICATION_PUMP)
        } else if bytes_eq(b, b"core_pro_broadcast_lagged_recovery") {
            Some(Self::CORE_PRO_BROADCAST_LAGGED_RECOVERY)
```

- [ ] **Step 4: Run test, expect PASS.**
- [ ] **Step 5: Build workspace, expect PASS.**
- [ ] **Step 6: Commit:**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "feat(spur-license): registry add spur-core event pipeline keys (5) for tier revamp Plan A"
```

---

## Task 7: Add spur-core review subsystem keys (6)

**REVISED 2026-04-26 (2nd pass) via 3-reviewer triangulation (gemini → kimi → codex).** First revision used `_basic`/`_custom`/`_backoff` suffixes (impl leaks). Codex's code reading (`review_sink.rs:34`, `orchestrator.rs:4517` timeout arm, `:4654` Retry arm, `spur-acp/src/config/mod.rs:246` config separation) confirmed the sink is router-only and timeout/retry are separable orchestrator branches with distinct config fields. Adopted kimi's 6-key naming (capability nouns).

**6 keys, 3 Free + 3 Pro:**
- Free: `review_sink` (router + manual resolution), `review_timeout` (auto-cancel), `review_retry` (press 'R' with system-default backoff)
- Pro: `review_auto_approve` (rule-based bypass), `review_timeout_routing` (custom FallbackAction → Slack/alt-agent), `review_retry_config` (configurable backoff + max-attempts)

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_review_keys_registered() {
        for s in &[
            "core_core_review_sink",
            "core_core_review_timeout",
            "core_core_review_retry",
            "core_pro_review_auto_approve",
            "core_pro_review_timeout_routing",
            "core_pro_review_retry_config",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**

- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: review subsystem (6) ---
    pub const CORE_CORE_REVIEW_SINK: Self = Self("core_core_review_sink");
    pub const CORE_CORE_REVIEW_TIMEOUT: Self = Self("core_core_review_timeout");
    pub const CORE_CORE_REVIEW_RETRY: Self = Self("core_core_review_retry");
    pub const CORE_PRO_REVIEW_AUTO_APPROVE: Self = Self("core_pro_review_auto_approve");
    pub const CORE_PRO_REVIEW_TIMEOUT_ROUTING: Self = Self("core_pro_review_timeout_routing");
    pub const CORE_PRO_REVIEW_RETRY_CONFIG: Self = Self("core_pro_review_retry_config");
```

Parser arms:

```rust
        // spur-core: review subsystem
        } else if bytes_eq(b, b"core_core_review_sink") {
            Some(Self::CORE_CORE_REVIEW_SINK)
        } else if bytes_eq(b, b"core_core_review_timeout") {
            Some(Self::CORE_CORE_REVIEW_TIMEOUT)
        } else if bytes_eq(b, b"core_core_review_retry") {
            Some(Self::CORE_CORE_REVIEW_RETRY)
        } else if bytes_eq(b, b"core_pro_review_auto_approve") {
            Some(Self::CORE_PRO_REVIEW_AUTO_APPROVE)
        } else if bytes_eq(b, b"core_pro_review_timeout_routing") {
            Some(Self::CORE_PRO_REVIEW_TIMEOUT_ROUTING)
        } else if bytes_eq(b, b"core_pro_review_retry_config") {
            Some(Self::CORE_PRO_REVIEW_RETRY_CONFIG)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core review keys (6) for tier revamp Plan A`

---

## Task 8: Add skills system keys (5)

**REVISED 2026-04-26 (gate-review pass).** Original draft mixed `core_core_skill_*` (2) + `skills_*` (3), violating block-label/key-prefix consistency and breaking grep-discoverability of the skills boundary. All 5 keys now share the `skills_*` prefix per gemini + claude-code review findings.

Adds: skills_core_registry, skills_core_atomic_installation, skills_core_render_per_vendor, skills_pro_custom, skills_pro_role_gating.

Place these in a dedicated `// --- skills (5) ---` block (NOT inside spur-core block) — this is a cross-cutting subsystem.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn skills_keys_registered() {
        for s in &[
            "skills_core_registry",
            "skills_core_atomic_installation",
            "skills_core_render_per_vendor",
            "skills_pro_custom",
            "skills_pro_role_gating",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts** in a new dedicated block (NOT inside the spur-core block):

```rust
    // --- skills (5) ---
    pub const SKILLS_CORE_REGISTRY: Self = Self("skills_core_registry");
    pub const SKILLS_CORE_ATOMIC_INSTALLATION: Self = Self("skills_core_atomic_installation");
    pub const SKILLS_CORE_RENDER_PER_VENDOR: Self = Self("skills_core_render_per_vendor");
    pub const SKILLS_PRO_CUSTOM: Self = Self("skills_pro_custom");
    pub const SKILLS_PRO_ROLE_GATING: Self = Self("skills_pro_role_gating");
```

Parser arms:

```rust
        // skills
        } else if bytes_eq(b, b"skills_core_registry") {
            Some(Self::SKILLS_CORE_REGISTRY)
        } else if bytes_eq(b, b"skills_core_atomic_installation") {
            Some(Self::SKILLS_CORE_ATOMIC_INSTALLATION)
        } else if bytes_eq(b, b"skills_core_render_per_vendor") {
            Some(Self::SKILLS_CORE_RENDER_PER_VENDOR)
        } else if bytes_eq(b, b"skills_pro_custom") {
            Some(Self::SKILLS_PRO_CUSTOM)
        } else if bytes_eq(b, b"skills_pro_role_gating") {
            Some(Self::SKILLS_PRO_ROLE_GATING)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add skills keys (5) for tier revamp Plan A`

---

## Task 9: Add spur-core peer mailbox keys (3)

Adds: peer_mailbox_router, peer_mailbox_ledger, peer_mailbox_stranded_recon. All Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_peer_mailbox_keys_registered() {
        for s in &[
            "core_pro_peer_mailbox_router",
            "core_pro_peer_mailbox_ledger",
            "core_pro_peer_mailbox_stranded_recon",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: peer mailbox (3) ---
    pub const CORE_PRO_PEER_MAILBOX_ROUTER: Self = Self("core_pro_peer_mailbox_router");
    pub const CORE_PRO_PEER_MAILBOX_LEDGER: Self = Self("core_pro_peer_mailbox_ledger");
    pub const CORE_PRO_PEER_MAILBOX_STRANDED_RECON: Self = Self("core_pro_peer_mailbox_stranded_recon");
```

Parser arms:

```rust
        // spur-core: peer mailbox
        } else if bytes_eq(b, b"core_pro_peer_mailbox_router") {
            Some(Self::CORE_PRO_PEER_MAILBOX_ROUTER)
        } else if bytes_eq(b, b"core_pro_peer_mailbox_ledger") {
            Some(Self::CORE_PRO_PEER_MAILBOX_LEDGER)
        } else if bytes_eq(b, b"core_pro_peer_mailbox_stranded_recon") {
            Some(Self::CORE_PRO_PEER_MAILBOX_STRANDED_RECON)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core peer mailbox keys (3) for tier revamp Plan A`

---

## Task 10: Add spur-core system events keys (4)

**REVISED 2026-04-26 (Wave 4 gate-review pass).** Per gemini findings:
- Removed `license_event_broadcast` (system wiring required for tier transitions \u2014 gating it is circular)
- Renamed `ext_notification` \u2192 `agent_notification` (drops impl-leak `ext_` prefix)

Net 4 Free keys (was 5).

Adds: conflict_detection, rate_limit_detection, permission_request_detection, agent_notification.

**Revised 2026-04-26 (Wave 4 redo gate-review):** Renamed `permission_request_prompt` → `permission_request_detection` per gemini finding (symmetric with `license_event_broadcast` removal): `_prompt` is UI wiring, `_detection` matches sibling capability nouns.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_system_events_keys_registered() {
        for s in &[
            "core_core_conflict_detection",
            "core_core_rate_limit_detection",
            "core_core_permission_request_detection",
            "core_core_agent_notification",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: system events (4) ---
    pub const CORE_CORE_CONFLICT_DETECTION: Self = Self("core_core_conflict_detection");
    pub const CORE_CORE_RATE_LIMIT_DETECTION: Self = Self("core_core_rate_limit_detection");
    pub const CORE_CORE_PERMISSION_REQUEST_DETECTION: Self =
        Self("core_core_permission_request_detection");
    pub const CORE_CORE_AGENT_NOTIFICATION: Self = Self("core_core_agent_notification");
```

Parser arms:

```rust
        // spur-core: system events
        } else if bytes_eq(b, b"core_core_conflict_detection") {
            Some(Self::CORE_CORE_CONFLICT_DETECTION)
        } else if bytes_eq(b, b"core_core_rate_limit_detection") {
            Some(Self::CORE_CORE_RATE_LIMIT_DETECTION)
        } else if bytes_eq(b, b"core_core_permission_request_detection") {
            Some(Self::CORE_CORE_PERMISSION_REQUEST_DETECTION)
        } else if bytes_eq(b, b"core_core_agent_notification") {
            Some(Self::CORE_CORE_AGENT_NOTIFICATION)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core system events keys (4) for tier revamp Plan A`

---

## Task 11: Add spur-core reliability & lifecycle keys (5)

**REVISED 2026-04-26 (Wave 4 gate-review pass).** Per gemini findings symmetric with Task 7: dropped `basic_` prefix; moved orphan_recovery (Risk #13 safety) and background_task_tracker (Risk #6 hygiene) from Pro to Free. Final 4F + 1P (event_replay only Pro upsell).

Adds: session_resume, session_resume_event_replay, plan_persistence, plan_orphan_recovery, background_task_tracker.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_reliability_keys_registered() {
        for s in &[
            "core_core_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_plan_persistence",
            "core_core_plan_orphan_recovery",
            "core_core_background_task_tracker",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: reliability & lifecycle (5) ---
    pub const CORE_CORE_SESSION_RESUME: Self = Self("core_core_session_resume");
    pub const CORE_PRO_SESSION_RESUME_EVENT_REPLAY: Self =
        Self("core_pro_session_resume_event_replay");
    pub const CORE_CORE_PLAN_PERSISTENCE: Self = Self("core_core_plan_persistence");
    pub const CORE_CORE_PLAN_ORPHAN_RECOVERY: Self = Self("core_core_plan_orphan_recovery");
    pub const CORE_CORE_BACKGROUND_TASK_TRACKER: Self =
        Self("core_core_background_task_tracker");
```

Parser arms:

```rust
        // spur-core: reliability & lifecycle
        } else if bytes_eq(b, b"core_core_session_resume") {
            Some(Self::CORE_CORE_SESSION_RESUME)
        } else if bytes_eq(b, b"core_pro_session_resume_event_replay") {
            Some(Self::CORE_PRO_SESSION_RESUME_EVENT_REPLAY)
        } else if bytes_eq(b, b"core_core_plan_persistence") {
            Some(Self::CORE_CORE_PLAN_PERSISTENCE)
        } else if bytes_eq(b, b"core_core_plan_orphan_recovery") {
            Some(Self::CORE_CORE_PLAN_ORPHAN_RECOVERY)
        } else if bytes_eq(b, b"core_core_background_task_tracker") {
            Some(Self::CORE_CORE_BACKGROUND_TASK_TRACKER)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core reliability keys (5) for tier revamp Plan A`

---

## Task 12: Add spur-mcp keys (14)

Per spec §4.3: 7 Free + 7 Pro.

**Revised 2026-04-26 (Wave 4 redo gate-review):** Per gemini gate-review (consistent with `delegate_basic` rename precedent), dropped 3 orphan suffixes:
- `mcp_core_pm_basic` → `mcp_core_pm` (no `_advanced` Pro counterpart)
- `mcp_core_pr_manual` → `mcp_core_pr` (no `_automated` Pro counterpart)
- `mcp_pro_review_advanced` → `mcp_pro_review` (no `_basic` Free counterpart)

Per claude-code consistency nit: block label simplified to `(14)` to match neighbour bare-`(N)` convention.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_mcp_keys_registered() {
        for s in &[
            "mcp_core_server_dispatch",
            "mcp_core_delegate",
            "mcp_core_outcome_fetch",
            "mcp_core_pm",
            "mcp_core_pr",
            "mcp_core_plan_ephemeral",
            "mcp_core_outcome_materializer",
            "mcp_pro_plan_durable",
            "mcp_pro_reconciler_journal_notify",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_mutation_executor",
            "mcp_pro_graph_tools",
            "mcp_pro_review",
            "mcp_pro_custom_tools",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-mcp (14) ---
    pub const MCP_CORE_SERVER_DISPATCH: Self = Self("mcp_core_server_dispatch");
    pub const MCP_CORE_DELEGATE: Self = Self("mcp_core_delegate");
    pub const MCP_CORE_OUTCOME_FETCH: Self = Self("mcp_core_outcome_fetch");
    pub const MCP_CORE_PM: Self = Self("mcp_core_pm");
    pub const MCP_CORE_PR: Self = Self("mcp_core_pr");
    pub const MCP_CORE_PLAN_EPHEMERAL: Self = Self("mcp_core_plan_ephemeral");
    pub const MCP_CORE_OUTCOME_MATERIALIZER: Self = Self("mcp_core_outcome_materializer");
    pub const MCP_PRO_PLAN_DURABLE: Self = Self("mcp_pro_plan_durable");
    pub const MCP_PRO_RECONCILER_JOURNAL_NOTIFY: Self = Self("mcp_pro_reconciler_journal_notify");
    pub const MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT: Self = Self("mcp_pro_signal_watcher_scope_drift");
    pub const MCP_PRO_MUTATION_EXECUTOR: Self = Self("mcp_pro_mutation_executor");
    pub const MCP_PRO_GRAPH_TOOLS: Self = Self("mcp_pro_graph_tools");
    pub const MCP_PRO_REVIEW: Self = Self("mcp_pro_review");
    pub const MCP_PRO_CUSTOM_TOOLS: Self = Self("mcp_pro_custom_tools");
```

Parser arms:

```rust
        // spur-mcp
        } else if bytes_eq(b, b"mcp_core_server_dispatch") {
            Some(Self::MCP_CORE_SERVER_DISPATCH)
        } else if bytes_eq(b, b"mcp_core_delegate") {
            Some(Self::MCP_CORE_DELEGATE)
        } else if bytes_eq(b, b"mcp_core_outcome_fetch") {
            Some(Self::MCP_CORE_OUTCOME_FETCH)
        } else if bytes_eq(b, b"mcp_core_pm") {
            Some(Self::MCP_CORE_PM)
        } else if bytes_eq(b, b"mcp_core_pr") {
            Some(Self::MCP_CORE_PR)
        } else if bytes_eq(b, b"mcp_core_plan_ephemeral") {
            Some(Self::MCP_CORE_PLAN_EPHEMERAL)
        } else if bytes_eq(b, b"mcp_core_outcome_materializer") {
            Some(Self::MCP_CORE_OUTCOME_MATERIALIZER)
        } else if bytes_eq(b, b"mcp_pro_plan_durable") {
            Some(Self::MCP_PRO_PLAN_DURABLE)
        } else if bytes_eq(b, b"mcp_pro_reconciler_journal_notify") {
            Some(Self::MCP_PRO_RECONCILER_JOURNAL_NOTIFY)
        } else if bytes_eq(b, b"mcp_pro_signal_watcher_scope_drift") {
            Some(Self::MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT)
        } else if bytes_eq(b, b"mcp_pro_mutation_executor") {
            Some(Self::MCP_PRO_MUTATION_EXECUTOR)
        } else if bytes_eq(b, b"mcp_pro_graph_tools") {
            Some(Self::MCP_PRO_GRAPH_TOOLS)
        } else if bytes_eq(b, b"mcp_pro_review") {
            Some(Self::MCP_PRO_REVIEW)
        } else if bytes_eq(b, b"mcp_pro_custom_tools") {
            Some(Self::MCP_PRO_CUSTOM_TOOLS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-mcp keys (14) for tier revamp Plan A`

---

## Task 13: Add spur-tui keys (10)

Per spec §4.4 (revised 2026-04-26): 10 Free.

**Revised 2026-04-26 (Wave 5 design-review pass).** 13 → 10 keys per 4-reviewer judge synthesis:
- Renamed `tui_core_notification_in_tui_drain` → `tui_core_notification_drain` (drop redundant `_in_tui_` infix per claude-code).
- Removed `tui_pro_telegram_bot_solo` — gate point belongs at CLI launch / spur-bot subsystem (codex). Will land as `bot_pro_telegram_solo` in Task 19.
- Removed `tui_pro_trace_source_react` — palette source is explicitly `// TODO` deferred in code (codex). Deferred to v1.1 backlog.
- Removed `tui_pro_custom_keybindings` — vaporware: no configurable keymap subsystem exists, only fixed handlers + Vim/Emacs edit mode (codex). Deferred to v2 backlog.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_tui_keys_registered() {
        for s in &[
            "tui_core_view_dashboard",
            "tui_core_view_session_detail",
            "tui_core_view_plan_inspector",
            "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser",
            "tui_core_view_landing_decision",
            "tui_core_view_composer",
            "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom",
            "tui_core_notification_drain",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-tui (10) ---
    pub const TUI_CORE_VIEW_DASHBOARD: Self = Self("tui_core_view_dashboard");
    pub const TUI_CORE_VIEW_SESSION_DETAIL: Self = Self("tui_core_view_session_detail");
    pub const TUI_CORE_VIEW_PLAN_INSPECTOR: Self = Self("tui_core_view_plan_inspector");
    pub const TUI_CORE_VIEW_PALETTE_OVERLAY: Self = Self("tui_core_view_palette_overlay");
    pub const TUI_CORE_VIEW_ISSUE_BROWSER: Self = Self("tui_core_view_issue_browser");
    pub const TUI_CORE_VIEW_LANDING_DECISION: Self = Self("tui_core_view_landing_decision");
    pub const TUI_CORE_VIEW_COMPOSER: Self = Self("tui_core_view_composer");
    pub const TUI_CORE_MODAL_COLLISION_ESCAPE: Self = Self("tui_core_modal_collision_escape");
    pub const TUI_CORE_INPUT_PASTE_AS_ATOM: Self = Self("tui_core_input_paste_as_atom");
    pub const TUI_CORE_NOTIFICATION_DRAIN: Self = Self("tui_core_notification_drain");
```

Parser arms:

```rust
        // spur-tui
        } else if bytes_eq(b, b"tui_core_view_dashboard") {
            Some(Self::TUI_CORE_VIEW_DASHBOARD)
        } else if bytes_eq(b, b"tui_core_view_session_detail") {
            Some(Self::TUI_CORE_VIEW_SESSION_DETAIL)
        } else if bytes_eq(b, b"tui_core_view_plan_inspector") {
            Some(Self::TUI_CORE_VIEW_PLAN_INSPECTOR)
        } else if bytes_eq(b, b"tui_core_view_palette_overlay") {
            Some(Self::TUI_CORE_VIEW_PALETTE_OVERLAY)
        } else if bytes_eq(b, b"tui_core_view_issue_browser") {
            Some(Self::TUI_CORE_VIEW_ISSUE_BROWSER)
        } else if bytes_eq(b, b"tui_core_view_landing_decision") {
            Some(Self::TUI_CORE_VIEW_LANDING_DECISION)
        } else if bytes_eq(b, b"tui_core_view_composer") {
            Some(Self::TUI_CORE_VIEW_COMPOSER)
        } else if bytes_eq(b, b"tui_core_modal_collision_escape") {
            Some(Self::TUI_CORE_MODAL_COLLISION_ESCAPE)
        } else if bytes_eq(b, b"tui_core_input_paste_as_atom") {
            Some(Self::TUI_CORE_INPUT_PASTE_AS_ATOM)
        } else if bytes_eq(b, b"tui_core_notification_drain") {
            Some(Self::TUI_CORE_NOTIFICATION_DRAIN)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-tui keys (10) for tier revamp Plan A`

---

## Task 14: Add spur-cli keys (9)

Per spec §4.5 (revised 2026-04-26): 9 Free.

**Revised 2026-04-26 (Wave 5 design-review pass).** 13 → 9 keys per 4-reviewer judge synthesis:
- Dropped `_command_` infix on every key (claude-code consistency: matches established spur-mcp precedent — 14 MCP tool keys are named `mcp_core_pm`/`_pr`/`_delegate`, not `mcp_core_tool_*`; in cli, every public capability IS a subcommand so the infix carries no information).
- Removed `cli_core_command_version` — Clap built-in `#[command(version)]` attribute, not a separate dispatch site (codex).
- Removed `cli_core_command_upgrade_trial` and `cli_core_command_upgrade_pro` — no `Commands::Upgrade` exists in code yet (codex). Deferred to v1.1 backlog (will land alongside trial/checkout implementation).
- Removed `cli_team_command_workflow` — Phase 3 print-only stub (codex). Deferred to v2 backlog (already marked v2 in spec).
- Updated `cli_core_license_activate` description to match actual `spur auth login --key` implementation; canonical command is `auth login` with `license activate` as planned alias.
- Updated `cli_core_connect` description: actual implementation is GitHub auth/connect, not "socket bindings".

Gemini's "drop all CLI keys as routing facade" critique rejected: keeping CLI as gate-layer is consistent with spur-mcp's already-merged tool-dispatch gates and provides defense-in-depth before downstream crate enforcement.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_cli_keys_registered() {
        for s in &[
            "cli_core_init",
            "cli_core_agents",
            "cli_core_sessions",
            "cli_core_run",
            "cli_core_exec",
            "cli_core_tui",
            "cli_core_cost",
            "cli_core_connect",
            "cli_core_license_activate",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-cli (9) ---
    pub const CLI_CORE_INIT: Self = Self("cli_core_init");
    pub const CLI_CORE_AGENTS: Self = Self("cli_core_agents");
    pub const CLI_CORE_SESSIONS: Self = Self("cli_core_sessions");
    pub const CLI_CORE_RUN: Self = Self("cli_core_run");
    pub const CLI_CORE_EXEC: Self = Self("cli_core_exec");
    pub const CLI_CORE_TUI: Self = Self("cli_core_tui");
    pub const CLI_CORE_COST: Self = Self("cli_core_cost");
    pub const CLI_CORE_CONNECT: Self = Self("cli_core_connect");
    pub const CLI_CORE_LICENSE_ACTIVATE: Self = Self("cli_core_license_activate");
```

Parser arms:

```rust
        // spur-cli
        } else if bytes_eq(b, b"cli_core_init") {
            Some(Self::CLI_CORE_INIT)
        } else if bytes_eq(b, b"cli_core_agents") {
            Some(Self::CLI_CORE_AGENTS)
        } else if bytes_eq(b, b"cli_core_sessions") {
            Some(Self::CLI_CORE_SESSIONS)
        } else if bytes_eq(b, b"cli_core_run") {
            Some(Self::CLI_CORE_RUN)
        } else if bytes_eq(b, b"cli_core_exec") {
            Some(Self::CLI_CORE_EXEC)
        } else if bytes_eq(b, b"cli_core_tui") {
            Some(Self::CLI_CORE_TUI)
        } else if bytes_eq(b, b"cli_core_cost") {
            Some(Self::CLI_CORE_COST)
        } else if bytes_eq(b, b"cli_core_connect") {
            Some(Self::CLI_CORE_CONNECT)
        } else if bytes_eq(b, b"cli_core_license_activate") {
            Some(Self::CLI_CORE_LICENSE_ACTIVATE)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-cli keys (9) for tier revamp Plan A`

---

## Task 15: Add spur-pm keys (5)

Per spec §4.6 (revised 2026-04-26): 4 Free + 1 Pro.

**Revised 2026-04-26 (Wave 5 design-review pass).** 11 → 5 keys per 4-reviewer judge synthesis (major rationalization driven by codex code-grounded findings — 6 of the original 11 keys gated capabilities that live in OTHER crates or don't exist):

**Renames (codex + claude-code):**
- `pm_core_pm_read` → `pm_core_browse` (drop awkward duplicate `pm_pm_` segment).
- `pm_core_pr_manual` → `pm_core_pr` (cross-crate parity with already-merged `mcp_core_pr`; same orphan `_manual` suffix rule).
- `pm_core_bv_adapter` → `pm_core_beads_graph_adapter` (drop opaque `bv` abbreviation; reframe as capability).

**Removed (codex code-grounded):**
- `pm_pro_github_auto` — actual implementation in `crates/spur-mcp/src/plan/reconciler.rs:623`. Deferred to v1.1 backlog (will land as `mcp_pro_pr_auto` in spur-mcp follow-up).
- `pm_pro_linear_sync` — vaporware: only `PmSource::Linear` enum value exists in `crates/spur-pm/src/types.rs:9`, no adapter. Deferred to v2 backlog.
- `pm_pro_plane_sync` — vaporware: same situation as Linear (`types.rs:10`). Deferred to v2 backlog.
- `pm_pro_signal_watcher` — duplicates `mcp_pro_signal_watcher_scope_drift` (already merged in Wave 4); real implementation lives in `crates/spur-mcp/src/plan/signal_watcher.rs`. Dropped.
- `pm_pro_auto_merge` — covered by already-merged `mcp_pro_review` (auto-merge gating policies live in `crates/spur-mcp/src/plan/reconciler.rs:205`). Dropped.
- `pm_team_webhooks` — vaporware: no receiver implementation. Deferred to v2 backlog.

**Narrowed scope:** `pm_pro_beads_advanced` retained but description narrowed from omnibus "plan persistence + projection + mutation + signal-watch + auto-merge" (those live in spur-mcp) to its actual PM-crate boundary: `PmService::advanced()` activation + `BeadsAdvanced` extension surface.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_pm_keys_registered() {
        for s in &[
            "pm_core_beads_basic",
            "pm_core_browse",
            "pm_core_pr",
            "pm_core_beads_graph_adapter",
            "pm_pro_beads_advanced",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-pm (5) ---
    pub const PM_CORE_BEADS_BASIC: Self = Self("pm_core_beads_basic");
    pub const PM_CORE_BROWSE: Self = Self("pm_core_browse");
    pub const PM_CORE_PR: Self = Self("pm_core_pr");
    pub const PM_CORE_BEADS_GRAPH_ADAPTER: Self = Self("pm_core_beads_graph_adapter");
    pub const PM_PRO_BEADS_ADVANCED: Self = Self("pm_pro_beads_advanced");
```

Parser arms:

```rust
        // spur-pm
        } else if bytes_eq(b, b"pm_core_beads_basic") {
            Some(Self::PM_CORE_BEADS_BASIC)
        } else if bytes_eq(b, b"pm_core_browse") {
            Some(Self::PM_CORE_BROWSE)
        } else if bytes_eq(b, b"pm_core_pr") {
            Some(Self::PM_CORE_PR)
        } else if bytes_eq(b, b"pm_core_beads_graph_adapter") {
            Some(Self::PM_CORE_BEADS_GRAPH_ADAPTER)
        } else if bytes_eq(b, b"pm_pro_beads_advanced") {
            Some(Self::PM_PRO_BEADS_ADVANCED)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-pm keys (5) for tier revamp Plan A`

---

## Task 16: Add spur-cost keys (3)

Per spec §4.7 (revised 2026-04-27): 2 Free + 1 Pro.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 6 → 3 keys per 4-reviewer judge synthesis:
- Renamed `cost_core_basic_display` → `cost_core_session_display` (claude-code: drop `_basic_` orphan suffix; codex: scoped to actual `today_summary` ledger).
- Removed `cost_core_ingestion_pipeline` — always-coupled prerequisite to all cost capabilities; not independently gateable. Codex confirmed code is JSONL-only (not ACP).
- Removed `cost_pro_sqlite_wal_mode` — codex ❌ NOT IMPLEMENTED. If implemented, would be database-correctness baseline (Risk #29) for Free per Wave 4 safety/liveness precedent, not a Pro upsell.
- Deferred `cost_pro_budget_caps` to v1.1 backlog — codex ❌ no spawn/runtime enforcement.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_cost_keys_registered() {
        for s in &[
            "cost_core_session_display",
            "cost_core_pricing_registry",
            "cost_pro_per_project_tracking",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-cost (3) ---
    pub const COST_CORE_SESSION_DISPLAY: Self = Self("cost_core_session_display");
    pub const COST_CORE_PRICING_REGISTRY: Self = Self("cost_core_pricing_registry");
    pub const COST_PRO_PER_PROJECT_TRACKING: Self = Self("cost_pro_per_project_tracking");
```

Parser arms:

```rust
        // spur-cost
        } else if bytes_eq(b, b"cost_core_session_display") {
            Some(Self::COST_CORE_SESSION_DISPLAY)
        } else if bytes_eq(b, b"cost_core_pricing_registry") {
            Some(Self::COST_CORE_PRICING_REGISTRY)
        } else if bytes_eq(b, b"cost_pro_per_project_tracking") {
            Some(Self::COST_PRO_PER_PROJECT_TRACKING)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-cost keys (3) for tier revamp Plan A`

---

## Task 17: Add spur-context keys (3)

Per spec §4.8 (revised 2026-04-27): 3 Pro.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 5 → 3 keys per 4-reviewer judge synthesis:
- Removed `ctx_pro_async_engine` — codex ⚠ no production callers found for `AsyncEngine`. Pure threading infrastructure with no user-visible boundary. Drop, do not defer.
- Deferred `ctx_pro_live_mode` to v1.1 backlog — codex ⚠ APIs exist but no CLI/user surface; gate has no enforcement point.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_context_keys_registered() {
        for s in &[
            "ctx_pro_duckdb_engine",
            "ctx_pro_daily_report",
            "ctx_pro_weekly_report",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-context (3) ---
    pub const CTX_PRO_DUCKDB_ENGINE: Self = Self("ctx_pro_duckdb_engine");
    pub const CTX_PRO_DAILY_REPORT: Self = Self("ctx_pro_daily_report");
    pub const CTX_PRO_WEEKLY_REPORT: Self = Self("ctx_pro_weekly_report");
```

Parser arms:

```rust
        // spur-context
        } else if bytes_eq(b, b"ctx_pro_duckdb_engine") {
            Some(Self::CTX_PRO_DUCKDB_ENGINE)
        } else if bytes_eq(b, b"ctx_pro_daily_report") {
            Some(Self::CTX_PRO_DAILY_REPORT)
        } else if bytes_eq(b, b"ctx_pro_weekly_report") {
            Some(Self::CTX_PRO_WEEKLY_REPORT)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-context keys (3) for tier revamp Plan A`

---

## Task 18: Add spur-worktree keys (2)

Per spec §4.9 (revised 2026-04-27): 2 Free.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 5 → 2 keys per 4-reviewer judge synthesis:
- Removed `worktree_core_artifact_resolver` — always-on for system to function (delegation outcomes can't be returned without artifact lookup); not independently gateable.
- Renamed `worktree_pro_cleanup_orphans` → `worktree_core_orphan_cleanup` AND moved Pro→Free (claude-code: verb→noun convention; codex confirmed code exists at `manager.rs:539` + `worktree_authority.rs:99`; Wave 4 safety/liveness precedent: garbage collection is a correctness invariant, never a paywall — analogous to Postgres VACUUM, RocksDB compaction).
- Deferred `worktree_pro_git_blob_store` to v1.1 backlog — codex ⚠ orchestrator hardwires GitBlob; no Free/Pro selector exists.
- Deferred `worktree_pro_custom_policies` to v1.1 backlog — codex ❌ only single cherry-pick path; vaporware.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_worktree_keys_registered() {
        for s in &[
            "worktree_core_isolation",
            "worktree_core_orphan_cleanup",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-worktree (2) ---
    pub const WORKTREE_CORE_ISOLATION: Self = Self("worktree_core_isolation");
    pub const WORKTREE_CORE_ORPHAN_CLEANUP: Self = Self("worktree_core_orphan_cleanup");
```

Parser arms:

```rust
        // spur-worktree
        } else if bytes_eq(b, b"worktree_core_isolation") {
            Some(Self::WORKTREE_CORE_ISOLATION)
        } else if bytes_eq(b, b"worktree_core_orphan_cleanup") {
            Some(Self::WORKTREE_CORE_ORPHAN_CLEANUP)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-worktree keys (2) for tier revamp Plan A`

---

## Task 19: Add spur-bot keys (3)

Per spec §4.10 (revised 2026-04-27): 3 Pro.

**Revised 2026-04-26 (Wave 5).** Added `bot_pro_telegram_solo` (relocated from spur-tui §4.4); per codex code-grounded review the gate point is `Commands::Bot` (`crates/spur-cli/src/main.rs:591`) / `run_telegram_bot` (`crates/spur-bot/src/telegram/mod.rs:9`), with single-operator filter at `router.rs:25`.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 7 → 3 keys per 4-reviewer judge synthesis. Core principle: bot sub-keys must be *independently business-toggleable*, not just real boundaries in code:
- Removed `bot_pro_runtime` — always-coupled to telegram_solo (no telegram bot without long-poll loop). Folded under umbrella.
- Removed `bot_pro_runtime_render` — always-coupled (raw text mode is degenerate UX, not a tier).
- Removed `bot_pro_callback_validation` — security invariant (analogous to dropped `license_core_ed25519_verify`); never a Pro upsell.
- Deferred `bot_team_multi_chat` to v2 backlog — codex ❌ no multi-user code.

Retained the 3 keys with plausible business tier axes: telegram_solo (umbrella), thread_registry (single-thread vs multi-thread bots), inline_review (passive notify-only vs interactive review bots).

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_bot_keys_registered() {
        for s in &[
            "bot_pro_telegram_solo",
            "bot_pro_thread_registry",
            "bot_pro_inline_review",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-bot (3) ---
    pub const BOT_PRO_TELEGRAM_SOLO: Self = Self("bot_pro_telegram_solo");
    pub const BOT_PRO_THREAD_REGISTRY: Self = Self("bot_pro_thread_registry");
    pub const BOT_PRO_INLINE_REVIEW: Self = Self("bot_pro_inline_review");
```

Parser arms:

```rust
        // spur-bot
        } else if bytes_eq(b, b"bot_pro_telegram_solo") {
            Some(Self::BOT_PRO_TELEGRAM_SOLO)
        } else if bytes_eq(b, b"bot_pro_thread_registry") {
            Some(Self::BOT_PRO_THREAD_REGISTRY)
        } else if bytes_eq(b, b"bot_pro_inline_review") {
            Some(Self::BOT_PRO_INLINE_REVIEW)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-bot keys (3) for tier revamp Plan A`

---

## Task 20: Add spur-license meta keys (2)

Per spec §4.11 (revised 2026-04-27): 2 Pro.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 6 → 2 keys per 4-reviewer judge synthesis. The original 6-key set conflated *runtime gating dispatch* (this registry's purpose) with *system manifest documentation* (which lives in spec body / `docs/architecture.md`).

Removed via **Bootstrap Paradox** principle (gemini + codex aligned):
- `license_core_facade_entitlement` — IS the gating mechanism (`FeatureGate::has`); cannot gate itself.
- `license_core_policy_resolver` — must run for ANY policy (including one that disables it) to load.
- `license_core_ed25519_verify` — build-time integrity invariant (`build.rs:28`); not a runtime capability.

Renamed + tier-shifted:
- `license_core_provider_heartbeat` → `license_pro_revocation_polling` (Free→Pro): networked Pro capability; Free runs offline-only.

Deferred to v1.1 backlog:
- `license_pro_quota_runtime_downgrade` — codex ⚠ runtime does not propagate license refreshes into `FeatureGate::update_state`; not enforced.

Retained as the only non-paradoxical license-system gate: `license_pro_offline_grace` (Pro-only by nature: Free has no polling so offline grace is moot/automatic).

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_license_keys_registered() {
        for s in &[
            "license_pro_revocation_polling",
            "license_pro_offline_grace",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-license meta (2) ---
    pub const LICENSE_PRO_REVOCATION_POLLING: Self = Self("license_pro_revocation_polling");
    pub const LICENSE_PRO_OFFLINE_GRACE: Self = Self("license_pro_offline_grace");
```

Parser arms:

```rust
        // spur-license meta
        } else if bytes_eq(b, b"license_pro_revocation_polling") {
            Some(Self::LICENSE_PRO_REVOCATION_POLLING)
        } else if bytes_eq(b, b"license_pro_offline_grace") {
            Some(Self::LICENSE_PRO_OFFLINE_GRACE)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-license meta keys (2) for tier revamp Plan A`

---

## Task 21: Add spur-blob-store keys (1) — Wave 7 revised

**Wave 7 4-reviewer + L9-MCTS synthesis:** original 4-key proposal reduced to 1 keep + 3 drops. Per spec §4.12 (revised) and §4.16 (Wave 7 entries):
- DROP `blob_core_memory_backend`, `blob_core_fs_backend`, `blob_pro_measured_backend` — trait-impl variants chosen at construction time / always-on telemetry hardwired in `Orchestrator::new` at `crates/spur-core/src/orchestrator.rs:963`. Not user-toggleable.
- KEEP `blob_pro_namespace_deletion` (renamed from `blob_pro_delete_namespace` per claude-code noun-pattern consistency review). Real CLI dispatch site: `spur gc outcomes --namespace`.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_blob_store_keys_registered() {
        for s in &[
            "blob_pro_namespace_deletion",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add const**

```rust
    // --- spur-blob-store (1: 0 Free + 1 Pro) ---
    pub const BLOB_PRO_NAMESPACE_DELETION: Self = Self("blob_pro_namespace_deletion");
```

Parser arm:

```rust
        // spur-blob-store
        } else if bytes_eq(b, b"blob_pro_namespace_deletion") {
            Some(Self::BLOB_PRO_NAMESPACE_DELETION)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add blob_pro_namespace_deletion (Wave 7) for tier revamp Plan A`

---

## Task 22: ~~Add spur-interactive keys (3)~~ — DROPPED ENTIRELY (Wave 7)

**Wave 7 4-reviewer + L9-MCTS synthesis: all 3 keys dropped.** Per spec §4.13 (revised) and §4.16 (Wave 7 entries):
- `interactive_core_frontend_host` — shared infrastructure used by both TUI (`crates/spur-cli/src/main.rs:710`) and Telegram bot (`crates/spur-bot/src/telegram/mod.rs:9`); not tier-gated.
- `interactive_core_review_lane_mpsc` — production correctness invariant (`SubmitReview` rejected on command lane at `crates/spur-interactive/src/host.rs:21`); architecture, not feature toggle.
- `interactive_core_shutdown_orchestrator` — always-on lifecycle hygiene tightly coupled to frontend_host.

**Skip this task entirely.** No code changes. No commit.

---

## Task 23: ~~Add cross-crate notification keys (2)~~ — DROPPED ENTIRELY (Wave 7)

**Wave 7 4-reviewer + L9-MCTS synthesis: both keys dropped/deferred. Entire `notif_*` namespace evaporates from v1.** Per spec §4.14 (revised) and §4.16 (Wave 7 entries):
- `notif_core_in_tui` — DROP. Redundant with already-merged `core_core_notification_pump` (producer at `crates/spur-core/src/notification_pump.rs:30`) + `tui_core_notification_drain` (consumer at `crates/spur-tui/src/app.rs:2552`); triple-naming the same path.
- `notif_pro_external_channels` — DEFER to §4.16 v1.1 backlog. Greenfield vaporware; no Slack/Discord/email/webhook subsystem exists. Telegram already has its own `bot_pro_*` keys.

**Skip this task entirely.** No code changes. No commit.

---

## Task 23b: Wave-8 second-order composition pruning (NEW — added 2026-04-27)

**Wave 8 4-reviewer (kimi mechanical truth-table + codex code-grounded coupling tracing) + L9-MCTS judge synthesis** identified 15 over-decomposed families, 4 additional drops, and 5 vaporware deferrals. Per spec §4.16 Wave-8 entries (consolidations + drops + defers), this task removes 38 prior-wave entries and adds 0 new keys. Net: 102 (post-Wave-7) → 64 (post-Wave-8) new keys; total registry 138 → 100 consts.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs` (remove consts, parser arms, per-crate test arms; replace with consolidated umbrella keys)

**Pruning checklist** (15 consolidations + 4 drops + 5 defers, total 38 entries removed):

**Consolidations (collapse N→1):**

- [ ] brain trio → `core_core_brain_session`: remove `core_core_brain_scheduler`, `core_core_continuation_bridge` (2 removed)
- [ ] workers pair → `core_core_parallel_workers`: remove `core_core_cancellable_semaphore` (1 removed)
- [ ] event sextet → `core_core_event_pipeline` (NEW umbrella replacing 6 keys): remove `core_core_event_funnel_broadcast`, `core_core_event_sink_ndjson_128mb`, `core_core_executor_lineage_projection`, `core_core_notification_pump`, `core_core_agent_notification`, `tui_core_notification_drain`; add `core_core_event_pipeline` (5 net removed)
- [ ] review trio → `core_core_review` (NEW umbrella replacing 3 keys): remove `core_core_review_sink`, `core_core_review_timeout`, `core_core_review_retry`; add `core_core_review` (2 net removed)
- [ ] review pro pair → `core_pro_review_auto_approve`: remove `core_pro_review_timeout_routing` (1 removed)
- [ ] peer_mailbox trio → `core_pro_peer_mailbox_router`: remove `core_pro_peer_mailbox_ledger`, `core_pro_peer_mailbox_stranded_recon` (2 removed)
- [ ] plan_persistence pair → `core_core_plan_persistence`: remove `core_core_plan_orphan_recovery` (1 removed)
- [ ] skills quartet → `skills_core_registry`: remove `skills_core_atomic_installation`, `skills_core_render_per_vendor`, `skills_pro_role_gating` (3 removed)
- [ ] ctx triple → `ctx_pro_duckdb_engine`: remove `ctx_pro_daily_report`, `ctx_pro_weekly_report` (2 removed)
- [ ] mcp delegate pair → `mcp_core_delegate`: remove `mcp_core_outcome_materializer` (1 removed)
- [ ] mcp plan_durable pair → `mcp_pro_plan_durable`: remove `mcp_pro_reconciler_journal_notify` (1 removed)
- [ ] mcp signal_watcher pair → `mcp_pro_signal_watcher_scope_drift`: remove `mcp_pro_mutation_executor` (1 removed)
- [ ] session_attach pair → `acp_core_session_attach_advisory_lock`: remove `acp_core_session_attach_degraded_nolock` (1 removed)
- [ ] tui shell trio → `tui_core_view_dashboard`: remove `tui_core_view_landing_decision`, `tui_core_view_composer` (2 removed)
- [ ] bot pair → `bot_pro_telegram_solo`: remove `bot_pro_thread_registry` (1 removed)

**Drops (non-toggleable / ghost / mechanism plumbing):**

- [ ] `core_core_background_task_tracker` (mechanism plumbing — JoinHandle ownership)
- [ ] `acp_core_adapter_cursor` (ghost — no AgentKind variant)
- [ ] `acp_core_adapter_opencode` (ghost — same)
- [ ] `acp_core_adapter_kimi` (ghost — same; `kimi` exists as delegation worker, separate namespace)

**Defers to §4.16 v1.1 backlog (vaporware):**

- [ ] `core_pro_brain_failover_auto_pool`
- [ ] `core_pro_broadcast_lagged_recovery`
- [ ] `core_core_conflict_detection`
- [ ] `core_core_rate_limit_detection`
- [ ] `mcp_pro_custom_tools`

**Step process:**

- [ ] **Step 1:** For each consolidation, write a failing test asserting the umbrella key exists (`FeatureKey::from_known("core_core_event_pipeline").is_some()`) and the absorbed keys do NOT exist (`FeatureKey::from_known("core_core_event_funnel_broadcast").is_none()`). This converts the registry from "additive" to "consolidated" semantics.
- [ ] **Step 2:** Remove the absorbed/dropped/deferred consts (38 lines) from the const block.
- [ ] **Step 3:** Remove the corresponding `else if bytes_eq` arms from `from_known()`.
- [ ] **Step 4:** Add the 2 NEW umbrella consts: `core_core_event_pipeline`, `core_core_review`.
- [ ] **Step 5:** Add the 2 NEW umbrella parser arms.
- [ ] **Step 6:** Update each per-crate test that referenced an absorbed/dropped key (consolidate or remove the test).
- [ ] **Step 7:** Run `cargo test --package spur-license --lib policy::feature_key` — expect ALL PASS with the new shape.
- [ ] **Step 8:** Run `cargo build --workspace` — expect PASS (no caller in main has migrated to new keys yet, so no compile-time references to dropped consts).
- [ ] **Step 9:** Commit: `refactor(spur-license): Wave 8 second-order composition rationalization (102→64 new keys; 15 consolidations + 4 drops + 5 defers)`

After this task completes, the registry has exactly 100 consts (36 legacy + 64 Wave-8-final new). Then proceed to Task 24 to add the comprehensive 64-key roundtrip test.

---

## Task 24: Final integration test — total count + comprehensive roundtrip

This task verifies the full 64-new-key registry (Wave 8 final count after second-order composition consolidation) and updates the count guard test from Task 2. Note: this task is now executed AFTER a Wave-8 registry-pruning task that removes 38 prior-wave entries (consolidations + drops + defers).

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs` (extend count test, add comprehensive roundtrip)

- [ ] **Step 1: Add the comprehensive roundtrip test**

Add this test inside `mod tests` (after `notification_keys_registered`):

```rust
    /// Asserts every new key from the tier revamp roundtrips correctly.
    /// Total: 64 new v1 keys (46 Free + 17 Pro v1 + 1 Pro v1.1 + 0 Team) — Wave 8 final.
    /// Wave 8 collapsed 15 over-decomposed families (compile-coupled / all-or-nothing
    /// substate space) into umbrella keys, dropped 4 (mechanism plumbing + ghost ACP
    /// adapters), and deferred 5 vaporware to §4.16 v1.1 backlog.
    #[test]
    fn tier_revamp_v1_keys_roundtrip() {
        const NEW_KEYS: &[&str] = &[
            // spur-acp (7) — Wave 8: dropped 3 ghost adapters (cursor/opencode/kimi — no AgentKind variants); merged session_attach_degraded_nolock into advisory_lock (degraded is fallback path of failed lock attempt)
            "acp_core_transport_stdio", "acp_core_transport_socket",
            "acp_core_adapter_claude_code", "acp_core_adapter_codex",
            "acp_core_adapter_gemini", "acp_core_adapter_kiro",
            "acp_core_session_attach_advisory_lock",
            // spur-core: brain (2) — Wave 8: consolidated brain_session+brain_scheduler+continuation_bridge → brain_session (scheduler requires session, bridge enqueues to ingress); deferred brain_failover_auto_pool (vaporware, no alternate pool)
            "core_core_brain_session",
            "core_core_brain_failover_manual_keystroke",
            // spur-core: workers (2) — Wave 8: merged cancellable_semaphore into parallel_workers (semaphore IS the parallelism mechanism)
            "core_core_parallel_workers",
            "core_pro_worker_heartbeat_watchdog",
            // spur-core: event pipeline (1) — Wave 8: collapsed funnel+sink+lineage+notification_pump+agent_notification+tui_notification_drain → event_pipeline (compile-coupled producer/consumer chain; sink subscribes to broadcast, lineage applied inside funnel, drain consumes from event bus); deferred broadcast_lagged_recovery (no recovery logic)
            "core_core_event_pipeline",
            // spur-core: review (3) — Wave 8: consolidated sink+timeout+retry → review (timeout/retry without sink = no receiver); merged auto_approve+timeout_routing → auto_approve (auto IS timeout fallback)
            "core_core_review",
            "core_pro_review_auto_approve",
            "core_pro_review_retry_config",
            // skills (2) — Wave 8: consolidated registry+atomic_installation+render_per_vendor+role_gating → registry (single installer code path)
            "skills_core_registry",
            "skills_pro_custom",
            // spur-core: peer mailbox (1) — Wave 8: collapsed router+ledger+stranded_recon → router (router constructor REQUIRES ledger+reconciler; compile-coupled)
            "core_pro_peer_mailbox_router",
            // spur-core: system events (1) — Wave 8: deferred conflict_detection + rate_limit_detection (no production emitters); kept permission_request_detection (real ACP callback flow); event_funnel_broadcast/notification_pump/agent_notification absorbed by event_pipeline above
            "core_core_permission_request_detection",
            // spur-core: reliability & lifecycle (3) — Wave 8: merged plan_persistence+orphan_recovery → plan_persistence (recovery is safety baseline, OFF state = orphans); dropped background_task_tracker (mechanism plumbing)
            "core_core_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_plan_persistence",
            // spur-mcp (10) — Wave 8: merged outcome_materializer into delegate (back-end mechanism with no separate MCP tool); merged reconciler_journal_notify into plan_durable (couples to beads+notify); merged mutation_executor into signal_watcher_scope_drift (compile-coupled apply_mutation call); deferred mcp_pro_custom_tools (no dynamic registry)
            "mcp_core_server_dispatch", "mcp_core_delegate",
            "mcp_core_outcome_fetch", "mcp_core_pm",
            "mcp_core_pr", "mcp_core_plan_ephemeral",
            "mcp_pro_plan_durable",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_graph_tools", "mcp_pro_review",
            // spur-tui (8) — Wave 8: collapsed dashboard+landing_decision+composer → dashboard (single view state graph); merged tui_core_notification_drain into core_core_event_pipeline above
            "tui_core_view_dashboard", "tui_core_view_session_detail",
            "tui_core_view_plan_inspector", "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser",
            "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom",
            // spur-cli (9) — KEEP_ATOMIC (each command is independent clap arm with separate handler)
            "cli_core_init", "cli_core_agents",
            "cli_core_sessions", "cli_core_run",
            "cli_core_exec", "cli_core_tui",
            "cli_core_cost", "cli_core_connect",
            "cli_core_license_activate",
            // spur-pm (5) — KEEP_ATOMIC (advanced requires basic — DOCUMENT_PREREQ)
            "pm_core_beads_basic", "pm_core_browse",
            "pm_core_pr", "pm_core_beads_graph_adapter",
            "pm_pro_beads_advanced",
            // spur-cost (3) — KEEP_ATOMIC with prereqs (display + per_project_tracking both require pricing_registry)
            "cost_core_session_display", "cost_core_pricing_registry",
            "cost_pro_per_project_tracking",
            // spur-context (1) — Wave 8: consolidated duckdb_engine + daily_report + weekly_report → duckdb_engine (reports are wrappers over AnalyticsEngine)
            "ctx_pro_duckdb_engine",
            // spur-worktree (2) — KEEP_ATOMIC with prereq (cleanup requires isolation; cleanup OFF is valid degradation)
            "worktree_core_isolation", "worktree_core_orphan_cleanup",
            // spur-bot (2) — Wave 8: merged thread_registry into telegram_solo (thread_registry without telegram makes no sense; single-thread is degraded telegram_solo); kept inline_review separate (security-conscious users can disable remote approval)
            "bot_pro_telegram_solo",
            "bot_pro_inline_review",
            // spur-license meta (2) — KEEP_ATOMIC with prereq (offline_grace meaningful only for Pro since only Pro polls)
            "license_pro_revocation_polling", "license_pro_offline_grace",
            // spur-blob-store (1) — Wave 7 final
            "blob_pro_namespace_deletion",
            // spur-interactive (0) — Wave 7 dropped all 3
            // Notifications (0) — Wave 7 dropped/deferred both
        ];

        assert_eq!(
            NEW_KEYS.len(),
            64,
            "Expected exactly 64 new tier-revamp v1 keys post-Wave-8 (was 135 \
             pre-Wave-5, 123 post-Wave-5, 107 post-Wave-6, 99 post-Wave-7; \
             Wave 8 net -35 keys: 15 family consolidations (compile-coupled / \
             all-or-nothing substate space) + 4 drops (background_task_tracker \
             mechanism plumbing + 3 ghost ACP adapters) + 5 vaporware deferrals \
             per spec §4.16 Wave-8 entries; offset by net -11 from prior v1.1 \
             keys absorbed into umbrellas), got {}",
            NEW_KEYS.len()
        );

        let mut seen = std::collections::HashSet::new();
        for s in NEW_KEYS {
            let parsed = FeatureKey::from_known(s);
            assert!(parsed.is_some(), "key {s:?} not parseable via from_known");
            let key = parsed.unwrap();
            assert_eq!(key.as_str(), *s, "as_str roundtrip mismatch for {s}");
            assert!(seen.insert(*s), "duplicate key in test list: {s}");
        }
    }
```

- [ ] **Step 2: Run the comprehensive test**

Run: `cargo test --package spur-license --lib policy::feature_key::tests::tier_revamp_v1_keys_roundtrip`

Expected: PASS — all 64 v1 keys roundtrip correctly.

- [ ] **Step 3: Run the full feature_key test suite**

Run: `cargo test --package spur-license --lib policy::feature_key`

Expected: ALL PASS — original 36-key tests + per-crate tests (Wave 8 collapsed many of these; expect ~13–15 surviving per-crate tests after consolidation pruning) + comprehensive 64-key test + count guard.

- [ ] **Step 4: Run the full spur-license test suite**

Run: `cargo test --package spur-license`

Expected: ALL PASS — including emission_audit and licenseseat_probe integration tests.

- [ ] **Step 5: Build the full workspace and run clippy**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`

Expected: Both PASS — no compile errors, no clippy warnings introduced.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "test(spur-license): comprehensive 64-key registry roundtrip for tier revamp Plan A (Wave 8 final)"
```

---

## Task 25: Document the registry-vs-policy mismatch (advance notice for Plan B)

After this plan ships, the `FeatureKey` registry has 36 OLD keys + 64 NEW keys = 100 total typed constants (Wave 8 final). The embedded `default_policy.json` STILL references only the OLD keys, so:
- Free users still get 11 Community features (per old policy)
- The 64 new keys are typed-known but not in any tier yet
- Plan B will rewrite the policy and migrate callers

This task adds an inline doc comment marking the boundary so the next contributor understands the staged migration.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs` (add boundary comment)
- Create: `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-status.md` (status hand-off note)

- [ ] **Step 1: Add boundary comment to feature_key.rs**

In `crates/spur-license/src/policy/feature_key.rs`, immediately AFTER the `// --- G2 flag keys (4) ---` block and BEFORE the `// === Tier revamp v1 keys ===` separator added in Task 3, insert:

```rust
    // ============================================================
    // === BOUNDARY: keys above this line are pre-tier-revamp ====
    // === (legacy v0 policy still references them); keys below ==
    // === this line are added by tier revamp Plan A and become ==
    // === active when Plan B ships the rewritten policy. ========
    // ============================================================
```

- [ ] **Step 2: Create the Plan A status hand-off**

Create `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-status.md`:

```markdown
# Tier Revamp Plan A — Status Hand-off

**Status:** ✅ Complete
**Date:** 2026-04-26
**Spec:** `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
**Plan:** `docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-registry-expansion.md`

## What Plan A delivered

- 64 new typed `FeatureKey` constants in `crates/spur-license/src/policy/feature_key.rs` (Wave 8 final, down from 135 across Waves 5+6+7+8)
- 1 new `QuotaKey` variant: `BrainFailoverChainDepth`
- Roundtrip test coverage for every new key (per-crate tests + comprehensive 99-key test)
- Count guard test for original 36-key registry (locks against accidental removal)
- Inline boundary comment marking legacy keys vs new keys

## What Plan A did NOT change

- `crates/spur-license/resources/default_policy.json` (still references legacy 36 keys)
- `crates/spur-license/build.rs` (build-time policy verification still uses legacy schema)
- `crates/spur-license/src/gate.rs` (existing API unchanged; new keys go through the same path)
- `crates/spur-license/src/licenseseat.rs` (no trial flow yet — Plan D)
- Any consumer crate (`spur-core`, `spur-mcp`, `spur-pm`, `spur-tui`, etc.) — no `FeatureGate::require()` calls added yet (Plan C)
- CLI commands (`spur upgrade trial`, `spur upgrade pro`, `spur license activate` not yet implemented — Plan D)

## Behavioral state after Plan A

- Free users: identical experience to pre-Plan A (legacy 11-key Community policy still active)
- Pro users (if any exist): identical experience (legacy 8 Pro keys still active)
- New 64 keys: typed-known but unreachable through `FeatureGate::has()` because no policy declares them in any tier
- Workspace builds clean; clippy passes; all tests green

## Plan B prerequisites (verify before starting Plan B)

- [ ] All 25 Plan A tasks committed
- [ ] `cargo test --package spur-license` passes
- [ ] `cargo build --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] Git log shows ~25 atomic commits with `tier revamp Plan A` in message

## What Plan B will do

1. Rewrite `crates/spur-license/resources/default_policy.json` per spec §5
2. Extend `PolicyResolver` to handle `@inherit:community` directive
3. Extend `PolicyResolver` to handle `v1_1_q3_roadmap` field
4. Re-sign with `spur-policy-2026-04` Ed25519 key (use `scripts/sign-policy.sh`)
5. Update `build.rs` compile-time check to validate new schema
6. Migrate existing call sites that reference legacy keys (see spec §8.2 rename map)
7. Remove legacy 36 keys from `feature_key.rs` after migration completes
8. Update `from_known()` to no longer parse legacy keys

After Plan B ships, the registry has only the 64 new keys and the policy reflects the new tier structure.
```

- [ ] **Step 3: Commit the status hand-off**

```bash
git add crates/spur-license/src/policy/feature_key.rs docs/superpowers/plans/2026-04-26-tier-revamp-plan-a-status.md
git commit -m "docs(spur-license): tier revamp Plan A boundary marker + status hand-off"
```

- [ ] **Step 4: Final verification — full test + lint pass**

Run: `cargo test --package spur-license && cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`

Expected: All three pass cleanly.

- [ ] **Step 5: Final commit (if any straggler files)**

Run: `git status` to confirm clean working tree. If anything is unstaged, investigate before committing.

---

## Self-Review Checklist (verification after all tasks complete)

After running all 25 tasks, verify the final state:

- [ ] `cargo test --package spur-license --lib feature_key 2>&1 | grep "test result"` shows ~22 passing tests (count_guard + 19 per-crate tests + comprehensive; Wave 7 dropped Tasks 22 + 23 entirely so per-crate test count fell from 21 to 19)
- [ ] `grep -c "pub const" crates/spur-license/src/policy/feature_key.rs` returns exactly 100 (36 legacy + 64 new post-Wave-8)
- [ ] `grep -c "Some(Self::" crates/spur-license/src/policy/feature_key.rs` returns exactly 100 in the `from_known` chain
- [ ] No call site in any other crate references the new keys yet (verify with `grep -r "FeatureKey::ACP_CORE_" crates/` returns ONLY hits in `feature_key.rs`)
- [ ] Workspace builds with no warnings
- [ ] All ~25 commits are atomic and follow the `tier revamp Plan A` naming pattern

If any check fails, fix before declaring Plan A complete.

---

## Plan A Summary

- **24 task groups** (Tasks 1-24) + 1 closure task (Task 25) = **25 tasks total**
- Each task is **6 steps** of TDD (write test → fail → implement → pass → build → commit), except Task 24 (5 steps, comprehensive test) and Task 25 (5 steps, hand-off)
- **~150 individual checkbox steps**
- Estimated execution time: **3-5 hours** for an engineer following the plan exactly (each commit is small and tested)
- Zero behavior change after Plan A — purely additive registry expansion

**Next plan:** `Plan B — Policy rewrite + signing + caller migration` (to be authored after Plan A executes successfully).
