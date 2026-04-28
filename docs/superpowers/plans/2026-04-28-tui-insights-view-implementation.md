# TUI Insights View — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each task follows red-green-refactor: write failing test, implement, verify, commit.

**Spec:** `docs/superpowers/specs/2026-04-28-tui-insights-view-design.md` (this branch).
**Predecessor plan:** `docs/superpowers/plans/2026-04-24-onboard-kimi-gemini-opencode-cost.md` (Gemini section unimplemented; this plan executes it as Task 4).
**Substrate branch:** `feat/insights-substrate-harvest` (worktree at `/Volumes/Projects/spur/.worktrees/insights-substrate-harvest`).

**Goal:** Implement Phase 1 of the TUI Insights view per the spec — 4-tab analytics surface (Overview, Timeline, Breakdown, Live) feature-gated as `analytics` (default OFF), plus 4 substrate repairs (R1-R4) and a dashboard cost-source switch under the same flag. Five well-known agents (Claude Code, Codex, Gemini, OpenCode, Kimi) become first-class in the analytics surface.

**Tech Stack:** Rust 2021 edition. Workspace deps in use: `duckdb-rs` (bundled, optional via `spur-context/duckdb`), `rusqlite` (bundled), `tokio` (multi-threaded runtime), `ratatui` 0.x (already used by spur-tui), `chrono`, `serde`, `serde_json`, `anyhow`, `tracing`, `directories`, `arc-swap` is **NOT** used (we stay on `tokio::sync::RwLock` per codex review).

**PR split rationale:** 8 deliverables across 3 PRs to keep each reviewable in <30 minutes:

- **PR A — substrate (R1, R2, R3)** — small, isolated, no TUI changes. ~80 LoC + tests.
- **PR B — Gemini extractor (R4)** — single new module, follows the existing `2026-04-24-onboard-kimi-gemini-opencode-cost.md` Step 3 design. ~150 LoC + tests.
- **PR C — Insights view + dashboard switch + ViewId/Action wiring + CI matrix** — the bulk of the work, gated entirely behind `analytics` feature so non-analytics builds are unaffected. ~1 700 LoC + tests.

Each PR cleanly stacks on the previous one. PR A and PR B can be parallelized (no shared files); PR C depends on PR A's `cost_source` semantics and PR B's `gemini_events` view existing.

**Invariants preserved across all 3 PRs:**
- `all_events` UNION schema (10 columns, exact types) unchanged.
- Pricing join (`all_events_with_cost`) keys on `model` — every new row must set `model` non-NULL or be willing to surface as `cost_source='unpriced'`.
- `cargo check -p spur-tui --no-default-features` must pass at every commit (no analytics dependency leakage into non-analytics builds).
- `cargo check -p spur-context --features duckdb` must pass at every commit.
- No breaking changes to public APIs of `spur-context` beyond the harvested `SessionRow.models` rename (already shipped on this branch).

---

## File Structure

| Path | PR | Action | Purpose |
|---|---|---|---|
| `crates/spur-context/src/engine.rs` | A | Modify | R1 prefix-strip in `extract_opencode_rows`; R3 OpenCode mtime in `newest_agent_mtime` |
| `crates/spur-cost/src/pricing.rs` | A | Modify | R2 `kimi-for-coding` entry in `with_builtin_prices` |
| `crates/spur-context/src/extractors/mod.rs` | B | Create | Submodule index |
| `crates/spur-context/src/extractors/gemini.rs` | B | Create | Gemini JSON extractor |
| `crates/spur-context/src/engine.rs` | B | Modify | Wire `discover_gemini_dir`, `create_gemini_view`, `AgentViewStatus.gemini`, mtime walk |
| `crates/spur-context/tests/fixtures/gemini/` | B | Create | Synthetic + redacted real session JSONs |
| `crates/spur-tui/Cargo.toml` | C | Modify | Add `analytics` feature + optional `spur-context` dep |
| `crates/spur-tui/src/views/insights/mod.rs` | C | Create | View impl + Drop + refresh-handle wiring |
| `crates/spur-tui/src/views/insights/state.rs` | C | Create | Tab/Granularity/Dimension enums + `InsightsSnapshot` |
| `crates/spur-tui/src/views/insights/builder.rs` | C | Create | `build_snapshot` — single `AsyncEngine::run` pass |
| `crates/spur-tui/src/views/insights/refresh.rs` | C | Create | Refresh task + signal channel |
| `crates/spur-tui/src/views/insights/tabs/{overview,timeline,breakdown,live}.rs` | C | Create | Per-tab renderers |
| `crates/spur-tui/src/views/insights/widgets/{kpi_strip,sparkline}.rs` | C | Create | Stateless ratatui widgets |
| `crates/spur-tui/src/views/mod.rs` | C | Modify | Register Insights module + stub fallback |
| `crates/spur-tui/src/action.rs` | C | Modify | Add `ViewId::Insights`, `Action::OpenInsights` |
| `crates/spur-tui/src/app.rs` | C | Modify | `LiveCostCache`, refresh task, route Insights, dashboard switch wiring |
| `crates/spur-tui/src/views/dashboard.rs` | C | Modify | `current_cost` reads `LiveCostCache` when feature on |
| `crates/spur-tui/src/components/status_bar.rs` | C | Modify | New ViewId arm + "via analytics" pill |
| `.github/workflows/*.yml` (or workspace CI config) | C | Modify | Add `--features analytics` job |

---

# PR A — Substrate Fixes (R1, R2, R3)

**Branch from:** `feat/insights-substrate-harvest` HEAD.
**Branch name:** `feat/insights-substrate-pr-a`.
**Estimated size:** ~80 LoC + 3 tests.
**Reviewer hint:** verify each repair against its spec section (§5.7 R1-R3).

## Task A.1: R1 — OpenCode model-prefix strip

**Context:** OpenCode stores model IDs verbatim from the upstream agent: `anthropic/claude-opus-4-5`, `google/gemini-2.5-pro`, `z-ai/...`, `moonshotai/...`, etc. The pricing match in `ALL_EVENTS_WITH_COST_VIEW` (`engine.rs:53-67`) uses `LIKE lower(p.model) || '-%'` which fails when the row starts with a provider segment. Result: `cost_source='unpriced'` for every provider-prefixed row despite registry having matching pricing. Spec §5.7 R1.

**Files:**
- Modify: `crates/spur-context/src/engine.rs` (extract_opencode_rows around line 714, plus a small helper).

### - [ ] Step A.1.1: Write the failing test

In `crates/spur-context/src/engine.rs` test module, add:

```rust
#[cfg(all(test, feature = "duckdb"))]
#[test]
fn strip_provider_prefix_handles_known_providers() {
    use super::strip_provider_prefix;
    assert_eq!(strip_provider_prefix("anthropic/claude-opus-4-5"), "claude-opus-4-5");
    assert_eq!(strip_provider_prefix("google/gemini-2.5-pro"), "gemini-2.5-pro");
    assert_eq!(strip_provider_prefix("openai/gpt-5"), "gpt-5");
    assert_eq!(strip_provider_prefix("z-ai/glm-4.6"), "glm-4.6");
    assert_eq!(strip_provider_prefix("moonshotai/kimi-k2"), "kimi-k2");
    // Already unprefixed — pass through.
    assert_eq!(strip_provider_prefix("claude-opus-4-5"), "claude-opus-4-5");
    assert_eq!(strip_provider_prefix("gpt-5-codex"), "gpt-5-codex");
    // Empty / edge cases.
    assert_eq!(strip_provider_prefix(""), "");
    assert_eq!(strip_provider_prefix("/leading-slash"), "leading-slash");
    // Multiple slashes — only first segment strips.
    assert_eq!(strip_provider_prefix("a/b/c"), "b/c");
}
```

Run: `cargo test -p spur-context --features duckdb strip_provider_prefix_handles_known_providers` — expect compile error (function doesn't exist).

### - [ ] Step A.1.2: Implement `strip_provider_prefix`

In `crates/spur-context/src/engine.rs`, near the top of the `impl AnalyticsEngine` block (or as a free fn just above it):

```rust
/// Strip a leading `<segment>/` from a model id.
///
/// OpenCode (and any future router-style agent) stores model strings as
/// `<provider>/<canonical>`. The pricing registry keys on the canonical name,
/// so we strip the provider prefix at extraction time rather than threading
/// the prefix into the pricing lookup. Only the first slash is consumed —
/// nested model names (which don't currently exist) would round-trip through
/// the second-segment lookup unchanged.
fn strip_provider_prefix(s: &str) -> &str {
    s.split_once('/').map_or(s, |(_, rest)| rest)
}
```

Run: `cargo test -p spur-context --features duckdb strip_provider_prefix_handles_known_providers` — expect green.

### - [ ] Step A.1.3: Apply the strip at OpenCode extraction

In `crates/spur-context/src/engine.rs::extract_opencode_rows` (around line 714), find the model assignment:

```rust
let model = data.get("modelID").and_then(|v| v.as_str()).map(|s| s.to_string());
```

Replace with:

```rust
let model = data
    .get("modelID")
    .and_then(|v| v.as_str())
    .map(|s| Self::strip_provider_prefix(s).to_string());
```

(Note: if `strip_provider_prefix` was defined as a free fn, use the bare name; if as an associated fn, use `Self::`.)

### - [ ] Step A.1.4: Add an integration assertion

In `crates/spur-context/src/engine.rs` test module, extend the existing OpenCode test (`test_opencode_events_from_sqlite_fixture`) with one new assertion that a row whose raw `modelID` was `anthropic/claude-opus-4-5` is stored as `claude-opus-4-5`:

```rust
let stored_model: Option<String> = engine.conn
    .query_row(
        "SELECT model FROM opencode_events WHERE session_id = ? LIMIT 1",
        params![FIXTURE_SESSION_ID_WITH_ANTHROPIC_PREFIX],
        |row| row.get(0),
    )
    .optional()?;
assert_eq!(stored_model.as_deref(), Some("claude-opus-4-5"));
```

If the existing fixture doesn't have an `anthropic/`-prefixed session, modify the fixture's SQL `INSERT` to include one `data` blob with `"modelID": "anthropic/claude-opus-4-5"`.

### - [ ] Step A.1.5: Run full test suite for spur-context

```bash
cd /Volumes/Projects/spur/.worktrees/insights-substrate-harvest
cargo test -p spur-context --features duckdb
```

Expect: all tests pass.

### - [ ] Step A.1.6: Commit

```bash
git add crates/spur-context/src/engine.rs
git commit -m "fix(spur-context): strip provider prefix from OpenCode modelIDs (R1)

OpenCode stores model strings as <provider>/<canonical> (anthropic/...,
google/..., z-ai/..., moonshotai/...). The longest-prefix LATERAL join
in all_events_with_cost matches against canonical names without the
provider segment, so prefixed rows surfaced as cost_source='unpriced'
despite registry matches existing.

Strips the first segment at extraction time; pricing match is now
correct for OpenCode-routed models from any provider.

Spec: docs/superpowers/specs/2026-04-28-tui-insights-view-design.md §5.7 R1"
```

## Task A.2: R2 — Kimi pricing entry

**Context:** Kimi's harvested ingester emits `model = "kimi-for-coding"` (hardcoded — Kimi is single-model). `PricingRegistry::with_builtin_prices()` has no entry for it, so every Kimi event surfaces as `cost_source='unpriced'`. The Insights view's "data quality" pill would show 100% Kimi traffic as unpriced. Spec §5.7 R2.

**Files:**
- Modify: `crates/spur-cost/src/pricing.rs::with_builtin_prices` around line 83-246.

### - [ ] Step A.2.1: Research current Kimi pricing

Open the Moonshot AI primary docs and find the `kimi-for-coding` price card. Record:
- Input price per 1M tokens
- Output price per 1M tokens
- Cache read price per 1M tokens (if differentiated; if not, set equal to input)
- Cache creation price per 1M tokens (if not exposed: 0.0)

If primary pricing is not yet public at implementation time, use placeholder `0.0` for all four fields and add a `// TODO: confirm from primary source` comment. The CASE expression in `all_events_with_cost` keys on `pricing.model IS NOT NULL`, not on price > 0, so `cost_source='priced'` will display $0.00 (correct) until the values are filled.

### - [ ] Step A.2.2: Write the failing test

In `crates/spur-cost/src/pricing.rs` test module:

```rust
#[test]
fn pricing_registry_includes_kimi_for_coding() {
    let pricing = PricingRegistry::with_builtin_prices();
    let entry = pricing.get("kimi-for-coding");
    assert!(entry.is_some(), "kimi-for-coding must be registered");
}
```

Run: `cargo test -p spur-cost pricing_registry_includes_kimi_for_coding` — expect failure (entry missing).

### - [ ] Step A.2.3: Add the registry entry

In `crates/spur-cost/src/pricing.rs::with_builtin_prices`, find the section that adds Anthropic / OpenAI / Google models (around line 85-200). After the Google block (or alphabetically among the providers), add:

```rust
// Moonshot AI — Kimi
registry.add(ModelPricing {
    model: "kimi-for-coding".into(),
    input_per_mtok: KIMI_INPUT_PRICE,    // TODO: confirm from primary source
    output_per_mtok: KIMI_OUTPUT_PRICE,  // TODO: confirm from primary source
    cache_read_per_mtok: KIMI_INPUT_PRICE,
    cache_create_per_mtok: 0.0,
});
```

Define `KIMI_INPUT_PRICE` / `KIMI_OUTPUT_PRICE` as `const f64` near the top of the file with `0.0` placeholders unless researched values are confirmed.

### - [ ] Step A.2.4: Verify

```bash
cargo test -p spur-cost
cargo test -p spur-context --features duckdb test_cost_source_column_values
```

Both must pass.

### - [ ] Step A.2.5: Commit

```bash
git add crates/spur-cost/src/pricing.rs
git commit -m "feat(spur-cost): register kimi-for-coding model pricing (R2)

Without this entry, every Kimi event surfaced as cost_source='unpriced'
despite token data being correctly extracted by the harvested Kimi
JSONL ingester. Pricing values are placeholder until confirmed from
Moonshot AI primary docs; cost_source becomes 'priced' immediately,
which is the correct provenance regardless of price magnitude.

Spec: docs/superpowers/specs/2026-04-28-tui-insights-view-design.md §5.7 R2"
```

## Task A.3: R3 — OpenCode SQLite mtime in `newest_agent_mtime`

**Context:** `newest_agent_mtime()` (`engine.rs:204-226`) walks `[claude_dir, codex_dir, kiro_dir, kimi_dir]` for JSONL mtimes. OpenCode is a single SQLite file at `~/.local/share/opencode/opencode.db`, not a directory. `refresh_cache()` therefore can't detect OpenCode-only staleness; OpenCode-only users get a permanently-fresh-from-its-perspective cache despite real DB updates. Spec §5.7 R3.

**Files:**
- Modify: `crates/spur-context/src/engine.rs::newest_agent_mtime` around line 204-226.

### - [ ] Step A.3.1: Add `filetime` dev-dep if not present

```bash
cd /Volumes/Projects/spur/.worktrees/insights-substrate-harvest
cargo metadata --format-version 1 | grep '"name":"filetime"' || \
  cargo add --dev filetime --package spur-context
```

(Required for the test; production code uses only `std::fs::metadata`.)

### - [ ] Step A.3.2: Write the failing test

In `crates/spur-context/src/engine.rs` test module:

```rust
#[cfg(all(test, feature = "duckdb"))]
#[test]
fn newest_agent_mtime_detects_opencode_db_changes() {
    use filetime::FileTime;
    use std::time::SystemTime;
    let tmp = TempDir::new().unwrap();
    // Set up an opencode.db at a known location and point env at it.
    let opencode_dir = tmp.path().join(".local/share/opencode");
    std::fs::create_dir_all(&opencode_dir).unwrap();
    let opencode_db = opencode_dir.join("opencode.db");
    std::fs::write(&opencode_db, b"").unwrap();
    std::env::set_var("OPENCODE_DATA_DIR", &opencode_dir);
    // Read first mtime.
    let first = AnalyticsEngine::newest_agent_mtime().unwrap();
    // Bump mtime by 60 seconds.
    let now = SystemTime::now();
    let bumped = now + std::time::Duration::from_secs(60);
    filetime::set_file_mtime(&opencode_db, FileTime::from_system_time(bumped)).unwrap();
    let second = AnalyticsEngine::newest_agent_mtime().unwrap();
    assert!(second > first, "expected newest_agent_mtime to detect OpenCode DB mtime change");
    std::env::remove_var("OPENCODE_DATA_DIR");
}
```

(Use `ENV_LOCK` if other tests in the module mutate env — check existing precedent in `engine.rs` and `real_fixtures.rs`.)

Run: `cargo test -p spur-context --features duckdb newest_agent_mtime_detects_opencode_db_changes` — expect failure.

### - [ ] Step A.3.3: Implement the fix

In `crates/spur-context/src/engine.rs::newest_agent_mtime`, after the existing JSONL-dir loop:

```rust
// OpenCode is a single SQLite file, not a directory walk.
let opencode_db = Self::discover_opencode_db();
if opencode_db.is_file() {
    if let Ok(meta) = std::fs::metadata(&opencode_db) {
        if let Ok(m) = meta.modified() {
            bump(m);
        }
    }
}
```

Verify `discover_opencode_db` already returns the right path (it does — used in `create_opencode_view`).

Run: `cargo test -p spur-context --features duckdb newest_agent_mtime_detects_opencode_db_changes` — expect green.

### - [ ] Step A.3.4: Run full test suite

```bash
cargo test -p spur-context --features duckdb
```

### - [ ] Step A.3.5: Commit

```bash
git add crates/spur-context/Cargo.toml crates/spur-context/src/engine.rs
git commit -m "fix(spur-context): include OpenCode SQLite mtime in cache staleness check (R3)

newest_agent_mtime() iterated only JSONL dirs (claude/codex/kiro/kimi).
OpenCode-only users got permanently-fresh cache from refresh_cache()'s
perspective despite real DB updates. Adds the single-file mtime check
so OpenCode DB changes correctly invalidate the cache.

Spec: docs/superpowers/specs/2026-04-28-tui-insights-view-design.md §5.7 R3"
```

## Task A.4: PR A finalize

### - [ ] Step A.4.1: Verify clean build matrix

```bash
cargo check -p spur-context
cargo check -p spur-context --features duckdb
cargo check -p spur-cost
cargo check -p spur-cli
cargo test -p spur-context --features duckdb
cargo test -p spur-cost
```

All must pass.

### - [ ] Step A.4.2: Open PR

```bash
git push -u origin feat/insights-substrate-pr-a
gh pr create --title "feat(insights): substrate fixes (R1, R2, R3)" \
  --body "$(cat <<'EOF'
## Summary
- R1: strip provider prefix from OpenCode modelIDs so pricing match works
- R2: register kimi-for-coding pricing entry
- R3: include OpenCode SQLite mtime in cache staleness check

Substrate work for the experimental TUI Insights view (default OFF). No
TUI changes in this PR — pure data-layer fixes that benefit the existing
\`spur cost\` CLI as well.

## Test plan
- [ ] cargo test -p spur-context --features duckdb passes
- [ ] cargo test -p spur-cost passes
- [ ] Manual: \`spur cost daily\` shows OpenCode-via-Anthropic rows as priced (was unpriced)
- [ ] Manual: \`spur cost daily\` shows Kimi rows as priced
- [ ] Manual: edit ~/.local/share/opencode/opencode.db, run spur cost, observe cache invalidation

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

# PR B — Gemini JSON Extractor (R4)

**Branch from:** PR A merge OR (if PR A is still in review) directly from `feat/insights-substrate-harvest` HEAD.
**Branch name:** `feat/insights-substrate-pr-b`.
**Estimated size:** ~150 LoC + 3 tests + fixtures.
**Reviewer hint:** verify against spec §5.7 R4 AND `docs/superpowers/plans/2026-04-24-onboard-kimi-gemini-opencode-cost.md` Step 3.

This PR creates the `crates/spur-context/src/extractors/` submodule for the first time. The harvested Kimi code lives inline in `engine.rs`; we do NOT move Kimi here in this PR (it would create a large diff that's unrelated to Gemini). A future cleanup PR can move Kimi into `extractors/kimi.rs`.

## Task B.1: Create extractors submodule

### - [ ] Step B.1.1: Create `extractors/mod.rs`

Path: `crates/spur-context/src/extractors/mod.rs`

```rust
//! Per-agent extractors that convert native storage formats into DuckDB-appendable rows.
//!
//! Currently houses Gemini's JSON-document parser. Future PRs may move
//! Kimi (JSONL pre/post pairing, currently inline in `engine.rs`) and
//! OpenCode (SQLite via rusqlite, currently inline) into this module.

use chrono::{DateTime, Utc};

#[cfg(feature = "duckdb")]
pub mod gemini;

/// Shape every extractor produces. Matches the per-agent table schema
/// in `engine.rs` so the appender call site is uniform.
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
```

### - [ ] Step B.1.2: Register submodule in `lib.rs`

In `crates/spur-context/src/lib.rs`, add after existing mod declarations:

```rust
mod extractors;
```

Keep it private — extractors are internal implementation.

## Task B.2: Gemini extractor

**Context:** Gemini CLI stores transcripts at `~/.gemini/tmp/<uuid>/chats/session-YYYY-MM-DDTHH-MM-<hash>.json`. Each file is a single JSON document with top-level `{ sessionId, projectHash, startTime, lastUpdated, messages[], kind }`. Each `messages[]` entry where `type == "gemini"` has `{ id, timestamp, content, model, tokens: { input, output, cached, thoughts, tool, total }, toolCalls?, thoughts? }`. Verified on dev machine 2026-04-28 against session 959c6910.

**Folding rules** (per spec §5.7 R4 and onboard plan Step 3):
- `input_tokens = tokens.input + tokens.tool` (tool tokens are model-context input)
- `output_tokens = tokens.output + tokens.thoughts` (thinking tokens bill at output rate)
- `cache_read_tokens = tokens.cached`
- `cache_creation_tokens = 0` (Gemini does not expose cache-creation separately)
- `cost_usd = None` (no per-message cost in transcript; rely on `gemini-2.5-pro` / `gemini-2.5-flash` pricing match)

### - [ ] Step B.2.1: Create the synthetic fixture

Path: `crates/spur-context/tests/fixtures/gemini/two_session_synthetic/9c90babd-aaaa-bbbb-cccc-ddddddddddd1/chats/session-2026-04-28T01-00-aaaaaaaa.json`

```json
{
  "sessionId": "9c90babd-aaaa-bbbb-cccc-ddddddddddd1",
  "projectHash": "abc123",
  "startTime": "2026-04-28T01:00:00Z",
  "lastUpdated": "2026-04-28T01:05:00Z",
  "kind": "chat",
  "messages": [
    {"id": "m1", "timestamp": "2026-04-28T01:00:00Z", "type": "user", "content": "hello"},
    {
      "id": "m2",
      "timestamp": "2026-04-28T01:01:00Z",
      "type": "gemini",
      "content": "hi",
      "model": "gemini-2.5-pro",
      "tokens": {"input": 100, "output": 20, "cached": 0, "thoughts": 5, "tool": 0, "total": 125}
    },
    {
      "id": "m3",
      "timestamp": "2026-04-28T01:02:00Z",
      "type": "gemini",
      "content": "second",
      "model": "gemini-2.5-pro",
      "tokens": {"input": 200, "output": 30, "cached": 80, "thoughts": 10, "tool": 5, "total": 325}
    }
  ]
}
```

### - [ ] Step B.2.2: Write the failing extractor test

Path: `crates/spur-context/src/extractors/gemini.rs`

```rust
//! Gemini CLI JSON-document extractor.
//!
//! Each Gemini session is one JSON file at
//! `~/.gemini/tmp/<uuid>/chats/session-*.json` containing `messages[]`. We
//! iterate messages and emit one row per `type:"gemini"` entry, folding
//! `thoughts` into output (priced at output rate per Google's published
//! pricing for 2.5/3.x Pro) and `tool` into input.

use super::ExtractedRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
struct SessionDoc {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(rename = "projectHash")]
    project_hash: Option<String>,
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: String,
    model: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Deserialize, Default)]
struct Tokens {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(default)]
    cached: i64,
    #[serde(default)]
    thoughts: i64,
    #[serde(default)]
    tool: i64,
}

/// Extract all Gemini session chat files under `tmp_root`.
///
/// `tmp_root` is `~/.gemini/tmp` — direct parent of per-session UUID dirs.
/// Recursively walks `<uuid>/chats/session-*.json` files.
pub fn extract(tmp_root: &Path) -> Result<Vec<ExtractedRow>> {
    let mut out = Vec::new();
    if !tmp_root.is_dir() {
        return Ok(out);
    }
    for entry in fs::read_dir(tmp_root)? {
        let entry = entry?;
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let chats_dir = session_dir.join("chats");
        if !chats_dir.is_dir() {
            continue;
        }
        for chat_entry in fs::read_dir(&chats_dir)? {
            let chat_entry = chat_entry?;
            let path = chat_entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            extract_file(&path, &mut out)
                .with_context(|| format!("failed to extract {}", path.display()))?;
        }
    }
    Ok(out)
}

fn extract_file(path: &Path, out: &mut Vec<ExtractedRow>) -> Result<()> {
    let bytes = fs::read(path)?;
    let doc: SessionDoc = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", path.display()))?;
    for m in &doc.messages {
        if m.kind != "gemini" {
            continue;
        }
        let tokens = m.tokens.as_ref().cloned().unwrap_or_default();
        let timestamp = m.timestamp.as_ref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        out.push(ExtractedRow {
            timestamp,
            session_id: doc.session_id.clone(),
            model: m.model.clone(),
            project: doc.project_hash.clone(),
            input_tokens: tokens.input + tokens.tool,
            output_tokens: tokens.output + tokens.thoughts,
            cache_read_tokens: tokens.cached,
            cache_creation_tokens: 0,
            cost_usd: None,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gemini/two_session_synthetic")
    }

    #[test]
    fn extract_synthetic_session() {
        let rows = extract(&fixture_dir()).unwrap();
        assert_eq!(rows.len(), 2, "two gemini messages in fixture");
        let r0 = &rows[0];
        assert_eq!(r0.session_id, "9c90babd-aaaa-bbbb-cccc-ddddddddddd1");
        assert_eq!(r0.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(r0.project.as_deref(), Some("abc123"));
        assert_eq!(r0.input_tokens, 100); // input(100) + tool(0)
        assert_eq!(r0.output_tokens, 25); // output(20) + thoughts(5)
        assert_eq!(r0.cache_read_tokens, 0);
        let r1 = &rows[1];
        assert_eq!(r1.input_tokens, 205); // input(200) + tool(5)
        assert_eq!(r1.output_tokens, 40); // output(30) + thoughts(10)
        assert_eq!(r1.cache_read_tokens, 80);
        assert!(r0.cost_usd.is_none() && r1.cost_usd.is_none());
    }
}
```

Run: `cargo test -p spur-context --features duckdb extractors::gemini` — expect green (test should pass once fixture is in place).

### - [ ] Step B.2.3: Add a multi-file dedup test

If the fixture has only one session subdirectory, add a second one (`9c90babd-eeee-ffff-aaaa-bbbbbbbbbbb2/chats/session-2026-04-28T02-00-bbbbbbbb.json`) with one `type:"gemini"` message. Add a test asserting `extract` returns 3 total rows across the two sessions.

### - [ ] Step B.2.4: Add the smoke test

```rust
#[test]
#[ignore]
fn smoke_real_gemini_dir() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return; };
    let tmp = home.join(".gemini/tmp");
    if !tmp.is_dir() { return; }
    let rows = extract(&tmp).unwrap();
    assert!(rows.len() > 0, "expected real Gemini sessions on this dev machine");
    let total_input: i64 = rows.iter().map(|r| r.input_tokens).sum();
    let total_output: i64 = rows.iter().map(|r| r.output_tokens).sum();
    eprintln!("gemini smoke: rows={} input={} output={}", rows.len(), total_input, total_output);
}
```

Run: `cargo test -p spur-context --features duckdb smoke_real_gemini_dir -- --ignored` (manual; verifies extractor against real dev data).

## Task B.3: Wire Gemini into AnalyticsEngine

### - [ ] Step B.3.1: Add discovery and view creation in `engine.rs`

In `crates/spur-context/src/engine.rs`, add a `discover_gemini_dir` next to `discover_kimi_dir` (around line 613):

```rust
/// Discover the Gemini sessions directory.
///
/// Probe order: `$GEMINI_HOME/tmp` → `~/.gemini/tmp`.
fn discover_gemini_dir() -> PathBuf {
    if let Ok(path) = env::var("GEMINI_HOME") {
        return PathBuf::from(path).join("tmp");
    }
    #[cfg(test)]
    { PathBuf::from("__spur_context_test_missing__/gemini") }
    #[cfg(not(test))]
    {
        directories::BaseDirs::new()
            .map(|b| b.home_dir().join(".gemini/tmp"))
            .unwrap_or_else(|| PathBuf::from("~/.gemini/tmp"))
    }
}
```

### - [ ] Step B.3.2: Implement `create_gemini_view`

After `create_kimi_view` (around line 734-806), add:

```rust
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

    let rows = crate::extractors::gemini::extract(tmp_root)
        .with_context(|| format!("failed to extract gemini sessions at {}", tmp_root.display()))?;

    if !rows.is_empty() {
        let mut appender = self.conn.appender("gemini_events_table")
            .context("failed to open gemini_events_table appender")?;
        for r in &rows {
            appender.append_row(params![
                r.timestamp.timestamp_millis(),
                r.session_id,
                "gemini",
                r.model,
                r.project,
                r.input_tokens,
                r.output_tokens,
                r.cache_read_tokens,
                r.cache_creation_tokens,
                r.cost_usd,
            ]).context("failed to append gemini row")?;
        }
        appender.flush().context("failed to flush gemini appender")?;
    }

    self.conn.execute_batch(
        r#"
        CREATE OR REPLACE VIEW gemini_events AS
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
        FROM gemini_events_table;
        "#,
    )?;
    Ok(())
}
```

### - [ ] Step B.3.3: Wire into `create_agent_views`

In `create_agent_views()` (around line 280-324), after the Kimi block, add:

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

### - [ ] Step B.3.4: Add `gemini` field to `AgentViewStatus`

In `crates/spur-context/src/engine.rs` around line 1661-1667 (the `AgentViewStatus` struct), add:

```rust
pub struct AgentViewStatus {
    pub claude: bool,
    pub codex: bool,
    pub kiro: bool,
    pub opencode: bool,
    pub kimi: bool,
    pub gemini: bool,  // NEW
}
```

Update `Default` impl if explicit (or rely on derived `Default`).

### - [ ] Step B.3.5: Add Gemini to `rebuild_unified_views`

Around `engine.rs:1031-1040`, the UNION ALL list, add `kimi_events UNION ALL` is already there — append `SELECT * FROM gemini_events`. Order: claude, codex, kiro, opencode, kimi, gemini.

### - [ ] Step B.3.6: Add Gemini mtime to `newest_agent_mtime`

In `engine.rs:204-226`, after the JSONL-dir loop, add the Gemini walk. Gemini uses `.json` extension (not `.jsonl`), so the existing `find_jsonl_files` won't match. Add a sibling helper `find_files_with_ext(dir, ext)` (per the onboard plan Step 3.5) and use it for Gemini:

```rust
// Gemini uses .json — separate walk.
if let Ok(files) = Self::find_files_with_ext(&Self::discover_gemini_dir(), "json") {
    for f in files {
        if let Ok(meta) = std::fs::metadata(&f) {
            if let Ok(m) = meta.modified() { bump(m); }
        }
    }
}
```

Helper (place next to `find_jsonl_files`):

```rust
fn find_files_with_ext(dir: &Path, ext: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() { return Ok(files); }
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

### - [ ] Step B.3.7: Verify

```bash
cargo test -p spur-context --features duckdb
cargo test -p spur-context --features duckdb extractors::gemini
cargo test -p spur-context --features duckdb smoke_real_gemini_dir -- --ignored  # manual
```

### - [ ] Step B.3.8: Commit

```bash
git add crates/spur-context/src/extractors/ \
        crates/spur-context/src/engine.rs \
        crates/spur-context/src/lib.rs \
        crates/spur-context/tests/fixtures/gemini/
git commit -m "feat(spur-context): Gemini JSON cost ingest with thoughts->output fold (R4)

Implements the Gemini section of the 2026-04-24 onboarding plan that was
designed but never built. Reads ~/.gemini/tmp/<uuid>/chats/session-*.json
single-document JSONs, emits one row per type='gemini' message. Folds
'thoughts' tokens into output (Google prices thinking at output rate)
and 'tool' tokens into input (tool-response context). cached -> cache_read.

Adds first src/extractors/ submodule. Future cleanup PR can move Kimi
out of engine.rs into extractors/kimi.rs.

Spec: docs/superpowers/specs/2026-04-28-tui-insights-view-design.md §5.7 R4
Plan: docs/superpowers/plans/2026-04-24-onboard-kimi-gemini-opencode-cost.md Step 3"
```

## Task B.4: PR B finalize

### - [ ] Step B.4.1: Open PR

```bash
git push -u origin feat/insights-substrate-pr-b
gh pr create --title "feat(insights): Gemini JSON extractor (R4)" \
  --body "$(cat <<'EOF'
## Summary
- Adds crates/spur-context/src/extractors/ submodule.
- Implements Gemini JSON extractor per the 2026-04-24 onboarding plan (Step 3) that was designed but never built.
- Wires gemini_events view into create_agent_views, rebuild_unified_views, newest_agent_mtime.

## Test plan
- [ ] cargo test -p spur-context --features duckdb passes
- [ ] cargo test smoke_real_gemini_dir -- --ignored passes (verifies against real ~/.gemini/tmp)
- [ ] Manual: spur cost daily shows gemini-2.5-pro rows with priced cost_source

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

# PR C — Insights View + Dashboard Switch + Wiring + CI

**Branch from:** PR A + PR B merged.
**Branch name:** `feat/insights-view-pr-c`.
**Estimated size:** ~1 700 LoC + tests.
**Reviewer hint:** the bulk of this PR is gated behind `analytics` feature; the unfeatured-build delta is small (ViewId/Action enum additions, stub View, status-bar arm).

## Task C.1: Cargo feature plumbing

### - [ ] Step C.1.1: Add `analytics` feature to spur-tui

`crates/spur-tui/Cargo.toml`:

```toml
[features]
default = ["markdown"]   # NOT including analytics
markdown = [...]          # existing
analytics = ["dep:spur-context", "spur-context/duckdb"]

[dependencies]
spur-context = { workspace = true, optional = true }
```

### - [ ] Step C.1.2: Verify both build configs

```bash
cargo check -p spur-tui --no-default-features
cargo check -p spur-tui --features analytics
```

Both must pass before proceeding.

## Task C.2: ViewId/Action enum additions

### - [ ] Step C.2.1: Add `ViewId::Insights`

In `crates/spur-tui/src/action.rs`, add the variant. **Always present**, no `#[cfg]`.

```rust
pub enum ViewId {
    Dashboard,
    SessionDetail,
    /* ...existing... */
    Insights,
}
```

### - [ ] Step C.2.2: Add `Action::OpenInsights`

Same file:

```rust
pub enum Action {
    /* ...existing... */
    OpenInsights,
}
```

### - [ ] Step C.2.3: Update exhaustive matches

Compiler will emit non-exhaustive errors at the 3 sites flagged in spec §5.9:

- `crates/spur-tui/src/app.rs:940` (route action → view) — add `Action::OpenInsights => self.push_view(ViewId::Insights)`
- `crates/spur-tui/src/app.rs:1536` (view dispatch on key) — add `ViewId::Insights => self.insights_view.handle_key(key)`
- `crates/spur-tui/src/components/status_bar.rs:195` (label per view) — add `ViewId::Insights => "Insights"`

(Use `cargo check -p spur-tui --no-default-features` to find any other match sites the spec missed.)

### - [ ] Step C.2.4: Add `Alt+i` keybinding

In the global key-handler (search for `KeyCode::Char(_) if mods == ALT` precedent in `app.rs`), add:

```rust
KeyCode::Char('i') if mods.contains(KeyModifiers::ALT) => {
    self.dispatch_action(Action::OpenInsights);
}
```

## Task C.3: Insights View module skeleton

### - [ ] Step C.3.1: Module tree

Create empty stubs (no logic yet) for all files under `crates/spur-tui/src/views/insights/`:

```
mod.rs
state.rs
builder.rs
refresh.rs
tabs/
  mod.rs
  overview.rs
  timeline.rs
  breakdown.rs
  live.rs
widgets/
  mod.rs
  kpi_strip.rs
  sparkline.rs
```

Each starts as `// stub — see plan task C.X` and a minimal `pub struct/fn` so `cargo check --features analytics` passes.

### - [ ] Step C.3.2: Register in `views/mod.rs`

```rust
#[cfg(feature = "analytics")]
pub mod insights;

#[cfg(not(feature = "analytics"))]
pub mod insights {
    use super::View;
    pub struct InsightsView;
    impl InsightsView { pub fn new() -> Self { Self } }
    impl View for InsightsView {
        fn render(&mut self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect, _ctx: &super::ViewContext) {
            // Render "feature disabled — rebuild with --features analytics" splash.
            // ... (use ratatui::widgets::Paragraph with centered text)
        }
        // other View trait methods: no-op
    }
}
```

### - [ ] Step C.3.3: Verify both build configs still pass

```bash
cargo check -p spur-tui --no-default-features
cargo check -p spur-tui --features analytics
```

## Task C.4: Snapshot types + builder

### - [ ] Step C.4.1: Define types in `state.rs`

Per spec §5.5:

```rust
// crates/spur-tui/src/views/insights/state.rs
use chrono::{DateTime, Utc};
use spur_context::{
    AgentViewStatus, DailyRow, LiveBlockRow, ModelRow, MonthlyRow, ProjectRow, WeeklyRow,
};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct InsightsSnapshot {
    pub fetched_at: DateTime<Utc>,
    pub queries: AtomicQueries,
    pub kpis: Kpis,
    pub agent_status: AgentViewStatus,
    pub engine_meta: EngineMeta,
}

#[derive(Debug, Clone)]
pub struct AtomicQueries {
    pub daily_90: Vec<DailyRow>,
    pub weekly_12: Vec<WeeklyRow>,
    pub monthly_6: Vec<MonthlyRow>,
    pub by_agent_30d: Vec<DailyRow>,
    pub by_model_30d: Vec<ModelRow>,
    pub by_project_30d: Vec<ProjectRow>,
    pub live_30min: Vec<LiveBlockRow>,
}

#[derive(Debug, Clone, Default)]
pub struct Kpis {
    pub today_cost: f64,
    pub last_7d_cost: f64,
    pub last_30d_cost: f64,
    pub mtd_cost: f64,
    pub active_session_count: usize,
    pub cache_hit_pct: f64,
    pub cost_source_split: CostSourceSplit,
    pub top_agent: Option<(String, f64)>,
    pub top_model: Option<(String, f64)>,
}

#[derive(Debug, Clone, Default)]
pub struct CostSourceSplit { pub native_pct: f64, pub priced_pct: f64, pub unpriced_pct: f64 }

#[derive(Debug, Clone, Default)]
pub struct EngineMeta { pub events_cache_rows: i64, pub last_refresh: DateTime<Utc>, pub agent_view_count: usize }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsightsTab { Overview, Timeline, Breakdown, Live }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Granularity { Daily, Weekly, Monthly }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension { Agent, Model, Project }

#[derive(Default)]
pub struct RefreshState {
    pub last_good: Option<InsightsSnapshot>,
    pub last_error: Option<Arc<anyhow::Error>>,
    pub refreshing: bool,
}
```

### - [ ] Step C.4.2: Implement `build_snapshot` in `builder.rs`

Per spec §5.4 — single `AsyncEngine::run` closure:

```rust
// crates/spur-tui/src/views/insights/builder.rs
use super::state::*;
use anyhow::Result;
use chrono::Utc;
use spur_context::AsyncEngine;

pub async fn build_snapshot(engine: &AsyncEngine) -> Result<InsightsSnapshot> {
    let queries = engine.run(|e| -> Result<AtomicQueries> {
        e.refresh_cache()?;
        e.use_cached_events()?;
        Ok(AtomicQueries {
            daily_90: e.daily_report(90)?,
            weekly_12: e.weekly_report(12)?,
            monthly_6: e.monthly_report(6)?,
            by_agent_30d: e.daily_report(30)?,
            by_model_30d: e.model_breakdown()?,
            by_project_30d: e.project_breakdown()?,
            live_30min: e.live_recent_sessions(30)?,
        })
    }).await?;
    let kpis = derive_kpis(&queries);
    Ok(InsightsSnapshot {
        fetched_at: Utc::now(),
        queries,
        kpis,
        agent_status: AgentViewStatus::default(), // TODO: expose via AsyncEngine
        engine_meta: EngineMeta { last_refresh: Utc::now(), ..Default::default() },
    })
}

fn derive_kpis(q: &AtomicQueries) -> Kpis {
    // Pure-function derivation. See state.rs Kpis struct fields.
    // ... implementation with .iter().sum() / .iter().fold()
    todo!()
}
```

### - [ ] Step C.4.3: Test `derive_kpis` with hand-built rows

The DTOs are not duckdb-feature-gated (verified during spec authoring at engine.rs:1462+). Construct hand-built `DailyRow`, `ModelRow`, `ProjectRow` and assert each Kpi field.

```rust
// crates/spur-tui/src/views/insights/state.rs (test module)
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use spur_context::DailyRow;

    fn day(date: &str, cost: f64) -> DailyRow {
        // construct minimal DailyRow per public fields
        todo!()
    }

    #[test]
    fn derive_kpis_today_and_7d_sums() {
        let q = AtomicQueries {
            daily_90: vec![day("2026-04-28", 4.21), day("2026-04-27", 5.10), /* ... */],
            ..Default::default()  // requires Default impl on AtomicQueries
        };
        let k = derive_kpis(&q);
        assert!((k.today_cost - 4.21).abs() < 0.001);
        // ...
    }
}
```

## Task C.5: Refresh task

### - [ ] Step C.5.1: Implement `spawn_refresh_task` in `refresh.rs`

Per spec §5.4 + §5.10:

```rust
// crates/spur-tui/src/views/insights/refresh.rs
use super::builder::build_snapshot;
use super::state::*;
use anyhow::anyhow;
use spur_context::AsyncEngine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

pub fn spawn_refresh_task(
    engine: AsyncEngine,
    state: Arc<RwLock<RefreshState>>,
    is_live_tab: Arc<AtomicBool>,
    mut signal_rx: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let interval = if is_live_tab.load(Ordering::Relaxed) {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(60)
            };
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                opt = signal_rx.recv() => { if opt.is_none() { return; } }
            }
            { let mut s = state.write().await; s.refreshing = true; }
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                build_snapshot(&engine),
            ).await;
            let mut s = state.write().await;
            s.refreshing = false;
            match result {
                Ok(Ok(snap))   => { s.last_good = Some(snap); s.last_error = None; }
                Ok(Err(e))     => { s.last_error = Some(Arc::new(e)); }
                Err(_)         => { s.last_error = Some(Arc::new(anyhow!("refresh timed out (30s)"))); }
            }
        }
    })
}
```

### - [ ] Step C.5.2: Test refresh task end-to-end

In `refresh.rs` test module: spin up an `AsyncEngine` against a tempdir with a synthetic Claude JSONL (use the existing P0.1 fixture pattern), spawn the refresh task, send one signal, await `last_good.is_some()`. Use `tokio::test`.

## Task C.6: View struct + Drop

### - [ ] Step C.6.1: Implement `InsightsView` in `mod.rs`

Per spec §5.4 + §5.10:

```rust
// crates/spur-tui/src/views/insights/mod.rs
mod builder;
mod refresh;
pub mod state;
mod tabs;
mod widgets;

use super::{View, ViewContext};
use ratatui::{Frame, layout::Rect};
use spur_context::AsyncEngine;
use state::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tokio::task::JoinHandle;

pub struct InsightsView {
    engine: AsyncEngine,
    state: Arc<RwLock<RefreshState>>,
    is_live_tab: Arc<AtomicBool>,
    signal_tx: mpsc::Sender<()>,
    refresh_handle: Option<JoinHandle<()>>,
    active_tab: InsightsTab,
    granularity: Granularity,
    dimension: Dimension,
}

impl InsightsView {
    pub fn new(engine: AsyncEngine) -> Self {
        let state = Arc::new(RwLock::new(RefreshState::default()));
        let is_live_tab = Arc::new(AtomicBool::new(false));
        let (signal_tx, signal_rx) = mpsc::channel(8);
        let handle = refresh::spawn_refresh_task(
            engine.clone(), state.clone(), is_live_tab.clone(), signal_rx,
        );
        let _ = signal_tx.try_send(());  // initial refresh
        Self {
            engine,
            state,
            is_live_tab,
            signal_tx,
            refresh_handle: Some(handle),
            active_tab: InsightsTab::Overview,
            granularity: Granularity::Daily,
            dimension: Dimension::Agent,
        }
    }
}

impl Drop for InsightsView {
    fn drop(&mut self) {
        if let Some(h) = self.refresh_handle.take() { h.abort(); }
        // NOTE: per Tokio docs, abort STOPS WAITING but does NOT cancel the
        // in-flight spawn_blocking task. The blocking thread runs to
        // completion and its result is dropped when no receiver remains.
    }
}

impl View for InsightsView {
    fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
        // Per spec §5.4 render block.
        todo!()
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<super::Action> {
        // Tab cycle, 1-4 jump, a/m/p dimension, D/W/M granularity, r refresh, Esc back.
        // When active_tab changes, update is_live_tab.
        todo!()
    }
}
```

## Task C.7: Tab implementations

### - [ ] Step C.7.1: Overview tab

`crates/spur-tui/src/views/insights/tabs/overview.rs` — KPI cards row, sparkline row, top-3 panels per spec §5.6.1. Read from `&snapshot.queries.daily_90[60..90]` for sparkline; sort+take(3) for top-3 lists.

### - [ ] Step C.7.2: Timeline tab

Per spec §5.6.2. `ratatui::widgets::BarChart` with one bar per period. Re-derives bars from `daily_90 / weekly_12 / monthly_6` based on `granularity` — no re-query.

### - [ ] Step C.7.3: Breakdown tab

Per spec §5.6.3. Pivot `by_agent_30d / by_model_30d / by_project_30d` based on `dimension`. Render as `ratatui::widgets::Table` with columns Sessions / Tokens / Cost / Cost source. Yellow tag for unpriced rows.

### - [ ] Step C.7.4: Live tab

Per spec §5.6.4. Read `live_30min`. One row per session, with a horizontal `Gauge` for tokens-per-minute relative to running avg. Update `is_live_tab` when this tab activates so refresh interval drops to 5s.

### - [ ] Step C.7.5: Per-tab snapshot tests

Use `ratatui::backend::TestBackend`:

```rust
#[test]
fn overview_tab_renders_kpis_and_sparkline() {
    let snap = synthetic_snapshot_one_day();
    let mut backend = TestBackend::new(120, 30);
    let mut frame_buf = Buffer::empty(Rect::new(0, 0, 120, 30));
    OverviewTab::render(&mut frame_buf, snap_buf_area(), &snap);
    insta::assert_snapshot!(frame_buf.to_text());  // or manual content check
}
```

If `insta` is not in workspace, do plain string compare against expected lines.

## Task C.8: Widgets

### - [ ] Step C.8.1: kpi_strip

`crates/spur-tui/src/views/insights/widgets/kpi_strip.rs` — stateless `pub fn render_kpi_strip(frame, area, kpis)`. ~80 LoC.

### - [ ] Step C.8.2: sparkline

`crates/spur-tui/src/views/insights/widgets/sparkline.rs` — thin wrapper around `ratatui::widgets::Sparkline`. ~50 LoC.

## Task C.9: Dashboard cost-source switch

### - [ ] Step C.9.1: Add `LiveCostCache` to App

Per spec §5.8. In `crates/spur-tui/src/app.rs` (gated `#[cfg(feature = "analytics")]`):

```rust
#[cfg(feature = "analytics")]
pub struct LiveCostCache {
    pub by_session: HashMap<SessionId, f64>,
    pub last_refresh: chrono::DateTime<chrono::Utc>,
    pub last_error: Option<Arc<anyhow::Error>>,
}
```

App holds:

```rust
#[cfg(feature = "analytics")]
analytics_engine: Option<AsyncEngine>,
#[cfg(feature = "analytics")]
live_cost_cache: Option<Arc<RwLock<LiveCostCache>>>,
#[cfg(feature = "analytics")]
live_cost_handle: Option<JoinHandle<()>>,
```

### - [ ] Step C.9.2: Spawn live-cost refresh task at App start

Pseudo-loop: every 5s if any session active, every 30s otherwise; one batched `engine.run(|e| { for sid in active { e.live_session_snapshot(sid)? } })` per tick.

### - [ ] Step C.9.3: Wire `DashboardView::current_cost`

Per spec §5.8 code block. When feature on, read from `LiveCostCache`; fall through to lineage if cache cold or session missing.

### - [ ] Step C.9.4: Add "via analytics" pill to status bar

In `crates/spur-tui/src/components/status_bar.rs`, add a status segment that reads "via analytics" when `analytics` is enabled AND the displayed session is in `LiveCostCache.by_session`. Otherwise hidden.

### - [ ] Step C.9.5: Test dashboard cost source

```rust
#[cfg(feature = "analytics")]
#[test]
fn dashboard_reads_from_live_cost_cache_when_present() {
    let cache = Arc::new(RwLock::new(LiveCostCache {
        by_session: hashmap! { sid("abc") => 4.21 },
        last_refresh: Utc::now(),
        last_error: None,
    }));
    let dash = DashboardView::with_cache(cache);
    assert_eq!(dash.current_cost(&sid("abc")), Some(4.21));
}

#[cfg(feature = "analytics")]
#[test]
fn dashboard_falls_through_to_lineage_when_cache_cold() {
    let cache = Arc::new(RwLock::new(LiveCostCache::default()));
    let dash = DashboardView::with_cache_and_lineage(cache, mock_lineage_with_cost(2.10));
    assert_eq!(dash.current_cost(&sid("xyz")), Some(2.10));
}
```

## Task C.10: CI matrix

### - [ ] Step C.10.1: Add `--features analytics` job

In the workspace CI config (`.github/workflows/ci.yml` or similar), add a job:

```yaml
analytics:
  runs-on: macos-latest
  steps:
    - uses: actions/checkout@v4
    - uses: dtolnay/rust-toolchain@stable
    - run: cargo check -p spur-tui --features analytics
    - run: cargo test -p spur-tui --features analytics
```

Keep the existing default-features and `--no-default-features` jobs unchanged.

### - [ ] Step C.10.2: Verify CI green locally

```bash
cargo check -p spur-tui --no-default-features
cargo check -p spur-tui
cargo check -p spur-tui --features analytics
cargo test -p spur-tui --no-default-features
cargo test -p spur-tui
cargo test -p spur-tui --features analytics
```

All six must pass.

## Task C.11: PR C finalize

### - [ ] Step C.11.1: Manual smoke test

1. Build: `cargo build --bin spur --features spur-tui/analytics`
2. Run: `target/debug/spur` against this dev machine.
3. Press `Alt+i` — Insights view opens, KPI cards populate within ~2 seconds.
4. Cycle tabs with `Tab` — Timeline shows BarChart, Breakdown shows pivot, Live shows current sessions.
5. Press `r` — explicit refresh.
6. Press `Esc` — returns to previous view.
7. Open Dashboard view, observe "via analytics" pill in status bar with active session's cost.
8. Build without feature: `cargo build --bin spur` — `Alt+i` shows "feature disabled" splash; dashboard cost matches pre-feature behavior.

### - [ ] Step C.11.2: Update CHANGELOG (or release notes file)

Add a "## Unreleased" entry mentioning the experimental `--features analytics` flag and what it enables.

### - [ ] Step C.11.3: Open PR

```bash
git push -u origin feat/insights-view-pr-c
gh pr create --title "feat(spur-tui): Insights view + dashboard analytics integration" \
  --body "$(cat <<'EOF'
## Summary
- Adds 4-tab Insights view (Overview, Timeline, Breakdown, Live) feature-gated as \`analytics\` (default OFF).
- Switches dashboard cost segment to read from \`AsyncEngine::live_session_snapshot\` (via shared \`LiveCostCache\`) when analytics is enabled — single source of truth.
- Adds "via analytics" pill to status bar when feature is on.
- Adds CI job for \`--features analytics\` build.

Built on PR A (substrate fixes) and PR B (Gemini extractor). Five well-known
agents (Claude Code, Codex, Gemini, OpenCode, Kimi) are first-class in the
analytics surface; Kiro stays a stub pending Phase 2 ACP UsageUpdate capture.

## Test plan
- [ ] cargo check -p spur-tui --no-default-features passes
- [ ] cargo check -p spur-tui --features analytics passes
- [ ] cargo test -p spur-tui --features analytics passes
- [ ] Manual: Alt+i opens Insights view; KPI cards populate ≤5s
- [ ] Manual: Tab cycles through 4 tabs
- [ ] Manual: Dashboard "via analytics" pill appears for active session
- [ ] Manual: \`cargo build\` (no features) produces a binary identical in behavior to pre-feature

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

---

# Verification matrix (final, post-PR-C-merge)

After all 3 PRs merge, the following must hold:

| Verification | Command | Expected |
|---|---|---|
| Default build matches pre-feature | `cargo build --bin spur` | Binary size and behavior identical to pre-PR-C |
| Feature build adds Insights | `cargo build --bin spur --features spur-tui/analytics` | Binary slightly larger; `Alt+i` opens view |
| All P0 tests pass | `cargo test -p spur-context --features duckdb` | 0 failures |
| All cost tests pass | `cargo test -p spur-cost` | 0 failures |
| TUI tests pass (no feature) | `cargo test -p spur-tui --no-default-features` | 0 failures |
| TUI tests pass (with feature) | `cargo test -p spur-tui --features analytics` | 0 failures |
| Five well-known agents visible in Insights | manual TUI smoke | Claude Code, Codex, Gemini, OpenCode, Kimi appear in Breakdown's Agent dimension |
| Dashboard "via analytics" pill | manual TUI smoke | Pill visible when feature on + cache warm |
| Provider-prefixed OpenCode pricing match | manual `spur cost daily` | OpenCode-via-Anthropic rows show as priced (not unpriced) |

---

# Phase 2 backlog (NOT in scope of this plan)

For convenience — the spec §6 deferral list. None of these are required for Phase 1 to ship:

1. Forecast tab — MTD projection, anomaly z-score, cache-efficiency view.
2. Kiro ACP `UsageUpdate` capture — orchestrator hook + new `acp_usage_events` view.
3. R5 — orchestrator `end_session_with_tokens` wiring (cleanup; lineage path retired).
4. Stash@{7} agent-name normalization revival.
5. `spur cost-insights --json` CLI promotion.
6. Promote `analytics` to default ON once stable.

When picking up Phase 2, reference spec §6 and any tracking issues created during PR C review.
