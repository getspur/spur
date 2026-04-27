# Tier Revamp Plan C — Runtime Enforcement Caller Survey

Date: 2026-04-28

Scope: survey only. Plan A delivered the typed 63-key registry; Plan B
rewrote the signed `default_policy.json` and migrated legacy callers.
Plan C is the *enforcement sweep*: the work of placing
`FeatureGate::has(...)` (or the `require`-style helpers) at every
runtime entry point so the 63 keys actually gate behavior. This survey
documents what is enforced today, what is not, and the shape of the
gap before any design lands.

> **Note on key count:** Earlier docs (Plan A status, Plan B survey)
> stated "64 keys / 48F+15P+1Pv1.1+0T". The actual registry block at
> `crates/spur-license/src/policy/feature_key.rs:28` is annotated
> "Wave-9 final shape: 63 keys". Manual count of the `pub const`
> declarations confirms 63: **47 Free, 15 Pro v1, 1 Pro v1.1
> (`core_pro_session_resume_event_replay`), 0 Team**. The cascade
> correction is filed in `docs/superpowers/plans/2026-04-28-tier-revamp-doc-count-correction.md`
> (Plan F0 follow-up).

This document does not prescribe enforcement. It is the input to a
subsequent spec (`docs/superpowers/specs/2026-MM-DD-tier-revamp-plan-c-design.md`)
and implementation plan.

## Grounding commands

- `rg` over `crates/` for `FeatureGate::(has|require|new)` and
  `gate\.(has|require|allow|active)\(` and `FeatureKey::([A-Z_]+)`,
  excluding `target/`.
- Manual reads of `crates/spur-license/src/policy/feature_key.rs:28-135`
  (the 63-key block; line-28 comment confirms the count) and
  `crates/spur-license/src/gate.rs:1-100`
  (the `FeatureGate` API surface).
- Manual read of `docs/superpowers/specs/2026-04-26-individual-tier-revamp-design.md`
  §4.1–4.13 (per-crate registry) and §6 (gate enforcement model) and
  §9.5 (Wave 9 risk acknowledgement: "Registry tier shifts ≠ runtime
  enforcement").

## Summary finding

The 63 Wave-9 keys exist as typed constants and resolve through the
signed policy after Plan B, but **only 3 keys have any production
enforcement callsite today**:

| Key | Production callsites | Class |
|---|---|---|
| `PM_PRO_BEADS_ADVANCED` | ~25 in `spur-mcp/src/server.rs` + ~13 in `spur-mcp/src/plan/*` | multi-site, single shape |
| `PM_CORE_BEADS_BASIC` | 1 in `spur-core/src/orchestrator.rs:1036` (startup beads warning) | single-site |
| `PM_CORE_BROWSE` | 1 in `spur-cli/src/lib.rs:7` (CLI PM-browse helper) | single-site |

That is 3 of 63 keys (4.8%). The remaining **60 keys are typed-known
but not consulted at any runtime decision point** — they exist for
policy/marketing/test purposes only.

The `FeatureGate` API surface itself is intentionally minimal:
`has(FeatureKey) -> bool`, `quota(QuotaKey) -> Option<QuotaValue>`,
`tier() -> Tier`, `is_flag_enabled(FlagKey) -> Option<bool>`. There is
no `require(FeatureKey) -> Result<(), GateError>` helper today; the
`require_feature` and `require_feature_response` patterns inside
`spur-mcp/src/server.rs` are local helpers, not library-level API.

## 1. Per-Key Enforcement Inventory

Status legend:
- **enforced** — at least one production (non-test) callsite consults the key today
- **partial** — touched by infrastructure but not by an explicit `gate.has` check (e.g., quota lookups for `parallel_workers` exist but no feature gate guards the fan-out path)
- **none** — typed-known, zero production callsites
- **N/A — always-on infra** — Plan A wave-6 dropped or absorbed, but key was kept for forward-compat marketing

The "natural callsite" column names the file/function where a single
gate would logically sit, grounded in current code; it is *not* a
prescription, just a survey hint.

### 1.1 spur-acp (6 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `ACP_CORE_TRANSPORT_STDIO` | F | none | `crates/spur-acp/src/connection/native.rs` — `NativeAcpConnection::new` (transport mode is selected from spawn args, not a separate constructor) |
| `ACP_CORE_TRANSPORT_SOCKET` | F | none | same constructor as `_STDIO`; gate fires once the resolved transport is known |
| `ACP_CORE_ADAPTER_CLAUDE_CODE` | F | none | `crates/spur-acp/src/agents/defaults.rs` (claude-code seed) |
| `ACP_CORE_ADAPTER_CODEX` | F | none | `crates/spur-acp/src/agents/defaults.rs` (codex seed) |
| `ACP_CORE_ADAPTER_KIRO` | F | none | `crates/spur-acp/src/agents/defaults.rs` (kiro seed) |
| `ACP_CORE_SESSION_ATTACH_ADVISORY_LOCK` | F | none | `crates/spur-acp/src/session_lock.rs` (`SessionAttachGuard::acquire`) |

All `acp_core_*` keys are Free baseline. Per spec §6.4, Free-tier
gates are "always-on" in v1 — a Community-tier `FeatureGate` returns
`true` for every Free key — so enforcement here is a *defense-in-depth*
guard against a tampered/expired policy that strips a Free key. Low
priority for v1; moderate priority for v2 when Team-tier policy could
plausibly disable transport variants (e.g., enterprise stdio-only).

### 1.2 spur-core (13 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `CORE_CORE_BRAIN_SESSION` | F | none (test only) | `crates/spur-core/src/orchestrator.rs` brain ACP session boot |
| `CORE_CORE_BRAIN_FAILOVER_MANUAL_KEYSTROKE` | F | none | brain rate-limit detector (`SwitchAgent` banner emit point) |
| `CORE_CORE_PARALLEL_WORKERS` | F | partial | semaphore is constructed unconditionally; quota `max_concurrent_workers` is consulted but no feature `has` check |
| `CORE_PRO_WORKER_HEARTBEAT_WATCHDOG` | P | none | watchdog spawn site in worker dispatch (today the watchdog is default-off — gate gates the *opt-in* surface) |
| `CORE_CORE_EVENT_PIPELINE` | F | partial | event pipeline boots unconditionally; this Free key is the umbrella used to kill the entire pipeline if policy revoked |
| `CORE_PRO_PEER_MAILBOX_ROUTER` | P | none | gate fires at the router-spawn site referenced by the `peer_mailbox_enabled: bool` config field at `crates/spur-acp/src/config/mod.rs:372-375` (the field gates construction; the router itself is plumbed elsewhere and is currently default-off) |
| `CORE_CORE_REVIEW` | F | none | review sink usage in `crates/spur-core/src/orchestrator.rs` (the `ReviewSink` type is owned by `spur-core`, not `spur-mcp`; gate fires where the sink is consulted from the orchestrator review-routing branches) |
| `CORE_CORE_REVIEW_RETRY_CONFIG` | F | none | retry-budget read in review dispatch (today `max_review_retries` is unconditionally honored) |
| `CORE_PRO_REVIEW_AUTO_APPROVE` | P | none | timeout-fallback / permission-fast-path branch in review router |
| `CORE_CORE_PERMISSION_REQUEST_DETECTION` | F | none | permission notif synthesis in `crates/spur-acp/src/connection/native.rs` |
| `CORE_CORE_SESSION_RESUME` | F | none | session-resume handler in orchestrator (`resume_session`) |
| `CORE_PRO_SESSION_RESUME_EVENT_REPLAY` | P | none (v1.1-Q3) | NDJSON replay path in resume handler — defers to v1.1 per spec |
| `CORE_CORE_PLAN_PERSISTENCE` | F | none | plan persistence persist site (`crates/spur-mcp/src/plan/mod.rs:770` and friends, currently gated only on `pm_pro_beads_advanced`) |

Headline observation: the `core_pro_*` keys are the **Pro conversion
moat for AFK confidence** (worker watchdog, peer mailbox, auto-approve,
event-replay resume) and have **zero enforcement** today.

### 1.3 skills (2 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `SKILLS_CORE_REGISTRY` | F | none | skills loader entry (today no separate crate — bundled at startup, see `crates/spur-cli/src/commands/init.rs:408` `run_skills_init`) |
| `SKILLS_PRO_CUSTOM` | P | none | org-internal custom-skill load path (greenfield per spec §9.5; deferred amplifier) |

`SKILLS_PRO_CUSTOM` is Plan E territory (skills marketplace).
`SKILLS_CORE_REGISTRY` should land in Plan C as a Free-baseline guard.

### 1.4 spur-mcp (10 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `MCP_CORE_SERVER_DISPATCH` | F | none | `crates/spur-mcp/src/server.rs:129` (already has the helper `if feature_gate.has(key)` shape — but currently consulted only with non-canonical keys) |
| `MCP_CORE_DELEGATE` | F | none | `delegate_to_worker` / `delegate_parallel` handlers in `server.rs` |
| `MCP_CORE_OUTCOME_FETCH` | F | none | `fetch_outcome_artifact` handler |
| `MCP_CORE_PM` | F | none | shared with `pm_core_browse` — overlap to resolve |
| `MCP_CORE_PR` | F | none | `create_pr` handler |
| `MCP_CORE_PLAN_EPHEMERAL` | F | none | ephemeral-plan submit/cancel paths |
| `MCP_CORE_GRAPH_TOOLS` | F | none | `graph_*` MCP tool handlers (Wave 9 tier-shifted Pro→Free) |
| `MCP_PRO_PLAN_DURABLE` | P | none (overlap) | plan persist site in `crates/spur-mcp/src/plan/mod.rs` — currently gated on `PM_PRO_BEADS_ADVANCED` instead |
| `MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT` | P | none (overlap) | `crates/spur-mcp/src/plan/signal_watcher.rs:77` — currently gated on `PM_PRO_BEADS_ADVANCED` |
| `MCP_PRO_REVIEW` | P | none | review-control MCP tool surface (`review_task`, etc.) |

Critical finding: **`MCP_PRO_PLAN_DURABLE` and
`MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT` are de-facto enforced today, but
through `PM_PRO_BEADS_ADVANCED`** — the unfinished Plan B legacy
mapping. Plan C must split the gate at these sites or the v1.1
"durable plans without beads" story is broken.

### 1.5 spur-tui (7 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `TUI_CORE_VIEW_DASHBOARD` | F | none | `crates/spur-tui/src/views/dashboard.rs` mount path |
| `TUI_CORE_VIEW_SESSION_DETAIL` | F | none | `crates/spur-tui/src/views/session_detail.rs` mount path |
| `TUI_CORE_VIEW_PLAN_INSPECTOR` | F | none | `crates/spur-tui/src/views/plan_inspector.rs` mount path |
| `TUI_CORE_VIEW_PALETTE_OVERLAY` | F | none | command palette open keystroke handler |
| `TUI_CORE_VIEW_ISSUE_BROWSER` | F | none | `crates/spur-tui/src/views/issue_browser.rs` mount path |
| `TUI_CORE_MODAL_COLLISION_ESCAPE` | F | none | `crates/spur-tui/src/components/collision_modal.rs:11-99` open path |
| `TUI_CORE_INPUT_PASTE_AS_ATOM` | F | none | clipboard-paste branch in input event router |

All 7 are Free baseline. Same defense-in-depth rationale as §1.1
applies. `TUI_CORE_MODAL_COLLISION_ESCAPE` is the *primitive* that
Plan D's capability-tease modals reuse — gate value here is "is the
modal subsystem available at all".

### 1.6 spur-cli (9 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `CLI_CORE_INIT` | F | none | `crates/spur-cli/src/commands/init.rs:55` `run` |
| `CLI_CORE_AGENTS` | F | none | agents subcommand dispatch |
| `CLI_CORE_SESSIONS` | F | none | sessions subcommand dispatch |
| `CLI_CORE_RUN` | F | none | `run` subcommand |
| `CLI_CORE_EXEC` | F | none | `exec` subcommand |
| `CLI_CORE_TUI` | F | none | `tui` subcommand entry |
| `CLI_CORE_COST` | F | none | `cost` subcommand |
| `CLI_CORE_CONNECT` | F | none | `connect` subcommand |
| `CLI_CORE_LICENSE_ACTIVATE` | F | none | `crates/spur-cli/src/commands/auth.rs` activate path |

Single-site each. Trivial to enforce — `CLI_CORE_*` keys map 1:1 to
top-level subcommand handlers in `crates/spur-cli/src/main.rs`.

### 1.7 spur-pm (5 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `PM_CORE_BEADS_BASIC` | F | **enforced** | `crates/spur-core/src/orchestrator.rs:1036` (startup warning) |
| `PM_CORE_BROWSE` | F | **enforced** | `crates/spur-cli/src/lib.rs:7` |
| `PM_CORE_PR` | F | none | `create_pr` MCP handler — overlaps `MCP_CORE_PR` |
| `PM_CORE_BEADS_GRAPH_ADAPTER` | F | none | beads graph adapter init (PM service construction) |
| `PM_PRO_BEADS_ADVANCED` | P | **enforced (heavily)** | ~38 callsites across `spur-mcp/src/server.rs` + `spur-mcp/src/plan/*` |

The enforced PM keys are the only proof that Plan C's enforcement
shape works end-to-end. `PM_PRO_BEADS_ADVANCED` is the *template* —
Plan C keys should follow the same `require_feature(...)?` shape.

### 1.8 spur-cost (3 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `COST_CORE_SESSION_DISPLAY` | F | none | per-session cost render in `crates/spur-tui/src/views/session_detail.rs` cost panel |
| `COST_CORE_PRICING_REGISTRY` | F | none | pricing-registry init in cost service construction |
| `COST_PRO_PER_PROJECT_TRACKING` | P | none | per-project rollup query path (today the rollup builds unconditionally; gate would block the *report* render) |

`COST_PRO_PER_PROJECT_TRACKING` is one of the 5 Pro headline
conversion triggers (spec §9.5) — high enforcement priority.

### 1.9 spur-context (1 key)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `CTX_PRO_DUCKDB_ENGINE` | P | none | `crates/spur-context/src/sql/` engine boot — today behind a build-time feature flag, not a runtime gate |

Build-time feature flag and runtime feature gate must both be on.
Plan C wires the runtime gate; the build flag stays.

### 1.10 spur-worktree (2 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `WORKTREE_CORE_ISOLATION` | F | none | worktree create path in spur-worktree crate |
| `WORKTREE_CORE_ORPHAN_CLEANUP` | F | none | orphan-cleanup task spawn site |

Both Free baseline; same "always-on infra defense" classification.

### 1.11 spur-bot (2 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `BOT_PRO_TELEGRAM_SOLO` | P | none | `crates/spur-bot/src/runtime.rs` telegram poller spawn |
| `BOT_PRO_INLINE_REVIEW` | P | none | inline-review handler in telegram bot dispatch |

Both Pro headline conversion triggers (Remote Control category) —
high enforcement priority, lowest cross-crate complexity (single
crate, two well-known entry points).

### 1.12 spur-license meta (2 keys)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `LICENSE_PRO_REVOCATION_POLLING` | P | none | revocation polling task spawn in `crates/spur-license/src/lib.rs` |
| `LICENSE_PRO_OFFLINE_GRACE` | P | none | offline-grace timer in license refresh path |

Self-referential — these gates govern the license subsystem itself.
Bootstrap concern: the gate must already be loaded before these
checks run. Practical resolution is to enforce *config* (whether
revocation polling is *attempted*) but always allow Pro users
through; guarantees no infinite regress.

### 1.13 spur-blob-store (1 key)

| Key | Tier | Status | Natural callsite |
|---|---|---|---|
| `BLOB_PRO_NAMESPACE_DELETION` | P | none | namespace-deletion handler in blob store |

Single-site. Pro gate.

## 2. Production Enforcement Today

```
crates/spur-mcp/src/server.rs:125-129       require_feature(FeatureKey, &FeatureGate) helper
                                            definition: `if feature_gate.has(key)`. Key is
                                            typed `FeatureKey`; no dynamic-string callsite
                                            exists in production today.
crates/spur-mcp/src/server.rs:547-4013      21 calls to require_feature(PM_PRO_BEADS_ADVANCED)
                                            (verified by ripgrep; counts include direct,
                                            self.require_feature, and super::require_feature)
crates/spur-mcp/src/plan/mod.rs:770-1179    6 calls to PM_PRO_BEADS_ADVANCED
                                            (line 3071 is a #[cfg(test)] test-fixture
                                            BTreeSet construction; not a live callsite)
crates/spur-mcp/src/plan/reconciler.rs      2 live calls (491, 794); 1 #[cfg(test)] fixture (907)
crates/spur-mcp/src/plan/projector.rs:387   1 call
crates/spur-mcp/src/plan/signal_watcher.rs:77    1 call
crates/spur-mcp/src/plan/mutation_executor.rs:60,370    2 calls
crates/spur-core/src/orchestrator.rs:1036   gate.has(PM_CORE_BEADS_BASIC)
crates/spur-cli/src/lib.rs:7                gate.has(PM_CORE_BROWSE)
```

## 3. Enforcement Shape Categorization

The buckets below carve **strictly by code topology**, not by
business semantics. Free baseline keys whose enforcement is
defense-in-depth are tagged with a `[defense]` metadata flag rather
than promoted to a separate bucket — that earlier shape (§3.4 in
v1 of the survey) double-counted Free `cli_core_*` / `acp_core_*` /
`worktree_core_*` keys against §3.1. The metadata tag preserves the
defense-in-depth observation without splitting the topology buckets.

### 3.1 Single-site (~42 keys)

One natural decision point per key. Pattern: top-level command
handler, view mount path, service constructor.

- All 9 `cli_core_*` keys [defense]
- All 7 `tui_core_*` keys [defense]
- All 6 `acp_core_*` keys [defense]
- 2 `worktree_core_*` keys [defense]
- 2 `bot_pro_*` keys
- 2 `license_pro_*` keys
- 1 `blob_pro_*` key
- `core_core_brain_session` [defense],
  `core_core_brain_failover_manual_keystroke` [defense]
- `core_pro_worker_heartbeat_watchdog`, `core_pro_peer_mailbox_router`
- `cost_pro_per_project_tracking`, `ctx_pro_duckdb_engine`
- `core_core_parallel_workers` [defense; quota is user-felt, gate
  just guards the multi-worker code path]
- `cost_core_session_display` [defense], `cost_core_pricing_registry`
  [defense]

Effort estimate: ~1 line per callsite × ~42 = small.

### 3.2 Multi-site, single shape (~10 keys)

Same gate consulted from multiple file:line locations within one
crate, all with identical shape (mirroring `PM_PRO_BEADS_ADVANCED`).

- `mcp_core_*` family (7 keys, each potentially called from 2-5 MCP
  handler entries in `crates/spur-mcp/src/server.rs`)
- `mcp_pro_plan_durable`, `mcp_pro_signal_watcher_scope_drift`,
  `mcp_pro_review` (each 2-3 sites)

Effort estimate: medium. Risk: easy to miss a callsite and create
inconsistent enforcement.

### 3.3 Multi-site, cross-crate (~11 keys)

Gate consulted across crate boundaries (orchestrator + tui + mcp).

- `core_core_event_pipeline` (orchestrator emit + tui drain + mcp passthrough)
- `core_core_review` (orchestrator + mcp + tui review banners)
- `core_core_review_retry_config`, `core_pro_review_auto_approve`
- `core_core_session_resume`, `core_pro_session_resume_event_replay`
- `core_core_plan_persistence`, `core_core_permission_request_detection`
- `pm_core_pr` / `pm_core_beads_graph_adapter` (overlap with mcp_core_pr / pm)
- `skills_core_registry` [defense], `skills_pro_custom`

Effort estimate: medium-high. Requires per-sub-feature wave
segmentation (see §5).

> **Bucket totals** sum to 63: ~42 + ~10 + ~11. Exact assignment of
> the boundary keys (`pm_core_pr`, `skills_core_registry`) depends on
> overlap resolution in §4 and may shift ±2 between buckets.

## 4. Overlap and ambiguity

Three known overlap zones that Plan C must resolve before the spec
phase:

### 4.1 PM/MCP overlap

`PM_CORE_PR` vs `MCP_CORE_PR` — both refer to PR creation. Today
spur-mcp owns the handler; spur-pm contains data model. Convention to
pick (proposal): if the runtime entry is an MCP tool, gate on
`MCP_CORE_*`; if it is a CLI subcommand, gate on `PM_CORE_*` /
`CLI_CORE_*`. Document in spec.

### 4.2 PM beads overlap

`MCP_PRO_PLAN_DURABLE`, `MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT` are
currently gated on `PM_PRO_BEADS_ADVANCED` (legacy single-key shape).
Splitting requires per-callsite gate replacement. Test surface
(`crates/spur-mcp/tests/advanced_gating_handlers.rs`) hard-codes
`pm_pro_beads_advanced` and must be re-fixtured.

### 4.3 Event pipeline umbrella

`CORE_CORE_EVENT_PIPELINE` was Wave-8-collapsed from
funnel+sink+lineage+pump. The spec says one umbrella, but the
underlying code has at least 4 distinct construction points. Decision
required: gate at the *publisher* (single point) or at every
*subscriber* (defense-in-depth).

## 5. Suggested Plan-C wave segmentation

Plan C is too large for one PR. Recommended wave decomposition,
ordered by *conversion-trigger value × engineering risk*. The Pro
headline waves are split 1:1 with spec §9.5's 5 conversion-trigger
categories — bundling all 10 Pro keys into one wave (the v1 of this
survey) is an integration bottleneck that defeats the purpose of
wave segmentation.

### 5.1 Free-tier defense-in-depth waves (low priority for revenue)

| Wave | Scope | Keys | Risk |
|---|---|---|---|
| C.1 | spur-cli command guards | 9 | trivial; all single-site [defense] |
| C.2 | spur-tui view + modal guards | 7 | low; test surface = render-output assertions [defense] |
| C.3 | spur-acp adapter+transport guards | 6 | low [defense] |
| C.4 | spur-worktree + spur-blob-store + license-meta | 5 | low (license-meta keys: see §6.3 below) |

### 5.2 Free-tier reliability + plumbing waves

| Wave | Scope | Keys | Risk |
|---|---|---|---|
| C.5 | spur-mcp `_core_*` 7-key sweep | 7 | medium; mirrors PM_PRO_BEADS_ADVANCED template |
| C.6 | review subsystem Free baseline (`core_core_review`, `core_core_review_retry_config`) | 2 | medium-high; orchestrator + mcp + tui |
| C.7 | session-resume Free + plan-persistence + permission-detection | 3 | medium |
| C.8 | event-pipeline umbrella decision + enforcement | 1 | medium-high (architectural choice; see §4.3) |
| C.9 | core_core_brain_* + core_core_parallel_workers + skills_core_registry | 4 | medium; pre-existing infra coupling |

### 5.3 Pro headline conversion-trigger waves (revenue-critical, parallelizable)

Each wave maps 1:1 with a spec §9.5 Pro headline category. They are
parallelizable because each headline lives in a different crate
boundary; one wave per category lets review surface stay scoped.

| Wave | §9.5 Category | Keys | Risk |
|---|---|---|---|
| C.10 | ★ Remote Control | `bot_pro_telegram_solo`, `bot_pro_inline_review` (2) | high; end-to-end Telegram smoke needed |
| C.11 | ★ Multi-Agent Coordination | `core_pro_peer_mailbox_router`, `mcp_pro_plan_durable`, `mcp_pro_signal_watcher_scope_drift` (3) | high; cross-crate, plan-durability test surface |
| C.12 | ★ Review Control Plane | `core_pro_review_auto_approve`, `core_pro_worker_heartbeat_watchdog`, `mcp_pro_review` (3) | high; auto-approve + watchdog interact |
| C.13 | ★ Cost Insights | `cost_pro_per_project_tracking`, `ctx_pro_duckdb_engine` (2) | medium; report-render gate only |
| C.14 | ★ Extensibility | `skills_pro_custom` (1; `pm_pro_beads_advanced` is already enforced) | medium; gate placement at custom-skill load path |

### 5.4 Cleanup wave

| Wave | Scope | Keys | Risk |
|---|---|---|---|
| C.15 | Overlap resolution (PM_CORE_PR vs MCP_CORE_PR; beads-advanced → plan_durable + signal_watcher_scope_drift split) | refactor only | medium |

### 5.5 Pro v1.1-Q3 deferred

`core_pro_session_resume_event_replay` is `[v1.1-Q3]` per spec
§4.2. Enforcement lands when the underlying NDJSON-replay path
ships; out of scope for Plan C v1 ship.

### 5.6 Wave totals

- Free defense-in-depth: 27 keys (C.1–C.4)
- Free reliability + plumbing: 17 keys (C.5–C.9)
- Pro headlines: 11 keys (C.10–C.14) — `pm_pro_beads_advanced` is
  already enforced and is the template
- Pro v1.1: 1 key (deferred, not in Plan C ship)
- Cleanup: 0 new keys (C.15)

**Total: 56 enforcement-add keys across 14 waves + 1 cleanup wave +
1 v1.1 deferred + 7 already-enforced = 63 ≡ Wave-9 final.**

The headline Pro waves (C.10–C.14) collectively unlock the iceberg
revenue thesis (spec §9.5). Free-tier waves (C.1–C.4) are mostly
defense-in-depth and could be deferred to v1.1 without affecting
conversion. Within C.10–C.14, individual waves are independent and
parallelizable.

## 6. Risks

### 6.1 Test-fixture explosion

Every gate added requires at least 2 tests: a positive (Pro user
sees Pro feature) and a negative (Free user blocked + error message).
With 60 keys to enforce, naive 2-test-per-key = 120 new tests. Plan
C spec must define a *parameterized* test harness (one fixture, many
keys) to avoid linear test growth.

### 6.2 Help-text consistency

`require_feature` produces an error message. Today
`PM_PRO_BEADS_ADVANCED` errors mention "advanced beads" but not the
upgrade URL. Plan C should standardize the error message format
across all gates (`§Plan D capability-tease pattern` will reuse
this).

### 6.3 license_pro_* gate evaluation order (no bootstrap paradox)

`license_pro_revocation_polling` and `license_pro_offline_grace`
appear self-referential ("they gate the license subsystem"), but
there is no actual bootstrap paradox: `FeatureGate` evaluates
synchronously from the locally-cached `~/.spur/license` JWT (built
during `FeatureGate::new` from policy + state) **before** any
background polling task spawns (`crates/spur-license/src/lib.rs:234-245`,
where `FeatureGate::new(policy)` is constructed before runtime
spawn). The license subsystem's `revocation_polling` task is itself
created downstream of the gate.

The correct enforcement shape is therefore **fail-closed**:
- Free-tier users do not get revocation polling (correct — they
  have no Pro license to revoke).
- Pro-tier users with a valid cached snapshot get the gate `true`
  and start polling.
- Pro-tier users with a tampered snapshot that strips the key get
  the gate `false` and are correctly denied — they get a stale
  license until the next interactive `spur auth refresh`.

The earlier "fail-open" reading would incorrectly cause offline
Free users to attempt server polls. Spec phase should write the
fail-closed contract explicitly.

### 6.4 PM_PRO_BEADS_ADVANCED collateral (low risk — already covered)

Splitting `MCP_PRO_PLAN_DURABLE` and `MCP_PRO_SIGNAL_WATCHER_SCOPE_DRIFT`
out from `PM_PRO_BEADS_ADVANCED` is low risk because Plan B's
`resolve_jwt_feature_key` (`crates/spur-license/src/gate.rs:222-260`)
already handles legacy keys via a one-to-many forward mapping.
Existing legacy-Pro JWT entries that contain only the umbrella
`pm_pro_beads_advanced` key continue to satisfy both new keys via
the resolver. No forced-license-refresh is required.

The cleanup wave C.15 just needs to add the new mappings to
`resolve_jwt_feature_key` (or its replacement).

### 6.5 Quota vs feature ambiguity

`CORE_CORE_PARALLEL_WORKERS` is a feature key, but the *user-felt*
gate is `max_concurrent_workers` (a `QuotaKey`). If the feature is
revoked but the quota is non-zero, today's code respects the quota.
Spec must define precedence: feature-key check happens first; quota
read only happens if feature is granted.

## 7. Out of scope for Plan C

Per the four spec §9.5 deferrals, Plan C does NOT include:

- Trial mechanism (Plan D)
- Capability-tease modals (Plan D)
- Skills marketplace publish/discover (Plan E)
- Team v2 reservation keys (Plan E)
- License-revocation server-side surface (Plan E or later)

Plan C is exclusively the **enforcement sweep** — placement of
existing-API `gate.has(...)` and `require_feature(...)?` calls
against the 63-key registry that Plan A built and Plan B made
authoritative.

## 8. Acceptance criteria for Plan C survey → spec transition

Before the Plan C *spec* phase begins, this survey must:

- [x] Enumerate every existing `FeatureGate::has` / `require_feature`
      callsite in production code (§2)
- [x] Map every one of the 63 Wave-9 keys to a status + natural
      callsite (§1)
- [x] Categorize keys by enforcement shape, strictly topological (§3)
- [x] Identify overlap and ambiguity zones (§4)
- [x] Propose wave segmentation grounded in conversion-trigger value,
      with §9.5 5-headline parallelism preserved (§5)
- [x] Enumerate cross-cutting risks with bootstrap-circularity
      correction (§6)
- [x] Triple-review by `worker://gemini` (architectural correctness +
      conversion thesis sanity), `worker://kimi` (callsite-grounding
      audit), and `worker://claude-code` (cross-doc consistency +
      Rust idiom + spec fidelity)
- [x] Lock the typed-error contract for downstream consumers:
      Plan C must export `FeatureGateError { key: FeatureKey, ... }`
      from the `require_feature` helper. Plan D D.6 (capability-tease
      modal) pattern-matches on this stable typed output (per Plan D
      §6.5); without a typed contract, Plan D inherits ad-hoc
      strings and the temporal coupling the survey is meant to break.

Reviewer findings applied inline (registry count 63, §3 bucket
dissolution, §5 wave shatter, §6.3 fail-closed correction,
review_sink.rs / NativeAcpConnection / server.rs:129 citation
fixes; cross-doc consistency: 64→63 ghost references swept).
