# Live Blocks Visualization

## Data Flow: Agent Sessions → Live Blocks

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   Claude    │     │    Codex    │     │    Kiro     │     │   Generic   │
│   Code      │     │             │     │             │     │             │
└──────┬──────┘     └──────┬──────┘     └──────┬──────┘     └──────┬──────┘
       │                   │                   │                   │
       └───────────────────┴───────────────────┴───────────────────┘
                               │
                               ▼
                  ┌────────────────────────┐
                  │   spur-core Orchestrator│
                  │  start_session()         │
                  │  delegate_to_worker()    │
                  │  end_session_with_tokens()│
                  └───────────┬────────────┘
                              │
                              ▼
                  ┌────────────────────────┐
                  │    SQLite cost.db      │
                  │  ┌─────────────────┐   │
                  │  │  sessions       │   │
                  │  │  • id           │   │
                  │  │  • agent        │   │
                  │  │  • model        │   │
                  │  │  • started_at   │   │
                  │  │  • ended_at     │   │  ← NULL = ACTIVE
                  │  │  • input_tokens │   │
                  │  │  • output_tokens│   │
                  │  │  • cost_usd     │   │
                  │  └─────────────────┘   │
                  │  ┌─────────────────┐   │
                  │  │ delegation_log  │   │
                  │  └─────────────────┘   │
                  └───────────┬────────────┘
                              │
                              ▼
                  ┌────────────────────────┐
                  │   spur-cost Reporter   │
                  │                        │
                  │  1. SQL Query          │
                  │     WHERE ended_at IS  │
                  │        NULL            │
                  │     OR ended_at >=     │
                  │        cutoff          │
                  │                        │
                  │  2. BurnRate Calc      │
                  │     tok/min = total /  │
                  │        (dur/60)        │
                  │                        │
                  │  3. Projection         │
                  │     cost_1h = $/hr     │
                  │                        │
                  └───────────┬────────────┘
                              │
              ┌───────────────┴───────────────┐
              ▼                               ▼
   ┌────────────────────┐         ┌────────────────────┐
   │  TablePresenter    │         │   JsonPresenter    │
   │  ASCII terminal    │         │   machine JSON     │
   └────────────────────┘         └────────────────────┘
```

---

## The SQL Query: Active vs Recent vs Cold

```sql
SELECT id, agent, model, project,
       started_at, ended_at, status,
       duration_seconds, estimated_cost_usd,
       input_tokens, output_tokens,
       cache_creation_tokens, cache_read_tokens
FROM sessions
WHERE ended_at IS NULL                       -- 🔴 ACTIVE
   OR (ended_at >= ?cutoff                   -- 🟡 RECENT
       AND status = 'running')
ORDER BY started_at DESC
LIMIT 100
```

### Time Axis Visualization

```
      │                                              │
      │◄──────── active_window_minutes ─────────────►│
      │                                              │
      │           cutoff                             │ now
      │             ▲                                │
──────┼─────────────┼────────────────────────────────┼──────► time
      │             │                                │
      │   ┌─────┐   │                                │
      │   │ 🔴  │   │   ┌─────┐                      │
      │   │ACTIVE│   │   │ 🟡  │                      │
      │   │sess1│   │   │RECENT│                     │
      │   └─────┘   │   │sess2│                      │
      │             │   └─────┘                      │
      │             │                                │
      │        ⚪ COLD (excluded)                    │
      │   ┌─────┐                                    │
      │   │sess3│ ended_at < cutoff                  │
      │   └─────┘                                    │
      │                                              │
```

| State | `ended_at` | Included? | Visual |
|-------|-----------|-----------|--------|
| **🔴 Active** | `NULL` | ✅ Yes | Still running |
| **🟡 Recent** | `>= cutoff` | ✅ Yes | Ended within window |
| **⚪ Cold** | `< cutoff` | ❌ No | Too old |

---

## DB Row → LiveBlock Transformation

### Input: SQLite Row

```
┌─────────────────────────────────────────────────────────────┐
│  sessions table row                                         │
├─────────────────────────────────────────────────────────────┤
│  id                 │ "sess-a1b2c3-d4e5"                     │
│  agent              │ "claude"                               │
│  model              │ "claude-sonnet-4"                      │
│  project            │ "spur-core"                            │
│  started_at         │ "2026-04-23T18:00:00Z"                 │
│  ended_at           │ NULL            ← 🔴 ACTIVE            │
│  status             │ "running"                              │
│  duration_seconds   │ 300 (5 minutes)                        │
│  estimated_cost_usd │ 0.75                                   │
│  input_tokens       │ 5000                                   │
│  output_tokens      │ 2500                                   │
│  cache_creation_tokens │ 0                                 │
│  cache_read_tokens  │ 500                                    │
└─────────────────────────────────────────────────────────────┘
```

### Transformation Pipeline

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│   Parse      │───►│   Compute    │───►│   Compute    │───►│   Package    │
│   Timestamps │    │   Totals     │    │   Burn Rate  │    │   LiveBlock  │
└──────────────┘    └──────────────┘    └──────────────┘    └──────────────┘
        │                  │                  │                  │
        ▼                  ▼                  ▼                  ▼
 started_at ──►    input_tokens  = 5000    dur_sec = 300    LiveBlock {
   → DateTime      output_tokens  = 2500    minutes = 5.0      session_id,
                  cache_creation = 0       ──────────────►    agent,
 ended_at ──►     cache_read     = 500     total = 8000       model,
   → Option                             tok/min = 1600.0      project,
                                         cost/hr = $9.00      started_at,
 last_activity =                        ──────────────►      last_activity,
   ended_at.or(started_at)              projected = $9.00    is_active: true,
                                                               input_tokens: 5000,
 is_active =                                                   output_tokens: 2500,
   ended_at.is_none()                                          cache_creation: 0,
                                         BurnRate {                 cache_read: 500,
                                           tokens_per_min: 1600.0, cost_usd: 0.75,
                                           cost_per_hour:  9.00,   burn_rate,
                                           observed_sec:   300     projected_cost
                                         }
```

### Output: LiveBlock

```
┌─────────────────────────────────────────────────────────────┐
│  LiveBlock                                                  │
├─────────────────────────────────────────────────────────────┤
│  session_id  = "sess-a1b2c3-d4e5"                           │
│  agent       = "claude"                                     │
│  model       = Some("claude-sonnet-4")                      │
│  project     = Some("spur-core")                            │
│  started_at  = 2026-04-23 18:00:00 UTC                      │
│  last_activity = 2026-04-23 18:00:00 UTC                    │
│  is_active   = true                                         │
│                                                             │
│  input_tokens     = 5000                                    │
│  output_tokens    = 2500                                    │
│  cache_creation_tokens = 0                                  │
│  cache_read_tokens = 500                                    │
│  cost_usd    = 0.75                                         │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  BurnRate                                           │    │
│  ├─────────────────────────────────────────────────────┤    │
│  │  tokens_per_minute = 1600.0                         │    │
│  │  cost_per_hour     = $9.00                          │    │
│  │  observed_seconds  = 300                            │    │
│  └─────────────────────────────────────────────────────┘    │
│                                                             │
│  projected_cost_1h = Some($9.00)                            │
└─────────────────────────────────────────────────────────────┘
```

---

## Burn Rate & Projection: The Math

```
                    ┌─────────────────────┐
                    │      INPUTS         │
                    ├─────────────────────┤
                    │  input_tokens       │
                    │  output_tokens      │
                    │  cache_creation     │
                    │  cache_read         │
                    │  cost_usd           │
                    │  duration_seconds   │
                    └──────────┬──────────┘
                               │
                               ▼
                    ┌─────────────────────┐
                    │   DERIVED VALUES    │
                    ├─────────────────────┤
                    │                     │
                    │  total_tokens =     │
                    │    in + out +       │
    ┌──────────────►│    cache + read     │
    │               │                     │
    │               │  minutes =          │
    │               │    dur_sec / 60     │
    │               │                     │
    │               └──────────┬──────────┘
    │                          │
    │         ┌────────────────┼────────────────┐
    │         ▼                ▼                ▼
    │  ┌────────────┐  ┌────────────┐  ┌────────────┐
    │  │ tokens/min │  │  cost/hour │  │ projection │
    │  ├────────────┤  ├────────────┤  ├────────────┤
    │  │            │  │            │  │            │
    └──┤ total_toks │  │  cost_usd  │  │  cost/hour │
       │ ────────── │  │ ────────── │  │            │
       │  minutes   │  │  minutes   │  │  × 1 hour  │
       │            │  │    × 60    │  │            │
       └────────────┘  └────────────┘  └────────────┘
```

### Numeric Example

```
┌──────────────────────────────────────────────────────────────────────┐
│  BRAIN SESSION: Claude Opus                                          │
├──────────────────────────────────────────────────────────────────────┤
│                                                                      │
│  Tokens consumed:                                                    │
│    ├── input:           10,000  @ $15/M  = $0.150                    │
│    ├── output:           5,000  @ $75/M  = $0.375                    │
│    └── cache read:       1,000  @ $1.5/M = $0.002                    │
│                                                                      │
│    Total tokens:        16,000                                       │
│    Total cost so far:   $0.527                                       │
│                                                                      │
│  Time elapsed: 10 minutes                                            │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │  BURN RATE                                                      │  │
│  │                                                                 │  │
│  │      16,000 tokens                                              │  │
│  │  ─────────────────── = 1,600 tokens/minute                      │  │
│  │      10 minutes                                                 │  │
│  │                                                                 │  │
│  │      $0.527                                                     │  │
│  │  ─────────────────── × 60 = $3.16 / hour                       │  │
│  │      10 minutes                                                 │  │
│  │                                                                 │  │
│  │  PROJECTED 1-HOUR COST: $3.16                                   │  │
│  │  "If Claude keeps this pace for a full hour"                    │  │
│  └────────────────────────────────────────────────────────────────┘  │
│                                                                      │
└──────────────────────────────────────────────────────────────────────┘
```

---

## TablePresenter: Terminal Output

```
╔══════════════════════════════════════════════════════════════════════════════╗
║                          LIVE USAGE REPORT                                   ║
╚══════════════════════════════════════════════════════════════════════════════╝

Session                Agent          Status     Tokens    Cost       Burn/hr
────────────────────────────────────────────────────────────────────────────────
sess-a1b2c3-d4         claude         🔴 active  16.0k     $0.5270    $3.1600
  → project: spur-core
  → 1600.0 tokens/min  |  projected 1hr: $3.1600

sess-x9y8z7-w2         codex          🔴 active   7.5k     $0.2500    $3.0000
  → project: spur-core
  → 1500.0 tokens/min  |  projected 1hr: $3.0000

sess-m3n4o5-p6         kiro           🟡 recent   3.2k     $0.1000    $1.2000
  → project: docs-site

────────────────────────────────────────────────────────────────────────────────

  Tokens:  in=20.7k    out=10.3k    cache=0        read=1.5k    total=26.7k
  Cost:    $0.8770    Duration: 25m    Sessions: 3
```

### Status Indicators

| Symbol | State | Meaning |
|--------|-------|---------|
| 🔴 | **Active** | `ended_at IS NULL` — session never ended |
| 🟡 | **Recent** | `ended_at >= cutoff` — ended within window |
| ⚪ | *Cold* | Excluded from report |

---

## JsonPresenter: Machine Output

```json
{
  "blocks": [
    {
      "session_id": "sess-a1b2c3-d4e5",
      "agent": "claude",
      "model": "claude-sonnet-4",
      "project": "spur-core",
      "started_at": "2026-04-23T18:00:00Z",
      "last_activity": "2026-04-23T18:00:00Z",
      "is_active": true,
      "input_tokens": 10000,
      "output_tokens": 5000,
      "cache_creation_tokens": 0,
      "cache_read_tokens": 1000,
      "cost_usd": 0.527,
      "tokens_per_minute": 1600.0,
      "cost_per_hour": 3.16,
      "projected_cost_1h": 3.16
    }
  ],
  "totals": {
    "input_tokens": 10000,
    "output_tokens": 5000,
    "cache_creation_tokens": 0,
    "cache_read_tokens": 1000,
    "total_tokens": 16000,
    "cost_usd": 0.527,
    "duration_seconds": 600,
    "session_count": 1
  }
}
```

---

## SessionReport: Delegation Tree View

The `SessionReport` is different from LiveReport — it shows the **hierarchy**
of brain sessions and their worker delegations:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      SESSION USAGE REPORT                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│ Delegation Trees:                                                           │
│ ─────────────────────────────────────────────────────────────────────────── │
│                                                                             │
│ [brain-1] claude | brain | $1.5000 | 10m | 16.0k tokens                    │
│   [worker-1] codex | worker | $0.5000 | 5m | 7.5k tokens                   │
│     [sub-1] kiro | worker | $0.2000 | 3m | 3.2k tokens                     │
│   [worker-2] generic | worker | $0.1000 | 2m | 1.5k tokens                 │
│                                                                             │
│ [brain-2] claude | brain | $0.8000 | 6m | 9.0k tokens                     │
│   [worker-3] codex | worker | $0.3000 | 4m | 4.5k tokens                   │
│                                                                             │
│ ─────────────────────────────────────────────────────────────────────────── │
│                                                                             │
│   Tokens:  in=38.2k   out=19.1k   cache=0      read=3.2k   total=57.7k     │
│   Cost:    $3.2000   Duration: 30m   Sessions: 6                           │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

The tree is built from `parent_session` links in the database:

```
┌─────────────┐     parent_session     ┌─────────────┐
│  brain-1    │◄───────────────────────│  worker-1   │
│  (no parent)│◄───────────────────────│  worker-2   │
└──────┬──────┘                        └──────┬──────┘
       │                                      │
       │         parent_session               │
       │◄─────────────────────────────────────┘
       │
       │  ┌─────────────┐
       └──┤   sub-1     │  ← worker-1's child
          │  parent=    │
          │  worker-1   │
          └─────────────┘
```

`total_cost()` recursively walks the tree:
```
brain-1.total_cost() = 1.50 + 0.50 + 0.20 + 0.10 = 2.30
```

---

## Code Reference Map

| Concept | File | Function/Struct |
|---------|------|----------------|
| Live report query | `reporter.rs` | `Reporter::live_report()` |
| Burn rate struct | `reports.rs` | `BurnRate` |
| Live block struct | `reports.rs` | `LiveBlock` |
| Table rendering | `presenter/table.rs` | `TablePresenter::render_live()` |
| JSON rendering | `presenter/json.rs` | `JsonPresenter::render_live()` |
| Delegation tree | `reports.rs` | `build_delegation_tree()` |
| Session node | `reports.rs` | `SessionNode` |
| Token aggregation | `reports.rs` | `Totals::from_entries()` |
