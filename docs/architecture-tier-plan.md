# SPUR Feature-Based Tier Plan

> Generated via MCTS simulation with second-order effect analysis. Maps all architectural capabilities (see [architecture.md](architecture.md)) to gated feature keys. No time-based subscriptions — pure feature entitlements.

## 1. Methodology: MCTS + Second-Order Thinking

We treat tier allocation as a **sequential decision game** where the objective is maximizing long-term value (adoption → conversion → retention → revenue) while minimizing churn and negative externalities.

### MCTS Phases Applied

| Phase | Activity |
|---|---|
| **Selection** | Identify architectural capabilities as decision nodes. Classify each by: direct value, ecosystem effect, conversion potential, churn risk if gated incorrectly. |
| **Expansion** | For each capability, expand two branches: "Free" vs "Gated". Apply second-order analysis to both. |
| **Simulation** | Roll out simulated user journeys (Solo Dev → Pro → Team → Enterprise) under each allocation. Measure conversion rate, time-to-value, and churn probability. |
| **Backpropagation** | Propagate journey outcomes back to feature nodes. Select allocations that maximize cumulative reward across all personas. |

### Second-Order Analysis Framework

For every feature gate, we answer:

1. **First-order**: "What revenue does this gate create?"
2. **Second-order**: "What behavior does this gate incentivize, and what are the downstream consequences?"
3. **Ecosystem effect**: "Does gating this feature hurt organic growth or create negative word-of-mouth?"
4. **Lock-in potential**: "Once a user has this feature, how costly is it to lose?"
5. **Network effect**: "Does this feature become more valuable as more team members use it?"

---

## 2. Architectural Capability → Feature Key Mapping

Each SPUR crate's capabilities are decomposed into atomic, gateable feature keys.

### 2.1 Orchestration (`spur-core`)

| Capability | Feature Key | Description |
|---|---|---|
| Single brain session | `brain_session` | One ACP brain connection, prompt/notification loop |
| Single worker delegation | `single_worker` | One concurrent `delegate_to_worker` execution |
| Parallel workers | `parallel_workers` | Multiple concurrent delegations (quota: 5 Pro, 10 Team, custom Enterprise) |
| Event sourcing | `event_persistence` | NDJSON event sink with 128MB rotation |
| Extended retention | `extended_retention` | Long-term archival, searchable history |
| Session resume | `session_resume` | Resume crashed sessions from lineage replay |

### 2.2 Review & Safety (`spur-core`)

| Capability | Feature Key | Description |
|---|---|---|
| Manual review gate | `manual_review` | Human-in-the-loop: approve/reject/modify per delegation |
| Auto-review policies | `auto_review_policies` | Configurable rules: auto-approve paths, retry policies, timeout actions |
| Shared review queue | `shared_review_queue` | Team-wide review inbox with assignment |

### 2.3 Terminal UI (`spur-tui`)

| Capability | Feature Key | Description |
|---|---|---|
| Dashboard | `tui_dashboard` | Real-time activity log, brain status, worker status |
| Session detail | `tui_session_detail` | Per-session event stream, notification drain |
| Basic lineage | `basic_lineage` | Execution flow visualization (single user) |
| Shared lineage | `shared_lineage` | Team-wide lineage projection, cross-session dependencies |

### 2.4 Git Worktree (`spur-worktree`)

| Capability | Feature Key | Description |
|---|---|---|
| Worktree isolation | `worktree_isolation` | Automatic `git worktree` creation/cleanup per delegation |
| Custom worktree policies | `custom_worktree_policies` | Merge strategies (squash/rebase), cleanup rules, branch naming templates |

### 2.5 Cost Tracking (`spur-cost`)

| Capability | Feature Key | Description |
|---|---|---|
| Basic cost display | `basic_cost_display` | Per-session running total |
| Advanced cost analytics | `advanced_cost_analytics` | Per-project breakdowns, trend graphs, export to CSV/JSON |
| Team cost dashboard | `team_cost_dashboard` | Manager view: per-user, per-project, budget alerts |

### 2.6 Project Management (`spur-pm`)

| Capability | Feature Key | Description |
|---|---|---|
| PM integration | `pm_integration` | GitHub/Linear/Plane adapters: issue sync, PR creation, status updates |
| PM webhooks | `pm_webhooks` | Bidirectional webhook handlers |

### 2.7 MCP Bridge (`spur-mcp`)

| Capability | Feature Key | Description |
|---|---|---|
| Standard tools | `mcp_standard_tools` | `delegate_to_worker`, `create_pr`, `get_issue` |
| Custom MCP tools | `custom_mcp_tools` | Register organization-internal tools |

### 2.8 Notifications (`spur-core`)

| Capability | Feature Key | Description |
|---|---|---|
| Basic notifications | `basic_notifications` | In-TUI notification pump |
| Custom notifications | `custom_notifications` | Slack/Discord/email routing, custom webhook endpoints |

### 2.9 Configuration & Governance

| Capability | Feature Key | Description |
|---|---|---|
| Local config | `local_config` | `~/.spur/config.toml` |
| Centralized config | `centralized_config` | Team-shared config with environment overrides |
| Custom policies | `custom_policies` | Organization-specific policy documents (signed overlays) |
| RBAC | `rbac` | Role-based access: admin, member, viewer |
| Audit logs | `audit_logs` | Compliance-ready event export (who did what, when) |
| SSO/SAML | `sso_saml` | Google Workspace, Okta, Azure AD integration |

---

## 3. Tier Allocation with Second-Order Justification

### 3.1 Community (Free) — "Experience the Magic"

**Philosophy**: Every feature required to experience SPUR's core value proposition must be free. We optimize for **time-to-aha-moment**, not immediate revenue.

| Feature Key | Justification | Second-Order Effect |
|---|---|---|
| `brain_session` | Table stakes — without orchestration, SPUR is useless | Free user experiences "agent that actually coordinates" → viral potential |
| `single_worker` | Core differentiator — without delegation, SPUR ≈ raw Claude Code | User sees worktree isolation + review gate → "this is safer than running agents directly" |
| `worktree_isolation` | Safety feature — without it, users fear main branch contamination | Eliminates #1 objection to agent tools: "it will mess up my repo" |
| `manual_review` | Safety feature — without it, free users have production incidents | Prevents angry tweets: "SPUR broke my code" → protects organic growth |
| `event_persistence` | Reliability — without it, crash = lost work | User trusts SPUR as a system, not a toy |
| `basic_lineage` | "Aha moment" — visualization of execution flow | User sees the value of event sourcing immediately |
| `tui_dashboard` | Differentiated surface — the TUI IS the product | Hiding TUI = hiding the product → zero conversion |
| `basic_cost_display` | **Critical** — without cost visibility, free users overspend on API calls | Prevents bill shock → prevents negative word-of-mouth |
| `basic_notifications` | Table stakes — in-app notification pump | Required for functional review loop |
| `local_config` | Table stakes — basic configuration | Required for any usable tool |
| `mcp_standard_tools` | Table stakes — standard MCP tool set | Without `delegate_to_worker`, SPUR is not SPUR |

**Quota**: 1 concurrent worker, 128MB event rotation, community support only.

---

### 3.2 Pro — "Scale Your Flow"

**Philosophy**: Gate features that create **natural friction at scale**. The upgrade trigger is hitting a limit that slows down the user's existing workflow — not missing a feature they never had.

| Feature Key | Justification | Second-Order Effect |
|---|---|---|
| `parallel_workers` | **Primary conversion driver**. Single worker feels slow on multi-step tasks | User perceives sequential execution as SPUR being "slow" → upgrade to remove friction |
| `auto_review_policies` | Power users doing repetitive tasks need automation | Manual review becomes friction after 10+ delegations/day |
| `session_resume` | Long-running sessions become valuable → losing them is painful | Creates sunk-cost attachment to SPUR |
| `advanced_cost_analytics` | Optimize spending, not just track it | User feels "Pro pays for itself" by reducing waste |
| `custom_worktree_policies` | Power users need custom merge strategies | Differentiates from "one-size-fits-all" Community |
| `custom_notifications` | Power users want Slack/Discord integration | Creates workflow integration → increases stickiness |
| `extended_retention` | Heavy users exceed 128MB quickly | Natural quota-based upgrade trigger |
| `tui_session_detail` | Deep dive into individual sessions | Power user feature, not needed for casual use |

**Quota**: 5 concurrent workers, 1GB event retention, priority Discord support.

**Pricing**: $12/month or $99 lifetime (perpetual license for current major version).

---

### 3.3 Team — "Ship Together"

**Philosophy**: Gate features with **network effects** and **sticky integrations**. The upgrade trigger is team coordination friction, not individual productivity.

| Feature Key | Justification | Second-Order Effect |
|---|---|---|
| `pm_integration` | **THE stickiest feature**. Once GitHub API keys, webhooks, issue templates are configured, switching cost is massive | If Pro users set this up individually → painful migration to team plan. Better to gate at Team so first setup creates org lock-in. |
| `shared_lineage` | Team visibility into all delegations | "If you don't use SPUR, you can't see what the team is doing" → viral within org |
| `team_cost_dashboard` | Manager/buyer persona needs visibility | Natural budget justification for purchase |
| `centralized_config` | New team members onboard with zero friction | Reduces team adoption friction → faster expansion |
| `rbac` | Required for any serious team (security) | Security team becomes advocate, not blocker |
| `shared_review_queue` | Team-wide review coordination | Prevents review bottlenecks → higher team throughput |
| `pm_webhooks` | Bidirectional sync with PM tools | Deepens integration stickiness |

**Quota**: 10 concurrent workers per seat, 10GB team retention, shared configuration.

**Pricing**: $29/seat/month (annual) or $39/seat/month (monthly). Minimum 3 seats.

---

### 3.4 Enterprise — "Govern at Scale"

**Philosophy**: Gate features that **enterprise buyers require** for security, compliance, and procurement. These are not productivity features — they are **risk-reduction** features.

| Feature Key | Justification | Second-Order Effect |
|---|---|---|
| `sso_saml` | Security team requirement | Unblocks procurement — without SSO, many enterprises cannot adopt |
| `audit_logs` | Compliance requirement (SOC2, ISO27001) | Security team becomes internal champion |
| `custom_policies` | Legal/compliance needs custom terms | Enables regulated industries (finance, healthcare, gov) |
| `on_premise` | Data residency requirement | Unblocks enterprises in EU, APAC with strict data laws |
| `custom_quotas` | Large-scale resource management | Prevents runaway API spend in large orgs |
| `custom_mcp_tools` | Internal tool integration | Large orgs have proprietary systems → SPUR becomes infrastructure |
| `dedicated_support` | SLA-backed response times | Reduces enterprise deployment risk |
| `sla_guarantee` | Uptime guarantees for critical workflows | Required for production deployment approvals |

**Quota**: Custom (negotiated), unlimited retention, custom rate limits.

**Pricing**: Contact sales. Typical range: $50–150/seat/month with annual minimum.

---

## 4. MCTS Simulation: User Journey Rollouts

### Journey A: Solo Developer (Community → Pro)

```
Day 1: Installs SPUR, runs `spur watch`
        ↓
        Brain orchestrates, delegates 1 task to worker
        ↓
        Sees TUI with lineage, reviews the change, approves
        ↓
        Sees cost: "$0.12 this session" → feels in control
        ↓
Day 3: Wants to run tests + lint in parallel while editing
        ↓
        Tries second delegation → "Community tier: 1 worker max"
        ↓
        Upgrade prompt: "Pro: 5 parallel workers + auto-approve"
        ↓
        Upgrades to Pro ($12/month)
```

**Conversion driver**: `parallel_workers` creates natural friction at the exact moment the user understands SPUR's value.

### Journey B: Pro Power User (Pro → Team)

```
Week 2: Using Pro with 5 parallel workers, auto-approve policies
        ↓
        Wants GitHub PR created automatically after delegation
        ↓
        Tries `spur config pm.github` → "Requires Team plan"
        ↓
        Convinces team lead: "We need shared lineage + GitHub sync"
        ↓
        Team lead upgrades to Team ($29/seat × 5 = $145/month)
        ↓
        Sets up GitHub integration once → whole team benefits
        ↓
        New team member onboarding: `git clone && spur watch` 
        (centralized config already has PM integration)
```

**Conversion driver**: `pm_integration` is sticky and creates team-wide lock-in. Centralized config reduces onboarding friction for new members.

### Journey C: Startup (Team → Enterprise)

```
Month 3: Team of 12 on Team plan, heavy usage
        ↓
        Security audit requires: "Who approved this deployment?"
        ↓
        Needs audit logs + SSO with Google Workspace
        ↓
        Upgrades to Enterprise (contact sales)
        ↓
        Security team signs off → procurement approves
```

**Conversion driver**: Enterprise features are procurement requirements, not user requests. The buyer (security/compliance) is different from the user (developer).

### Journey D: Churn Risk Path (What We Avoid)

```
Bad Path 1: Free user can't delegate → SPUR feels useless
             → Uninstalls in 5 minutes
             
Bad Path 2: Free user can't see cost → Gets $50 API bill
             → Angry tweet → Lost 10 potential customers
             
Bad Path 3: Free user can't use worktrees → Contaminates main
             → git reset --hard → Data loss fear → Never returns
             
Bad Path 4: Pro user sets up GitHub individually → Team wants to adopt
             → "I have to reconfigure everything?" → Adoption blocked
```

**Mitigation**: Community includes table stakes. PM integration is Team-only from day one.

---

## 5. PolicyDocument Schema (Updated)

```json
{
  "schema_version": 1,
  "issued_at": "2026-04-21T00:00:00Z",
  "tier_policies": {
    "community": {
      "features": [
        "brain_session",
        "single_worker",
        "worktree_isolation",
        "manual_review",
        "event_persistence",
        "basic_lineage",
        "tui_dashboard",
        "basic_cost_display",
        "basic_notifications",
        "local_config",
        "mcp_standard_tools"
      ],
      "quotas": {
        "max_concurrent_workers": 1,
        "event_retention_mb": 128,
        "max_team_members": 1
      },
      "metadata": {
        "label": "Community",
        "description": "Free tier with core orchestration, safety, and visibility."
      }
    },
    "pro": {
      "features": [
        "brain_session",
        "single_worker",
        "worktree_isolation",
        "manual_review",
        "event_persistence",
        "basic_lineage",
        "tui_dashboard",
        "basic_cost_display",
        "basic_notifications",
        "local_config",
        "mcp_standard_tools",
        "parallel_workers",
        "auto_review_policies",
        "session_resume",
        "advanced_cost_analytics",
        "custom_worktree_policies",
        "custom_notifications",
        "extended_retention",
        "tui_session_detail"
      ],
      "quotas": {
        "max_concurrent_workers": 5,
        "event_retention_gb": 1,
        "max_team_members": 1
      },
      "metadata": {
        "label": "Pro",
        "description": "Scale your flow with parallel workers, automation, and analytics."
      }
    },
    "team": {
      "features": [
        "brain_session",
        "single_worker",
        "worktree_isolation",
        "manual_review",
        "event_persistence",
        "basic_lineage",
        "tui_dashboard",
        "basic_cost_display",
        "basic_notifications",
        "local_config",
        "mcp_standard_tools",
        "parallel_workers",
        "auto_review_policies",
        "session_resume",
        "advanced_cost_analytics",
        "custom_worktree_policies",
        "custom_notifications",
        "extended_retention",
        "tui_session_detail",
        "pm_integration",
        "shared_lineage",
        "team_cost_dashboard",
        "centralized_config",
        "rbac",
        "shared_review_queue",
        "pm_webhooks"
      ],
      "quotas": {
        "max_concurrent_workers_per_seat": 10,
        "event_retention_gb": 10,
        "min_seats": 3
      },
      "metadata": {
        "label": "Team",
        "description": "Ship together with PM integration, shared lineage, and team governance."
      }
    },
    "enterprise": {
      "features": [
        "brain_session",
        "single_worker",
        "worktree_isolation",
        "manual_review",
        "event_persistence",
        "basic_lineage",
        "tui_dashboard",
        "basic_cost_display",
        "basic_notifications",
        "local_config",
        "mcp_standard_tools",
        "parallel_workers",
        "auto_review_policies",
        "session_resume",
        "advanced_cost_analytics",
        "custom_worktree_policies",
        "custom_notifications",
        "extended_retention",
        "tui_session_detail",
        "pm_integration",
        "shared_lineage",
        "team_cost_dashboard",
        "centralized_config",
        "rbac",
        "shared_review_queue",
        "pm_webhooks",
        "sso_saml",
        "audit_logs",
        "custom_policies",
        "custom_quotas",
        "custom_mcp_tools",
        "dedicated_support",
        "sla_guarantee"
      ],
      "quotas": {
        "max_concurrent_workers_per_seat": "custom",
        "event_retention": "unlimited",
        "min_seats": "custom"
      },
      "metadata": {
        "label": "Enterprise",
        "description": "Govern at scale with SSO, audit logs, and custom deployment."
      }
    }
  },
  "flags": {
    "kill_advanced_planner": {
      "enabled": true,
      "description": "Kill switch on the new agent planner."
    },
    "enable_browser_tool": {
      "enabled": false,
      "rollout_percent": 5.0,
      "tier_filter": ["pro", "team", "enterprise"],
      "description": "Gradual ramp candidate. 5% of Pro+ installs."
    },
    "enable_compaction_v2": {
      "enabled": true,
      "description": "Kill switch on V2 compaction logic."
    },
    "enable_telemetry": {
      "enabled": false,
      "description": "Off until telemetry spec lands."
    }
  }
}
```

---

## 6. Implementation Checklist

### Phase 1: Update FeatureKey Registry
- [ ] Replace placeholder keys in `feature_key.rs` with architectural keys
- [ ] Group keys by crate/capability for grep-discoverability
- [ ] Add quota key constants (`max_concurrent_workers`, etc.)

### Phase 2: Update PolicyDocument
- [ ] Rewrite `default_policy.json` with new tier allocations
- [ ] Re-sign with `spur-policy-2026-04` key
- [ ] Verify `build.rs` compile-time check passes

### Phase 3: Add Quota Enforcement
- [ ] `parallel_workers`: Semaphore limit in `orchestrator.rs`
- [ ] `event_persistence`: Rotation threshold in `EventSink`
- [ ] `max_team_members`: Enforcement in `CommunityProvider`

### Phase 4: Add Feature Gates in Code
- [ ] `auto_review_policies`: Gate in `ReviewSink`
- [ ] `pm_integration`: Gate in `spur-pm` adapters
- [ ] `custom_notifications`: Gate in `NotificationPump`
- [ ] `shared_lineage`: Gate in `ExecutorLineage` projection
- [ ] `rbac`: Gate in MCP tool dispatch

### Phase 5: Update Onboarding Docs
- [ ] Rewrite `community-tier.md` with architectural feature list
- [ ] Add `pro-tier.md` with conversion triggers
- [ ] Add `team-tier.md` with collaboration features
- [ ] Add `enterprise-tier.md` with security/compliance features

---

## 7. Key Decisions & Trade-offs

### Decision 1: Cost Tracking in Community (Not Gated)
- **Alternative considered**: Gate cost tracking behind Pro
- **Second-order analysis**: Free users without cost visibility overspend → bill shock → negative word-of-mouth → reduced organic growth
- **Decision**: Basic cost display is Community. Advanced analytics (per-project, trends) is Pro.

### Decision 2: PM Integration in Team (Not Pro)
- **Alternative considered**: Allow Pro users to set up GitHub integration
- **Second-order analysis**: If Pro users configure PM individually, team adoption requires painful reconfiguration → lower Team conversion
- **Decision**: PM integration is Team-only. Creates org-level lock-in from first setup.

### Decision 3: Worktree Isolation in Community (Not Gated)
- **Alternative considered**: Gate custom worktree policies only, make basic isolation Pro
- **Second-order analysis**: Without isolation, users fear main branch contamination → they won't try SPUR at all
- **Decision**: Basic worktree isolation is Community. Custom policies (merge strategies, naming) is Pro.

### Decision 4: No Time-Based Trial
- **Alternative considered**: 14-day Pro trial
- **Second-order analysis**: Time pressure creates anxiety; feature-based limits let users explore at their own pace
- **Decision**: Community is the trial. It's unlimited in time, limited in concurrency/integrations. Users upgrade when they hit friction, not when a timer expires.

### Decision 5: Lifetime License for Pro
- **Alternative considered**: Subscription only
- **Second-order analysis**: Solo developers hate subscriptions for tools. Lifetime option builds goodwill and cash flow.
- **Decision**: Pro offers both $12/month and $99 lifetime. Lifetime is perpetual for current major version (e.g., v1.x).

---

## 8. Success Metrics

| Metric | Target | Measurement |
|---|---|---|
| Community → Pro conversion rate | > 8% | `spur auth login` events / total installs |
| Pro → Team conversion rate | > 15% | Team activations from Pro keys |
| Time-to-first-delegation | < 2 min | Telemetry (when enabled) |
| Day-7 retention (Community) | > 40% | Users who run `spur watch` again within 7 days |
| Net Promoter Score (Pro+) | > 50 | In-app survey |
| Churn rate (Team) | < 3%/month | Cancellation events |

---

*Last updated: 2026-04-21. Tier plan is living document — re-run MCTS simulation quarterly using actual usage data.*
