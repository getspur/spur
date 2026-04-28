# SPUR Profile & Performance Monitoring Spec

## Status: Draft

## 1. Goals

1. **User self-awareness**: Help users understand SPUR's resource footprint and their brain session costs in real time.
2. **Developer observability**: Give SPUR maintainers structured data to diagnose slowness, memory growth, and event-loop backpressure.
3. **Zero network, zero surprise**: All metrics stay local. Nothing is transmitted. The user owns every byte of telemetry.
4. **Near-zero overhead**: Metrics collection must not materially affect the ~9–32 MB RSS and ~2–3% CPU baseline of a SPUR TUI instance.

## 2. Non-Goals

- Cloud telemetry / analytics pipeline
- Prometheus / OpenTelemetry export endpoints
- Automatic crash reporting
- Metrics that require elevated privileges (e.g., `perf_event_open`, `dtrace` at runtime)

## 3. Constraints

| Constraint | Rationale |
|-----------|-----------|
| Local-only | Privacy-first product promise |
| Opt-in disk persistence | No hidden log growth; user controls retention |
| < 0.5% CPU overhead | Must not compete with brain processes for cores |
| < 1 MB RAM overhead | Must not inflate the lightweight TUI footprint |
| macOS + Linux parity | Primary developer platforms |
| No new heavy dependencies | Prefer `sysinfo` (already used in ecosystem) or stdlib |

## 4. Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        SPUR TUI                              │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │  Collector   │  │   Store      │  │   Renderer       │  │
│  │  (1 Hz tick) │→ │  (ring buf)  │→ │  (TUI panels)    │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
│         ↑                           ↓                      │
│    sysinfo::System            ratatui::widgets             │
│    AtomicU64 counters                                        │
└─────────────────────────────────────────────────────────────┘
                              ↓
                    ┌──────────────────┐
                    │   On-Demand      │
                    │   Export (CLI)   │
                    │   JSONL / CSV    │
                    └──────────────────┘
```

### 4.1 Collector

Runs on a dedicated tokio task at **1 Hz** (configurable, default 1s).

**Self-metrics (SPUR process)**
- `spur_cpu_percent` — CPU % of the SPUR TUI process
- `spur_rss_bytes` — Resident set size
- `spur_vsz_bytes` — Virtual memory size
- `spur_fd_count` — Open file descriptors (Unix only)

**Brain-metrics (per connected brain)**
- `brain_cpu_percent` — CPU % per brain PID
- `brain_rss_bytes` — RSS per brain PID
- `brain_uptime_seconds` — Brain process elapsed time

**TUI-metrics (in-process, zero syscalls)**
- `tui_frame_time_ms` — Render loop duration (p50, p99 histogram)
- `tui_event_drain_count` — Events processed per frame
- `tui_async_task_count` — Outstanding tokio tasks (tokio `RuntimeMetrics` if available)
- `tui_memory_growth_bytes` — Delta from startup RSS (approximate leak detector)

**Orchestrator-metrics**
- `acp_message_count` — Messages dispatched since last tick
- `acp_error_count` — Failed dispatches since last tick
- `session_active_count` — Currently running brain sessions
- `session_pending_review_count` — Awaiting user review
- `cost_dollars` — Cumulative USD (already exists in status bar)

### 4.2 Store

An **in-memory ring buffer** per metric series. No disk I/O on the hot path.

```rust
pub struct MetricRing {
    capacity: usize,        // default: 300 samples = 5 minutes at 1 Hz
    samples: Vec<Sample>,   // pre-allocated VecDeque-like circular buffer
    write_idx: usize,
}

pub struct Sample {
    timestamp: DateTime<Utc>,
    value: f64,
}
```

Memory budget: 300 samples × 16 bytes × ~20 series = **~96 KB**.

Optional disk persistence (opt-in via config):
- Append-only JSONL to `.spur/logs/metrics/YYYY-MM-DD.jsonl`
- Rotated daily, max 7 days retention
- Written asynchronously in batches (every 30s or on graceful shutdown)

### 4.3 Renderer

New TUI components:

1. **`ProfilePanel`** — Overlay/modal triggered by `[Alt-p]` or `:profile` command
   - Real-time sparklines for CPU + memory (self + brains)
   - Frame time histogram bar chart
   - Top processes table (sortable by CPU / memory)

2. **`InsightsTab::Performance`** — New tab alongside existing Live / History
   - Session-level resource attribution (which brain cost how much CPU/RAM)
   - Cost-to-resource correlation ($/hr vs. CPU %)

3. **StatusBar enhancement**
   - Optional compact mode: `▲13MB` memory badge when `spur_rss_bytes` exceeds threshold
   - Yellow/red coloring when brain CPU > 50% (helps catch runaway Claude processes)

### 4.4 Exporter

CLI extension to existing `spur profile`:

```bash
# Export last N minutes of metrics to local file
spur profile export --minutes 60 --format jsonl -o ./spur-metrics.jsonl

# Export as CSV for spreadsheet analysis
spur profile export --since "2026-04-29T00:00:00Z" --format csv

# Inspect what's in the local buffer without writing to disk
spur profile inspect --series spur_rss_bytes --last 300
```

## 5. Data Model

```rust
/// Internal metric identifier. No free strings on hot path.
pub enum MetricKey {
    SpurCpuPercent,
    SpurRssBytes,
    SpurVszBytes,
    BrainCpuPercent { pid: u32 },
    BrainRssBytes { pid: u32 },
    TuiFrameTimeMs,
    TuiEventDrainCount,
    AcpMessageCount,
    AcpErrorCount,
    SessionActiveCount,
    CostDollars,
}

/// Snapshot emitted by Collector every tick.
pub struct MetricsSnapshot {
    pub timestamp: DateTime<Utc>,
    pub values: Vec<(MetricKey, f64)>,
}
```

## 6. Component Design

### 6.1 `spur-core::metrics` (new module)

```rust
pub struct MetricsCollector {
    sys: sysinfo::System,
    spur_pid: u32,
    tracked_brains: Vec<u32>,
    ring: MetricStore,
}

impl MetricsCollector {
    pub fn new(capacity: usize) -> Self;
    pub fn track_brain(&mut self, pid: u32);
    pub fn untrack_brain(&mut self, pid: u32);
    pub fn tick(&mut self) -> MetricsSnapshot; // called every 1s
}
```

### 6.2 `spur-tui::components::profile_panel` (new module)

- Uses `ratatui::widgets::Sparkline` for time series
- Uses `ratatui::widgets::BarChart` for histograms
- Receives `MetricsSnapshot` via async channel from `MetricsCollector`

### 6.3 `spur-cli::commands::profile` (extend)

Extend existing `profile.rs`:
- Add `Export` and `Inspect` subcommands
- Reuse `MetricsStore` serialization logic

## 7. Configuration

New optional `[metrics]` section in `SpurConfig`:

```toml
[metrics]
enabled = true              # master switch
tick_interval_seconds = 1   # collector frequency
history_capacity = 300      # samples to keep in memory (~5 min at 1 Hz)

[metrics.persistence]
enabled = false             # opt-in; default off for privacy
directory = ".spur/logs/metrics"
rotation_days = 7

[metrics.alerts]            # user-facing thresholds, not telemetry
rss_threshold_mb = 100      # status bar turns yellow above this
brain_cpu_threshold = 50.0  # status bar turns red above this
```

## 8. Implementation Phases

### Phase 1: Foundation (1–2 days)
- [ ] Add `spur-core/src/metrics/` with `MetricKey`, `MetricRing`, `MetricsCollector`
- [ ] Integrate `sysinfo` crate (with `default-features = false`, multithreading disabled)
- [ ] 1 Hz tokio task in `spur-core` or `spur-cli` startup
- [ ] Wire `track_brain` / `untrack_brain` to brain spawn/kill events

### Phase 2: TUI Visualization (2–3 days)
- [ ] `ProfilePanel` component with sparklines
- [ ] `[Alt-p]` keybinding to toggle overlay
- [ ] StatusBar memory badge + threshold coloring

### Phase 3: Export & Persistence (1–2 days)
- [ ] `spur profile export` CLI subcommand
- [ ] Optional JSONL persistence (opt-in config)
- [ ] `spur profile inspect` for local buffer introspection

### Phase 4: Insights Integration (2–3 days)
- [ ] New `InsightsTab::Performance` with session-level attribution
- [ ] Cost-to-resource correlation view
- [ ] Frame time histogram for render-loop debugging

## 9. Overhead Budget

| Component | CPU | RAM | Notes |
|-----------|-----|-----|-------|
| `sysinfo` targeted refresh (1 Hz) | < 0.1% | ~0 | Single PID refresh, not global scan |
| Atomic counters (TUI metrics) | ~0% | ~200 B | stdlib only |
| Ring buffer (300 × 20 series) | ~0% | ~96 KB | Pre-allocated |
| ProfilePanel render | ~0% | ~0 | Only when visible |
| JSONL persistence | ~0% | ~0 | Async batched write every 30s |
| **Total** | **< 0.5%** | **< 200 KB** | Against SPUR TUI baseline |

## 10. Privacy Checklist

- [ ] No network I/O in metrics pipeline
- [ ] No user content in metrics (no prompts, no file paths, no code)
- [ ] Disk persistence is opt-in, default off
- [ ] All data lives in user's `.spur/` directory
- [ ] Export requires explicit user command
- [ ] Retention is bounded (configurable, default 7 days)

## 11. Dependencies

| Crate | Version | Feature | Purpose |
|-------|---------|---------|---------|
| `sysinfo` | `0.33` | `default-features = false` | Cross-platform process stats |
| `chrono` | workspace | — | Timestamps |

No `metrics`, `opentelemetry`, `prometheus`, or `tokio-console` crates. The `tracing` ecosystem already in SPUR is sufficient for developer debugging; this spec covers user-facing runtime observability only.

## 12. Acceptance Criteria

1. `spur profile monitor --pid <spur_pid>` shows live CPU/memory without SPUR's own CPU increasing by more than 1%.
2. `[Alt-p]` opens a ProfilePanel showing 60s of sparkline history within 100ms.
3. StatusBar memory badge appears when SPUR RSS > 50 MB and turns red when brain CPU > 50%.
4. `spur profile export --minutes 10` produces a valid JSONL file with ≥ 600 samples.
5. No `.spur/logs/metrics/` directory is created unless user explicitly sets `metrics.persistence.enabled = true`.
