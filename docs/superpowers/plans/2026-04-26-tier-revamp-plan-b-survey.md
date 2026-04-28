# Tier Revamp Plan B Legacy-Key Caller Survey

Date: 2026-04-27

Scope: survey only. This document inventories the 36 pre-tier-revamp
`FeatureKey` constants and exact legacy string literals before Plan B rewrites
the signed policy, migrates callers, and removes the legacy registry block.

Grounding commands used:

- `rg` over `crates/` for `FeatureKey::CONST_NAME` and exact `"string_value"`
  literals, excluding `target/`.
- Manual reads of spec sections 4.15, 4.16, 5, 8.2, and 9.1.
- Manual reads of `crates/spur-license/src/policy/feature_key.rs` and
  `crates/spur-license/resources/default_policy.json`.

Summary finding: 75 non-registry exact code/test references across 14 files,
plus the signed policy payload at
`crates/spur-license/resources/default_policy.json:3`, which still embeds all
legacy tier features and the four legacy G2 flags.

## 1. Inventory of the 36 Legacy Keys

The "tier" column is the first tier where the key appears in the current
signed `default_policy.json` payload. G2 rows are policy flags, not tier
entitlements.

| legacy const name | legacy string value | tier in default_policy.json | spec 8.2 rename target | replacement v1 const name |
|---|---|---|---|---|
| `BRAIN_SESSION` | `brain_session` | community | `core_core_brain_session` | `CORE_CORE_BRAIN_SESSION` |
| `SINGLE_WORKER` | `single_worker` | community | removed; subsumed by `core_core_parallel_workers` with quota=2 | no direct const; candidate absorber `CORE_CORE_PARALLEL_WORKERS` + quota |
| `WORKTREE_ISOLATION` | `worktree_isolation` | community | `worktree_core_isolation` | `WORKTREE_CORE_ISOLATION` |
| `MANUAL_REVIEW` | `manual_review` | community | folded into `core_core_review_sink` | final target consolidated to `CORE_CORE_REVIEW` |
| `EVENT_PERSISTENCE` | `event_persistence` | community | `core_core_event_sink_ndjson_128mb` | final target consolidated to `CORE_CORE_EVENT_PIPELINE` |
| `BASIC_LINEAGE` | `basic_lineage` | community | `core_core_executor_lineage_projection` | final target consolidated to `CORE_CORE_EVENT_PIPELINE` |
| `TUI_DASHBOARD` | `tui_dashboard` | community | `tui_core_view_dashboard` | `TUI_CORE_VIEW_DASHBOARD` |
| `BASIC_COST_DISPLAY` | `basic_cost_display` | community | `cost_core_basic_display` | final target renamed to `COST_CORE_SESSION_DISPLAY` |
| `BASIC_NOTIFICATIONS` | `basic_notifications` | community | subsumed by `core_core_notification_pump` + `tui_core_notification_drain` | final target consolidated to `CORE_CORE_EVENT_PIPELINE` |
| `LOCAL_CONFIG` | `local_config` | community | `cli_core_command_*` subsumed | no clean const; candidate absorber is the `CLI_CORE_*` command family |
| `MCP_STANDARD_TOOLS` | `mcp_standard_tools` | community | `mcp_core_*` (5 keys) | split: `MCP_CORE_SERVER_DISPATCH`, `MCP_CORE_DELEGATE`, `MCP_CORE_OUTCOME_FETCH`, `MCP_CORE_PM`, `MCP_CORE_PR` |
| `PARALLEL_WORKERS` | `parallel_workers` | pro | `core_core_parallel_workers` | `CORE_CORE_PARALLEL_WORKERS` |
| `AUTO_REVIEW_POLICIES` | `auto_review_policies` | pro | `core_pro_review_policy_auto_approve` + custom timeout/retry knobs | final split: `CORE_PRO_REVIEW_AUTO_APPROVE`; retry config moved to `CORE_CORE_REVIEW_RETRY_CONFIG` |
| `SESSION_RESUME` | `session_resume` | pro | `core_core_session_resume` + `core_pro_session_resume_event_replay` | `CORE_CORE_SESSION_RESUME` + `CORE_PRO_SESSION_RESUME_EVENT_REPLAY` |
| `ADVANCED_COST_ANALYTICS` | `advanced_cost_analytics` | pro | `cost_pro_per_project_tracking` + `ctx_pro_*` | `COST_PRO_PER_PROJECT_TRACKING` + `CTX_PRO_DUCKDB_ENGINE` |
| `CUSTOM_WORKTREE_POLICIES` | `custom_worktree_policies` | pro | `worktree_pro_custom_policies` | unmapped in final registry; spec 4.16 defers/drops as vaporware |
| `CUSTOM_NOTIFICATIONS` | `custom_notifications` | pro | `notif_pro_external_channels` deferred to spec 4.16 | no final v1 const |
| `EXTENDED_RETENTION` | `extended_retention` | pro | removed; subsumed by quota lift | no const; quota-only |
| `TUI_SESSION_DETAIL` | `tui_session_detail` | pro | `tui_core_view_session_detail` | `TUI_CORE_VIEW_SESSION_DETAIL` |
| `PM_INTEGRATION` | `pm_integration` | team | split into PM keys per spec 4.6 | semantic split; final PM crate keys are `PM_CORE_BEADS_BASIC`, `PM_CORE_BROWSE`, `PM_CORE_PR`, `PM_CORE_BEADS_GRAPH_ADAPTER`, `PM_PRO_BEADS_ADVANCED` |
| `SHARED_LINEAGE` | `shared_lineage` | team | deferred to Team v2 | no final v1 const |
| `TEAM_COST_DASHBOARD` | `team_cost_dashboard` | team | unmapped in spec 8.2 | no final v1 const |
| `CENTRALIZED_CONFIG` | `centralized_config` | team | unmapped in spec 8.2 | no final v1 const |
| `RBAC` | `rbac` | team | unmapped in spec 8.2 | no final v1 const |
| `SHARED_REVIEW_QUEUE` | `shared_review_queue` | team | unmapped in spec 8.2 | no final v1 const |
| `PM_WEBHOOKS` | `pm_webhooks` | team | unmapped in spec 8.2 | no final v1 const; `pm_team_webhooks` is v2 backlog |
| `SSO_SAML` | `sso_saml` | enterprise | unmapped in spec 8.2 | no final v1 const |
| `AUDIT_LOGS` | `audit_logs` | enterprise | unmapped in spec 8.2 | no final v1 const |
| `CUSTOM_POLICIES` | `custom_policies` | enterprise | unmapped in spec 8.2 | no final v1 const |
| `CUSTOM_MCP_TOOLS` | `custom_mcp_tools` | enterprise | `mcp_pro_custom_tools` | no final v1 const; spec 4.16 defers custom tools |
| `DEDICATED_SUPPORT` | `dedicated_support` | enterprise | unmapped in spec 8.2 | no final v1 const |
| `SLA_GUARANTEE` | `sla_guarantee` | enterprise | unmapped in spec 8.2 | no final v1 const |
| `KILL_ADVANCED_PLANNER` | `kill_advanced_planner` | G2 flag | unmapped in spec 8.2 | no replacement registry key |
| `ENABLE_BROWSER_TOOL` | `enable_browser_tool` | G2 flag | unmapped in spec 8.2 | no replacement registry key |
| `ENABLE_COMPACTION_V2` | `enable_compaction_v2` | G2 flag | unmapped in spec 8.2 | no replacement registry key |
| `ENABLE_TELEMETRY` | `enable_telemetry` | G2 flag | unmapped in spec 8.2 | no replacement registry key |

## 2. Caller Enumeration

Shared registry and policy sites:

| crate :: file :: line range | class | legacy refs |
|---|---|---|
| `spur-license :: crates/spur-license/src/policy/feature_key.rs :: 46-89` | registry | all 36 legacy const definitions |
| `spur-license :: crates/spur-license/src/policy/feature_key.rs :: 217-287` | registry | all 36 `from_known()` legacy branches |
| `spur-license :: crates/spur-license/src/policy/feature_key.rs :: 477-708` | registry tests | all 36 in unit coverage and count guard |
| `spur-license :: crates/spur-license/resources/default_policy.json :: 3` | signed policy payload | all 32 legacy G1 feature strings plus all 4 legacy G2 flag strings |

Non-registry exact code/test references:

| legacy key(s) | crate :: file :: line range | class | notes |
|---|---|---|---|
| `BRAIN_SESSION` / `brain_session` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 6, 18-19, 42-43` | test | typed key and parser assertions |
| `BRAIN_SESSION` | `spur-license :: crates/spur-license/tests/community_smoke.rs :: 40` | test | Community gate assertion |
| `BRAIN_SESSION` | `spur-license :: crates/spur-license/tests/feature_gate.rs :: 8` | test | Community gate assertion |
| `BRAIN_SESSION` | `spur-license :: crates/spur-license/src/gate.rs :: 216` | test | inactive-license fail-closed assertion |
| `brain_session` | `spur-license :: crates/spur-license/src/community.rs :: 106` | test | `CommunityProvider` entitlement assertion |
| `brain_session` | `spur-license :: crates/spur-license/src/policy/mod.rs :: 302, 312, 335, 347` | policy tests | embedded resolver expectations |
| `brain_session` | `spur-mcp :: crates/spur-mcp/src/server.rs :: 141-147, 5065, 5103` | string collision / DO-NOT-RENAME | JSON projection field for outcome sections, not a policy key |
| `SINGLE_WORKER` / `single_worker` | `spur-license :: crates/spur-license/tests/community_smoke.rs :: 41`; `crates/spur-license/tests/feature_gate.rs :: 9`; `crates/spur-license/src/community.rs :: 107`; `crates/spur-license/src/policy/mod.rs :: 303` | tests | Community entitlement expectations |
| `WORKTREE_ISOLATION` | `spur-license :: crates/spur-license/tests/community_smoke.rs :: 42` | test | Community gate assertion |
| `MCP_STANDARD_TOOLS` / `mcp_standard_tools` | `spur-license :: crates/spur-license/src/policy/mod.rs :: 304` | policy test | embedded community feature expectation |
| `PARALLEL_WORKERS` / `parallel_workers` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 7`; `crates/spur-license/tests/community_smoke.rs :: 43`; `crates/spur-license/tests/feature_gate.rs :: 10, 28`; `crates/spur-license/src/gate.rs :: 227, 231, 236`; `crates/spur-license/src/community.rs :: 108, 138`; `crates/spur-license/src/policy/mod.rs :: 305, 313` | tests | Pro/community entitlement and snapshot assertions |
| `AUTO_REVIEW_POLICIES` / `auto_review_policies` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 22-23`; `crates/spur-license/src/community.rs :: 139`; `crates/spur-license/src/policy/mod.rs :: 314` | tests | Pro entitlement and parser spot checks |
| `SESSION_RESUME` / `session_resume` | `spur-acp :: crates/spur-acp/src/agents/defaults.rs :: 117` | string collision / DO-NOT-RENAME | ACP agent capability keyword, not a policy key |
| `PM_INTEGRATION` / `pm_integration` | `spur-cli :: crates/spur-cli/src/main.rs :: 664, 939` | production gating | PM service construction gate in CLI/TUI host paths |
| `PM_INTEGRATION` / `pm_integration` | `spur-core :: crates/spur-core/src/orchestrator.rs :: 861, 8618` | production gating + test | startup beads warning gate, plus test helper entitlement string |
| `PM_INTEGRATION` | `spur-license :: crates/spur-license/tests/community_smoke.rs :: 44` | test | Community does not have PM integration |
| `RBAC` / `rbac` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 25` | test | parser spot check only |
| `DEDICATED_SUPPORT` / `dedicated_support` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 27-28` | test | parser spot check only |
| `KILL_ADVANCED_PLANNER` / `kill_advanced_planner` | `spur-cli :: crates/spur-cli/src/commands/flags.rs :: 61`; `spur-tui :: crates/spur-tui/src/app.rs :: 154` | production flag surface | G2 flag is listed by CLI and summarized by TUI |
| `KILL_ADVANCED_PLANNER` / `kill_advanced_planner` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 9-10`; `crates/spur-license/tests/flag_evaluator.rs :: 11, 21-22, 33, 43, 57`; `crates/spur-license/tests/feature_gate.rs :: 36, 49, 60`; `crates/spur-license/src/policy/mod.rs :: 286`; `spur-cli :: crates/spur-cli/tests/flags_smoke.rs :: 16` | tests | flag evaluator, policy, and CLI smoke coverage |
| `ENABLE_BROWSER_TOOL` / `enable_browser_tool` | `spur-cli :: crates/spur-cli/src/commands/flags.rs :: 62`; `spur-tui :: crates/spur-tui/src/app.rs :: 155` | production flag surface | G2 flag is listed by CLI and summarized by TUI |
| `ENABLE_BROWSER_TOOL` / `enable_browser_tool` | `spur-license :: crates/spur-license/tests/feature_key.rs :: 31-32`; `spur-cli :: crates/spur-cli/tests/flags_smoke.rs :: 17` | tests | parser and CLI smoke coverage |
| `ENABLE_COMPACTION_V2` | `spur-cli :: crates/spur-cli/src/commands/flags.rs :: 63`; `spur-tui :: crates/spur-tui/src/app.rs :: 156` | production flag surface | G2 flag is listed by CLI and summarized by TUI |
| `ENABLE_TELEMETRY` | `spur-cli :: crates/spur-cli/src/commands/flags.rs :: 64`; `spur-tui :: crates/spur-tui/src/app.rs :: 157` | production flag surface | G2 flag is listed by CLI and summarized by TUI |

Legacy keys with no non-registry exact refs beyond `feature_key.rs` and the
signed policy payload:

`MANUAL_REVIEW`, `EVENT_PERSISTENCE`, `BASIC_LINEAGE`, `TUI_DASHBOARD`,
`BASIC_COST_DISPLAY`, `BASIC_NOTIFICATIONS`, `LOCAL_CONFIG`,
`ADVANCED_COST_ANALYTICS`, `CUSTOM_WORKTREE_POLICIES`,
`CUSTOM_NOTIFICATIONS`, `EXTENDED_RETENTION`, `TUI_SESSION_DETAIL`,
`SHARED_LINEAGE`, `TEAM_COST_DASHBOARD`, `CENTRALIZED_CONFIG`,
`SHARED_REVIEW_QUEUE`, `PM_WEBHOOKS`, `SSO_SAML`, `AUDIT_LOGS`,
`CUSTOM_POLICIES`, `CUSTOM_MCP_TOOLS`, `SLA_GUARANTEE`.

## 3. Per-Crate Impact Estimate

| crate | files touched | estimated line churn | classification | notes |
|---|---:|---:|---|---|
| `spur-license` | 9-11 | 300-500 | mixed resolver/schema, policy, mechanical tests | Must update `PolicyDocument`, `TierPolicy`, `PolicyResolver`, `build.rs`, `default_policy.json`, legacy registry removal, `from_known()`, and policy/gate tests. |
| `spur-core` | 1 | 8-20 | semantic merge | `PM_INTEGRATION` currently gates beads startup warning; target depends on PM split decision. |
| `spur-cli` | 3 | 25-60 | semantic merge + flag strategy | Two `PM_INTEGRATION` service gates plus `flags list` legacy G2 flag registry. |
| `spur-tui` | 1 | 8-25 | flag strategy | `compute_flag_summary()` hard-codes the four legacy G2 flag keys. |
| `spur-acp` | 0 intended | 0 | DO-NOT-RENAME collision | `session_resume` at `agents/defaults.rs:117` is an ACP capability keyword, not a policy key. |
| `spur-mcp` | 0 intended | 0 | DO-NOT-RENAME collision | `brain_session` at `server.rs:141-147,5065,5103` is a JSON projection field. |

Plan B impact is concentrated in `spur-license`, `spur-core`, `spur-cli`, and
`spur-tui`. `spur-acp` and `spur-mcp` have exact string collisions only.

## 4. Unmapped Gaps

There are 14 outright spec 8.2 omissions. These should not be silently
bulk-renamed.

| legacy key | recommendation | rationale |
|---|---|---|
| `team_cost_dashboard` | drop / Team v2 human decision | v1 spec now defers Team tier; no final registry key. |
| `centralized_config` | drop / Team v2 human decision | no v1 shared config feature key exists. |
| `rbac` | drop / Team v2 human decision | no v1 multi-user RBAC surface; Telegram bot is single-operator. |
| `shared_review_queue` | drop / Team v2 human decision | no Team review inbox in v1; do not map to local review gates. |
| `pm_webhooks` | drop / Team v2 human decision | spec 4.16 says `pm_team_webhooks` is vaporware. |
| `sso_saml` | drop / Enterprise future | no v1 enterprise identity subsystem. |
| `audit_logs` | drop / Enterprise future | SPUR audit sentinels exist, but not an enterprise audit-log entitlement. |
| `custom_policies` | human decision | policy overlays exist conceptually, but no final feature key. |
| `dedicated_support` | drop | commercial/support promise, not runtime-gateable code. |
| `sla_guarantee` | drop | commercial/SLA promise, not runtime-gateable code. |
| `kill_advanced_planner` | human decision | current G2 flag has production surfaces but no replacement registry key. |
| `enable_browser_tool` | human decision | current G2 flag has production surfaces but no replacement registry key. |
| `enable_compaction_v2` | human decision | current G2 flag has production surfaces but no replacement registry key. |
| `enable_telemetry` | human decision | spec 5 keeps a telemetry flag concept, but spec 8.2 has no registry replacement. |

Track 23 no-clean-final-v1 rows if Plan B treats removed, deferred, and
subsumed targets as gaps: the 14 omissions above plus `single_worker`,
`manual_review`, `event_persistence`, `basic_lineage`, `basic_notifications`,
`local_config`, `custom_worktree_policies`, `custom_notifications`, and
`extended_retention`.

Additional split rows are mapped but require semantic migration rather than a
mechanical rename: `mcp_standard_tools`, `auto_review_policies`,
`session_resume`, `advanced_cost_analytics`, and especially `pm_integration`.

## 5. Migration Risk Assessment

High-risk callsites:

- `PM_INTEGRATION` is the only legacy G1 key used in production gating:
  `crates/spur-cli/src/main.rs:664`, `crates/spur-cli/src/main.rs:939`, and
  `crates/spur-core/src/orchestrator.rs:861`. The old key mixed PM browse,
  PR creation, advanced beads extensions, and MCP durable-plan behavior. Do not
  map it blindly to one `PM_*` key without deciding which concrete behavior each
  callsite gates.
- The four G2 flags have no replacement registry in Plan A's 63-key final
  shape, but they are surfaced by CLI and TUI. Removing the legacy 36 before
  designing a flag registry or replacement constants will make
  `flags list`, TUI flag summary, and flag tests fail.
- `brain_session` is a DO-NOT-RENAME string collision in `spur-mcp` outcome
  projection JSON. It is also adjacent to many `brain_session_id` ACP/MCP wire
  fields. Bulk text replacement would risk serialization compatibility.
- `session_resume` is a DO-NOT-RENAME string collision in
  `spur-acp/src/agents/defaults.rs:117`; it is an agent capability lint token,
  not a policy entitlement.
- Existing ACP/MCP round-trip tests should be treated as compatibility guards.
  Any migration sweep that touches `brain_session_id`, outcome projection keys,
  or ACP event payload strings should run targeted serialization tests, not just
  `spur-license` tests.
- Spec section 5 is stale relative to the Wave-8.5-final 63-key registry: it
  still lists keys later dropped, consolidated, renamed, or deferred. Rewriting
  `default_policy.json` must use `feature_key.rs` final registry and spec 4.15
  / 4.16, not copy the section 5 JSON verbatim.
- `build.rs` currently accepts schema major 1 only, while Plan B wants the
  schema extensions for `@inherit:community`, `policy_version`, `expires_at`,
  and `v1_1_q3_roadmap`. Schema/resolver changes must land before the signed
  policy is rewritten.

## 6. Recommended Wave Decomposition

Wave 1: schema + resolver extension only, no caller changes. Add support for
schema version 2, `policy_version`, `expires_at`, `@inherit:community`, and
`v1_1_q3_roadmap`. Ensure roadmap keys are parsed but never activated as
current-tier features.

Wave 2: default policy rewrite + re-sign + build validation. Replace the
legacy signed payload with the 63-key Wave-8.5-final policy, re-sign with
`spur-policy-2026-04`, and update `build.rs` to validate the new schema shape.
Rollback is restoring the old signed policy and schema cap.

Wave 3: mechanical `spur-license` migration. Update Community/Pro policy tests,
gate tests, quota expectations, and typed parser tests to use final v1 keys
where the mapping is one-to-one or an agreed umbrella absorber exists. Keep the
legacy constants present during this wave.

Wave 4: PM semantic migration. Replace `PM_INTEGRATION` production gates in
`spur-cli` and `spur-core` with explicit PM/MCP v1 targets after deciding each
callsite's behavior boundary. Review this wave separately because it changes
runtime availability of PM service wiring.

Wave 5: G2 flag strategy. Either introduce a separate typed flag registry or
add replacement v1 flag constants before removing the legacy four. Update
`spur flags list`, TUI flag summary, and flag evaluator tests together.

Wave 6: collision and serialization guard wave. Add or update negative grep
tests/round-trip tests proving that `brain_session` outcome JSON,
`brain_session_id` wire fields, and ACP `session_resume` capability keywords
were not bulk-renamed.

Wave 7: legacy removal + final grep sweep. Delete the 36-key legacy block,
remove legacy branches from `from_known()`, update count guards, and run a full
grep sweep for every legacy const and exact policy string. This wave should be
small, reviewable, and easy to revert because all semantic migrations already
landed earlier.
