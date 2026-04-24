# L9 Rust + Data-Engineering Review — `spur-cost` & `spur-context`

- **Date:** 2026-04-24
- **Reviewer:** L9 Rust / data-engineering staff review
- **Scope:** `crates/spur-cost` (~3.0k LOC) and `crates/spur-context` (~2.4k LOC), working-tree state (not yet committed in part)
- **Method:** full file read; cross-reference between Rust ingestors and DuckDB SQL convert views; MCTS-style challenge rounds; empirical build verification
- **Out of scope:** new spec, implementation plan. This is a findings-only review.

## Verdict (TL;DR)

- `spur-cost`'s Rust ingestors (`ingest/claude.rs`, `ingest/codex.rs`) are correct against the real agent JSONL shapes, and its `pricing.rs` tiered-pricing math is right in the common case. But the crate carries one critical determinism bug, one high-severity silent cost under-report, and the unresolved two-path problem flagged in every prior architectural review.
- `spur-context`'s Rust wiring (DuckDB connection, async wrapper, report types) is well-structured, but the SQL convert views in `engine.rs` deserialize a schema that **Claude Code and Codex do not actually write**. The crate's test suite passes only because fixtures are hand-written in the view's invented schema. Pointed at real `~/.config/claude/projects/**/*.jsonl` or `~/.codex/sessions/**/*.jsonl`, the analytics engine returns empty or NULL-valued rows.
- `cargo test -p spur-context` fails to link on a default developer machine (`ld: library 'duckdb' not found`); the system-`libduckdb` dependency is undocumented in `Cargo.toml`.
- The two crates each reimplement daily/weekly/monthly/live/session reports end-to-end (`spur-cost::reporter` reads JSONL, `spur-context::reporter` queries DuckDB views); they share no ground truth and can report different numbers for the same window.

Ship-blocking: **C1, C2, C3**. Everything else should be fixed before feature work expands against these crates.

---

## Critical (ship-blocking)

### C1 — `spur-context` Claude SQL convert view reads a phantom schema

**Files:** `crates/spur-context/src/engine.rs:180-205`

The Claude convert view projects:

```sql
timestamp::TIMESTAMP     AS timestamp,
sessionId                AS session_id,
model                    AS model,
project                  AS project,
tokenUsage.inputTokens   AS input_tokens,
tokenUsage.outputTokens  AS output_tokens,
tokenUsage.cacheReadTokens      AS cache_read_tokens,
tokenUsage.cacheCreationTokens  AS cache_creation_tokens,
costUSD                  AS cost_usd
FROM read_json_auto('.../**/*.jsonl', ignore_errors = true)
```

Cross-reference the Rust ingestor, which is the ground-truth schema (`crates/spur-cost/src/ingest/claude.rs:57-92`):

```rust
struct ClaudeUsageEntry {
    #[serde(rename = "sessionId")] session_id: Option<String>,
    timestamp: String,
    message: ClaudeMessage,          // NESTED
    #[serde(rename = "costUSD")] cost_usd: Option<f64>,
}
struct ClaudeMessage { usage: ClaudeUsage, model: Option<String> }
struct ClaudeUsage {
    input_tokens: u64,                    // snake_case
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}
```

**Five of ten columns are wrong:**

| View expects | Reality |
|---|---|
| `tokenUsage.inputTokens` | `message.usage.input_tokens` |
| `tokenUsage.outputTokens` | `message.usage.output_tokens` |
| `tokenUsage.cacheReadTokens` | `message.usage.cache_read_input_tokens` |
| `tokenUsage.cacheCreationTokens` | `message.usage.cache_creation_input_tokens` |
| `model` (top-level) | `message.model` |
| `project` (top-level) | not in JSONL; path-derived in Rust |

**Failure mode.** With `ignore_errors=true`, DuckDB silently emits NULL for unresolved field paths. `SUM(NULL)` is NULL. `rusqlite`/`duckdb-rs` binding of NULL into `i64` in `DailyRow` will either error at row-level or produce empty output. User-visible: "I have 2 GB of Claude logs and `daily_report()` returns `[]`."

**Why tests don't catch it.** `test_claude_events_from_fixture` (`engine.rs:987-1042`) writes hand-built JSONL in the *view's* schema:

```json
{"timestamp":"...","sessionId":"sess-1","costUSD":0.05,
 "tokenUsage":{"inputTokens":1000,...},"model":"...","project":"spur"}
```

…which no real Claude Code build emits. Tests validate the view against itself.

**Empirical support.** Repo grep finds no real Claude JSONL fixture; only `.beads/issues.jsonl` exists. The view has never been exercised against a real sample.

**Remediation shape.** Either (a) rewrite the view to traverse the real nested structure (`message.usage.input_tokens`, etc., plus path-based project extraction via `regexp_extract(filename, 'projects/([^/]+)', 1)`), or (b) adopt the `spur-analytics-duckdb-refined.md` plan: have `ClaudeIngestor` write normalized JSONL to a Spur-owned directory and point the view at the stable schema.

---

### C2 — `spur-context` Codex SQL convert view reads a phantom schema

**Files:** `crates/spur-context/src/engine.rs:207-255`

The view expects each JSONL line to have `timestamp`, `session_id`, `model`, and nested `last_token_usage` / `total_token_usage` objects at the top level.

Real Codex schema per the Rust ingestor (`crates/spur-cost/src/ingest/codex.rs:14-40, 147-254`) is a stream of typed events:

```json
{"type":"turn_context","timestamp":"...","payload":{"model":"gpt-5"}}
{"type":"event_msg","timestamp":"...",
 "payload":{"type":"token_count","info":{
    "last_token_usage":{"input_tokens":...,"cached_input_tokens":...},
    "total_token_usage":{"input_tokens":...,"cached_input_tokens":...},
    "model":"gpt-5"}}}
```

Model is carried on `turn_context` events; token totals live under `payload.info.*_token_usage`; `session_id` is derived from the *filename* by the Rust ingestor, not present in the JSONL. The SQL view references none of these paths correctly.

**Failure mode.** `read_json_auto` infers a `type VARCHAR, payload STRUCT(...)` schema; the view's `session_id`, `last_token_usage.input_tokens`, etc. all resolve to NULL. The LAG-based delta logic (lines 227-231) runs over NULLs, producing an empty result set.

**Why tests don't catch it.** `test_codex_events_delta_logic` (`engine.rs:1044-1121`) again writes fixtures in the view's imagined flat schema, not real Codex output. The passing delta math in the test reflects SQL correctness against a schema the world does not produce.

**Remediation shape.** Same two options as C1. Real schema work is non-trivial: the view must `WHERE type='event_msg' AND payload.type='token_count'`, navigate `payload.info.last_token_usage.*`, carry `current_model` across rows via a window function (the turn_context event), and derive `session_id` from `regexp_extract(filename, '([^/]+)\.jsonl$', 1)`.

---

### C3 — `PricingRegistry::get()` substring fallback is nondeterministic and fires on blank model strings

**Files:** `crates/spur-cost/src/pricing.rs:220-243`

```rust
pub fn get(&self, model: &str) -> Option<&ModelPricing> {
    let key = model.to_lowercase();
    if let Some(p) = self.models.get(&key) { return Some(p); }
    if let Some(canonical) = self.aliases.get(&key) {
        if let Some(p) = self.models.get(canonical) { return Some(p); }
    }
    // Substring match (fuzzy)
    for (k, v) in &self.models {
        if k.contains(&key) || key.contains(k) { return Some(v); }
    }
    None
}
```

Two bugs compound:

1. **HashMap iteration order is randomized** (std's default SipHash). The `for (k, v) in &self.models` loop returns the first matching entry, but "first" is random per process. Two runs of the same binary, same registry, same query → potentially different prices.
2. **`String::contains("")` is always true.** If `model` is `""` (entirely plausible: Claude ingestor does `entry.message.model.filter(|m| m != "<synthetic>")` which *keeps* `Some("")`; any malformed row could feed this), the substring loop returns a random entry from the registry.

**Observable consequence.** A Codex row with missing/empty `model` string, or a Claude row where `message.model` is empty, gets assigned random pricing. Aggregated costs drift between runs. The `compute_costs` path in `spur_cost::Reporter::compute_costs` (`reporter.rs:97-114`) is the direct entry point.

**Concrete failure:**
```rust
let reg = PricingRegistry::with_builtin_prices();
let a = reg.get(""); // Some(random_pricing)
let b = reg.get(""); // Same process: same answer.
// Restart the process: potentially different answer.
```

**Remediation.** Remove the substring fallback entirely; return `None` on miss. If fuzzy matching is genuinely wanted, require `key.len() >= 4`, sort candidates deterministically (e.g., by model name ascending, longest-prefix-match preferred), and log at `info` level when the fallback fires. Callers must be prepared to handle `None` and either warn or skip the row rather than silently bill.

---

## High

### H4 — `estimate_cost_from_tokens` silently falls back to the `CostTier` time heuristic on unknown models

**File:** `crates/spur-cost/src/estimator.rs:46-62`

```rust
pub fn estimate_cost_from_tokens(tier, duration, usage, model) -> f64 {
    if let Some(model_name) = model {
        let registry = PricingRegistry::with_builtin_prices();
        if let Some(cost) = calculate_cost_for_model(usage, model_name, &registry) {
            return cost;
        }
    }
    estimate_cost(tier, duration)  // time-based
}
```

The time tier is $0.008/s for "High", $0.003/s for "Medium", $0.001/s for "Low" (`estimator.rs:10-17`). A 1-minute Medium session returns $0.18 regardless of token volume. For a session that actually generated 200k tokens at `claude-sonnet-4` pricing, the real cost is several dollars. Silent massive under-count.

Coupled with C3 and the substring fallback's permissive matching, the fallback path fires less often than it should — but when the registry genuinely misses (e.g., new model slug), the user sees implausibly small costs with no warning.

**Remediation.** Return `Result<f64, Error>` or `Option<f64>` and force the caller to explicitly choose between "use time fallback", "skip", or "error". Emit a `tracing::warn!` when the model is not found.

Also: the function allocates a fresh `PricingRegistry::with_builtin_prices()` on every call. Move to `LazyLock<PricingRegistry>` or accept `&PricingRegistry` from the caller.

---

### H5 — `AnalyticsEngine::query_json` type-erodes every column to String and swallows read errors

**File:** `crates/spur-context/src/engine.rs:808-829`

```rust
let rows = stmt.query_map([], |row| {
    let mut obj = serde_json::Map::new();
    for (i, name) in column_names.iter().enumerate() {
        let value: String = row.get(i).unwrap_or_default();
        obj.insert(name.clone(), serde_json::Value::String(value));
    }
    Ok(serde_json::Value::Object(obj))
})?;
```

Two problems:

1. `row.get::<String>(i)` on a numeric column returns `Err(InvalidColumnType)`; `.unwrap_or_default()` converts that to `""`. A user running `SELECT SUM(cost_usd) FROM ...` gets `{"sum": ""}` with no indication an error occurred.
2. NULL, 0, and type-mismatch errors all render identically as empty string. Downstream JSON consumers (MCP tools) cannot distinguish cases.

**Remediation.** Use `duckdb`'s `ValueRef` API to switch on the SQL type and emit a proper `serde_json::Value::{Number, Null, String, Bool}`. Propagate errors via `?` instead of `unwrap_or_default`. If the JSON shape must remain strings, at minimum distinguish Null (`"null"` or `Value::Null`) from decode error.

---

### H6 — `spur-context` tests are non-hermetic; read the developer's real `~/.config/claude`, `~/.codex`, `~/.kiro`

**File:** `crates/spur-context/src/engine.rs:129-157, 966-974`

`setup_engine()` unconditionally calls `create_agent_views()`, which calls `discover_claude_dir`/`discover_codex_dir`/`discover_kiro_dir`. These resolve to the user's real home directories unless env vars are set. On a developer machine with actual Claude logs, the union view includes those logs during tests. Tests that assert empty-then-populated states become order-dependent and machine-dependent.

The tests that write temp JSONL files then `CREATE OR REPLACE VIEW claude_events AS ...` to override the discovered view do protect themselves — but the engine has already touched the user's data, and any test that doesn't override the view leaks it.

**Remediation.** Extract discovery into a trait or function-parameter:

```rust
pub trait DataDirs { fn claude(&self) -> Option<PathBuf>; ... }
pub fn create_agent_views_with(&self, dirs: &dyn DataDirs) -> Result<...>
```

Tests inject a `TempDir`-backed implementation. Production uses the env-var/home-dir version. Also: set `HOME` and the three agent env vars to `tmp.path()` at the top of each test (cheap, no architecture change) — but that's brittle because Rust runs tests in parallel; env is process-global.

---

### H7 — Two independent data paths, never reconciled (continuation of the prior-review finding)

`CostTracker::start_session` / `end_session_with_tokens` writes to SQLite `sessions` (tokens + cost per session). `CostTracker::daily_report` → `self.reporter()` returns `spur_cost::Reporter`, which reads JSONL via `IngestionPipeline` and *ignores SQLite entirely*. Meanwhile `spur-context::AnalyticsEngine` is a third surface reading JSONL from a different SQL-view path.

Consequences:
- Kiro sessions always report $0: `KiroIngestor::load_file` returns `Ok(vec![])` by design (Kiro tokens flow via ACP `UsageUpdate`, not filesystem). The SQLite path *would* capture these if `end_session_with_tokens` is called from the orchestrator — but `CostTracker::daily_report` doesn't read SQLite.
- Users get wildly different numbers depending on which API they call. `tracker.today_summary()` reads SQLite; `tracker.daily_report()` reads JSONL.
- There is no test that asserts "numbers from SQLite match numbers from JSONL for the same window".

This is the #1 unresolved architectural issue flagged in `cost-principal-review.md`, `cost-duckdb-reassessment.md`, `cost-context-second-order-review.md`, and `spur-analytics-duckdb-refined.md`. The latest code does not move the needle.

**Remediation shape (per prior docs):** Pick one source of truth. Either (A) drop the JSONL path from `CostTracker` public API and route all reporting through SQLite + agent-reported tokens (with Kiro hooked via ACP), or (B) drop `sessions` token columns and make SQLite a metadata-only store; JSONL is the ledger.

---

### H-new — `spur-context` build is undocumentedly environment-dependent

**File:** `crates/spur-context/Cargo.toml`

`cargo test -p spur-context --no-run` fails with `ld: library 'duckdb' not found` on a default macOS dev environment. The `duckdb` crate supports `DUCKDB_DOWNLOAD_LIB=1` to fetch a prebuilt library, but `Cargo.toml` sets no feature, and no `README` or build note inside `spur-context/` signals this. CI presumably has `libduckdb` installed; a new contributor `cargo test`s and gets a linker error.

**Remediation.** Add `features = ["bundled"]` to the `duckdb` dependency (ships libduckdb with the crate) *or* document the `DUCKDB_DOWNLOAD_LIB=1` requirement in a crate-level `README.md` and fail fast in `build.rs`.

---

### H-new — SQLite `init_db` has no concurrency pragmas; default `journal_mode=DELETE`, `synchronous=FULL`

**File:** `crates/spur-cost/src/db.rs:75-197`

No `PRAGMA journal_mode=WAL`, no `synchronous=NORMAL`, no `foreign_keys=ON`, no `busy_timeout`. If more than one process or more than one `Connection` opens `cost.db` (likely: `spur-core` orchestrator + `spur-mcp` tools), concurrent writes hit `SQLITE_BUSY`. No FK integrity between `sessions.id` and `delegation_log.brain_session`/`worker_session`.

**Remediation.**
```rust
conn.execute_batch("
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA foreign_keys = ON;
    PRAGMA busy_timeout = 5000;
")?;
```
Add FK constraints in the `CREATE TABLE` for `delegation_log`. Verify orchestrator doesn't rely on orphaned delegations surviving session deletion.

---

## Medium

### M8 — `session_count` counts events, not sessions

**Files:** `crates/spur-cost/src/reports.rs:62-76, 187-209`

```rust
t.session_count = entries.len() as u64;  // events, not sessions
bd.session_count += 1;                    // same
```

A single Claude session typically writes dozens of JSONL lines. Display of "Sessions: 347" for a day with 5 actual sessions is wildly misleading — users make capacity decisions on this number.

**Remediation.** Rename to `event_count` on both types, or compute real session count via `entries.iter().filter_map(|e| e.session_id.as_ref()).collect::<HashSet<_>>().len()`.

---

### M9 — `LiveSessionTracker::burn_rate` computes cost-per-event, not burn rate

**File:** `crates/spur-context/src/live.rs:94-105`

```rust
self.last_snapshot.as_ref().map(|s| {
    if s.events == 0 { 0.0 }
    else { s.cost_usd / (s.events.max(1) as f64) }
})
```

"Burn rate" means cost per unit time. This returns cost per event. The name and intent disagree; downstream UI that renders "$X/hr" from this will be wrong by whatever the events-per-second rate happens to be.

The `Reporter::live_report` path does compute burn rate correctly (`reporter.rs:267-274` and `spur-context/src/reporter.rs:472-485`), using `cost / minutes * 60`. The tracker's method is the odd one out.

**Remediation.** Either drop `LiveSessionTracker::burn_rate` (the two Reporter paths already compute it), or compute it properly using `last_poll` timestamps on at least two snapshots.

---

### M10 — Alias pricing rows can collide with canonical rows in DuckDB `pricing` PK

**Files:** `crates/spur-context/src/engine.rs:351-378`, `crates/spur-context/src/sql/schema.sql:13-21`

`pricing.model` is `VARCHAR PRIMARY KEY`. `load_pricing` inserts `known_models()` then `aliases()`. Both `models` and `aliases` are lower-cased; if any alias lowercases to the same string as a canonical model (or another alias), the second `INSERT` raises PK violation and `load_pricing` returns `Err`. Today's aliases (`claude-opus` → `claude-opus-4`, `gpt-5.3-codex` → `gpt-5-codex`) don't collide, so the bug hasn't fired. A future alias that collides will silently break pricing load.

**Remediation.** Use `INSERT OR REPLACE`, or check collisions up-front and `tracing::warn!` on duplicate, or extend the schema to a `pricing_alias` side table and `JOIN` in `all_events_with_cost`.

---

### M11 — `AsyncEngine` mutex serializes read-only queries; `unwrap()` on poison

**File:** `crates/spur-context/src/async_engine.rs:65-71`

```rust
tokio::task::spawn_blocking(move || {
    let mut engine = inner.lock().unwrap();  // poison → panic
    f(&mut engine)
})
```

Two issues:
- `inner.lock().unwrap()` violates iron law #1. Poisoned mutex → `spawn_blocking` task panics; the panic propagates as `JoinError`. At least handle poison explicitly or use `parking_lot::Mutex` (never poisons).
- All queries — including read-only ones — serialize through a single `Mutex<AnalyticsEngine>`. DuckDB supports multiple concurrent read connections. Under heavy MCP read load this becomes the bottleneck. A connection pool (even two connections: write + read) would help; or switch to `Arc<AnalyticsEngine>` + take `&AnalyticsEngine` in blocking closures (requires `Connection: Sync`, which DuckDB's C connection is not — so pool is the path).

**Remediation.** Short-term: swap `std::sync::Mutex` for `parking_lot::Mutex` to eliminate the poison surface. Longer-term: open multiple `Connection`s (they share the same DuckDB database file safely in read mode) and rotate via a small pool.

---

### M12 — `Reporter::live_report` rebuilds fake `TokenEvent`s to compute grand totals

**File:** `crates/spur-cost/src/reporter.rs:306-323`

The function computes `blocks`, which already contain per-session token/cost totals, then reconstructs a `Vec<TokenEvent>` from them and calls `Totals::from_entries` on it. This is visible computational duplication and also hides that `session_count` from `Totals::from_entries` will equal `blocks.len()` here (which is actually correct for live report — but contradicts M8's event-counting everywhere else).

**Remediation.** Sum `blocks.iter()` directly into a `Totals` struct; drop the fake-TokenEvent reconstruction.

---

### M13 — `estimate_cost_from_tokens` allocates the entire `PricingRegistry` on every call

**File:** `crates/spur-cost/src/estimator.rs:53`

`PricingRegistry::with_builtin_prices()` inserts ~10 models + aliases on each call. In a batch-report path iterating thousands of events, this is thousands of HashMap allocations.

**Remediation.** `static PRICING: LazyLock<PricingRegistry> = LazyLock::new(PricingRegistry::with_builtin_prices);` or pass `&PricingRegistry` from caller.

---

### M15 — No foreign-key integrity in `cost.db`

Already mentioned under H-new (SQLite pragmas). Separating it because FK constraints require explicit declaration in the `CREATE TABLE delegation_log`:

```sql
brain_session  TEXT NOT NULL REFERENCES sessions(id),
worker_session TEXT NOT NULL REFERENCES sessions(id),
```

Currently a delegation can reference a nonexistent session; `update_delegation_end` happily updates orphaned rows.

---

## Low

### L17 — `find_jsonl_files` / `glob_jsonl_recursive` have no symlink-cycle or depth guard
Both `spur-context/src/engine.rs:166-178` and `spur-cost/src/ingest/mod.rs:197-218`. A pathological symlink loop in the home dir (rare but legal) hangs the process. Add a `max_depth` or visited-inode set.

### L19 — `IngestionPipeline::load_range` filters after `load_all`, wasting I/O
`spur-cost/src/ingest/mod.rs:185-191`. For a narrow window query against a 2 GB history, this re-parses the entire history. Prior reviews flagged this as the O(total_history) bug; no fix in current code. Push `from`/`to` into the per-file filter by checking file mtime before parse, or at least short-circuit lines whose timestamp string is clearly outside the window.

### L20 — `build_session_tree` always returns depth=0; `children` never populated
`spur-cost/src/reports.rs:275-318`. The `SessionNode` type advertises a tree via `children: Vec<SessionNode>`, `depth: u32`, and `total_cost()` recursing children — but the ingestion pipeline never populates `parent_session`, so the "tree" is always a flat forest of roots. Either wire up the SQLite `delegation_log.brain_session → worker_session` relationship (which is the structural join the blocks-visualization doc already depends on) or rename the API to `build_session_forest(...) -> Vec<SessionSummary>` and drop the tree fiction.

### L21 — Claude ingestor silently assigns `Utc::now()` to malformed timestamps
`spur-cost/src/ingest/claude.rs:161-164`. A bad line doesn't drop the row or bubble an error; it relabels the row to "now". Bad data clusters into today's report and inflates current-day cost.

### L24 — Divergent agent-discovery rules between ingestor and engine
Claude ingestor honors `$CLAUDE_CONFIG_DIR` (comma-separated), XDG default, and legacy `~/.claude`. Engine honors only `$CLAUDE_CONFIG_DIR` (single) and XDG. A user with legacy `~/.claude/projects` gets data in `spur-cost` reports but not in `spur-context` reports — another source of divergence.

---

## Architectural observation (not a bug, but it frames the remediation)

Of the seven architecture documents in `docs/spur/` that cover this boundary, the direction converges on: **DuckDB reads normalized JSONL that the Rust ingestors produce**. The current code does the opposite — DuckDB reads raw agent JSONL with SQL that doesn't match real shapes, *and* the Rust ingestors also parse the same raw files for `spur-cost`'s own reporter. Whichever of C1/C2's remediation shapes you pick (rewrite views against real nested schemas vs. write normalized JSONL), picking one is prerequisite to undoing the three-way duplication between `spur_cost::Reporter`, `spur_context::Reporter`, and `CostTracker`'s SQLite query path. Keep all three, and every future bug has to be fixed in three places.

---

## Test-discipline note

The most alarming single observation isn't any individual bug, it's that the test suites assert correctness against fixtures written in the *expected* schema rather than the *real* schema. `spur-context`'s fixture JSONL matches the SQL view; the Rust ingestors' fixture JSONL matches the ingestor. Both suites are green. Neither suite asserts "given a real Claude JSONL line copy-pasted from my home directory, the system produces the right tokens/cost." The repo contains no real sample (`git grep -l jsonl` only finds `.beads/issues.jsonl`).

A single pinned integration-test fixture per agent — one known-good JSONL line copy-pasted from a real session, committed under `crates/spur-cost/tests/fixtures/` — would have caught C1 and C2 on day one. Recommend adding these before the next feature on these crates.

---

## Rust-idiom / iron-law violations (summary)

| File | Line | Violation |
|---|---|---|
| `spur-context/src/async_engine.rs` | 67 | `.unwrap()` on `Mutex::lock()` |
| `spur-context/src/async_engine.rs` | 54 | `.unwrap()` on `Mutex::into_inner()` |
| `spur-context/src/reporter.rs` | 27 | `.unwrap()` on `and_hms_opt(0,0,0)` (benign, pattern) |
| `spur-cost/src/reporter.rs` | 28 | same `.unwrap()` (benign, pattern) |
| `spur-context/src/engine.rs` | 821 | `unwrap_or_default()` swallows `row.get` errors (H5) |
| `spur-cost/src/ingest/claude.rs` | 163-164 | `unwrap_or_else(|_| Utc::now())` (L21) |

None of the `.unwrap()`s are currently reachable from malicious input, but they violate iron law #1 and should be converted to `.expect("reason")` at minimum, or proper error returns.

---

## Suggested ordering of remediation

1. **Decide C1/C2's remediation shape** (rewrite views vs. normalized JSONL) — this unblocks everything else because it changes which code gets touched.
2. **Fix C3** (remove substring fallback in `PricingRegistry::get`). Small change, high correctness gain.
3. **Add pinned real-JSONL integration fixtures** (one Claude sample, one Codex sample, committed under `tests/fixtures/`), wire them into both crates' test suites. This pre-empts future drift.
4. **Fix H4** (surface model-miss as a warning or Result, don't silently time-fallback).
5. **Fix H5** (properly typed JSON emission in `query_json`).
6. **Pick a single source of truth** for H7 and delete the other path's reporter.
7. **H-new: libduckdb bundling** (`features = ["bundled"]`) so `cargo test` works on a default machine.
8. Remaining mediums and lows can land opportunistically.

Items 1-3 are the critical path. Items 4-7 block the next wave of features.
