# `spur cost` Self-Test Verdict — Empirical Grounding of the L9 Review

- **Date:** 2026-04-24
- **Reviewer:** L9 Rust / data-engineering staff review (self-test of code-level review 2026-04-24-spur-cost-spur-context-l9-review)
- **Mode:** Empirical. Built the CLI, ran real commands, probed the cost.db and real agent JSONL, executed the fix-branch DuckDB views against 1.8 GB of real Claude data.
- **Branch under test:** `fix/l9-review-criticals` (C3, C1, C2 landed)

## TL;DR

- **C3 (PricingRegistry fix): real correctness win. Merge.**
- **C1 and C2 (SQL view rewrites): tests are green but the views DO NOT WORK against real Claude/Codex JSONL.** DuckDB's `read_json_auto` schema inference against heterogeneous event streams fails before the projection ever runs. The previous review + kimi review + test suite all missed this class of failure because every fixture was homogeneous.
- **`spur cost` is a cosmetic report today.** 489 SQLite sessions, 0 with tokens, 0 with model, 0 with project. Every number shown is a `duration × time-tier` fiction. Real cost on 1.8 GB of Claude JSONL is likely 1-2 orders of magnitude higher than what the CLI shows.
- **Nothing in the `spur` binary actually reaches the `spur-context` DuckDB engine.** C1/C2 affect only `cargo test`.

Recommendation: **merge C3. Hold C1 and C2** pending (a) a real-JSONL integration fixture and (b) a schema-inference-robust rewrite OR a decision to switch to Shape B (normalized JSONL).

---

## Method

Self-test protocol:

1. Build `./target/debug/spur` from `fix/l9-review-criticals`.
2. Run `spur cost`, `spur cost --week`, `spur cost --by project`, `spur cost --export csv`.
3. Dump `~/.spur/cost.db`: row counts, distinct columns, NULL rates.
4. Count real agent JSONL on disk.
5. Run the fix-branch Claude and Codex SQL views as-written against real `~/.claude/projects/**/*.jsonl`. Capture DuckDB's actual error.
6. Try `union_by_name=true` and `sample_size=-1` to see if DuckDB's schema inference can recover.
7. Inspect real Claude Code JSONL directly (jq) to determine what fields actually exist.
8. Cross-check findings against prior review doc.

Outputs captured verbatim below.

---

## Empirical observations

### 1. `spur cost` output

```
$ ./target/debug/spur cost
Agent                 Cost Sessions   Duration
-----------------------------------------------
codex           $    14.67       40      244m
claude-code     $     0.06        5        0m
claude-code-sj  $     0.00        1        0m
-----------------------------------------------
Total           $    14.73

$ ./target/debug/spur cost --week
claude-code-acp $   159.56      122      886m
codex           $    88.65       88     1477m
codex-acp       $    47.96       22      799m
kimi            $     0.53        7        8m
claude-code     $     0.06        5        0m
opencode-acp    $     0.00        1        0m
gemini          $     0.00        9        0m
claude-code-sj  $     0.00        1        0m
-----------------------------------------------
Total           $   296.75

$ ./target/debug/spur cost --by project
Agent                 Cost Sessions   Duration
-----------------------------------------------
codex           $    14.67       40      244m
...
Total           $    14.73

By project:
  (unassigned): $297.01 (489 sessions)          ← scope mismatch: today's table + all-time breakdown
```

Observations: 11 lines of agent-registry INFO/WARN logs precede every output (noise). `--by project` prints today's rollup then all-time project totals in the same block with no scope label.

### 2. `cost.db` inventory

```sql
SELECT COUNT(*) AS total,
       SUM(CASE WHEN input_tokens IS NOT NULL THEN 1 ELSE 0 END) AS with_tokens,
       SUM(CASE WHEN model IS NOT NULL THEN 1 ELSE 0 END) AS with_model,
       SUM(CASE WHEN project IS NOT NULL THEN 1 ELSE 0 END) AS with_project,
       SUM(CASE WHEN estimated_cost_usd > 0 THEN 1 ELSE 0 END) AS with_cost
FROM sessions;
→ 489 | 0 | 0 | 0 | 69
```

- 489 total sessions
- **0 with token counts**
- **0 with a model string**
- **0 with a project**
- Only 69/489 have a non-zero cost

Per-agent cost numbers are `duration_seconds × CostTier rate`:

| Agent            | Sessions | Input tokens | Output tokens | Estimated cost |
|------------------|----------|--------------|---------------|----------------|
| claude-code-acp  | 178      | 0            | 0             | $159.56        |
| codex            | 96       | 0            | 0             | $88.65         |
| codex-acp        | 27       | 0            | 0             | $47.97         |
| kiro             | 151      | 0            | 0             | $0.24          |
| claude-code      | 18       | 0            | 0             | $0.06          |

$159.56 / 886 min ≈ **$10.8/hr**, matching `CostTier::Medium` (0.003 $/s). This is a time-based fiction, not a token-based bill.

### 3. Real agent JSONL on disk (not reached by any CLI)

```
~/.claude/projects/    → 4110 jsonl files, 1.8 GB  (legacy path)
~/.codex/sessions/     →  220 jsonl files, 160 MB
~/.kiro/               →  (various; ACP protocol logs, no tokens)
```

The user has ~1.8 GB of actual Claude Code usage. The CLI's cost report references zero of it.

### 4. `spur-context` engine against real data — the damning test

Ran the C1-rewritten view literally:

```sql
CREATE OR REPLACE VIEW claude_events AS
SELECT
    timestamp::TIMESTAMP AS timestamp,
    sessionId AS session_id,
    'claude' AS agent,
    NULLIF(message.model, '<synthetic>') AS model,
    NULLIF(regexp_extract(filename, '.*/projects/([^/]+)/.*[.]jsonl$', 1), '') AS project,
    message.usage.input_tokens AS input_tokens,
    message.usage.output_tokens AS output_tokens,
    message.usage.cache_read_input_tokens AS cache_read_tokens,
    message.usage.cache_creation_input_tokens AS cache_creation_tokens,
    costUSD AS cost_usd
FROM read_json_auto('/Users/kevintruong/.claude/projects/**/*.jsonl',
                    filename = true, ignore_errors = true);
```

Actual result:

```
Binder Error: Referenced column "costUSD" not found in FROM clause!
Candidate bindings: "toolUseID", "content", "isCompactSummary",
                    "compactMetadata", "isSnapshotUpdate"
```

Adding `union_by_name = true` widens the candidates (now includes `cause`, `prRepository`, etc.) but still misses `costUSD`, `message`, `message.usage.*`. Setting `sample_size = -1` does not help either. **DuckDB's JSON schema inference, applied to the real heterogeneous Claude JSONL, never surfaces the usage/cost fields.**

### 5. Why: real Claude JSONL is heterogeneous

`head -100 <real-file> | jq -r .type | sort -u`:

| Type             | Count | Carries usage? |
|------------------|-------|----------------|
| `user`           | 5     | no             |
| `assistant`      | 2     | YES (`.message.usage.*`) |
| `attachment`     | 4     | no             |
| `queue-operation`| 2     | no             |
| `last-prompt`    | 1     | no             |

- `message.usage.*` only on `type="assistant"` rows.
- **`costUSD` is not a top-level field in any row.** `find ~/.claude/projects -name '*.jsonl' -exec grep -l costUSD {} \;` hits 3/4110 files, almost certainly inside text bodies, not as a key.
- Current Claude Code version writes richer usage: `service_tier`, `cache_creation.{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}`, `server_tool_use`, `inference_geo` — none modeled by the ingestor or view.

The Rust ingestor tolerates missing `costUSD` because serde returns `None` and the field is `Option<f64>`. DuckDB's bound column reference cannot tolerate it.

### 6. What `spur cost` actually executes

Reading `crates/spur-cli/src/main.rs:411-464`:

- `spur cost` → `CostTracker::today_summary()` / `week_summary()` / `by_project()`.
- All three hit the SQLite path in `spur-cost`. None touch `spur-context` or any JSONL ingestor.
- **The CLI has no subcommand for the DuckDB analytics engine at all.**

So the "critical fixes" I landed in C1/C2 affect only one thing today: `cargo test -p spur-context`. No end user will see a difference.

---

## New findings (additive to the earlier review)

### NEW-CRIT-1 — C1 and C2 fixes are empirically non-functional against real data

The SQL is syntactically correct against an IDEAL schema. DuckDB's `read_json_auto` applied to heterogeneous Claude Code / Codex JSONL produces a narrower inferred schema that doesn't contain the projected columns; CREATE VIEW fails before any row is read. The earlier review + the kimi review + the passing cargo tests all missed this because every test fixture was homogeneous.

Remediation options:
  (a) **Explicit column schema**: pass `columns = {sessionId:'VARCHAR', timestamp:'VARCHAR', message:'JSON', costUSD:'DOUBLE', ...}` to `read_json_auto`. Drift-proof but verbose.
  (b) **Raw JSON text + `json_extract`**: read JSONL as `{text: VARCHAR}`, navigate via `json_extract(text, '$.message.usage.input_tokens')::BIGINT`. Robust to unseen fields and heterogeneity. Slower per-row but correct.
  (c) **Filter at parse via `WHERE type='assistant'`** requires `type` in the inferred schema first — circular.
  (d) **Shape B from the spec**: Rust ingestor writes a normalized JSONL with a fixed schema; DuckDB reads that. This was the original recommendation in `docs/spur/spur-analytics-duckdb-refined.md`. The empirical data argues strongly for Shape B now.

Recommendation: commit a pinned real Claude JSONL fixture (one session file from the user's machine, anonymized) to `crates/spur-context/tests/fixtures/`, write an integration test that CREATE VIEWs + counts rows, and only then choose between (a)/(b)/(d).

### NEW-CRIT-2 — `spur cost` CLI reaches zero of the new analytics engine

No subcommand routes to `spur-context`. C1/C2 have no user-visible impact today. Before investing more in Shape A/B/C of the view, wire a CLI path (e.g. `spur cost --engine duckdb` or `spur context report daily`) that actually exercises the engine, otherwise the fix is unfalsifiable by users.

### NEW-HIGH-1 — Orchestrator does not populate token data

`489/489` sessions have NULL input/output/cache tokens, NULL model, NULL project. The `end_session_with_tokens` path exists (`CostTracker::end_session_with_tokens`, wired to `db::update_session_end_with_tokens`) but no caller is exercising it. Without token data, C3's pricing-registry fix is latent; every SQLite cost remains a time-tier fiction. This is the "two-path divergence" (H7) confirmed in production data.

### NEW-HIGH-2 — `spur cost --by project` scope mismatch in single output

Prints `Total $14.73 (today)` immediately followed by `By project: (unassigned) $297.01 (489 sessions)` (all-time). Two different windows on the same screen without labels. Either show both windows per-project, or scope the `by_project()` call to the same window as the primary table.

### NEW-HIGH-3 — Real Claude Code JSONL in Apr-2026 does NOT carry `costUSD`

Neither the fix nor the ingestor should assume a pre-calculated cost field. Every Claude cost must be computed from tokens + pricing. The original `RESEARCH-ccusage.md` comment about "use agent-reported costUSD when present (Claude)" is stale for current Claude Code.

### NEW-MED-1 — Logging noise on every `spur cost`

Stderr dumps 11 lines of agent-registry INFO/WARN before output. Either route `spur_acp::agents::defaults` logs to `trace` level or set `RUST_LOG` default to `warn` in the CLI entrypoint for user-facing subcommands.

### NEW-MED-2 — Env-var divergence: `CLAUDE_CONFIG_DIR` interpretation

- `spur-context::engine::discover_claude_dir()` treats `$CLAUDE_CONFIG_DIR` as the literal path to the projects directory.
- `spur-cost::ingest::claude::discover_paths()` treats `$CLAUDE_CONFIG_DIR` as the parent and JOINS `projects/` onto it.
- Same variable, two readings. A user who sets it either way will get empty results in one path.

### NEW-MED-3 — Claude Code usage schema has drifted

New fields in `message.usage` not handled by the ingestor:
  `service_tier`, `cache_creation.{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}`, `server_tool_use.{web_search_requests, web_fetch_requests}`, `inference_geo`, `speed`, `iterations`

The last two affect billing tier (standard/priority) — omitting them means cost estimates ignore that dimension entirely.

### NEW-LOW-1 — `agent` names split across `foo` and `foo-acp`

`codex` (96 sessions) vs. `codex-acp` (27), `claude-code` (18) vs. `claude-code-acp` (178). Same underlying agent, different protocol wrapper. Reports should probably collapse these or at least display them grouped.

### NEW-LOW-2 — `has_jsonl_files` returns false if any subdirectory's `read_dir` errors

`find_jsonl_files` propagates Err from any subdirectory via `?`, then `is_ok_and(|v| !v.is_empty())` turns that Err into false. One permission-denied subdir under `~/.claude` silently disables Claude discovery. Prefer `walkdir` with ignore-on-error semantics.

---

## Verdict on the three criticals

| Finding | Judge verdict | Merge? |
|---|---|---|
| **C3** — substring fallback | Real bug, real fix, independently verified, tests strengthened | **YES, merge** |
| **C1** — Claude SQL view | Conceptually right, empirically broken on real data. Passing tests proved nothing. | **HOLD** |
| **C2** — Codex SQL view | Same as C1. | **HOLD** |

## Recommended next actions

1. **Split the merge**: land C3 now (correctness win, no downstream risk). Keep C1 and C2 on `fix/l9-review-criticals`.

2. **Before retrying C1/C2**: commit one real anonymized Claude Code JSONL file and one real Codex JSONL file to `crates/spur-context/tests/fixtures/real/`. Gate the SQL view tests on these. A fix that can't CREATE VIEW against the real sample is not a fix.

3. **Choose an architecture shape with empirical evidence in hand**:
   - Shape A revisited — use `json_extract(text, '$.path')` over raw JSONL text so DuckDB's schema inference is sidestepped entirely. Type-cast at projection. Robust to heterogeneity and upstream drift.
   - Shape B — Rust ingestor writes `normalized/*.jsonl` with fixed schema; DuckDB reads that. Zero drift risk, two writes per event. Prior docs favored this; the empirical pain of Shape A now argues for it.

4. **Separate work — wire the DuckDB engine into a CLI subcommand** (`spur context report --daily`, or replace `spur cost`'s data source). Without this, any further spur-context fix is unfalsifiable by users.

5. **Separate work — make the orchestrator call `end_session_with_tokens`**. Until this happens, C3 doesn't matter to users and every `spur cost` number is fiction. This is the single highest-leverage fix and is not in `spur-cost` or `spur-context` — it lives in `spur-core`.

6. **Fix `spur cost --by project` scoping bug** (two windows on one screen) and quiet the log noise. Small but user-facing.
