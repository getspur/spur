# TUI Insights View — design

**Status:** design pending review (single-pass authored 2026-04-28; codex worker review applied)
**Date:** 2026-04-28
**Owner:** Kevin Truong (kevin.truong.ds@gmail.com)
**Predecessors:**
- `2026-04-24-onboard-kimi-gemini-opencode-cost.md` — onboarding plan for Kimi/Gemini/OpenCode (already on main; Kimi shipped, OpenCode shipped, **Gemini extractor never written** — this spec executes the Gemini section)
- `2026-04-24-spur-cost-p0-correctness-tranche.md` — P0 correctness tranche (already on main as plan; **5 of 11 P0 items implemented on `feat/spur-cost-p0-correctness`**, harvested into the current branch)
- `2026-04-28-agent-model-effort-surface-design.md` — concurrent M10.1 status-bar work (independent surface; this spec does NOT touch the status bar)

**Substrate branch:** `feat/insights-substrate-harvest` (worktree at `/Volumes/Projects/spur/.worktrees/insights-substrate-harvest`). Six commits ahead of main (P0.1, P0.2/3/4, P0.6, P0.8, Codex cache audit doc, Kimi JSONL ingest with conflict-resolved merge). `cargo check -p spur-context --features duckdb` and `cargo check -p spur-cli` both pass; tests compile clean.

**Background:** Spur today has two independent cost-reporting stacks. The orchestrator owns a SQLite tracker (`spur-cost::tracker.rs`) producing time-estimated costs from `ExecutorLineage`; the TUI dashboard reads this lineage. Separately, `spur-context::AnalyticsEngine` ingests agent-native session logs (Claude/Codex JSONL, OpenCode SQLite, Kimi JSONL) into DuckDB and produces token-accurate cost reports surfaced via the `spur cost` CLI. The two paths produce different numbers because (a) the orchestrator never calls `end_session_with_tokens` (`crates/spur-core/src/orchestrator.rs:1897, 3024, 3160, 3662`), and (b) the dashboard surface has no view onto the analyst path. Users currently have no in-TUI window into the analyst's data and no historical context beyond the live session.

---

## 1. Goal

Ship a feature-gated TUI surface that exposes spur-context's analytics directly to the user while running the agent, with **single-source-of-truth cost** when the feature is enabled. After this work:

- Pressing `Alt+a` from any view opens an Insights view with four tabs (Overview, Timeline, Breakdown, Live) backed by `spur-context::AnalyticsEngine`. (Alt+a chosen to avoid collision with the pre-existing Alt+i vim-mode toggle.)
- Five well-known agents are first-class citizens: Claude Code, Codex, Gemini, OpenCode, Kimi. Kiro stays a stub with a clearly-labeled "no token data — ACP capture pending" badge (Phase 2 work).
- The dashboard's cost segment, when the `analytics` feature is on, sources from the same `AnalyticsEngine::live_session_snapshot` query the Insights view consumes — not from `ExecutorLineage`. One number, one truth, on screen.
- The feature is **default OFF** (experimental). When off, the Insights view shows a "feature disabled — rebuild with `--features analytics`" splash, the dashboard continues to use lineage cost (pre-existing behavior), and `cargo build` produces a binary identical in size and capability to today's.
- A "via analytics" pill on the status bar makes the current data lens visible whenever the experimental flag is on.

## 2. Non-goals

- **No new analytics queries beyond what `AnalyticsEngine` already exposes.** Phase 1 composes existing typed methods (`daily_report`, `weekly_report`, `monthly_report`, `model_breakdown`, `project_breakdown`, `live_recent_sessions`); no new SQL files except `R3`'s `newest_agent_mtime` change.
- **No Forecast tab in Phase 1.** MTD projection, anomaly detection, cache-efficiency view defer to Phase 2.
- **No orchestrator refactor in Phase 1.** `end_session_with_tokens` wiring remains untouched (R5 deferred). The dashboard cost-source switch is a TUI-only change that bypasses the SQLite tracker entirely when analytics is on.
- **No agent-name normalization.** `claude-code-acp` / `codex-acp` / `gemini-acp` strings stay as-is; the harvested branch did not include `stash@{7}`'s rename. Display labels in the Insights view strip the `-acp` suffix at render time only (cosmetic).
- **No persistence of view state across sessions.** Active tab, granularity, dimension reset on each open.
- **No new pricing rows for unknown models.** Unpriced rows surface with `cost_source = "unpriced"` and a visible badge — never silently as $0.
- **No CLI changes.** The `spur cost` CLI keeps its existing surface; future `spur cost-insights --json` is a Phase 3 idea, explicitly out of scope.

## 3. Background — what exists post-harvest

Concrete ground (file:line on `feat/insights-substrate-harvest`):

| Producer / capability | Consumer slot | Status |
|---|---|---|
| `AnalyticsEngine::daily_report(days) -> Vec<DailyRow>` (`crates/spur-context/src/engine.rs:1118`) | `spur cost daily` CLI only | ✅ harvested; reusable |
| `AnalyticsEngine::weekly_report(weeks)` (`engine.rs:1154`) / `monthly_report(months)` (`engine.rs:1190`) | CLI only | ✅ harvested |
| `AnalyticsEngine::model_breakdown()` (`engine.rs:1338`) / `project_breakdown()` (`engine.rs:1357`) | CLI only | ✅ harvested |
| `AnalyticsEngine::live_recent_sessions(minutes)` (`engine.rs:1296`) returning `Vec<LiveBlockRow>` | CLI only | ✅ harvested; per-session burn rate, projected hourly |
| `AnalyticsEngine::live_session_snapshot(session_id)` (`engine.rs:1403`) | CLI only | ✅ harvested |
| `AsyncEngine` wrapping `Arc<Mutex<AnalyticsEngine>>` (`crates/spur-context/src/async_engine.rs:38`) with `run<F, R>(&self, f: F) -> Result<R>` escape hatch (`async_engine.rs:60`) | unused | ✅ harvested |
| `cost_source` column (P0.3) computed in `ALL_EVENTS_WITH_COST_VIEW` (`engine.rs:53`): `'native'` / `'priced'` / `'unpriced'` | exposed on every row type | ✅ harvested |
| `SessionRow::models: Option<String>` (P0.8 multi-model aggregation) — comma-separated via `string_agg(DISTINCT model)` | n/a | ✅ harvested (breaking rename from `.model`) |
| Kimi `kimi_events` view (`engine.rs:734`) — pre/post `_usage` pairing | n/a | ✅ harvested |
| OpenCode model IDs stored verbatim (`anthropic/...`, `google/...`, `z-ai/...`, `moonshotai/...`) defeating `LIKE 'lower(p.model) || '-%''` LATERAL prefix matcher in `all_events_with_cost` | manifests as `cost_source='unpriced'` for any provider-prefixed row | ❌ R1 needed |
| `kimi-for-coding` model unregistered in `PricingRegistry::with_builtin_prices()` (`crates/spur-cost/src/pricing.rs:84-246`) | every Kimi event surfaces as `cost_source='unpriced'` despite token data being correct | ❌ R2 needed |
| `newest_agent_mtime()` (`engine.rs:204-226`) iterates `[claude_dir, codex_dir, kiro_dir, kimi_dir]` only — OpenCode SQLite excluded | OpenCode-only users get permanently-stale cache | ❌ R3 needed |
| Gemini transcript files at `~/.gemini/tmp/<uuid>/chats/session-*.json` (single-doc JSON; per-message `tokens { input, output, cached, thoughts, tool, total }` block; verified on dev machine 2026-04-28) | no extractor anywhere | ❌ R4 needed; design fully specified in `2026-04-24-onboard-kimi-gemini-opencode-cost.md` Step 3 |
| Kiro JSONL at `~/.kiro/sessions/cli/<uuid>.jsonl` containing only ACP protocol logs (`Prompt`, `AssistantMessage`, `ToolResults`) — **no token fields**; billing arrives via ACP `UsageUpdate` notifications, not files (`crates/spur-cost/src/ingest/kiro.rs:3-18`) | stub view (`engine.rs:530-533`) | ⚠️ Phase 2 |
| `spur-tui::App` driving multi-threaded tokio runtime + view trait + `worker_streams::*` channels for agent output | view registry; existing `markdown` feature precedent in `Cargo.toml:10` | ✅ pattern reference |
| `DashboardView` cost segment (`crates/spur-tui/src/views/dashboard.rs:504`) reading `ExecutorLineage::current_attempt().cost_usd` | hardcoded lineage source | ❌ Phase 1 swaps to analytics when feature on |
| 3 exhaustive matches that gain a new `ViewId::Insights` arm (`crates/spur-tui/src/app.rs:940`, `app.rs:1536`, `crates/spur-tui/src/components/status_bar.rs:195`) | n/a | ❌ wiring work |

The ❌ rows are Phase 1 work. The ⚠️ row is Phase 2.

## 4. User-felt problem

A user running Spur today has these blind spots:

- "How much have I spent today across all my agents?" — only answerable by exiting Spur and running `spur cost daily`.
- "Which model is dominating my spend this week?" — same.
- "Is this Codex session burning faster than usual?" — no historical baseline.
- "Why is my Gemini run not showing up in `spur cost`?" — silent omission; user can't tell whether the agent ran at all from the analyst's perspective.
- "Why does the dashboard say I spent $X but `spur cost` says $Y?" — divergent cost sources; user has no resolution mechanism.
- "Why is OpenCode showing $0 cost for sessions I know used Claude?" — provider-prefixed model IDs don't match the pricing registry.

## 5. Design

### 5.1 Architecture overview

```
                       feature: analytics  (default OFF)
                              │
            ┌─────────────────┴─────────────────┐
            │ ON                                │ OFF
            │                                   │
   ┌────────┴────────┐                  ┌──────┴──────┐
   │  Insights View  │                  │ Stub splash │
   │  4 tabs         │                  │ "feature    │
   │  refresh task   │                  │  disabled"  │
   └────────┬────────┘                  └─────────────┘
            │ reads
            ▼
   ┌─────────────────┐
   │   AsyncEngine   │ ◄──── Dashboard cost segment also reads here
   │  (spur-context) │       when feature is ON
   └────────┬────────┘
            │ Arc<Mutex<AnalyticsEngine>> via spawn_blocking
            ▼
   ┌─────────────────┐
   │  AnalyticsEngine│
   │   DuckDB views  │
   │  ┌───────────┐  │
   │  │claude_events│  │
   │  │codex_events │  │
   │  │opencode_events │ ← R1 prefix-strip applied at ingest
   │  │kimi_events  │  │ ← R2 pricing entry added
   │  │gemini_events│  │ ← R4 NEW extractor
   │  │kiro_events  │  │ ← stub (Phase 2)
   │  └─────┬─────┘  │
   │        ▼        │
   │  all_events_with_cost  ← cost_source: native/priced/unpriced
   └────────┬────────┘
            │
            ▼
       agent JSONLs / SQLite / JSON
       (R3: OpenCode mtime check fixed)
```

### 5.2 Crate topology

The snapshot builder lives in **`spur-tui`**, not `spur-context`. Rationale: `daily_90`, `weekly_12`, `monthly_6`, `live_30min` are TUI policy choices. `spur-context` stays consumer-shape-agnostic; its existing parameterized methods are sufficient. (Codex review correctly flagged that putting tab-shape policy into `spur-context` would freeze the consumer's choices into the data crate.)

```
spur-context (existing, harvested)
  ├── No new modules.
  ├── R1: extract_opencode_rows model-prefix strip   (~25 LoC + test)
  ├── R3: newest_agent_mtime gains opencode mtime    (~30 LoC)
  └── R4: NEW src/extractors/gemini.rs               (~150 LoC + tests)
       + src/extractors/mod.rs (new submodule index)
       + engine.rs: discover_gemini_dir, create_gemini_view, status field

spur-cost (existing)
  └── R2: with_builtin_prices() gains kimi-for-coding entry  (~5 LoC)

spur-tui (existing, gains feature `analytics` — default OFF)
  └── views/insights/
      ├── mod.rs                 ~150 LoC: View impl, refresh-handle wiring, Drop
      ├── state.rs               ~100 LoC: InsightsTab/Granularity/Dimension enums; RefreshState
      ├── builder.rs             ~200 LoC: build_snapshot() — single AsyncEngine::run pass
      ├── refresh.rs             ~150 LoC: spawn_refresh_task, tick logic, abort-on-Drop
      ├── tabs/
      │   ├── overview.rs        ~300 LoC: KPIs + sparkline + top-3 lists
      │   ├── timeline.rs        ~300 LoC: BarChart D/W/M
      │   ├── breakdown.rs       ~250 LoC: pivot a/m/p
      │   └── live.rs            ~250 LoC: per-session burn rate
      └── widgets/
          ├── kpi_strip.rs       ~80 LoC: stateless renderer
          └── sparkline.rs       ~50 LoC: thin wrapper over ratatui::Sparkline
```

### 5.3 Feature gating

```toml
# crates/spur-tui/Cargo.toml
[features]
default = ["markdown"]                                 # analytics NOT in default
markdown = [...]                                       # existing
analytics = ["dep:spur-context", "spur-context/duckdb"]

[dependencies]
spur-context = { workspace = true, optional = true }
```

`ViewId::Insights` and `Action::OpenInsights` are **always present** regardless of the feature flag. Rationale: gating an enum variant changes its discriminant size across configurations, breaking exhaustive matches in unpredictable ways. The View *body* is gated:

```rust
// crates/spur-tui/src/views/mod.rs
#[cfg(feature = "analytics")]
pub mod insights;

#[cfg(not(feature = "analytics"))]
pub mod insights {
    pub struct InsightsView;
    impl InsightsView { pub fn new() -> Self { Self } }
    impl super::View for InsightsView { /* render disabled splash */ }
}
```

CI matrix (added to `.github/workflows/ci.yml` or equivalent):
- `cargo check -p spur-tui --no-default-features` — must compile, view stub renders splash
- `cargo check -p spur-tui --features analytics` — full view compiles
- `cargo test -p spur-tui --features analytics` — view + tab tests pass

### 5.4 Concurrency model

The single most important correction from codex review: **all queries run inside one `AsyncEngine::run` closure**. Parallel async query calls would serialize on the inner `Arc<Mutex<AnalyticsEngine>>` AND occupy multiple blocking-pool slots. One closure = one mutex acquisition = one blocking thread.

```rust
// crates/spur-tui/src/views/insights/builder.rs
pub async fn build_snapshot(engine: &AsyncEngine) -> Result<InsightsSnapshot> {
    let queries = engine
        .run(|e| -> Result<AtomicQueries> {
            // R3 ensures OpenCode staleness is detected
            e.refresh_cache()?;
            e.use_cached_events()?;
            Ok(AtomicQueries {
                daily_90: e.daily_report(90)?,
                weekly_12: e.weekly_report(12)?,
                monthly_6: e.monthly_report(6)?,
                by_agent_30d: e.daily_report(30)?,    // grouped by agent in renderer
                by_model_30d: e.model_breakdown()?,
                by_project_30d: e.project_breakdown()?,
                live_30min: e.live_recent_sessions(30)?,
            })
        })
        .await?;
    let kpis = derive_kpis(&queries);
    Ok(InsightsSnapshot {
        fetched_at: Utc::now(),
        queries,
        kpis,
        agent_status: engine.agent_status_snapshot().await?,
        engine_meta: engine.meta_snapshot().await?,
    })
}
```

State:

```rust
pub struct InsightsView {
    engine: AsyncEngine,                                  // Clone via internal Arc
    state: Arc<RwLock<RefreshState>>,                     // tokio::sync::RwLock
    refresh_handle: Option<tokio::task::JoinHandle<()>>,
    active_tab: InsightsTab,                              // enum, not dyn
    granularity: Granularity,                             // Daily | Weekly | Monthly
    dimension: Dimension,                                 // Agent | Model | Project
}

pub struct RefreshState {
    pub last_good: Option<InsightsSnapshot>,              // shown across refresh failures
    pub last_error: Option<Arc<anyhow::Error>>,           // pill if Some
    pub refreshing: bool,                                 // spinner indicator
}
```

Refresh loop:

```rust
async fn refresh_loop(
    engine: AsyncEngine,
    state: Arc<RwLock<RefreshState>>,
    mut signal: mpsc::Receiver<()>,
    is_live_tab: Arc<AtomicBool>,
) {
    loop {
        let interval = if is_live_tab.load(Ordering::Relaxed) {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(60)
        };
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = signal.recv() => {}
        }
        state.write().await.refreshing = true;
        // Wrap in timeout: codex correctly noted that this STOPS WAITING,
        // not "cancels query" — spawn_blocking is uncancellable; we drop the result.
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            build_snapshot(&engine),
        ).await;
        let mut s = state.write().await;
        s.refreshing = false;
        match result {
            Ok(Ok(snap))   => { s.last_good = Some(snap); s.last_error = None; }
            Ok(Err(e))     => { s.last_error = Some(Arc::new(e)); /* keep last_good */ }
            Err(_timeout)  => { s.last_error = Some(Arc::new(anyhow!("refresh timed out (30s)"))); }
        }
    }
}
```

`Drop` aborts the JoinHandle. **Documented invariant:** abort prevents *future* publishing but does NOT cancel an in-flight `spawn_blocking` query — the OS thread runs to completion, and the closure result is discarded when no receiver remains. Tokio docs are explicit on this (https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).

Render path:

```rust
fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
    let guard = match self.state.try_read() {
        Ok(g) => g,
        Err(_) => { /* render previous frame's cached layout */ return; }
    };
    let chunks = layout::vertical(area, &[
        Length(3),    // KPI header
        Length(2),    // tab strip
        Min(0),       // body
        Length(1),    // status pill row
    ]);
    render_kpi_strip(frame, chunks[0], guard.last_good.as_ref().map(|s| &s.kpis));
    render_tab_strip(frame, chunks[1], self.active_tab);
    match (&guard.last_good, self.active_tab) {
        (Some(snap), InsightsTab::Overview)  => OverviewTab::render(frame, chunks[2], snap),
        (Some(snap), InsightsTab::Timeline)  => TimelineTab::render(frame, chunks[2], snap, self.granularity),
        (Some(snap), InsightsTab::Breakdown) => BreakdownTab::render(frame, chunks[2], snap, self.dimension),
        (Some(snap), InsightsTab::Live)      => LiveTab::render(frame, chunks[2], snap),
        (None, _) => render_loading_or_error(frame, chunks[2], &guard),
    }
    render_status_pill(frame, chunks[3], &guard);
}
```

### 5.5 Type design

```rust
// crates/spur-tui/src/views/insights/state.rs
pub struct InsightsSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub queries: AtomicQueries,
    pub kpis: Kpis,
    pub agent_status: AgentViewStatus,           // re-export from spur-context
    pub engine_meta: EngineMeta,
}

pub struct AtomicQueries {
    pub daily_90: Vec<DailyRow>,
    pub weekly_12: Vec<WeeklyRow>,
    pub monthly_6: Vec<MonthlyRow>,
    pub by_agent_30d: Vec<DailyRow>,
    pub by_model_30d: Vec<ModelRow>,
    pub by_project_30d: Vec<ProjectRow>,
    pub live_30min: Vec<LiveBlockRow>,
}

pub struct Kpis {
    pub today_cost: f64,
    pub last_7d_cost: f64,
    pub last_30d_cost: f64,
    pub mtd_cost: f64,
    pub active_session_count: usize,
    pub cache_hit_pct: f64,                      // cache_read / (input + cache_read)
    pub cost_source_split: CostSourceSplit,      // {native_pct, priced_pct, unpriced_pct}
    pub top_agent: Option<(String, f64)>,
    pub top_model: Option<(String, f64)>,
}

pub struct EngineMeta {
    pub events_cache_rows: i64,
    pub last_refresh: DateTime<Utc>,
    pub agent_view_count: usize,                 // how many agents have non-stub data
}

pub enum InsightsTab { Overview, Timeline, Breakdown, Live }
pub enum Granularity { Daily, Weekly, Monthly }
pub enum Dimension { Agent, Model, Project }
```

`Kpis` is eagerly derived because it's shown in the persistent header. Tab views read `&queries` and project their slice on-demand — no per-tab cached state.

### 5.6 Tab specifications

#### 5.6.1 Overview tab

Layout (top to bottom):

```
╭───────── Today ──────────╮ ╭───────── 7d ─────────╮ ╭──── MTD ────╮ ╭── Cache hit ──╮
│ $4.21   ↑3% vs yesterday │ │ $28.40  ▁▂▃▅▇▆▅      │ │ $112.00     │ │ 47.8%         │
╰──────────────────────────╯ ╰──────────────────────╯ ╰─────────────╯ ╰───────────────╯

Cost provenance: ▓▓▓▓▓▓▓ 42% native  ▒▒▒▒▒ 51% priced  ░ 7% unpriced

Top agents (30d)              Top models (30d)              Top projects (30d)
1. claude-code   $89.12       1. claude-opus-4-5  $74.50    1. spur          $41.20
2. codex         $52.40       2. gpt-5-codex      $52.40    2. mermaid-v2    $18.30
3. opencode      $11.05       3. claude-sonnet-4  $14.62    3. (none)        $9.40
```

The 7d KPI sparkline reads `daily_90[60..90]`. Top-3 lists read `by_agent_30d` / `by_model_30d` / `by_project_30d`, sorted descending by cost. Cost-provenance bar reads `kpis.cost_source_split`.

#### 5.6.2 Timeline tab

```
Granularity: [D]aily  Weekly  Monthly       (range: last 90 days)

█▆▇▅▆▇█▇▆▅▇█▇▆█▇█▆█▇█▇▆▇█▆█▇█▇▆▇▇█▇▆▆▇█▇█▇▆█▇█▇█▇█▇▆█▇█▆▇█▇▆█▇▆▇█▇▆█▇█▇█▆█▇▆▇█▇█▇▆▇█▆▇█▇▆▆▇
$    .                                                                                   $12.40
```

`ratatui::widgets::BarChart` with one bar per day (or week/month). User toggles `D`/`W`/`M`. Granularity selection re-derives bar data from `daily_90` / `weekly_12` / `monthly_6` — no re-query.

#### 5.6.3 Breakdown tab

```
Dimension: Agent  [M]odel  Project       (window: last 30 days)

Model                       Sessions   Tokens (in/out)        Cost      Cost source
claude-opus-4-5                  142   8.4M / 920K           $74.50    native
gpt-5-codex                       98   3.2M / 410K           $52.40    priced
claude-sonnet-4-5                 76   2.1M / 180K           $14.62    native+priced
gemini-2.5-pro                    34     1.4M / 95K          $11.20    priced (R4)
kimi-for-coding                   18      720K / 38K          $0.00*   priced (R2)
opencode/...                       9      210K / 22K          $0.00*   unpriced
                                                              ─────
                                                              $152.72

* unpriced rows are surfaced as $0 with a visible "(unpriced)" tag — not silently summed.
```

Pivots `by_agent_30d` (agent dim) / `by_model_30d` (model dim) / `by_project_30d` (project dim). User toggles `A`/`M`/`P`. The "Cost source" column pulls the `cost_source` aggregate from the row's cost-source histogram.

#### 5.6.4 Live tab

```
Active sessions (last 30 min)              refresh: 5s          [via analytics]

session_id              agent          model               tokens     burn $/min   $/hr proj
abc123                  claude-code    claude-opus-4-5     34.2K          $0.41    $24.60
def456                  codex          gpt-5-codex         12.8K          $0.18    $10.80
ghi789                  gemini         gemini-2.5-pro       8.1K          $0.09    $5.40
                                                                          ─────
                                                                          $40.80

Idle agents: opencode (last seen 12 min ago), kimi (last seen 47 min ago)
```

Reads `live_30min`. Per-session row is a small horizontal `ratatui::widgets::Gauge` (not shown above) for tokens-per-minute relative to the session's running average. Refresh interval is 5s while this tab is active (60s otherwise).

### 5.7 Substrate repairs (Phase 1 internal scope)

#### R1 — OpenCode model-prefix strip

**File:** `crates/spur-context/src/engine.rs::extract_opencode_rows` around line 714.
**Change:** introduce `fn strip_provider_prefix(s: &str) -> &str { s.split_once('/').map_or(s, |(_, rest)| rest) }`. Apply at extraction time so `anthropic/claude-opus-4-5` is stored as `claude-opus-4-5`.
**Test:** unit test on the helper covering `anthropic/x`, `google/x`, `z-ai/x`, `moonshotai/x`, `nested/path/here` (only the first `/` strips), and unprefixed `claude-x`.
**Expected impact:** OpenCode-via-Anthropic rows shift from `cost_source='unpriced'` to `cost_source='priced'`. No schema change; no breaking API.

#### R2 — Kimi pricing entry

**File:** `crates/spur-cost/src/pricing.rs::with_builtin_prices()` around line 84-246.
**Change:** add `ModelPricing { model: "kimi-for-coding".into(), input: ?, output: ?, cache_read: 0.0, cache_create: 0.0 }`. Pricing values must be researched from Moonshot AI primary sources before merge; if unavailable at merge time, the entry registers as `0.0` with a `// TODO: confirm from primary source` comment so `cost_source='priced'` displays $0 correctly rather than `cost_source='unpriced'`.
**Test:** existing pricing tests cover model lookup; add one assertion that `pricing.get("kimi-for-coding").is_some()`.
**Expected impact:** Kimi rows surface with `cost_source='priced'` (or 'unpriced' if values are 0.0 placeholder).

#### R3 — OpenCode SQLite mtime in `newest_agent_mtime`

**File:** `crates/spur-context/src/engine.rs:204-226`.
**Change:** the function currently iterates `[claude_dir, codex_dir, kiro_dir, kimi_dir]` (all directories). After R4 also adds `gemini_dir`. OpenCode's `~/.local/share/opencode/opencode.db` is a single FILE. Add a separate branch:

```rust
let opencode_db = Self::discover_opencode_db();
if opencode_db.is_file() {
    if let Ok(meta) = std::fs::metadata(&opencode_db) {
        if let Ok(m) = meta.modified() {
            bump(m);
        }
    }
}
```

**Test:** unit test creates a tempdir with a fake opencode.db, writes initial content, calls `newest_agent_mtime()`, then bumps the file's mtime via `filetime::set_file_mtime`, calls again, and asserts the second call returned a strictly-greater timestamp.
**Expected impact:** OpenCode-only users (or any user with any OpenCode activity) get cache invalidation on OpenCode DB changes.

#### R4 — Gemini JSON extractor

**File (new):** `crates/spur-context/src/extractors/gemini.rs`.
**File (new):** `crates/spur-context/src/extractors/mod.rs` (submodule index — first time spur-context has `extractors/`).
**File (modified):** `crates/spur-context/src/engine.rs` — add `discover_gemini_dir`, `create_gemini_view`, `AgentViewStatus.gemini` field, append to `create_agent_views()` and `rebuild_unified_views()`.
**Implementation:** Execute the existing 1 405-line plan (`docs/superpowers/plans/2026-04-24-onboard-kimi-gemini-opencode-cost.md` Step 3) with these substantive choices:
  - Discovery: `$GEMINI_HOME/tmp` (env override) → `~/.gemini/tmp` (default); recursive walk for `chats/session-*.json` files.
  - Extension is `.json` (not `.jsonl`) — needs a new `find_files_with_ext(dir, ext)` helper alongside the existing `find_jsonl_files`.
  - Schema: top-level `{ sessionId, projectHash, startTime, lastUpdated, messages[], kind }`. Iterate `messages` where `type == "gemini"`. Each message has `id`, `timestamp` (ISO-8601), `content`, `model`, `tokens { input, output, cached, thoughts, tool, total }`.
  - Token folding: `input_tokens = tokens.input + tokens.tool` (tool tokens are model-context input). `output_tokens = tokens.output + tokens.thoughts` (thinking tokens bill at output rate per Google pricing). `cache_read_tokens = tokens.cached`. `cache_creation_tokens = 0` (Gemini doesn't expose cache creation separately).
  - `cost_usd` = `None` (no per-message cost in transcript; rely on `PricingRegistry` match against `model`).
  - `project` field = `projectHash` from top-level.
  - `session_id` = top-level `sessionId`.
**Tests (3):**
  - Synthetic 4-message session with mixed token shapes; assert exact tokens produced.
  - Multi-file scan against a tempdir with two sessions; assert dedup by sessionId and message order.
  - `#[ignore]` smoke test against real `~/.gemini/tmp` if directory exists; assert `n > 0`.
**Expected impact:** Gemini sessions appear in all reports with token-accurate breakdowns. `cost_source='priced'` because `gemini-2.5-pro` and `gemini-2.5-flash` are already in the registry.

### 5.8 Dashboard cost-source switch (Phase 1 critical)

**File:** `crates/spur-tui/src/views/dashboard.rs:504` and any other call site that reads cost from `ExecutorLineage`.
**Constraint:** `View::render` is sync; `AsyncEngine::run` is async (`async_engine.rs:60`). Render cannot block on a query, so we use a shared periodic-refresh cache, mirroring the InsightsView pattern.

**App-owned shared cache** (only constructed when `analytics` is on):

```rust
// crates/spur-tui/src/app.rs (additions, all #[cfg(feature = "analytics")])
pub struct LiveCostCache {
    pub by_session: HashMap<SessionId, f64>,                  // session_id → live cost_usd
    pub last_refresh: DateTime<Utc>,
    pub last_error: Option<Arc<anyhow::Error>>,
}

// App owns:
analytics_engine: Option<AsyncEngine>,
live_cost_cache: Option<Arc<RwLock<LiveCostCache>>>,
live_cost_refresh_handle: Option<JoinHandle<()>>,
```

A small refresh task (5 s interval when any session is active, 30 s otherwise) polls `engine.run(|e| e.live_session_snapshot(session_id))` for each active session via a single closure that batches all currently-active session IDs:

```rust
let costs = engine.run(|e| -> Result<HashMap<SessionId, f64>> {
    let mut out = HashMap::new();
    for sid in &active_session_ids {
        if let Some(snap) = e.live_session_snapshot(sid)? {
            out.insert(sid.clone(), snap.cost_usd);
        }
    }
    Ok(out)
}).await?;
```

Both `DashboardView` and `InsightsView` get a clone of `Arc<RwLock<LiveCostCache>>` from App.

**DashboardView render-path read:**

```rust
fn current_cost(&self, session_id: &SessionId) -> Option<f64> {
    #[cfg(feature = "analytics")]
    {
        if let Some(cache) = &self.live_cost_cache {
            if let Ok(guard) = cache.try_read() {
                if let Some(c) = guard.by_session.get(session_id) {
                    return Some(*c);
                }
                // analytics on but session not yet in cache → fall through to lineage as bridge
            }
        }
    }
    self.lineage.current_attempt().map(|a| a.cost_usd)
}
```

The fall-through to lineage during the "analytics on, cache not yet warm" window prevents cost from blanking on first frame; once the refresh task populates the cache, subsequent renders read the analyst path.

A single "via analytics" pill on the status bar makes the active source visible whenever the feature is on AND the cache has data for the displayed session.

### 5.9 ViewId / Action wiring

```rust
// crates/spur-tui/src/action.rs
pub enum ViewId {
    Dashboard,
    SessionDetail,
    /* ... existing ... */
    Insights,                      // NEW (always present)
}

pub enum Action {
    /* ... existing ... */
    OpenInsights,                  // NEW
}
```

Three exhaustive-match update sites:

| Site | What |
|---|---|
| `crates/spur-tui/src/app.rs:940` (route action → view) | new arm: `Action::OpenInsights => self.push_view(ViewId::Insights)` |
| `crates/spur-tui/src/app.rs:1536` (view dispatch on key) | new arm: `ViewId::Insights => self.insights_view.handle_key(key)` |
| `crates/spur-tui/src/components/status_bar.rs:195` (label per view) | new arm: `ViewId::Insights => "Insights"` |

`Alt+a` global keybinding emits `Action::OpenInsights`. `Esc` from the Insights view returns to the previous view via the existing view-stack pattern.

### 5.10 Refresh policy

| Trigger | Effect |
|---|---|
| View mounted | Spawn refresh task with initial signal |
| Active tab is Live | 5s tick |
| Active tab is anything else | 60s tick |
| `r` key pressed | Send `()` on signal channel — out-of-band refresh |
| `SpurEvent::DelegationCompleted` (cross-cutting) | Send `()` on signal channel |
| View dropped | `JoinHandle::abort()` — future publishes stopped; in-flight blocking task runs to completion and result is dropped |

### 5.11 Error and "no data" surfacing

- A failing `build_snapshot` keeps `last_good` rendered with a red-tinted error pill at the bottom showing `format!("{:#}", err)`.
- Per-agent "no data collected" badge surfaces when `AgentViewStatus.<agent>` is false. Kiro is the only Phase 1 case.
- `cost_source='unpriced'` rows render with a yellow `(unpriced)` tag in any cell where they appear. Total-row cost sums show only priced + native subtotals; unpriced excluded from sum (with a `+ $? unpriced` annotation).

## 6. Phase 2 (deferred, tracked, not in spec)

- **Forecast tab** — MTD projection, anomaly z-score over rolling window, cache-efficiency view per agent/model.
- **Kiro ACP UsageUpdate capture** — orchestrator hook writes `~/.spur/acp_usage_events.jsonl` from every UsageUpdate; spur-context adds `create_acp_usage_view()`. Unblocks Kiro AND any future ACP-only agent.
- **R5 — orchestrator `end_session_with_tokens`** — invisible to this spec's view (we read DuckDB) but useful for keeping the legacy SQLite tracker accurate as an audit trail; cleanup not blocker.
- **Stash@{7} agent-name normalization** — separate task; revives the `claude-code-acp → claude-code` rename against current M10 main.
- **`spur cost-insights --json`** — promote the in-TUI builder to a CLI command for scripting; needs the builder to move from `spur-tui` to a shared crate (or extracted into `spur-context::insights` as previously discussed).
- **Promote `analytics` to default ON** — once the experimental phase validates the unified cost-source approach, retire lineage-cost on the dashboard surface.

## 7. Test strategy

| Layer | Test | Tooling |
|---|---|---|
| R1 helper | `strip_provider_prefix("anthropic/x") == "x"` | unit test in engine.rs |
| R2 pricing | `pricing.get("kimi-for-coding").is_some()` | existing pricing tests |
| R3 mtime | tempdir + filetime crate; bump mtime, assert detected | unit test in engine.rs |
| R4 Gemini | synthetic session JSON → expected `Vec<ExtractedRow>`; multi-file dedup | unit test in extractors/gemini.rs |
| R4 Gemini smoke | `#[ignore]` test against real `~/.gemini/tmp` | dev-machine smoke |
| AtomicQueries → Kpis | pure-function derivation with hand-built rows | unit test in state.rs |
| Tab rendering | `ratatui::backend::TestBackend` + `Buffer::diff` snapshot comparisons | per tab |
| Refresh task | tempdir with synthetic JSONL + AsyncEngine; assert RefreshState transitions | integration test in refresh.rs |
| Dashboard cost source | with/without feature flag, assert correct source called | integration test on dashboard.rs |

Unit tests for tab rendering and KPI derivation use hand-built `AtomicQueries` — no DuckDB required, because the row DTOs in `spur-context::engine` are NOT gated by the `duckdb` feature (`engine.rs:1462+` confirmed unconditional).

## 8. Migration & rollout

- **Default off.** No user sees a behavior change on `cargo build` after this lands.
- **Opt-in via `cargo build --features analytics`.** Users with DuckDB tooling installed (or willing to wait for the bundled-libduckdb build) can flip the flag.
- **No database migration.** spur-context's `events_cache` schema is unchanged; `cost_source` is computed at query time.
- **No config-file change.** No new TOML keys.
- **Forward compatibility:** the harvested `SessionRow.models: Option<String>` (P0.8) is the only breaking field rename in this work; spur-cli has been verified to compile against it.

## 9. Risks & mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Cold-start refresh latency on large histories (5-10s) | high | medium | `RefreshState.refreshing` flag drives spinner; `last_good` keeps showing previous frame |
| `spawn_blocking` thread accumulation if user opens/closes view rapidly | low | low | 30s timeout caps per-task lifetime; tokio default blocking pool is 512 threads |
| Pricing miss for new model (not in registry) | high | low | `cost_source='unpriced'` tag is visible; total excludes unpriced from sum |
| Dashboard cost-source switch surprises users on first feature flip | medium | medium | "via analytics" pill makes lens visible; documented in CHANGELOG/README |
| R4 Gemini schema drift (Gemini CLI changes JSON layout) | low | medium | Extractor uses `serde(default)` + lenient `Option` fields; missing fields fall back to 0 not error |
| OpenCode upgrade breaks SQLite schema (Drizzle migration) | low | medium | spur-context already uses defensive `column_names_for_table` introspection (`engine.rs::OpenCode` block) |
| Concurrent refresh task + dashboard analytics query → mutex contention | medium | low | Both go through `AsyncEngine::run`; mutex queue is fair; render path uses `try_read` to never block |
| Codex `project = NULL` pollutes "by Project" rollup | medium | low | Renderer collapses NULL to "(none)" bucket and labels it visibly |

## 10. Open questions for plan author

1. Should the "via analytics" pill colorize differently per source (native green / priced blue / unpriced yellow), or stay neutral with text only?
2. Should `r` for manual refresh be a documented keybinding or hidden? (Lean documented.)
3. Phase 1 hardcodes Daily-90 / Weekly-12 / Monthly-6 windows — surface as a config option or keep fixed?
4. When Insights view is open and the user runs `spur cost daily` from another terminal, both compete for DuckDB. Acceptable for Phase 1 (both readers, no writes); revisit if locking becomes an issue.
5. Should the dashboard cost-source switch ALSO surface `cost_source` provenance (e.g., a tiny `(priced)` after the dollar amount)? Currently planned: no, keep dashboard minimal. Open for review.

---

**Phase 1 deliverable summary** — 8 work items:

1. R1 OpenCode model-prefix strip (~25 LoC + test)
2. R2 Kimi pricing entry (~5 LoC)
3. R3 OpenCode SQLite mtime in `newest_agent_mtime` (~30 LoC + test)
4. R4 Gemini JSON extractor (~150 LoC + 3 tests)
5. `views/insights/` tree behind `analytics` feature, default OFF (~1 580 LoC across 10 files)
6. Dashboard cost-source switch under same flag (~50 LoC)
7. ViewId/Action additions + 3 match-site updates (~10 LoC)
8. CI matrix: `--no-default-features` and `--features analytics` (~10 lines YAML)

Total estimated net-new: ~1 860 LoC + tests.
Estimated test build time additional: negligible (`cargo test --features analytics` adds duckdb compile to test binary).
Estimated PR size: medium-large; recommended split into 3 PRs: (a) R1+R2+R3 substrate fixes, (b) R4 Gemini extractor, (c) Insights view + dashboard switch.
