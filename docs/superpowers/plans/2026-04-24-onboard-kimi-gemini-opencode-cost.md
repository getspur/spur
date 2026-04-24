# Onboard Kimi / Gemini / OpenCode into spur-cost Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `spur-context::AnalyticsEngine` to ingest and report cost/token usage for three additional AI coding agents — Kimi, Gemini, and OpenCode — so `spur cost` reflects true cross-agent spend.

**Architecture:** The load-bearing path for `spur cost` is `AnalyticsEngine` (DuckDB views over agent-native storage). We extend that path — *not* the dormant `spur_cost::Ingestor` trait — with three new agent-specific extractors that materialize rows into DuckDB tables exposed as `kimi_events`, `gemini_events`, `opencode_events` views. All three agents lack the timestamp-per-message + JSONL shape that Claude/Codex use, so all three use the **Rust extractor → DuckDB appender → materialized-table-backed view** pattern (validated by the OpenCode spike on 2026-04-24, 1558 rows / $15.44 against real data).

**Tech Stack:** Rust 2021, `duckdb-rs` (bundled, with appender API), `rusqlite` (bundled, already in workspace), `serde_json`, `chrono`, existing `spur-context` + `spur-cost` crates.

---

## File Structure

| Path | Action | Purpose |
|---|---|---|
| `crates/spur-context/Cargo.toml` | Modify | Ensure `rusqlite` dep (already added during spike) |
| `crates/spur-context/src/engine.rs` | Modify | Add Kimi/Gemini/OpenCode extractors, discovery, view wiring; extend `AgentViewStatus` |
| `crates/spur-context/src/extractors/mod.rs` | Create | Submodule to hold per-agent extractor logic (keeps engine.rs under the current ~1400-line budget) |
| `crates/spur-context/src/extractors/kimi.rs` | Create | JSONL pre/post pairing + delta derivation + file-mtime timestamp |
| `crates/spur-context/src/extractors/gemini.rs` | Create | Single-doc JSON parser + per-message token extraction |
| `crates/spur-context/src/extractors/opencode.rs` | Create | rusqlite read + DuckDB appender (promoted from spike) |
| `crates/spur-cli/src/main.rs` | Modify | Extend status-hint at line ~1011 with new agents |

**Decomposition rationale:** Each extractor is ~80-150 LOC including tests. Splitting into `src/extractors/<agent>.rs` keeps `engine.rs` focused on DuckDB view orchestration. The existing `create_claude_view` / `create_codex_view` stay in `engine.rs` because they are pure SQL; the new three move their parsing logic into the submodule and expose a single `extract(db_path_or_dir) -> Vec<Row>` function called by engine.rs.

**Invariants preserved:**
- `all_events` UNION schema (10 columns, exact types) — every new view must match.
- Pricing join (`all_events_with_cost`) works only on `model` — so every row must set `model` to something non-NULL when cost is NULL, else the join silently drops rows.
- `refresh_cache()` mtime detection must see each agent's data dir.

---

## Task 1: OpenCode extractor (codify the spike)

**Context:** The OpenCode path was prototyped and validated on 2026-04-24 (1558 rows extracted, $15.44 cost matches raw SQLite aggregate exactly). The spike currently lives inline in `engine.rs`. This task moves it to `extractors/opencode.rs` and commits.

**Files:**
- Create: `crates/spur-context/src/extractors/mod.rs`
- Create: `crates/spur-context/src/extractors/opencode.rs`
- Modify: `crates/spur-context/src/engine.rs` (remove inline extractor, call into submodule)
- Modify: `crates/spur-context/src/lib.rs` (`pub mod extractors;` only if any type is re-exported; otherwise leave private)

### - [ ] Step 1.1: Create the submodule skeleton

Create `crates/spur-context/src/extractors/mod.rs`:

```rust
//! Per-agent extractors that convert native storage formats into DuckDB-appendable rows.
//!
//! These exist for agents whose on-disk format doesn't map cleanly to a single
//! `read_csv_auto` / `read_json` DuckDB view:
//!
//! - **OpenCode** — stored in SQLite (Drizzle ORM), not flat files.
//! - **Kimi** — JSONL with cumulative token counts (requires pairing) and no timestamps.
//! - **Gemini** — single-document JSON per file with nested `messages[]`.
//!
//! Each extractor returns a `Vec<ExtractedRow>` that the caller pushes into DuckDB
//! via the appender API. Row schema exactly matches the 10-column `all_events`
//! UNION contract in `schema.sql`.

use chrono::{DateTime, Utc};

pub mod opencode;

/// Shape every extractor produces. Matches the `opencode_events_table` /
/// `kimi_events_table` / `gemini_events_table` DuckDB schema so the appender
/// call site is identical across agents.
#[derive(Debug, Clone)]
pub struct ExtractedRow {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub model: Option<String>,
    pub project: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: Option<f64>,
}

impl ExtractedRow {
    /// Unix milliseconds, for appending to the BIGINT `timestamp_ms` column.
    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp.timestamp_millis()
    }
}
```

### - [ ] Step 1.2: Move the OpenCode extractor into `extractors/opencode.rs`

Create `crates/spur-context/src/extractors/opencode.rs`:

```rust
//! OpenCode SQLite (Drizzle) extractor.
//!
//! OpenCode stores sessions in `~/.local/share/opencode/opencode.db`. Assistant
//! messages carry provider-returned `tokens {input,output,reasoning,cache}` and
//! a pre-computed `cost` field. We trust that cost verbatim — OpenCode computes
//! it from the upstream provider's `usage` block per response; re-applying our
//! PricingRegistry would produce divergent numbers.
//!
//! Reasoning tokens are folded into `output_tokens` because OpenRouter (and
//! most upstream providers OpenCode speaks to) bill them at the output rate.

use super::ExtractedRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::path::Path;

/// Extract all assistant-role messages from an OpenCode SQLite database.
///
/// Rows with all-zero token fields (typically failed API calls with an error
/// payload) are skipped to match the filter semantics of `codex_events`.
pub fn extract(db_path: &Path) -> Result<Vec<ExtractedRow>> {
    // `mode=ro` + `immutable=1` avoids creating WAL sidecar files and is safe
    // even if the user's opencode process has the DB open for writes.
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        db_path.to_string_lossy()
    );
    let conn = rusqlite::Connection::open_with_flags(
        uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("failed to open opencode db at {}", db_path.display()))?;

    let mut stmt = conn.prepare(
        r#"
        SELECT m.time_created, m.session_id, p.worktree, m.data
        FROM message m
        JOIN session s ON s.id = m.session_id
        JOIN project p ON p.id = s.project_id
        WHERE json_extract(m.data, '$.role') = 'assistant'
        "#,
    )?;

    let raw: Vec<(i64, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(raw.len());
    for (ts_ms, session_id, worktree, data_json) in raw {
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&data_json) else {
            continue;
        };
        let tokens = data.get("tokens").cloned().unwrap_or(serde_json::Value::Null);
        let input = tokens.get("input").and_then(|v| v.as_i64()).unwrap_or(0);
        let output = tokens.get("output").and_then(|v| v.as_i64()).unwrap_or(0);
        let reasoning = tokens.get("reasoning").and_then(|v| v.as_i64()).unwrap_or(0);
        let cache_read = tokens
            .get("cache")
            .and_then(|c| c.get("read"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let cache_write = tokens
            .get("cache")
            .and_then(|c| c.get("write"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        if input == 0 && output == 0 && reasoning == 0 && cache_read == 0 && cache_write == 0 {
            continue;
        }

        let Some(timestamp) = DateTime::<Utc>::from_timestamp_millis(ts_ms) else {
            continue;
        };

        let model = data
            .get("modelID")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let cost = data.get("cost").and_then(|v| v.as_f64());
        let project = std::path::Path::new(&worktree)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());

        out.push(ExtractedRow {
            timestamp,
            session_id,
            model,
            project,
            input_tokens: input,
            output_tokens: output + reasoning,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_write,
            cost_usd: cost,
        });
    }
    Ok(out)
}
```

### - [ ] Step 1.3: Register the module in `lib.rs`

Modify `crates/spur-context/src/lib.rs` — add the module between existing `pub mod` lines:

```rust
pub mod async_engine;
pub mod engine;
pub mod extractors;  // add this
pub mod live;
pub mod reporter;
```

### - [ ] Step 1.4: Rewrite the engine-side OpenCode hook

Modify `crates/spur-context/src/engine.rs` — replace the inline `extract_opencode_rows` and `OpenCodeRow` struct with a call into the new module. Change `create_opencode_view` body to:

```rust
fn create_opencode_view(&self, db_path: &Path) -> Result<()> {
    // Fresh materialized table — drop any prior contents so re-runs are
    // idempotent and the view reflects current DB state.
    self.conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS opencode_events_table;
        CREATE TABLE opencode_events_table (
            timestamp_ms          BIGINT,
            session_id            VARCHAR,
            agent                 VARCHAR,
            model                 VARCHAR,
            project               VARCHAR,
            input_tokens          BIGINT,
            output_tokens         BIGINT,
            cache_read_tokens     BIGINT,
            cache_creation_tokens BIGINT,
            cost_usd              DOUBLE
        );
        "#,
    )?;

    let rows = crate::extractors::opencode::extract(db_path)?;

    if !rows.is_empty() {
        let mut appender = self
            .conn
            .appender("opencode_events_table")
            .context("failed to open opencode_events_table appender")?;
        for r in &rows {
            appender
                .append_row(params![
                    r.timestamp_ms(),
                    &r.session_id,
                    "opencode",
                    &r.model,
                    &r.project,
                    r.input_tokens,
                    r.output_tokens,
                    r.cache_read_tokens,
                    r.cache_creation_tokens,
                    r.cost_usd,
                ])
                .context("failed to append opencode row")?;
        }
        appender
            .flush()
            .context("failed to flush opencode appender")?;
    }

    self.conn.execute_batch(
        r#"
        CREATE OR REPLACE VIEW opencode_events AS
        SELECT
            epoch_ms(timestamp_ms) AS timestamp,
            session_id,
            agent,
            model,
            project,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            cost_usd
        FROM opencode_events_table;
        "#,
    )?;

    tracing::debug!(
        path = %db_path.display(),
        rows = rows.len(),
        "populated opencode_events"
    );
    Ok(())
}
```

Delete the inline `OpenCodeRow` struct and `extract_opencode_rows` function from `engine.rs`.

### - [ ] Step 1.5: Verify the existing unit test still passes

Run: `cargo test -p spur-context --lib test_opencode_events_from_sqlite_fixture -- --nocapture`
Expected: `test result: ok. 1 passed`

### - [ ] Step 1.6: Verify the smoke test still passes

Run: `cargo test -p spur-context --lib smoke_opencode_real_db -- --ignored --nocapture`
Expected on a machine with OpenCode installed:
```
smoke: rows=1558 input=Some(7150109) output=Some(575473) cost=Some(15.441...)
test engine::tests::smoke_opencode_real_db ... ok
```
(Exact numbers will differ per machine; `rows > 0` is the hard assertion.)

### - [ ] Step 1.7: Commit

```bash
git add crates/spur-context/src/extractors/mod.rs \
        crates/spur-context/src/extractors/opencode.rs \
        crates/spur-context/src/engine.rs \
        crates/spur-context/src/lib.rs \
        crates/spur-context/Cargo.toml \
        Cargo.lock
git commit -m "feat(spur-context): OpenCode SQLite cost ingest via rusqlite+DuckDB appender"
```

---

## Task 2: Kimi extractor

**Context:** Kimi stores sessions in `~/.kimi/sessions/<project_hash>/<session_uuid>/context.jsonl`. The file has no timestamps and no model field. Usage appears as `_usage` rows with a running cumulative `token_count`; empirical sampling across 5 sessions confirmed the pattern is **exactly 2× `_usage` entries per `assistant` turn** (pre-send context size, then post-receive context size). We pair odd/even `_usage` rows by file order, derive `output = post − pre` and `input = GREATEST(pre − LAG(post), 0)`, and stamp events with file mtime (Kimi precision is session-level).

**Files:**
- Create: `crates/spur-context/src/extractors/kimi.rs`
- Modify: `crates/spur-context/src/extractors/mod.rs` (`pub mod kimi;`)
- Modify: `crates/spur-context/src/engine.rs` (new `discover_kimi_dir`, `create_kimi_view`, wire into `create_agent_views` + `rebuild_unified_views` + `newest_agent_mtime`)
- Modify: `AgentViewStatus` — add `kimi: bool`

### - [ ] Step 2.1: Write the failing extractor unit test

Append to `crates/spur-context/src/extractors/kimi.rs` (creating the file with just the test + the stub module body):

```rust
//! Kimi JSONL extractor.
//!
//! See Task 2 header of the onboarding plan for the empirical pre/post pairing
//! rationale. No per-turn timestamps exist in Kimi session files; event
//! timestamps use file mtime shifted back by turn index to preserve ordering.

use super::ExtractedRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Extract all Kimi session files under `sessions_root`.
///
/// `sessions_root` is the directory containing `<project_hash>/<session_uuid>/context.jsonl`
/// (typically `~/.kimi/sessions`).
pub fn extract(sessions_root: &Path) -> Result<Vec<ExtractedRow>> {
    let files = discover_session_files(sessions_root)?;
    let mut out = Vec::new();
    for file in files {
        match extract_one(&file) {
            Ok(mut rows) => out.append(&mut rows),
            Err(e) => tracing::debug!(
                file = %file.display(),
                error = %e,
                "skipping malformed kimi session"
            ),
        }
    }
    Ok(out)
}

fn discover_session_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    for project in fs::read_dir(root)? {
        let project = project?;
        if !project.path().is_dir() {
            continue;
        }
        for session in fs::read_dir(project.path())? {
            let session = session?;
            let ctx = session.path().join("context.jsonl");
            if ctx.is_file() {
                out.push(ctx);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct KimiRow {
    role: String,
    #[serde(default)]
    token_count: Option<u64>,
}

fn extract_one(path: &Path) -> Result<Vec<ExtractedRow>> {
    let file = fs::File::open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    let mtime: DateTime<Utc> = fs::metadata(path)?
        .modified()
        .ok()
        .and_then(|m| DateTime::<Utc>::from(m).into())
        .unwrap_or_else(Utc::now);

    let mut usage: Vec<u64> = Vec::new();
    let mut assistant_turns: u64 = 0;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<KimiRow>(&line) else {
            continue;
        };
        match row.role.as_str() {
            "_usage" => {
                if let Some(t) = row.token_count {
                    usage.push(t);
                }
            }
            "assistant" => assistant_turns += 1,
            _ => {}
        }
    }

    // Invariant: 2 × assistant turns = _usage rows. If this breaks, fall back
    // to treating deltas as input-only and log a warning.
    let (session_id, project) = session_and_project_from_path(path);
    let mut rows = Vec::new();
    let turns = usage.len() / 2;
    if turns * 2 != usage.len() || turns != assistant_turns as usize {
        tracing::warn!(
            file = %path.display(),
            usage_count = usage.len(),
            assistant_count = assistant_turns,
            "kimi _usage/assistant count mismatch; falling back to input-only deltas"
        );
        let mut prev = 0u64;
        for (i, &cur) in usage.iter().enumerate() {
            let delta = cur.saturating_sub(prev);
            prev = cur;
            if delta == 0 {
                continue;
            }
            rows.push(ExtractedRow {
                timestamp: offset_turn(mtime, usage.len(), i),
                session_id: session_id.clone(),
                model: Some("kimi-for-coding".to_string()),
                project: project.clone(),
                input_tokens: delta as i64,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                cost_usd: None,
            });
        }
        return Ok(rows);
    }

    let mut prev_post: Option<u64> = None;
    for t in 0..turns {
        let pre = usage[t * 2];
        let post = usage[t * 2 + 1];
        let output = post.saturating_sub(pre);
        let input = match prev_post {
            Some(p) => pre.saturating_sub(p),
            None => pre, // first turn: full pre is net-new input
        };
        prev_post = Some(post);
        if output == 0 && input == 0 {
            continue;
        }
        rows.push(ExtractedRow {
            timestamp: offset_turn(mtime, turns, t),
            session_id: session_id.clone(),
            model: Some("kimi-for-coding".to_string()),
            project: project.clone(),
            input_tokens: input as i64,
            output_tokens: output as i64,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            cost_usd: None,
        });
    }
    Ok(rows)
}

/// Back-date earlier turns by one second each so within-session ordering is
/// preserved even though Kimi itself only gives us file-mtime precision.
fn offset_turn(mtime: DateTime<Utc>, total: usize, idx: usize) -> DateTime<Utc> {
    mtime - chrono::Duration::seconds((total.saturating_sub(idx + 1)) as i64)
}

fn session_and_project_from_path(path: &Path) -> (String, Option<String>) {
    // path: .../sessions/<project_hash>/<session_uuid>/context.jsonl
    let session = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let project = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());
    (session, project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_session(tmp: &Path, project: &str, session: &str, lines: &[&str]) -> PathBuf {
        let dir = tmp.join("sessions").join(project).join(session);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("context.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        for l in lines {
            writeln!(f, "{}", l).unwrap();
        }
        path
    }

    #[test]
    fn pairs_pre_post_usage() {
        let tmp = TempDir::new().unwrap();
        write_session(
            tmp.path(),
            "projhash",
            "sess-uuid",
            &[
                r#"{"role":"_system_prompt","content":"sys"}"#,
                r#"{"role":"_checkpoint","id":0}"#,
                r#"{"role":"user","content":"hi"}"#,
                r#"{"role":"_usage","token_count":13000}"#,
                r#"{"role":"assistant","content":"hello"}"#,
                r#"{"role":"_usage","token_count":13500}"#,
                r#"{"role":"tool","content":"t"}"#,
                r#"{"role":"_usage","token_count":14000}"#,
                r#"{"role":"assistant","content":"second"}"#,
                r#"{"role":"_usage","token_count":14100}"#,
            ],
        );
        let rows = extract(&tmp.path().join("sessions")).unwrap();
        assert_eq!(rows.len(), 2);

        // Turn 1: pre=13000, post=13500 → input=13000 (first turn), output=500
        assert_eq!(rows[0].input_tokens, 13000);
        assert_eq!(rows[0].output_tokens, 500);
        assert_eq!(rows[0].model.as_deref(), Some("kimi-for-coding"));
        assert_eq!(rows[0].session_id, "sess-uuid");
        assert_eq!(rows[0].project.as_deref(), Some("projhash"));
        assert!(rows[0].cost_usd.is_none());

        // Turn 2: pre=14000, post=14100 → input=14000-13500=500, output=100
        assert_eq!(rows[1].input_tokens, 500);
        assert_eq!(rows[1].output_tokens, 100);

        // Within-session ordering preserved
        assert!(rows[0].timestamp < rows[1].timestamp);
    }

    #[test]
    fn fallback_on_mismatched_counts() {
        let tmp = TempDir::new().unwrap();
        // Deliberately off-pattern: 3 _usage for 1 assistant
        write_session(
            tmp.path(),
            "ph",
            "sess",
            &[
                r#"{"role":"user","content":"x"}"#,
                r#"{"role":"_usage","token_count":100}"#,
                r#"{"role":"_usage","token_count":150}"#,
                r#"{"role":"assistant","content":"y"}"#,
                r#"{"role":"_usage","token_count":200}"#,
            ],
        );
        let rows = extract(&tmp.path().join("sessions")).unwrap();
        // Fallback path: cumulative diffs, all as input.
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[1].input_tokens, 50);
        assert_eq!(rows[2].input_tokens, 50);
        assert!(rows.iter().all(|r| r.output_tokens == 0));
    }
}
```

### - [ ] Step 2.2: Register the module

Modify `crates/spur-context/src/extractors/mod.rs` — add `pub mod kimi;` below `pub mod opencode;`.

### - [ ] Step 2.3: Run the extractor unit tests (expect red → green)

Run: `cargo test -p spur-context --lib extractors::kimi:: -- --nocapture`
Expected: `test result: ok. 2 passed`.

If either test fails, fix the extractor — do NOT loosen the test. The pairing logic must match the empirical Kimi behavior.

### - [ ] Step 2.4: Wire into the engine

Modify `crates/spur-context/src/engine.rs` — add next to `create_kiro_view`:

```rust
fn discover_kimi_dir() -> PathBuf {
    if let Ok(path) = env::var("KIMI_HOME") {
        return PathBuf::from(path).join("sessions");
    }
    #[cfg(test)]
    {
        PathBuf::from("__spur_context_test_missing__/kimi")
    }
    #[cfg(not(test))]
    {
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".kimi/sessions"))
            .unwrap_or_else(|| PathBuf::from("~/.kimi/sessions"))
    }
}

fn create_kimi_view(&self, sessions_root: &Path) -> Result<()> {
    self.conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS kimi_events_table;
        CREATE TABLE kimi_events_table (
            timestamp_ms          BIGINT,
            session_id            VARCHAR,
            agent                 VARCHAR,
            model                 VARCHAR,
            project               VARCHAR,
            input_tokens          BIGINT,
            output_tokens         BIGINT,
            cache_read_tokens     BIGINT,
            cache_creation_tokens BIGINT,
            cost_usd              DOUBLE
        );
        "#,
    )?;

    let rows = crate::extractors::kimi::extract(sessions_root)?;
    if !rows.is_empty() {
        let mut appender = self.conn.appender("kimi_events_table")?;
        for r in &rows {
            appender.append_row(params![
                r.timestamp_ms(),
                &r.session_id,
                "kimi",
                &r.model,
                &r.project,
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens,
                r.cost_usd,
            ])?;
        }
        appender.flush()?;
    }

    self.conn.execute_batch(
        r#"
        CREATE OR REPLACE VIEW kimi_events AS
        SELECT
            epoch_ms(timestamp_ms) AS timestamp,
            session_id, agent, model, project,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd
        FROM kimi_events_table;
        "#,
    )?;

    tracing::debug!(
        root = %sessions_root.display(),
        rows = rows.len(),
        "populated kimi_events"
    );
    Ok(())
}
```

In `AgentViewStatus`, add `pub kimi: bool,` — directly after `pub kiro: bool,`. Note that `pub opencode: bool` is already present from Task 1.

In `create_agent_views()`, add between the Kiro branch and the OpenCode branch:

```rust
// ─── Kimi ───────────────────────────────────────────────
let kimi_dir = Self::discover_kimi_dir();
if kimi_dir.is_dir() {
    match self.create_kimi_view(&kimi_dir) {
        Ok(()) => {
            status.kimi = true;
            tracing::debug!(dir = %kimi_dir.display(), "created kimi_events view");
        }
        Err(e) => {
            tracing::warn!(dir = %kimi_dir.display(), error = %e, "failed to create kimi_events view, using stub");
            self.create_empty_stub("kimi_events")?;
        }
    }
} else {
    self.create_empty_stub("kimi_events")?;
    tracing::debug!("created empty kimi_events stub");
}
```

In `rebuild_unified_views()`, extend the UNION:

```rust
CREATE OR REPLACE VIEW all_events AS
SELECT * FROM claude_events
UNION ALL SELECT * FROM codex_events
UNION ALL SELECT * FROM kiro_events
UNION ALL SELECT * FROM kimi_events
UNION ALL SELECT * FROM opencode_events;
```

Replace the whole `newest_agent_mtime` function body with:

```rust
fn newest_agent_mtime() -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut bump = |m: std::time::SystemTime| {
        newest = Some(match newest {
            Some(cur) if cur >= m => cur,
            _ => m,
        });
    };
    for dir in [
        Self::discover_claude_dir(),
        Self::discover_codex_dir(),
        Self::discover_kiro_dir(),
        Self::discover_kimi_dir(),
    ] {
        if let Ok(files) = Self::find_jsonl_files(&dir) {
            for f in files {
                if let Ok(meta) = std::fs::metadata(&f) {
                    if let Ok(m) = meta.modified() {
                        bump(m);
                    }
                }
            }
        }
    }
    // OpenCode: check the DB file directly (not JSONL).
    let db = Self::discover_opencode_db();
    if let Ok(meta) = std::fs::metadata(&db) {
        if let Ok(m) = meta.modified() {
            bump(m);
        }
    }
    newest
}
```

Note: Gemini's extension to this function is added in Task 3 Step 3.4, not here.

### - [ ] Step 2.5: Add an engine integration test

Append to `#[cfg(test)] mod tests` in `engine.rs`, after `test_opencode_events_from_sqlite_fixture`:

```rust
#[test]
fn test_kimi_events_from_fixture() {
    let tmp = TempDir::new().unwrap();
    let kimi_root = tmp.path().join("kimi");
    let sessions = kimi_root.join("sessions");
    let dir = sessions.join("proj-hash/sess-1");
    std::fs::create_dir_all(&dir).unwrap();
    let mut f = std::fs::File::create(dir.join("context.jsonl")).unwrap();
    for line in [
        r#"{"role":"_system_prompt","content":"sys"}"#,
        r#"{"role":"user","content":"q"}"#,
        r#"{"role":"_usage","token_count":1000}"#,
        r#"{"role":"assistant","content":"a"}"#,
        r#"{"role":"_usage","token_count":1200}"#,
    ] {
        writeln!(f, "{}", line).unwrap();
    }

    let engine = setup_engine();
    engine.create_kimi_view(&sessions).unwrap();

    let (n, i, o): (i64, i64, i64) = engine
        .conn
        .query_row(
            "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens) FROM kimi_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(i, 1000);
    assert_eq!(o, 200);
}

#[test]
#[ignore]
fn smoke_kimi_real_dir() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let sessions = home.join(".kimi/sessions");
    if !sessions.is_dir() { return }
    let engine = AnalyticsEngine::open_in_memory().unwrap();
    engine.initialize().unwrap();
    engine.create_kimi_view(&sessions).unwrap();
    let (n, i, o): (i64, Option<i64>, Option<i64>) = engine.conn.query_row(
        "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens) FROM kimi_events",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    eprintln!("kimi smoke: rows={} input={:?} output={:?}", n, i, o);
    assert!(n > 0, "expected kimi data on this machine");
}
```

### - [ ] Step 2.6: Run all engine tests

Run: `cargo test -p spur-context --lib`
Expected: all new kimi tests pass; no regression in claude/codex/opencode tests. (The pre-existing `test_reporter_live_report_large_window` failure is not caused by this task — leave it for a separate bug fix.)

### - [ ] Step 2.7: Verify against real data

Run: `cargo test -p spur-context --lib smoke_kimi_real_dir -- --ignored --nocapture`
Expected: `kimi smoke: rows=N input=Some(..) output=Some(..)` with N matching roughly the number of assistant turns across your Kimi sessions.

Cross-check: `find ~/.kimi/sessions -name context.jsonl -exec grep -c '"role":"assistant"' {} + | awk -F: '{s+=$2} END {print s}'` — this is the count we expect.

### - [ ] Step 2.8: Commit

```bash
git add crates/spur-context/src/extractors/kimi.rs \
        crates/spur-context/src/extractors/mod.rs \
        crates/spur-context/src/engine.rs
git commit -m "feat(spur-context): Kimi JSONL cost ingest via pre/post _usage pairing"
```

---

## Task 3: Gemini extractor

**Context:** Gemini CLI stores sessions in `~/.gemini/tmp/<session_uuid>/chats/session-YYYY-MM-DDTHH-MM-<hash>.json` — one JSON document per file with `messages[]` array. Each `type:"gemini"` message has a per-turn `tokens { input, output, cached, thoughts, tool, total }` block and a `timestamp` ISO string. Thinking tokens bill as output (Google pricing for 2.5/3.x Pro as of 2026-04). No pre-computed cost — rely on PricingRegistry fallback (which returns NULL until Gemini pricing is added).

**Files:**
- Create: `crates/spur-context/src/extractors/gemini.rs`
- Modify: `crates/spur-context/src/extractors/mod.rs`
- Modify: `crates/spur-context/src/engine.rs`

### - [ ] Step 3.1: Write the failing extractor test

Create `crates/spur-context/src/extractors/gemini.rs`:

```rust
//! Gemini CLI single-document JSON extractor.
//!
//! Each Gemini session is one JSON file containing `messages[]`. We iterate
//! messages and emit one row per `type:"gemini"` entry. `thoughts` tokens
//! fold into `output_tokens` (Google prices them at the output rate as of
//! Gemini 2.5/3.x Pro). `tool` tokens fold into `input_tokens` because they
//! represent tool-response context presented to the model.

use super::ExtractedRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Extract all Gemini session chat files under `tmp_root`.
///
/// `tmp_root` is `~/.gemini/tmp` — direct parent of per-session UUID dirs.
pub fn extract(tmp_root: &Path) -> Result<Vec<ExtractedRow>> {
    let mut out = Vec::new();
    for file in discover_session_files(tmp_root)? {
        match extract_one(&file) {
            Ok(mut rows) => out.append(&mut rows),
            Err(e) => tracing::debug!(
                file = %file.display(),
                error = %e,
                "skipping malformed gemini session"
            ),
        }
    }
    Ok(out)
}

fn discover_session_files(tmp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !tmp_root.is_dir() {
        return Ok(out);
    }
    for session in fs::read_dir(tmp_root)? {
        let session = session?;
        let chats_dir = session.path().join("chats");
        if !chats_dir.is_dir() {
            continue;
        }
        for chat in fs::read_dir(chats_dir)? {
            let chat = chat?;
            let p = chat.path();
            if p.extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(p);
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct GeminiSession {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default, rename = "projectHash")]
    project_hash: Option<String>,
    #[serde(default)]
    messages: Vec<GeminiMessage>,
}

#[derive(Debug, Deserialize)]
struct GeminiMessage {
    #[serde(default, rename = "type")]
    msg_type: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tokens: Option<GeminiTokens>,
}

#[derive(Debug, Deserialize, Default)]
struct GeminiTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cached: u64,
    #[serde(default)]
    thoughts: u64,
    #[serde(default)]
    tool: u64,
}

fn extract_one(path: &Path) -> Result<Vec<ExtractedRow>> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let sess: GeminiSession = serde_json::from_str(&data)?;

    let mut rows = Vec::new();
    for m in sess.messages {
        if m.msg_type.as_deref() != Some("gemini") {
            continue;
        }
        let Some(tokens) = m.tokens else { continue };
        if tokens.input == 0 && tokens.output == 0 && tokens.thoughts == 0 && tokens.cached == 0 {
            continue;
        }
        let ts = m
            .timestamp
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        rows.push(ExtractedRow {
            timestamp: ts,
            session_id: sess.session_id.clone(),
            model: m.model,
            project: sess.project_hash.clone(),
            input_tokens: (tokens.input + tokens.tool) as i64,
            output_tokens: (tokens.output + tokens.thoughts) as i64,
            cache_read_tokens: tokens.cached as i64,
            cache_creation_tokens: 0,
            cost_usd: None,
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn extracts_gemini_messages_with_thoughts_as_output() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("tmp/sess-uuid/chats");
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "sessionId": "sess-uuid",
            "projectHash": "proj-hash",
            "messages": [
                {"type": "user", "timestamp": "2026-04-21T04:16:10Z"},
                {
                    "type": "gemini",
                    "timestamp": "2026-04-21T04:16:15Z",
                    "model": "gemini-3.1-pro-preview",
                    "tokens": {"input": 1000, "output": 50, "cached": 200, "thoughts": 150, "tool": 30, "total": 1430}
                },
                {"type": "gemini", "timestamp": "2026-04-21T04:17:00Z", "tokens": {"input": 0, "output": 0}}
            ]
        });
        std::fs::write(
            dir.join("session-2026-04-21T04-16.json"),
            serde_json::to_string(&body).unwrap(),
        )
        .unwrap();

        let rows = extract(&tmp.path().join("tmp")).unwrap();
        assert_eq!(rows.len(), 1, "user + zero-token gemini must be filtered");
        assert_eq!(rows[0].session_id, "sess-uuid");
        assert_eq!(rows[0].project.as_deref(), Some("proj-hash"));
        assert_eq!(rows[0].model.as_deref(), Some("gemini-3.1-pro-preview"));
        // input = 1000 + 30 (tool)
        assert_eq!(rows[0].input_tokens, 1030);
        // output = 50 + 150 (thoughts)
        assert_eq!(rows[0].output_tokens, 200);
        assert_eq!(rows[0].cache_read_tokens, 200);
        assert!(rows[0].cost_usd.is_none());
    }
}
```

### - [ ] Step 3.2: Register the module

Modify `crates/spur-context/src/extractors/mod.rs` — add `pub mod gemini;`.

### - [ ] Step 3.3: Run the unit test

Run: `cargo test -p spur-context --lib extractors::gemini::tests::extracts_gemini_messages_with_thoughts_as_output -- --nocapture`
Expected: `test result: ok. 1 passed`.

### - [ ] Step 3.4: Wire into the engine

Modify `crates/spur-context/src/engine.rs` — add:

```rust
fn discover_gemini_dir() -> PathBuf {
    if let Ok(path) = env::var("GEMINI_HOME") {
        return PathBuf::from(path).join("tmp");
    }
    #[cfg(test)]
    {
        PathBuf::from("__spur_context_test_missing__/gemini")
    }
    #[cfg(not(test))]
    {
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".gemini/tmp"))
            .unwrap_or_else(|| PathBuf::from("~/.gemini/tmp"))
    }
}

fn create_gemini_view(&self, tmp_root: &Path) -> Result<()> {
    self.conn.execute_batch(
        r#"
        DROP TABLE IF EXISTS gemini_events_table;
        CREATE TABLE gemini_events_table (
            timestamp_ms          BIGINT,
            session_id            VARCHAR,
            agent                 VARCHAR,
            model                 VARCHAR,
            project               VARCHAR,
            input_tokens          BIGINT,
            output_tokens         BIGINT,
            cache_read_tokens     BIGINT,
            cache_creation_tokens BIGINT,
            cost_usd              DOUBLE
        );
        "#,
    )?;
    let rows = crate::extractors::gemini::extract(tmp_root)?;
    if !rows.is_empty() {
        let mut appender = self.conn.appender("gemini_events_table")?;
        for r in &rows {
            appender.append_row(params![
                r.timestamp_ms(),
                &r.session_id,
                "gemini",
                &r.model,
                &r.project,
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens,
                r.cost_usd,
            ])?;
        }
        appender.flush()?;
    }
    self.conn.execute_batch(
        r#"
        CREATE OR REPLACE VIEW gemini_events AS
        SELECT epoch_ms(timestamp_ms) AS timestamp,
               session_id, agent, model, project,
               input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, cost_usd
        FROM gemini_events_table;
        "#,
    )?;
    tracing::debug!(root = %tmp_root.display(), rows = rows.len(), "populated gemini_events");
    Ok(())
}
```

Add `pub gemini: bool,` to `AgentViewStatus` (after `pub kimi: bool,`).

In `create_agent_views()`, add the Gemini branch between Kimi and OpenCode:

```rust
// ─── Gemini ─────────────────────────────────────────────
let gemini_dir = Self::discover_gemini_dir();
if gemini_dir.is_dir() {
    match self.create_gemini_view(&gemini_dir) {
        Ok(()) => {
            status.gemini = true;
            tracing::debug!(dir = %gemini_dir.display(), "created gemini_events view");
        }
        Err(e) => {
            tracing::warn!(dir = %gemini_dir.display(), error = %e, "failed to create gemini_events view, using stub");
            self.create_empty_stub("gemini_events")?;
        }
    }
} else {
    self.create_empty_stub("gemini_events")?;
    tracing::debug!("created empty gemini_events stub");
}
```

Extend `rebuild_unified_views()` UNION:

```rust
CREATE OR REPLACE VIEW all_events AS
SELECT * FROM claude_events
UNION ALL SELECT * FROM codex_events
UNION ALL SELECT * FROM kiro_events
UNION ALL SELECT * FROM kimi_events
UNION ALL SELECT * FROM gemini_events
UNION ALL SELECT * FROM opencode_events;
```

Extend `newest_agent_mtime()` — add `Self::discover_gemini_dir()` to the `for dir in [...]` loop. Gemini session files have `.json` extension, not `.jsonl`, so `find_jsonl_files` won't match them. Add a dedicated walk after the existing loop and before the OpenCode DB check:

```rust
// Gemini uses .json (not .jsonl) — separate walk.
if let Ok(files) = Self::find_files_with_ext(&Self::discover_gemini_dir(), "json") {
    for f in files {
        if let Ok(meta) = std::fs::metadata(&f) {
            if let Ok(m) = meta.modified() {
                bump(m);
            }
        }
    }
}
```

And add the helper next to `find_jsonl_files`:

```rust
fn find_files_with_ext(dir: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            files.extend(Self::find_files_with_ext(&path, ext)?);
        } else if path.extension().and_then(|s| s.to_str()) == Some(ext) {
            files.push(path);
        }
    }
    Ok(files)
}
```

### - [ ] Step 3.5: Engine integration test

Append to `#[cfg(test)] mod tests` in `engine.rs`:

```rust
#[test]
fn test_gemini_events_from_fixture() {
    let tmp = TempDir::new().unwrap();
    let gemini_root = tmp.path().join("gemini");
    let dir = gemini_root.join("tmp/sess-1/chats");
    std::fs::create_dir_all(&dir).unwrap();
    let body = serde_json::json!({
        "sessionId": "sess-1",
        "projectHash": "ph",
        "messages": [{
            "type": "gemini",
            "timestamp": "2026-04-21T04:16:15Z",
            "model": "gemini-3.1-pro",
            "tokens": {"input": 500, "output": 50, "cached": 100, "thoughts": 30, "tool": 20}
        }]
    });
    std::fs::write(dir.join("s.json"), serde_json::to_string(&body).unwrap()).unwrap();

    let engine = setup_engine();
    engine.create_gemini_view(&gemini_root.join("tmp")).unwrap();

    let (n, i, o, c): (i64, i64, i64, i64) = engine
        .conn
        .query_row(
            "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens), SUM(cache_read_tokens) FROM gemini_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(i, 520); // 500 + 20 tool
    assert_eq!(o, 80);  // 50 + 30 thoughts
    assert_eq!(c, 100);
}

#[test]
#[ignore]
fn smoke_gemini_real_dir() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
    let tmp = home.join(".gemini/tmp");
    if !tmp.is_dir() { return }
    let engine = AnalyticsEngine::open_in_memory().unwrap();
    engine.initialize().unwrap();
    engine.create_gemini_view(&tmp).unwrap();
    let (n, i, o): (i64, Option<i64>, Option<i64>) = engine.conn.query_row(
        "SELECT COUNT(*), SUM(input_tokens), SUM(output_tokens) FROM gemini_events",
        [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    eprintln!("gemini smoke: rows={} input={:?} output={:?}", n, i, o);
    assert!(n > 0, "expected gemini data on this machine");
}
```

### - [ ] Step 3.6: Run full spur-context test suite

Run: `cargo test -p spur-context --lib`
Expected: all new gemini tests pass; prior tests still pass.

### - [ ] Step 3.7: Smoke test against real data

Run: `cargo test -p spur-context --lib smoke_gemini_real_dir -- --ignored --nocapture`
Expected: non-zero row count. Cross-check: count `type:"gemini"` entries across all chat JSONs:

```bash
find ~/.gemini/tmp -name 'session-*.json' -exec jq '[.messages[]|select(.type=="gemini")]|length' {} + | awk '{s+=$1} END {print s}'
```

The smoke count must equal (or be at most off-by-filter-skipped) that total.

### - [ ] Step 3.8: Commit

```bash
git add crates/spur-context/src/extractors/gemini.rs \
        crates/spur-context/src/extractors/mod.rs \
        crates/spur-context/src/engine.rs
git commit -m "feat(spur-context): Gemini JSON cost ingest with thoughts->output fold"
```

---

## Task 4: CLI status-hint wiring

**Context:** When no cost data is found, `spur cost` prints a status hint listing which agent views were successfully created. The hint currently shows only claude/codex/kiro — must include the three new agents.

**Files:**
- Modify: `crates/spur-cli/src/main.rs:~1011`

### - [ ] Step 4.1: Locate and extend the status hint

Modify `crates/spur-cli/src/main.rs`. Locate:

```rust
let status_hint = format!(
    "(engine views: claude={}, codex={}, kiro={})",
    status.claude, status.codex, status.kiro,
);
```

Replace with:

```rust
let status_hint = format!(
    "(engine views: claude={}, codex={}, kiro={}, kimi={}, gemini={}, opencode={})",
    status.claude, status.codex, status.kiro, status.kimi, status.gemini, status.opencode,
);
```

### - [ ] Step 4.2: Compile check

Run: `cargo check -p spur-cli`
Expected: clean.

### - [ ] Step 4.3: Smoke the CLI end-to-end

Run: `cargo run -p spur-cli --quiet -- cost --week`
Expected: output includes per-agent rows for any agents the current user has data for. On a machine with OpenCode data, the `opencode` row should show costs within ~$0.01 of `SELECT SUM(cost) FROM message WHERE ...` on the live db.

### - [ ] Step 4.4: Commit

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): cost --status hint reports kimi/gemini/opencode view state"
```

---

## Task 5 (optional, separate PR recommended): Pre-existing clippy drive-by

**Context:** Two `literal with an empty format string` errors exist in `engine.rs` at lines 1535 and 1541 on the `test_codex_events_delta_logic` test — pre-existing on `main`, blocking `cargo clippy --workspace -- -D warnings`. Unrelated to this onboarding work but worth clearing so CI stays green.

**Files:**
- Modify: `crates/spur-context/src/engine.rs` (two `writeln!(file, "{}", r#"..."#)` calls)

### - [ ] Step 5.1: Replace the indirect format strings

For each of the two flagged `writeln!` calls, change:

```rust
writeln!(
    file,
    "{}",
    r#"{"type":"event_msg", ...}"#
)
.unwrap();
```

To:

```rust
writeln!(
    file,
    r#"{{"type":"event_msg", ...}}"#
)
.unwrap();
```

(Escape literal braces by doubling them.)

### - [ ] Step 5.2: Verify clippy clean

Run: `cargo clippy -p spur-context --lib --tests -- -D warnings`
Expected: no errors.

### - [ ] Step 5.3: Commit

```bash
git add crates/spur-context/src/engine.rs
git commit -m "chore(spur-context): fix clippy literal-with-empty-format on codex test"
```

---

## Final verification

After all 4 (or 5) tasks land:

### - [ ] Full regression sweep

Run: `cargo test -p spur-context --lib`
Expected: 19+ passing tests (2 original + 1 opencode spike + 2 kimi extractor + 1 kimi engine + 1 gemini extractor + 1 gemini engine + 3 ignored smokes). The pre-existing `test_reporter_live_report_large_window` may still fail — that's a separate known bug on `main`.

### - [ ] Workspace clippy

Run: `cargo clippy --workspace --lib --tests -- -D warnings`
Expected: clean (Task 5 required for workspace-level clean).

### - [ ] End-to-end smoke

Run: `cargo run -p spur-cli --quiet -- cost --week`
Expected: each of kimi / gemini / opencode reports appear in the agent totals when that agent has data on disk.

### - [ ] Cross-verify OpenCode cost

```bash
sqlite3 ~/.local/share/opencode/opencode.db \
  "SELECT ROUND(SUM(json_extract(data,'\$.cost')),4)
   FROM message WHERE json_extract(data,'\$.role')='assistant';"
```

Compare against the `opencode` cost in `spur cost` output — must match to the cent.

---

## Explicit non-goals

- **Not touching** `spur_cost::Ingestor` trait, `spur_cost::Reporter`, or `IngestionPipeline`. They are dormant for end-users; consolidation is a separate L9 refactor.
- **Not adding** pricing rows for Kimi / Gemini models. Cost stays NULL until rates are confirmed from primary sources. The LEFT JOIN in `all_events_with_cost` already handles this.
- **Not fixing** the duplicated pricing math between `spur-cost/src/estimator.rs` and the SQL `ALL_EVENTS_WITH_COST_VIEW`. Known drift; separate ticket.
- **Not deleting** the Kiro stub. ACP-transported billing is the correct architectural boundary and is documented.

## Risks & rollback

| Risk | Mitigation |
|---|---|
| Kimi pre/post pattern breaks in a future Kimi CLI release | Extractor logs a warning and falls back to cumulative-diff-as-input; users see conservative numbers, not silently wrong ones |
| Gemini schema changes (new token category) | Unknown fields are ignored by `serde::Deserialize`; nothing breaks. If a new category appears that should count as output (new reasoning variant), plan an extension |
| OpenCode DB locked by running process | `mode=ro&immutable=1` uses SQLite's immutable read path which doesn't acquire any lock |
| DuckDB appender type errors | Caught in Task 1; timestamp stored as BIGINT in table, cast to TIMESTAMP in view |
| `discover_*_dir()` returns false-positive path on `cfg(test)` | All three use the `__spur_context_test_missing__` sentinel matching the existing convention |

Rollback path: each task is a single commit. `git revert` a task to drop that agent; the UNION in `all_events` still works because unused stubs return zero rows.
