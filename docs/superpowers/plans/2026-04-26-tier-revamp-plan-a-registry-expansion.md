# Tier Revamp — Plan A: Registry Expansion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 135 new `FeatureKey` constants and 1 new `QuotaKey` variant to `spur-license` (additive — old keys remain alongside new keys). No behavior change yet; this is the typed registry foundation that Plans B–E build on.

**Architecture:** Pure additive changes to `crates/spur-license/src/policy/feature_key.rs` and `crates/spur-license/src/quota.rs`. New keys follow the `<crate>_<tier>_<capability>` naming convention from the spec. The existing `from_known()` parser is extended; the existing `FeatureKey` newtype + `bytes_eq()` const helper are reused unchanged. After this plan ships, the codebase has a dual registry (old + new keys); Plan B will rewrite the policy doc and migrate all existing call sites; Plan B's final task will remove the old keys.

**Tech Stack:** Rust 2021, `spur-license` crate, `cargo test --package spur-license`, no new dependencies.

**Spec reference:** `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md` §4 (full feature key registry, 135 keys total).

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `crates/spur-license/src/quota.rs` | Modify | Add `BrainFailoverChainDepth` variant to `QuotaKey` enum |
| `crates/spur-license/src/policy/feature_key.rs` | Modify | Add 135 new `pub const` declarations grouped by crate prefix; extend `from_known()` parser; add new tests |

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
//! §4 for the full 135-key registry.
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

## Task 10: Add spur-core system events keys (5)

Adds: conflict_detection, rate_limit_detection, license_event_broadcast, permission_request_prompt, ext_notification.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_system_events_keys_registered() {
        for s in &[
            "core_core_conflict_detection",
            "core_core_rate_limit_detection",
            "core_core_license_event_broadcast",
            "core_core_permission_request_prompt",
            "core_core_ext_notification",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: system events (5) ---
    pub const CORE_CORE_CONFLICT_DETECTION: Self = Self("core_core_conflict_detection");
    pub const CORE_CORE_RATE_LIMIT_DETECTION: Self = Self("core_core_rate_limit_detection");
    pub const CORE_CORE_LICENSE_EVENT_BROADCAST: Self = Self("core_core_license_event_broadcast");
    pub const CORE_CORE_PERMISSION_REQUEST_PROMPT: Self = Self("core_core_permission_request_prompt");
    pub const CORE_CORE_EXT_NOTIFICATION: Self = Self("core_core_ext_notification");
```

Parser arms:

```rust
        // spur-core: system events
        } else if bytes_eq(b, b"core_core_conflict_detection") {
            Some(Self::CORE_CORE_CONFLICT_DETECTION)
        } else if bytes_eq(b, b"core_core_rate_limit_detection") {
            Some(Self::CORE_CORE_RATE_LIMIT_DETECTION)
        } else if bytes_eq(b, b"core_core_license_event_broadcast") {
            Some(Self::CORE_CORE_LICENSE_EVENT_BROADCAST)
        } else if bytes_eq(b, b"core_core_permission_request_prompt") {
            Some(Self::CORE_CORE_PERMISSION_REQUEST_PROMPT)
        } else if bytes_eq(b, b"core_core_ext_notification") {
            Some(Self::CORE_CORE_EXT_NOTIFICATION)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core system events keys (5) for tier revamp Plan A`

---

## Task 11: Add spur-core reliability & lifecycle keys (5)

Adds: basic_session_resume, session_resume_event_replay, basic_plan_persistence, plan_orphan_recovery, background_task_tracker.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_core_reliability_keys_registered() {
        for s in &[
            "core_core_basic_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_basic_plan_persistence",
            "core_pro_plan_orphan_recovery",
            "core_pro_background_task_tracker",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-core: reliability & lifecycle (5) ---
    pub const CORE_CORE_BASIC_SESSION_RESUME: Self = Self("core_core_basic_session_resume");
    pub const CORE_PRO_SESSION_RESUME_EVENT_REPLAY: Self = Self("core_pro_session_resume_event_replay");
    pub const CORE_CORE_BASIC_PLAN_PERSISTENCE: Self = Self("core_core_basic_plan_persistence");
    pub const CORE_PRO_PLAN_ORPHAN_RECOVERY: Self = Self("core_pro_plan_orphan_recovery");
    pub const CORE_PRO_BACKGROUND_TASK_TRACKER: Self = Self("core_pro_background_task_tracker");
```

Parser arms:

```rust
        // spur-core: reliability & lifecycle
        } else if bytes_eq(b, b"core_core_basic_session_resume") {
            Some(Self::CORE_CORE_BASIC_SESSION_RESUME)
        } else if bytes_eq(b, b"core_pro_session_resume_event_replay") {
            Some(Self::CORE_PRO_SESSION_RESUME_EVENT_REPLAY)
        } else if bytes_eq(b, b"core_core_basic_plan_persistence") {
            Some(Self::CORE_CORE_BASIC_PLAN_PERSISTENCE)
        } else if bytes_eq(b, b"core_pro_plan_orphan_recovery") {
            Some(Self::CORE_PRO_PLAN_ORPHAN_RECOVERY)
        } else if bytes_eq(b, b"core_pro_background_task_tracker") {
            Some(Self::CORE_PRO_BACKGROUND_TASK_TRACKER)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-core reliability keys (5) for tier revamp Plan A`

---

## Task 12: Add spur-mcp keys (14)

Per spec §4.3: 7 Free + 7 Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_mcp_keys_registered() {
        for s in &[
            "mcp_core_server_dispatch",
            "mcp_core_delegate_basic",
            "mcp_core_outcome_fetch",
            "mcp_core_pm_basic",
            "mcp_core_pr_manual",
            "mcp_core_plan_ephemeral",
            "mcp_core_outcome_materializer",
            "mcp_pro_plan_durable",
            "mcp_pro_reconciler_journal_notify",
            "mcp_pro_signal_watcher_scope_drift",
            "mcp_pro_mutation_executor",
            "mcp_pro_graph_tools",
            "mcp_pro_review_advanced",
            "mcp_pro_custom_tools",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-mcp (14: 7 Free + 7 Pro) ---
    pub const MCP_CORE_SERVER_DISPATCH: Self = Self("mcp_core_server_dispatch");
    pub const MCP_CORE_DELEGATE_BASIC: Self = Self("mcp_core_delegate_basic");
    pub const MCP_CORE_OUTCOME_FETCH: Self = Self("mcp_core_outcome_fetch");
    pub const MCP_CORE_PM_BASIC: Self = Self("mcp_core_pm_basic");
    pub const MCP_CORE_PR_MANUAL: Self = Self("mcp_core_pr_manual");
    pub const MCP_CORE_PLAN_EPHEMERAL: Self = Self("mcp_core_plan_ephemeral");
    pub const MCP_CORE_OUTCOME_MATERIALIZER: Self = Self("mcp_core_outcome_materializer");
    pub const MCP_PRO_PLAN_DURABLE: Self = Self("mcp_pro_plan_durable");
    pub const MCP_PRO_RECONCILER_JOURNAL_NOTIFY: Self = Self("mcp_pro_reconciler_journal_notify");
    pub const MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT: Self = Self("mcp_pro_signal_watcher_scope_drift");
    pub const MCP_PRO_MUTATION_EXECUTOR: Self = Self("mcp_pro_mutation_executor");
    pub const MCP_PRO_GRAPH_TOOLS: Self = Self("mcp_pro_graph_tools");
    pub const MCP_PRO_REVIEW_ADVANCED: Self = Self("mcp_pro_review_advanced");
    pub const MCP_PRO_CUSTOM_TOOLS: Self = Self("mcp_pro_custom_tools");
```

Parser arms:

```rust
        // spur-mcp
        } else if bytes_eq(b, b"mcp_core_server_dispatch") {
            Some(Self::MCP_CORE_SERVER_DISPATCH)
        } else if bytes_eq(b, b"mcp_core_delegate_basic") {
            Some(Self::MCP_CORE_DELEGATE_BASIC)
        } else if bytes_eq(b, b"mcp_core_outcome_fetch") {
            Some(Self::MCP_CORE_OUTCOME_FETCH)
        } else if bytes_eq(b, b"mcp_core_pm_basic") {
            Some(Self::MCP_CORE_PM_BASIC)
        } else if bytes_eq(b, b"mcp_core_pr_manual") {
            Some(Self::MCP_CORE_PR_MANUAL)
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
        } else if bytes_eq(b, b"mcp_pro_review_advanced") {
            Some(Self::MCP_PRO_REVIEW_ADVANCED)
        } else if bytes_eq(b, b"mcp_pro_custom_tools") {
            Some(Self::MCP_PRO_CUSTOM_TOOLS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-mcp keys (14) for tier revamp Plan A`

---

## Task 13: Add spur-tui keys (13)

Per spec §4.4: 10 Free + 3 Pro.

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
            "tui_core_notification_in_tui_drain",
            "tui_pro_telegram_bot_solo",
            "tui_pro_trace_source_react",
            "tui_pro_custom_keybindings",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-tui (13: 10 Free + 3 Pro) ---
    pub const TUI_CORE_VIEW_DASHBOARD: Self = Self("tui_core_view_dashboard");
    pub const TUI_CORE_VIEW_SESSION_DETAIL: Self = Self("tui_core_view_session_detail");
    pub const TUI_CORE_VIEW_PLAN_INSPECTOR: Self = Self("tui_core_view_plan_inspector");
    pub const TUI_CORE_VIEW_PALETTE_OVERLAY: Self = Self("tui_core_view_palette_overlay");
    pub const TUI_CORE_VIEW_ISSUE_BROWSER: Self = Self("tui_core_view_issue_browser");
    pub const TUI_CORE_VIEW_LANDING_DECISION: Self = Self("tui_core_view_landing_decision");
    pub const TUI_CORE_VIEW_COMPOSER: Self = Self("tui_core_view_composer");
    pub const TUI_CORE_MODAL_COLLISION_ESCAPE: Self = Self("tui_core_modal_collision_escape");
    pub const TUI_CORE_INPUT_PASTE_AS_ATOM: Self = Self("tui_core_input_paste_as_atom");
    pub const TUI_CORE_NOTIFICATION_IN_TUI_DRAIN: Self = Self("tui_core_notification_in_tui_drain");
    pub const TUI_PRO_TELEGRAM_BOT_SOLO: Self = Self("tui_pro_telegram_bot_solo");
    pub const TUI_PRO_TRACE_SOURCE_REACT: Self = Self("tui_pro_trace_source_react");
    pub const TUI_PRO_CUSTOM_KEYBINDINGS: Self = Self("tui_pro_custom_keybindings");
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
        } else if bytes_eq(b, b"tui_core_notification_in_tui_drain") {
            Some(Self::TUI_CORE_NOTIFICATION_IN_TUI_DRAIN)
        } else if bytes_eq(b, b"tui_pro_telegram_bot_solo") {
            Some(Self::TUI_PRO_TELEGRAM_BOT_SOLO)
        } else if bytes_eq(b, b"tui_pro_trace_source_react") {
            Some(Self::TUI_PRO_TRACE_SOURCE_REACT)
        } else if bytes_eq(b, b"tui_pro_custom_keybindings") {
            Some(Self::TUI_PRO_CUSTOM_KEYBINDINGS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-tui keys (13) for tier revamp Plan A`

---

## Task 14: Add spur-cli keys (12 Free + 1 Team)

Per spec §4.5 plus §6.2 trial CLI commands: init, agents, sessions, run, exec, tui, cost, connect, version, upgrade_trial, upgrade_pro, license_activate, workflow (Team).

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_cli_keys_registered() {
        for s in &[
            "cli_core_command_init",
            "cli_core_command_agents",
            "cli_core_command_sessions",
            "cli_core_command_run",
            "cli_core_command_exec",
            "cli_core_command_tui",
            "cli_core_command_cost",
            "cli_core_command_connect",
            "cli_core_command_version",
            "cli_core_command_upgrade_trial",
            "cli_core_command_upgrade_pro",
            "cli_core_command_license_activate",
            "cli_team_command_workflow",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-cli (13: 12 Free + 1 Team) ---
    pub const CLI_CORE_COMMAND_INIT: Self = Self("cli_core_command_init");
    pub const CLI_CORE_COMMAND_AGENTS: Self = Self("cli_core_command_agents");
    pub const CLI_CORE_COMMAND_SESSIONS: Self = Self("cli_core_command_sessions");
    pub const CLI_CORE_COMMAND_RUN: Self = Self("cli_core_command_run");
    pub const CLI_CORE_COMMAND_EXEC: Self = Self("cli_core_command_exec");
    pub const CLI_CORE_COMMAND_TUI: Self = Self("cli_core_command_tui");
    pub const CLI_CORE_COMMAND_COST: Self = Self("cli_core_command_cost");
    pub const CLI_CORE_COMMAND_CONNECT: Self = Self("cli_core_command_connect");
    pub const CLI_CORE_COMMAND_VERSION: Self = Self("cli_core_command_version");
    pub const CLI_CORE_COMMAND_UPGRADE_TRIAL: Self = Self("cli_core_command_upgrade_trial");
    pub const CLI_CORE_COMMAND_UPGRADE_PRO: Self = Self("cli_core_command_upgrade_pro");
    pub const CLI_CORE_COMMAND_LICENSE_ACTIVATE: Self = Self("cli_core_command_license_activate");
    pub const CLI_TEAM_COMMAND_WORKFLOW: Self = Self("cli_team_command_workflow");
```

Parser arms:

```rust
        // spur-cli
        } else if bytes_eq(b, b"cli_core_command_init") {
            Some(Self::CLI_CORE_COMMAND_INIT)
        } else if bytes_eq(b, b"cli_core_command_agents") {
            Some(Self::CLI_CORE_COMMAND_AGENTS)
        } else if bytes_eq(b, b"cli_core_command_sessions") {
            Some(Self::CLI_CORE_COMMAND_SESSIONS)
        } else if bytes_eq(b, b"cli_core_command_run") {
            Some(Self::CLI_CORE_COMMAND_RUN)
        } else if bytes_eq(b, b"cli_core_command_exec") {
            Some(Self::CLI_CORE_COMMAND_EXEC)
        } else if bytes_eq(b, b"cli_core_command_tui") {
            Some(Self::CLI_CORE_COMMAND_TUI)
        } else if bytes_eq(b, b"cli_core_command_cost") {
            Some(Self::CLI_CORE_COMMAND_COST)
        } else if bytes_eq(b, b"cli_core_command_connect") {
            Some(Self::CLI_CORE_COMMAND_CONNECT)
        } else if bytes_eq(b, b"cli_core_command_version") {
            Some(Self::CLI_CORE_COMMAND_VERSION)
        } else if bytes_eq(b, b"cli_core_command_upgrade_trial") {
            Some(Self::CLI_CORE_COMMAND_UPGRADE_TRIAL)
        } else if bytes_eq(b, b"cli_core_command_upgrade_pro") {
            Some(Self::CLI_CORE_COMMAND_UPGRADE_PRO)
        } else if bytes_eq(b, b"cli_core_command_license_activate") {
            Some(Self::CLI_CORE_COMMAND_LICENSE_ACTIVATE)
        } else if bytes_eq(b, b"cli_team_command_workflow") {
            Some(Self::CLI_TEAM_COMMAND_WORKFLOW)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-cli keys (13) for tier revamp Plan A`

---

## Task 15: Add spur-pm keys (10 + 1 Team)

Per spec §4.6: 4 Free + 6 Pro + 1 Team.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_pm_keys_registered() {
        for s in &[
            "pm_core_beads_basic",
            "pm_core_pm_read",
            "pm_core_pr_manual",
            "pm_core_bv_adapter",
            "pm_pro_beads_advanced",
            "pm_pro_github_auto",
            "pm_pro_linear_sync",
            "pm_pro_plane_sync",
            "pm_pro_signal_watcher",
            "pm_pro_auto_merge",
            "pm_team_webhooks",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-pm (11: 4 Free + 6 Pro + 1 Team) ---
    pub const PM_CORE_BEADS_BASIC: Self = Self("pm_core_beads_basic");
    pub const PM_CORE_PM_READ: Self = Self("pm_core_pm_read");
    pub const PM_CORE_PR_MANUAL: Self = Self("pm_core_pr_manual");
    pub const PM_CORE_BV_ADAPTER: Self = Self("pm_core_bv_adapter");
    pub const PM_PRO_BEADS_ADVANCED: Self = Self("pm_pro_beads_advanced");
    pub const PM_PRO_GITHUB_AUTO: Self = Self("pm_pro_github_auto");
    pub const PM_PRO_LINEAR_SYNC: Self = Self("pm_pro_linear_sync");
    pub const PM_PRO_PLANE_SYNC: Self = Self("pm_pro_plane_sync");
    pub const PM_PRO_SIGNAL_WATCHER: Self = Self("pm_pro_signal_watcher");
    pub const PM_PRO_AUTO_MERGE: Self = Self("pm_pro_auto_merge");
    pub const PM_TEAM_WEBHOOKS: Self = Self("pm_team_webhooks");
```

Parser arms:

```rust
        // spur-pm
        } else if bytes_eq(b, b"pm_core_beads_basic") {
            Some(Self::PM_CORE_BEADS_BASIC)
        } else if bytes_eq(b, b"pm_core_pm_read") {
            Some(Self::PM_CORE_PM_READ)
        } else if bytes_eq(b, b"pm_core_pr_manual") {
            Some(Self::PM_CORE_PR_MANUAL)
        } else if bytes_eq(b, b"pm_core_bv_adapter") {
            Some(Self::PM_CORE_BV_ADAPTER)
        } else if bytes_eq(b, b"pm_pro_beads_advanced") {
            Some(Self::PM_PRO_BEADS_ADVANCED)
        } else if bytes_eq(b, b"pm_pro_github_auto") {
            Some(Self::PM_PRO_GITHUB_AUTO)
        } else if bytes_eq(b, b"pm_pro_linear_sync") {
            Some(Self::PM_PRO_LINEAR_SYNC)
        } else if bytes_eq(b, b"pm_pro_plane_sync") {
            Some(Self::PM_PRO_PLANE_SYNC)
        } else if bytes_eq(b, b"pm_pro_signal_watcher") {
            Some(Self::PM_PRO_SIGNAL_WATCHER)
        } else if bytes_eq(b, b"pm_pro_auto_merge") {
            Some(Self::PM_PRO_AUTO_MERGE)
        } else if bytes_eq(b, b"pm_team_webhooks") {
            Some(Self::PM_TEAM_WEBHOOKS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-pm keys (11) for tier revamp Plan A`

---

## Task 16: Add spur-cost keys (6)

Per spec §4.7: 3 Free + 3 Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_cost_keys_registered() {
        for s in &[
            "cost_core_basic_display",
            "cost_core_pricing_registry",
            "cost_core_ingestion_pipeline",
            "cost_pro_per_project_tracking",
            "cost_pro_sqlite_wal_mode",
            "cost_pro_budget_caps",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-cost (6: 3 Free + 3 Pro) ---
    pub const COST_CORE_BASIC_DISPLAY: Self = Self("cost_core_basic_display");
    pub const COST_CORE_PRICING_REGISTRY: Self = Self("cost_core_pricing_registry");
    pub const COST_CORE_INGESTION_PIPELINE: Self = Self("cost_core_ingestion_pipeline");
    pub const COST_PRO_PER_PROJECT_TRACKING: Self = Self("cost_pro_per_project_tracking");
    pub const COST_PRO_SQLITE_WAL_MODE: Self = Self("cost_pro_sqlite_wal_mode");
    pub const COST_PRO_BUDGET_CAPS: Self = Self("cost_pro_budget_caps");
```

Parser arms:

```rust
        // spur-cost
        } else if bytes_eq(b, b"cost_core_basic_display") {
            Some(Self::COST_CORE_BASIC_DISPLAY)
        } else if bytes_eq(b, b"cost_core_pricing_registry") {
            Some(Self::COST_CORE_PRICING_REGISTRY)
        } else if bytes_eq(b, b"cost_core_ingestion_pipeline") {
            Some(Self::COST_CORE_INGESTION_PIPELINE)
        } else if bytes_eq(b, b"cost_pro_per_project_tracking") {
            Some(Self::COST_PRO_PER_PROJECT_TRACKING)
        } else if bytes_eq(b, b"cost_pro_sqlite_wal_mode") {
            Some(Self::COST_PRO_SQLITE_WAL_MODE)
        } else if bytes_eq(b, b"cost_pro_budget_caps") {
            Some(Self::COST_PRO_BUDGET_CAPS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-cost keys (6) for tier revamp Plan A`

---

## Task 17: Add spur-context keys (5)

Per spec §4.8: all Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_context_keys_registered() {
        for s in &[
            "ctx_pro_duckdb_engine",
            "ctx_pro_async_engine",
            "ctx_pro_live_mode",
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
    // --- spur-context (5: all Pro) ---
    pub const CTX_PRO_DUCKDB_ENGINE: Self = Self("ctx_pro_duckdb_engine");
    pub const CTX_PRO_ASYNC_ENGINE: Self = Self("ctx_pro_async_engine");
    pub const CTX_PRO_LIVE_MODE: Self = Self("ctx_pro_live_mode");
    pub const CTX_PRO_DAILY_REPORT: Self = Self("ctx_pro_daily_report");
    pub const CTX_PRO_WEEKLY_REPORT: Self = Self("ctx_pro_weekly_report");
```

Parser arms:

```rust
        // spur-context
        } else if bytes_eq(b, b"ctx_pro_duckdb_engine") {
            Some(Self::CTX_PRO_DUCKDB_ENGINE)
        } else if bytes_eq(b, b"ctx_pro_async_engine") {
            Some(Self::CTX_PRO_ASYNC_ENGINE)
        } else if bytes_eq(b, b"ctx_pro_live_mode") {
            Some(Self::CTX_PRO_LIVE_MODE)
        } else if bytes_eq(b, b"ctx_pro_daily_report") {
            Some(Self::CTX_PRO_DAILY_REPORT)
        } else if bytes_eq(b, b"ctx_pro_weekly_report") {
            Some(Self::CTX_PRO_WEEKLY_REPORT)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-context keys (5) for tier revamp Plan A`

---

## Task 18: Add spur-worktree keys (5)

Per spec §4.9: 2 Free + 3 Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_worktree_keys_registered() {
        for s in &[
            "worktree_core_isolation",
            "worktree_core_artifact_resolver",
            "worktree_pro_git_blob_store",
            "worktree_pro_custom_policies",
            "worktree_pro_cleanup_orphans",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-worktree (5: 2 Free + 3 Pro) ---
    pub const WORKTREE_CORE_ISOLATION: Self = Self("worktree_core_isolation");
    pub const WORKTREE_CORE_ARTIFACT_RESOLVER: Self = Self("worktree_core_artifact_resolver");
    pub const WORKTREE_PRO_GIT_BLOB_STORE: Self = Self("worktree_pro_git_blob_store");
    pub const WORKTREE_PRO_CUSTOM_POLICIES: Self = Self("worktree_pro_custom_policies");
    pub const WORKTREE_PRO_CLEANUP_ORPHANS: Self = Self("worktree_pro_cleanup_orphans");
```

Parser arms:

```rust
        // spur-worktree
        } else if bytes_eq(b, b"worktree_core_isolation") {
            Some(Self::WORKTREE_CORE_ISOLATION)
        } else if bytes_eq(b, b"worktree_core_artifact_resolver") {
            Some(Self::WORKTREE_CORE_ARTIFACT_RESOLVER)
        } else if bytes_eq(b, b"worktree_pro_git_blob_store") {
            Some(Self::WORKTREE_PRO_GIT_BLOB_STORE)
        } else if bytes_eq(b, b"worktree_pro_custom_policies") {
            Some(Self::WORKTREE_PRO_CUSTOM_POLICIES)
        } else if bytes_eq(b, b"worktree_pro_cleanup_orphans") {
            Some(Self::WORKTREE_PRO_CLEANUP_ORPHANS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-worktree keys (5) for tier revamp Plan A`

---

## Task 19: Add spur-bot keys (6)

Per spec §4.10: 5 Pro + 1 Team.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_bot_keys_registered() {
        for s in &[
            "bot_pro_runtime",
            "bot_pro_thread_registry",
            "bot_pro_runtime_render",
            "bot_pro_callback_validation",
            "bot_pro_inline_review",
            "bot_team_multi_chat",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-bot (6: 5 Pro + 1 Team) ---
    pub const BOT_PRO_RUNTIME: Self = Self("bot_pro_runtime");
    pub const BOT_PRO_THREAD_REGISTRY: Self = Self("bot_pro_thread_registry");
    pub const BOT_PRO_RUNTIME_RENDER: Self = Self("bot_pro_runtime_render");
    pub const BOT_PRO_CALLBACK_VALIDATION: Self = Self("bot_pro_callback_validation");
    pub const BOT_PRO_INLINE_REVIEW: Self = Self("bot_pro_inline_review");
    pub const BOT_TEAM_MULTI_CHAT: Self = Self("bot_team_multi_chat");
```

Parser arms:

```rust
        // spur-bot
        } else if bytes_eq(b, b"bot_pro_runtime") {
            Some(Self::BOT_PRO_RUNTIME)
        } else if bytes_eq(b, b"bot_pro_thread_registry") {
            Some(Self::BOT_PRO_THREAD_REGISTRY)
        } else if bytes_eq(b, b"bot_pro_runtime_render") {
            Some(Self::BOT_PRO_RUNTIME_RENDER)
        } else if bytes_eq(b, b"bot_pro_callback_validation") {
            Some(Self::BOT_PRO_CALLBACK_VALIDATION)
        } else if bytes_eq(b, b"bot_pro_inline_review") {
            Some(Self::BOT_PRO_INLINE_REVIEW)
        } else if bytes_eq(b, b"bot_team_multi_chat") {
            Some(Self::BOT_TEAM_MULTI_CHAT)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-bot keys (6) for tier revamp Plan A`

---

## Task 20: Add spur-license meta keys (6)

Per spec §4.11: 4 Free + 2 Pro. Self-referential — these gate license-system features.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_license_keys_registered() {
        for s in &[
            "license_core_facade_entitlement",
            "license_core_policy_resolver",
            "license_core_ed25519_verify",
            "license_core_provider_heartbeat",
            "license_pro_offline_grace",
            "license_pro_quota_runtime_downgrade",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-license meta (6: 4 Free + 2 Pro) ---
    pub const LICENSE_CORE_FACADE_ENTITLEMENT: Self = Self("license_core_facade_entitlement");
    pub const LICENSE_CORE_POLICY_RESOLVER: Self = Self("license_core_policy_resolver");
    pub const LICENSE_CORE_ED25519_VERIFY: Self = Self("license_core_ed25519_verify");
    pub const LICENSE_CORE_PROVIDER_HEARTBEAT: Self = Self("license_core_provider_heartbeat");
    pub const LICENSE_PRO_OFFLINE_GRACE: Self = Self("license_pro_offline_grace");
    pub const LICENSE_PRO_QUOTA_RUNTIME_DOWNGRADE: Self = Self("license_pro_quota_runtime_downgrade");
```

Parser arms:

```rust
        // spur-license meta
        } else if bytes_eq(b, b"license_core_facade_entitlement") {
            Some(Self::LICENSE_CORE_FACADE_ENTITLEMENT)
        } else if bytes_eq(b, b"license_core_policy_resolver") {
            Some(Self::LICENSE_CORE_POLICY_RESOLVER)
        } else if bytes_eq(b, b"license_core_ed25519_verify") {
            Some(Self::LICENSE_CORE_ED25519_VERIFY)
        } else if bytes_eq(b, b"license_core_provider_heartbeat") {
            Some(Self::LICENSE_CORE_PROVIDER_HEARTBEAT)
        } else if bytes_eq(b, b"license_pro_offline_grace") {
            Some(Self::LICENSE_PRO_OFFLINE_GRACE)
        } else if bytes_eq(b, b"license_pro_quota_runtime_downgrade") {
            Some(Self::LICENSE_PRO_QUOTA_RUNTIME_DOWNGRADE)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-license meta keys (6) for tier revamp Plan A`

---

## Task 21: Add spur-blob-store keys (4)

Per spec §4.12: 2 Free + 2 Pro.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_blob_store_keys_registered() {
        for s in &[
            "blob_core_memory_backend",
            "blob_core_fs_backend",
            "blob_pro_measured_backend",
            "blob_pro_delete_namespace",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-blob-store (4: 2 Free + 2 Pro) ---
    pub const BLOB_CORE_MEMORY_BACKEND: Self = Self("blob_core_memory_backend");
    pub const BLOB_CORE_FS_BACKEND: Self = Self("blob_core_fs_backend");
    pub const BLOB_PRO_MEASURED_BACKEND: Self = Self("blob_pro_measured_backend");
    pub const BLOB_PRO_DELETE_NAMESPACE: Self = Self("blob_pro_delete_namespace");
```

Parser arms:

```rust
        // spur-blob-store
        } else if bytes_eq(b, b"blob_core_memory_backend") {
            Some(Self::BLOB_CORE_MEMORY_BACKEND)
        } else if bytes_eq(b, b"blob_core_fs_backend") {
            Some(Self::BLOB_CORE_FS_BACKEND)
        } else if bytes_eq(b, b"blob_pro_measured_backend") {
            Some(Self::BLOB_PRO_MEASURED_BACKEND)
        } else if bytes_eq(b, b"blob_pro_delete_namespace") {
            Some(Self::BLOB_PRO_DELETE_NAMESPACE)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-blob-store keys (4) for tier revamp Plan A`

---

## Task 22: Add spur-interactive keys (3)

Per spec §4.13: all Free.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn spur_interactive_keys_registered() {
        for s in &[
            "interactive_core_frontend_host",
            "interactive_core_review_lane_mpsc",
            "interactive_core_shutdown_orchestrator",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- spur-interactive (3: all Free) ---
    pub const INTERACTIVE_CORE_FRONTEND_HOST: Self = Self("interactive_core_frontend_host");
    pub const INTERACTIVE_CORE_REVIEW_LANE_MPSC: Self = Self("interactive_core_review_lane_mpsc");
    pub const INTERACTIVE_CORE_SHUTDOWN_ORCHESTRATOR: Self = Self("interactive_core_shutdown_orchestrator");
```

Parser arms:

```rust
        // spur-interactive
        } else if bytes_eq(b, b"interactive_core_frontend_host") {
            Some(Self::INTERACTIVE_CORE_FRONTEND_HOST)
        } else if bytes_eq(b, b"interactive_core_review_lane_mpsc") {
            Some(Self::INTERACTIVE_CORE_REVIEW_LANE_MPSC)
        } else if bytes_eq(b, b"interactive_core_shutdown_orchestrator") {
            Some(Self::INTERACTIVE_CORE_SHUTDOWN_ORCHESTRATOR)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add spur-interactive keys (3) for tier revamp Plan A`

---

## Task 23: Add cross-crate notification keys (2)

Per spec §4.14: 1 Free + 1 Pro (v1.1).

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn notification_keys_registered() {
        for s in &[
            "notif_core_in_tui",
            "notif_pro_external_channels",
        ] {
            assert!(FeatureKey::from_known(s).is_some(), "missing {s}");
        }
    }
```

- [ ] **Step 2: Run test, expect FAIL.**
- [ ] **Step 3: Add consts**

```rust
    // --- Notifications (cross-crate, 2: 1 Free + 1 Pro v1.1) ---
    pub const NOTIF_CORE_IN_TUI: Self = Self("notif_core_in_tui");
    pub const NOTIF_PRO_EXTERNAL_CHANNELS: Self = Self("notif_pro_external_channels");
```

Parser arms:

```rust
        // Notifications
        } else if bytes_eq(b, b"notif_core_in_tui") {
            Some(Self::NOTIF_CORE_IN_TUI)
        } else if bytes_eq(b, b"notif_pro_external_channels") {
            Some(Self::NOTIF_PRO_EXTERNAL_CHANNELS)
```

- [ ] **Step 4-5: Run test (PASS), build (PASS).**
- [ ] **Step 6: Commit:** `feat(spur-license): registry add notification keys (2) for tier revamp Plan A`

---

## Task 24: Final integration test — total count + comprehensive roundtrip

This task verifies the full 135-new-key registry and updates the count guard test from Task 2.

**Files:**
- Modify: `crates/spur-license/src/policy/feature_key.rs` (extend count test, add comprehensive roundtrip)

- [ ] **Step 1: Add the comprehensive roundtrip test**

Add this test inside `mod tests` (after `notification_keys_registered`):

```rust
    /// Asserts every new key from the tier revamp roundtrips correctly.
    /// Total: 135 new keys (78 Free + 41 Pro v1 + 10 Pro v1.1 + 3 Team + 3 trial CLI).
    #[test]
    fn tier_revamp_v1_keys_roundtrip() {
        const NEW_KEYS: &[&str] = &[
            // spur-acp (11)
            "acp_core_transport_stdio", "acp_core_transport_socket",
            "acp_core_adapter_claude_code", "acp_core_adapter_codex",
            "acp_core_adapter_gemini", "acp_core_adapter_kiro",
            "acp_core_adapter_cursor", "acp_core_adapter_opencode",
            "acp_core_adapter_kimi",
            "acp_core_session_attach_advisory_lock",
            "acp_core_session_attach_degraded_nolock",
            // spur-core: brain & scheduling (5)
            "core_core_brain_session", "core_core_brain_scheduler",
            "core_core_brain_failover_manual_keystroke",
            "core_pro_brain_failover_auto_pool",
            "core_core_continuation_bridge",
            // spur-core: workers (3)
            "core_core_parallel_workers", "core_core_cancellable_semaphore",
            "core_pro_worker_heartbeat_watchdog",
            // spur-core: event pipeline (5)
            "core_core_event_funnel_broadcast",
            "core_core_event_sink_ndjson_128mb",
            "core_core_executor_lineage_projection",
            "core_core_notification_pump",
            "core_pro_broadcast_lagged_recovery",
            // spur-core: review (5)
            "core_core_review_sink", "core_core_review_policy_manual",
            "core_pro_review_policy_auto_approve",
            "core_pro_review_policy_timeout_fallback",
            "core_pro_review_policy_retry",
            // skills (5)
            "core_core_skill_registry", "core_core_skill_atomic_installation",
            "skills_core_render_per_vendor",
            "skills_pro_custom", "skills_pro_role_gating",
            // spur-core: peer mailbox (3)
            "core_pro_peer_mailbox_router", "core_pro_peer_mailbox_ledger",
            "core_pro_peer_mailbox_stranded_recon",
            // spur-core: system events (5)
            "core_core_conflict_detection", "core_core_rate_limit_detection",
            "core_core_license_event_broadcast",
            "core_core_permission_request_prompt",
            "core_core_ext_notification",
            // spur-core: reliability (5)
            "core_core_basic_session_resume",
            "core_pro_session_resume_event_replay",
            "core_core_basic_plan_persistence",
            "core_pro_plan_orphan_recovery",
            "core_pro_background_task_tracker",
            // spur-mcp (14)
            "mcp_core_server_dispatch", "mcp_core_delegate_basic",
            "mcp_core_outcome_fetch", "mcp_core_pm_basic",
            "mcp_core_pr_manual", "mcp_core_plan_ephemeral",
            "mcp_core_outcome_materializer",
            "mcp_pro_plan_durable", "mcp_pro_reconciler_journal_notify",
            "mcp_pro_signal_watcher_scope_drift", "mcp_pro_mutation_executor",
            "mcp_pro_graph_tools", "mcp_pro_review_advanced",
            "mcp_pro_custom_tools",
            // spur-tui (13)
            "tui_core_view_dashboard", "tui_core_view_session_detail",
            "tui_core_view_plan_inspector", "tui_core_view_palette_overlay",
            "tui_core_view_issue_browser", "tui_core_view_landing_decision",
            "tui_core_view_composer", "tui_core_modal_collision_escape",
            "tui_core_input_paste_as_atom", "tui_core_notification_in_tui_drain",
            "tui_pro_telegram_bot_solo", "tui_pro_trace_source_react",
            "tui_pro_custom_keybindings",
            // spur-cli (13)
            "cli_core_command_init", "cli_core_command_agents",
            "cli_core_command_sessions", "cli_core_command_run",
            "cli_core_command_exec", "cli_core_command_tui",
            "cli_core_command_cost", "cli_core_command_connect",
            "cli_core_command_version", "cli_core_command_upgrade_trial",
            "cli_core_command_upgrade_pro", "cli_core_command_license_activate",
            "cli_team_command_workflow",
            // spur-pm (11)
            "pm_core_beads_basic", "pm_core_pm_read",
            "pm_core_pr_manual", "pm_core_bv_adapter",
            "pm_pro_beads_advanced", "pm_pro_github_auto",
            "pm_pro_linear_sync", "pm_pro_plane_sync",
            "pm_pro_signal_watcher", "pm_pro_auto_merge",
            "pm_team_webhooks",
            // spur-cost (6)
            "cost_core_basic_display", "cost_core_pricing_registry",
            "cost_core_ingestion_pipeline", "cost_pro_per_project_tracking",
            "cost_pro_sqlite_wal_mode", "cost_pro_budget_caps",
            // spur-context (5)
            "ctx_pro_duckdb_engine", "ctx_pro_async_engine",
            "ctx_pro_live_mode", "ctx_pro_daily_report",
            "ctx_pro_weekly_report",
            // spur-worktree (5)
            "worktree_core_isolation", "worktree_core_artifact_resolver",
            "worktree_pro_git_blob_store", "worktree_pro_custom_policies",
            "worktree_pro_cleanup_orphans",
            // spur-bot (6)
            "bot_pro_runtime", "bot_pro_thread_registry",
            "bot_pro_runtime_render", "bot_pro_callback_validation",
            "bot_pro_inline_review", "bot_team_multi_chat",
            // spur-license meta (6)
            "license_core_facade_entitlement", "license_core_policy_resolver",
            "license_core_ed25519_verify", "license_core_provider_heartbeat",
            "license_pro_offline_grace", "license_pro_quota_runtime_downgrade",
            // spur-blob-store (4)
            "blob_core_memory_backend", "blob_core_fs_backend",
            "blob_pro_measured_backend", "blob_pro_delete_namespace",
            // spur-interactive (3)
            "interactive_core_frontend_host", "interactive_core_review_lane_mpsc",
            "interactive_core_shutdown_orchestrator",
            // Notifications (2)
            "notif_core_in_tui", "notif_pro_external_channels",
        ];

        assert_eq!(
            NEW_KEYS.len(),
            135,
            "Expected exactly 135 new tier-revamp keys, got {}",
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

Expected: PASS — all 135 keys roundtrip correctly.

- [ ] **Step 3: Run the full feature_key test suite**

Run: `cargo test --package spur-license --lib policy::feature_key`

Expected: ALL PASS — original 36-key tests + 21 new per-crate tests + comprehensive 135-key test + count guard.

- [ ] **Step 4: Run the full spur-license test suite**

Run: `cargo test --package spur-license`

Expected: ALL PASS — including emission_audit and licenseseat_probe integration tests.

- [ ] **Step 5: Build the full workspace and run clippy**

Run: `cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings`

Expected: Both PASS — no compile errors, no clippy warnings introduced.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-license/src/policy/feature_key.rs
git commit -m "test(spur-license): comprehensive 135-key registry roundtrip for tier revamp Plan A"
```

---

## Task 25: Document the registry-vs-policy mismatch (advance notice for Plan B)

After this plan ships, the `FeatureKey` registry has 36 OLD keys + 135 NEW keys = 171 total typed constants. The embedded `default_policy.json` STILL references only the OLD keys, so:
- Free users still get 11 Community features (per old policy)
- The 135 new keys are typed-known but not in any tier yet
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

- 135 new typed `FeatureKey` constants in `crates/spur-license/src/policy/feature_key.rs`
- 1 new `QuotaKey` variant: `BrainFailoverChainDepth`
- Roundtrip test coverage for every new key (per-crate tests + comprehensive 135-key test)
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
- New 135 keys: typed-known but unreachable through `FeatureGate::has()` because no policy declares them in any tier
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

After Plan B ships, the registry has only the 135 new keys and the policy reflects the new tier structure.
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

- [ ] `cargo test --package spur-license --lib feature_key 2>&1 | grep "test result"` shows ~22 passing tests (count_guard + 21 per-crate tests + comprehensive)
- [ ] `grep -c "pub const" crates/spur-license/src/policy/feature_key.rs` returns at least 171 (36 legacy + 135 new)
- [ ] `grep -c "Some(Self::" crates/spur-license/src/policy/feature_key.rs` returns at least 171 in the `from_known` chain
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
