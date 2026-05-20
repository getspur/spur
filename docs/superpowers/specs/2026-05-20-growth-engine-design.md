# growth-engine — Design Spec

**Status:** approved-for-implementation-planning
**Date:** 2026-05-20
**Author:** Claude (brain) + Kevin (founder), brainstormed via `superpowers:brainstorming`
**Scope:** α — internal-only subsystem for SPUR's own growth function, not a shipped product feature
**Substrate:** Shape C — Rust crate (`crates/spur-growth-engine`) for deterministic data layer + Claude skills + markdown for brain-driven synthesis
**Supersedes:** the implicit ad-hoc design embedded in `scripts/publish-to-buffer.mjs` (will be migrated and deleted)

---

## Executive Summary

SPUR is a Rust-native orchestrator for AI coding agents, pre-launch. Its founder is single-handedly running marketing, which today (2026-05-20) consists of three ad-hoc tools we shipped this session: the `growth-loop` Claude skill (daily content production), the `growth-loop-media` skill (image gen via codex worker), and `scripts/publish-to-buffer.mjs` (a 513-line Node script that pushes drafts to Buffer's GraphQL API).

The system produces content, but it has no feedback loop: outcomes aren't captured, strategy doesn't adapt, and the only "data" the brain consults is whatever it finds via WebSearch each morning. Posts are written, scheduled, published — and forgotten.

**growth-engine** closes the loop. It is a small Rust subsystem (`crates/spur-growth-engine`) that owns deterministic data harvesting and storage from social platforms (Buffer first, X/Threads forward-compatible), exposes typed query results to the brain via a CLI contract, and lets Claude skills produce both daily tactical proposals (data-aware content for tomorrow) and weekly strategic proposals (where to focus next week). The user (founder) approves all proposals; the system never publishes anything autonomously without prior human review.

This spec is the architectural contract before implementation. After approval, the writing-plans skill will turn it into a phased implementation plan.

---

## Goals

1. **Capture outcomes for every published post**, automatically and idempotently, at 24h / 7d / 30d post-fire.
2. **Make growth data queryable** as typed Rust results, not raw JSONL or string parsing.
3. **Feed both internal outcome data and external market intelligence** into daily theme selection (the "4-quadrant model").
4. **Produce weekly strategy proposals** the founder can read in 5 minutes, edit freely, and approve by `git mv` + commit.
5. **Replace `scripts/publish-to-buffer.mjs`** with a typed `spur growth publish` subcommand that preserves the draft-only invariant and adds `customScheduled` support.
6. **Never publish autonomously.** All decisions (daily theme, weekly strategy, what hits Buffer) go through user approval gates.
7. **Stay within α scope.** No multi-org support, no web UI, no PostHog, no shipped product feature.

## Non-Goals

- Multi-user / multi-org / multi-brand.
- A web dashboard or any visual UI beyond `--format=table` for terminal humans.
- Cross-channel attribution to product telemetry (SPUR has no installed product yet).
- A/B testing infrastructure beyond what the strategy proposals already capture.
- Auto-reply / auto-respond / auto-DM. The Threads algorithm rewards re-engagement; that's a separate, deliberate-by-design skill.
- PostHog integration (earmarked for v2 when SPUR product ships).
- Reddit publishing (Buffer doesn't connect to Reddit; manual for now).
- Self-rescheduling cron (recurring daily cron only; no `cron-that-changes-its-own-schedule` foot-guns).

---

## 1. Architecture Overview

Three execution surfaces, all the brain at different cron triggers, coordinating via files in the repo and the `spur growth` CLI.

```
                  ┌──────────────────────────────────────────────────┐
                  │                  growth-engine                    │
                  │                                                   │
   external ─────►│   ┌──────────────┐         ┌──────────────────┐  │
   market intel   │   │ growth-loop  │◄────────│  active-strategy │  │
   (WebSearch)    │   │ (daily       │  reads  │  (markdown,      │  │
                  │   │  research +  │         │   user-approved) │  │
                  │   │  draft)      │         └─────────▲────────┘  │
                  │   └──────┬───────┘                   │           │
                  │          │ writes draft              │ approved  │
                  │          ▼                           │ by user   │
                  │   ┌──────────────┐         ┌─────────┴────────┐  │
                  │   │ tactical-    │────────►│ proposed-strategy │  │
                  │   │ proposal     │ Mon AM  │  (markdown,       │  │
                  │   │ (in artifact)│         │   weekly)         │  │
                  │   └──────┬───────┘         └────────▲──────────┘  │
                  │          │ user                     │              │
                  │          │ approves                 │ Sunday brain │
                  │          ▼                          │              │
                  │   ┌──────────────┐                  │              │
                  │   │ spur growth  │                  │              │
                  │   │ publish      │                  │              │
                  │   └──────┬───────┘                  │              │
                  │          │ post fires                │              │
                  │          ▼                          │              │
                  │   ┌──────────────────────────┐      │              │
                  │   │ spur growth capture (24h)│──────┘              │
                  │   │ writes outcomes.jsonl    │  query              │
                  │   └─────────────┬────────────┘                     │
                  │                 │                                  │
                  │   ┌─────────────▼────────────┐                     │
                  │   │   DuckDB analytics       │                     │
                  │   │   (reads JSONL in place) │                     │
                  │   └──────────────────────────┘                     │
                  └──────────────────────────────────────────────────┘
```

### File layout

```
crates/spur-growth-engine/             # NEW Rust crate (Shape C data layer)
├── Cargo.toml
├── src/                                # extractors, store, query, capture, publish, strategy
└── tests/

.claude/skills/
├── growth-engine/SKILL.md              # NEW — orchestrator skill (daily + weekly entry)
├── growth-loop-capture/SKILL.md        # NEW — brain wrapper around `spur growth capture`
├── growth-loop-strategy/SKILL.md       # NEW — weekly proposal author skill
├── growth-loop/SKILL.md                # EXTEND — step 0.5 consults briefing
├── growth-loop-media/SKILL.md          # UNCHANGED
└── growth-loop-publish/SKILL.md        # EXTEND — points at `spur growth publish` once Phase 3 lands

resource/growth-loop/
├── YYYY-MM-DD.md                       # daily artifacts (existing)
├── analytics/
│   ├── posts.jsonl                     # NEW — one row per published post
│   ├── outcomes.jsonl                  # NEW — append-only outcome captures
│   ├── snapshots.jsonl                 # NEW — weekly account snapshots
│   ├── qualitative.jsonl               # NEW — manual notes
│   └── market-intel.jsonl              # NEW — durable market signal
├── strategy/
│   ├── proposed-YYYY-MM-DD.md          # NEW — Sunday brain output
│   └── active-YYYY-MM-DD.md            # NEW — user-approved Mondays
└── research/
    └── (existing market-intel artifacts)

scripts/publish-to-buffer.mjs           # KEEP THROUGH PHASE 3; delete after cutover
```

### Three execution surfaces

1. **Daily growth-loop** — runs ~09:00 Saigon. Captures yesterday's outcomes, consults active strategy + intel briefing, drafts today's content, presents for approval, publishes drafts.
2. **Outcome capture** — runs as step ① of the daily orchestrator. Idempotent. Can also run standalone via `spur growth capture` whenever needed.
3. **Weekly strategic proposal** — runs Sunday ~18:00 Saigon. Aggregates last 7d data, evaluates last week's active strategy, writes `proposed-<next-Monday>.md`. User reviews Monday, approves by `git mv`.

---

## 2. Storage Schema

Five append-only JSONL files under `resource/growth-loop/analytics/`. The Rust crate's `store/` module owns the schema; JSONL is the serialization format. The crate enforces append-only and dedup invariants the JSONL files alone cannot.

### `analytics/posts.jsonl` — one row per published post

| Field | Type | Notes |
|---|---|---|
| `post_id` | string (unique) | Buffer post ID — primary key |
| `channel` | enum | `x` / `threads` / `linkedin` |
| `channel_handle` | string | `kevinvutr`, `crazyguy1805`, etc. |
| `kind` | enum | `single` / `thread_head` / `thread_continuation` / `reply` |
| `parent_post_id` | string? | for thread continuations & replies |
| `text` | string | full post body |
| `text_length` | u32 | char count |
| `text_hash` | string | sha256 — dedup + identity |
| `assets_count` | u32 | media items attached |
| `theme` | string | from artifact's "Theme of the day" |
| `format` | enum | `single` / `thread` / `question` / `observation` / `data_driven` |
| `tribe_target` | enum[] | `claude` / `codex` / `cursor` / `aider` / `both` / `general` |
| `peer_mentions` | string[] | `["@bhvbhushan", "CodeLedger"]` |
| `fired_at` | DateTime<Utc> | when it actually published |
| `created_at` | DateTime<Utc> | when the row was written |
| `artifact_path` | PathBuf | `resource/growth-loop/YYYY-MM-DD.md` |
| `active_strategy_ref` | PathBuf? | filename of the active strategy at draft time |

### `analytics/outcomes.jsonl` — append-only outcome captures

Multiple rows per post by design — captured at 24h, then 7d, then 30d. Latest derivable via `MAX_BY(field, captured_at)`.

| Field | Type | Notes |
|---|---|---|
| `post_id` | string | FK to posts.jsonl |
| `captured_at` | DateTime<Utc> | when measurement taken |
| `hours_since_fire` | u32 | raw |
| `bucket` | enum | `h24` / `h168` / `h720` — derived |
| `impressions` | u32? | nullable — API may not return |
| `likes` | u32? | |
| `replies` | u32? | |
| `reposts` | u32? | |
| `clicks` | u32? | link clicks |
| `bookmarks` | u32? | |
| `engagement_rate` | f64? | computed: (likes+replies+reposts+bookmarks) / impressions |
| `source` | enum | `buffer_api` / `x_api` / `threads_api` / `manual` |
| `status` | enum | `ok` / `missing` / `partial` |

### `analytics/snapshots.jsonl` — weekly account-level snapshots

| Field | Type | Notes |
|---|---|---|
| `channel_id` | string | |
| `channel` | enum | `x` / `threads` / `linkedin` |
| `snapshot_at` | DateTime<Utc> | |
| `iso_week` | string | `"2026-W21"` |
| `total_followers` | u32 | |
| `posts_this_week` | u32 | derived from posts.jsonl |
| `impressions_this_week` | u64 | sum of latest outcomes |
| `replies_received_this_week` | u32 | sum |
| `follower_delta_7d` | i32 | this_week - prior_week |
| `source` | enum | |

### `analytics/qualitative.jsonl` — your notes, append-only

| Field | Type | Notes |
|---|---|---|
| `captured_at` | DateTime<Utc> | |
| `post_id` | string? | if tied to specific post |
| `iso_week` | string? | for weekly observations |
| `tag` | enum | `voice_drift` / `audience_quality` / `competitor_move` / `opportunity` / `risk` |
| `note` | string | the actual observation |
| `author` | enum | `user` / `brain` |

### `analytics/market-intel.jsonl` — durable market signal

| Field | Type | Notes |
|---|---|---|
| `observed_at` | DateTime<Utc> | when research surfaced it |
| `iso_week` | string | for weekly grouping |
| `topic` | string | normalized (e.g. `"claude-rate-limit"`, `"cursor-composer-2.5"`) |
| `kind` | enum | `trend` / `peer_move` / `voc_quote` / `news` / `competitor_ship` |
| `intensity` | enum | `low` / `medium` / `high` |
| `summary` | string | one-line description |
| `source_urls` | string[] | citations |
| `peer_subject` | string? | for `peer_move`/`competitor_ship` |
| `actionable_for_spur` | bool | brain's read of credible angle availability |

### DuckDB views (defined in `crates/spur-growth-engine/src/query/views.rs`)

Three core views the query API exposes. SQL is version-pinned in the Rust source so changes are explicit.

```sql
-- Latest outcome per post (rolling current state)
CREATE VIEW v_post_latest AS
SELECT p.*, o.impressions, o.likes, o.replies, o.engagement_rate
FROM read_json_auto('posts.jsonl') p
LEFT JOIN (
  SELECT post_id,
    MAX_BY(impressions,     captured_at) AS impressions,
    MAX_BY(likes,           captured_at) AS likes,
    MAX_BY(replies,         captured_at) AS replies,
    MAX_BY(engagement_rate, captured_at) AS engagement_rate
  FROM read_json_auto('outcomes.jsonl')
  GROUP BY post_id
) o ON p.post_id = o.post_id;

-- Weekly performance by theme
CREATE VIEW v_theme_weekly AS
SELECT strftime(fired_at, '%G-W%V') AS iso_week, theme,
       COUNT(*) AS posts,
       AVG(impressions) AS avg_impressions,
       AVG(replies) AS avg_replies,
       AVG(engagement_rate) AS avg_eng
FROM v_post_latest
GROUP BY iso_week, theme;

-- Best-performing format × tribe combos (last 30d, ≥3 samples)
CREATE VIEW v_winners AS
SELECT format, list_aggregate(tribe_target, 'string_agg', ',') AS tribes,
       COUNT(*) AS sample, AVG(engagement_rate) AS avg_eng
FROM v_post_latest
WHERE fired_at >= NOW() - INTERVAL '30 days'
GROUP BY format, tribes
HAVING sample >= 3
ORDER BY avg_eng DESC;
```

---

## 3. Daily Tactical Loop

The daily run is a five-step orchestration. The new `growth-engine` skill is the orchestrator; existing `growth-loop` gets one minor extension.

```
┌─────────────────────────────────────────────────────────────────────┐
│ growth-engine skill — daily orchestrator (fires ~09:00 Saigon)      │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  ① capture-outcomes  (delegates to growth-loop-capture skill,       │
│                       which shells out to `spur growth capture`)    │
│                                                                     │
│  ② consult-analytics (in-session, brain-driven)                     │
│      - Read strategy/active-*.md (latest by date)                   │
│      - Shell out: `spur growth summary --format=json`               │
│      - Parse typed DailyBriefing                                    │
│      - Brain composes a "today's intel briefing" (5–10 lines        │
│        markdown for the artifact)                                   │
│                                                                     │
│  ③ growth-loop (EXISTING skill, +1 new sub-step 0.5)                │
│      - 0.5 (NEW): consume briefing + active strategy                │
│      - 1–9: unchanged                                               │
│      - Artifact MUST include `active_strategy_ref:` in frontmatter  │
│        and a `## Tactical proposal` section citing the 4-quadrant   │
│        decision                                                     │
│                                                                     │
│  ④ user-review-gate  (manual, per A scope)                          │
│                                                                     │
│  ⑤ spur growth publish <artifact>                                   │
│      - Replaces scripts/publish-to-buffer.mjs after Phase 3 cutover │
│      - Appends to posts.jsonl with active_strategy_ref filled in    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Theme selection via the 4-quadrant model

When the brain has both internal data and external market intel:

```
              external HOT
   ┌──────────────────────┬──────────┐
   │ STRONG WINNER         │ COLD     │
   │ internal +, external+ │ START    │
   │ → double down         │ PROBE    │
   ├──────────────────────┼──────────┤
   │ RE-ANGLE              │ SKIP     │
   │ internal +, market    │ no signal│
   │ stale → fresh framing │          │
   └──────────────────────┴──────────┘
              internal +
```

The daily artifact's `## Tactical proposal` section MUST name the quadrant, with citations:

```markdown
## Tactical proposal
**Quadrant:** STRONG WINNER (internal: thread×Claude-tribe×rate-limit avg 320 imp last 30d; external: Anthropic May 6 statement still trending, intensity=high last 7d).
**Today's angle:** continue the rate-limit thread cadence with a new evidence-driven hook.
**Format:** thread > single.
**Tribe:** Claude primary, Codex tertiary.
```

If the brain can't credibly name a quadrant → COLD_START or NO_EXTERNAL_SIGNAL flag explicit, with fallback to active strategy.

### Cold-start behavior

If `outcomes.jsonl` has < 5 rows total OR `market-intel.jsonl` is empty, the briefing labels itself cold-start and the loop falls back to the default strategy from `product-marketing.md`. Brain never fabricates trends.

---

## 4. Weekly Strategic Loop

Sunday 18:00 Saigon (= 11:00 UTC = 07:00 EDT). Proposal lands Sunday night; user reviews Monday morning before the 09:00 daily run.

### Inputs (DuckDB queries)

- `v_post_latest` filtered to last 7d (per-post outcomes + categorization)
- `v_theme_weekly` last 4 weeks (trend over time)
- `v_winners` with last-week subset
- Week-over-week follower deltas from `snapshots.jsonl`
- `qualitative.jsonl` filtered to current iso_week
- Previous active strategy markdown (to evaluate adherence + outcome)

### Output: `strategy/proposed-YYYY-MM-DD.md` (Monday's date)

Structure is fixed (brain composes content within it):

```markdown
# Growth strategy proposal — Week of YYYY-MM-DD

**Proposed by:** brain (growth-engine weekly run)
**Generated:** <ISO timestamp>
**Previous active strategy:** strategy/active-<prior-Monday>.md
**For your approval by:** Monday <next-Monday> 09:00 Saigon

## Last week's performance
| Metric | This week | Last week | Δ |
|---|---|---|---|
…

## What worked
- <2–4 bullets citing post IDs + numbers + categorization>

## What didn't
- <1–3 bullets, factual not editorial>

## Proposed strategy for week of <next Monday>
**Strategic theme (durable):** …
**Format priorities (ranked):** …
**Tribe focus:** primary / secondary / skip
**Posting cadence:** per channel
**Peers to mention** (with pre-clearance notes): …
**Avoid this week:** …

## Open questions for you
- <1–3 explicit decisions requiring user judgment>

## How to approve
1. Edit this file freely.
2. `git mv strategy/proposed-YYYY-MM-DD.md strategy/active-<next-Monday>.md`
3. `git commit -m "approve growth strategy for week of <next-Monday>"`
4. Monday's growth-engine run consults `active-<next-Monday>.md` automatically.

## Cold-start / insufficient-data flags
(Only present when triggered)
```

### What the proposal CAN say vs CANNOT say autonomously

**CAN propose:**
- Format priority shifts (≥5 samples)
- Cadence adjustments
- Time-slot adjustments
- Tribe focus tilts
- New peer mentions (with explicit "pre-clearance needed" flag)

**CANNOT propose without flagging as "major shift requires your explicit OK":**
- Voice / tone changes
- ICP redefinition
- Anti-peer content (positioning negatively)
- Channel additions (LinkedIn / TikTok / video)

These always go in `## Open questions for you`, never baked silently into the strategy.

### Approval mechanics

- Only one `active-*.md` is "current": the one with the highest date prefix. The brain reads via `query::active_strategy()`.
- Old `active-*.md` files preserved as audit trail.
- `proposed-*.md` becomes `active-*.md` via `git mv` + commit. Atomic.
- If no approval by Monday 09:00: fallback to most recent existing `active-*.md`. Graceful, not blocking.

### Cold-start weekly behavior

If N < 5 outcome rows, weekly run STILL produces a proposal labeled `🟡 COLD START`, carrying forward `product-marketing.md` default strategy and citing what's needed to graduate.

---

## 5. Market Intelligence Integration

External research (the existing growth-loop research phase) writes to `market-intel.jsonl` as a side-effect, in addition to populating the daily artifact prose.

### Write path

When `growth-loop` runs steps 2–4 (research-x / research-reddit / research-competitors), each surfaced item produces a `MarketIntelItem`:

```rust
pub struct MarketIntelItem {
    pub observed_at: DateTime<Utc>,
    pub iso_week: String,
    pub topic: String,                  // normalized
    pub kind: MarketIntelKind,
    pub intensity: Intensity,
    pub summary: String,
    pub source_urls: Vec<String>,
    pub peer_subject: Option<String>,
    pub actionable_for_spur: bool,
}
```

The brain uses `spur growth ingest-research <file>` to append parsed items from a research output file. Topic normalization is brain-driven (a small vocabulary of canonical topic slugs documented in the skill).

### Read path

`spur growth summary` joins internal winners (`v_winners`) against recent market-intel (`market-intel.jsonl` last 7d) and includes both in the `DailyBriefing`. The 4-quadrant decision uses both inputs.

### Failure modes

- WebSearch unavailable → `growth-loop` step 0.5 falls back to internal-only mode; briefing says "Market intel unavailable this run; theme picked from internal winners."
- Internal data empty → market-intel-only mode; briefing says "Cold-start: theme picked from market-intel only."
- Both empty → full default-strategy fallback.

---

## 6. Capture Mechanics

The `spur growth capture` command, invoked by the `growth-loop-capture` skill as step ① of the daily orchestrator.

### Algorithm

```
1. RECONCILE: list Buffer-side posts marked sent in last 30d
   - cross-check against posts.jsonl
   - for any sent post not in posts.jsonl (user-published-from-UI case),
     append with source: buffer_reconcile

2. CAPTURE OUTCOMES: for each post in posts.jsonl:
   - bucket current hours_since_fire into {h24, h168, h720}
   - if no outcomes row exists for (post_id, bucket) pair:
     - fetch analytics from Buffer's GraphQL
     - if Buffer returns null (or post deleted): append status:missing row
     - else append row with measurements
   - stop capturing for posts older than 30d

3. WEEKLY SNAPSHOT (only on Sundays, or when --weekly flag passed):
   - fetch current total_followers per channel
   - compute follower_delta_7d vs prior week
   - append to snapshots.jsonl
```

### Idempotency contract

A single post can have multiple outcome rows; the dedup key is `(post_id, bucket)`. The store's `append_outcome` returns `Ok(false)` if the row already exists, `Ok(true)` if newly written. The script can run hourly, daily, weekly — same result.

### Buffer API shape (assumed; verified in Phase 1 task 1)

```graphql
query PostAnalytics($id: ID!) {
  post(id: $id) {
    id status
    analytics { impressions reach likes replies reposts quotes clicks bookmarks }
  }
}

query ChannelFollowers($id: ID!) {
  channel(id: $id) { id service metrics { totalFollowers totalFollowing } }
}
```

**Open question to verify at implementation time:** does Buffer expose these on the free tier? Phase 1's first task is a 5-minute curl probe. If fields are paywalled, fall back to manual-capture mode where the script prompts the user to enter numbers — schema still works, source enum becomes `manual`.

### Failure handling

| Failure | Exit | Action |
|---|---|---|
| Buffer 401/403 | 4 | Stop, alert via PushNotification. Token rotation needed. |
| Buffer 5xx / network | 2 | Exponential backoff (3 retries: 5s/30s/300s), then skip. Next run retries. |
| Post deleted from Buffer | — | Append `status: missing`, `source: buffer_reconcile`. Stop trying. |
| Field missing in response | — | Write what's available, leave others null. |
| Rate limit (Buffer 100/day) | 5 | Pause; resume tomorrow. Log how many posts uncaptured. |

### Manual qualitative capture

```bash
spur growth note --post-id 6a0d... --tag audience_quality --text "Reply from senior-dev was thoughtful"
spur growth note --week-summary 2026-W21 --tag competitor_move --text "Cursor shipped Composer 2.5"
```

Both append to `qualitative.jsonl`. Author defaults to `user` (set to `brain` when invoked from inside a brain run for self-reflections).

---

## 7. The CLI Contract

The cohesion point in Shape C. Brain skills shell out to `spur growth <verb>`; the Rust crate owns determinism, types, retries, redaction.

### Subcommand surface

```
spur growth capture                                  # daily outcome capture; idempotent
spur growth reconcile                                # Buffer-side cross-check
spur growth snapshot                                 # weekly account-level snapshot

spur growth query <view> [--filter K=V]              # run a named DuckDB view → JSON
spur growth summary                                  # composite → DailyBriefing JSON
spur growth winners --window 30d                     # best combos JSON

spur growth ingest-research <file>                   # append to market-intel.jsonl
spur growth note --post-id <id> --tag <tag> --text "..."   # manual qualitative

spur growth publish <artifact-path>                  # replaces publish-to-buffer.mjs (drafts)
spur growth schedule <artifact-path> --due-at <ISO>  # customScheduled variant

spur growth propose-weekly                           # generates strategy/proposed-<Mon>.md
```

### Standard contract

- Default output: JSON to stdout. `--format=table` for human display.
- Exit codes: `0` ok, `1` config/arg, `2` upstream API, `3` data integrity, `4` auth, `5` rate-limit.
- All write commands idempotent. Re-running safe.
- All read commands deterministic for fixed input.
- `--dry-run` on `capture`, `publish`, `schedule`, `propose-weekly`.
- `BUFFER_ACCESS_TOKEN` and other secrets read from env only — never accepted via `--token` flag.

### Crate layout

```
crates/spur-growth-engine/
├── Cargo.toml
├── src/
│   ├── lib.rs                    # public Rust API
│   ├── main.rs                   # clap-based CLI dispatcher
│   ├── config.rs                 # env config: BUFFER_ACCESS_TOKEN, channel ids, store path
│   ├── error.rs                  # typed errors → exit codes
│   ├── extractors/
│   │   ├── mod.rs
│   │   └── buffer.rs             # Buffer GraphQL client, retry, redaction
│   ├── store/
│   │   ├── mod.rs                # trait Store (atomic append, dedup, read)
│   │   ├── jsonl.rs              # filesystem impl
│   │   ├── posts.rs              # struct PostRecord + invariants
│   │   ├── outcomes.rs           # struct OutcomeRecord + bucket logic
│   │   ├── snapshots.rs
│   │   ├── qualitative.rs
│   │   └── market_intel.rs
│   ├── query/
│   │   ├── mod.rs
│   │   ├── duckdb_conn.rs        # connection management (read-only attach to JSONL)
│   │   ├── views.rs              # SQL view definitions, version-pinned
│   │   └── api.rs                # typed Rust query functions
│   ├── capture.rs                # daily capture orchestrator
│   ├── publish.rs                # replaces publish-to-buffer.mjs
│   ├── strategy.rs               # weekly proposal markdown writer
│   └── briefing.rs               # composite DailyBriefing builder
└── tests/
    ├── fixtures/
    │   ├── buffer_post_analytics_ok.json
    │   ├── buffer_post_analytics_deleted.json
    │   └── buffer_rate_limit_429.json
    ├── extractors_buffer.rs      # property tests (mocked HTTP)
    ├── store_idempotency.rs      # append-only invariants
    ├── query_views.rs            # known-data → known-answer
    └── e2e_capture.rs            # full capture loop, fixture extractor
```

### Typed enums (the determinism punchline)

```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Channel { X, Threads, LinkedIn }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PostKind { Single, ThreadHead, ThreadContinuation, Reply }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PostFormat { Single, Thread, Question, Observation, DataDriven }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Tribe { Claude, Codex, Cursor, Aider, Both, General }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeBucket { H24, H168, H720 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSource { BufferApi, XApi, ThreadsApi, Manual }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus { Ok, Missing, Partial }
```

Refactor a category, the compiler tells you every consumer. JSON serializes/deserializes via serde with snake_case.

### Typed query API

```rust
pub fn daily_briefing(store: &impl Store) -> Result<DailyBriefing> { ... }

#[derive(Serialize, Debug)]
pub struct DailyBriefing {
    pub active_strategy_path: Option<PathBuf>,
    pub active_strategy_summary: Option<String>,
    pub yesterday_outcomes: Vec<PostWithOutcome>,
    pub running_7d: RunningTotals,
    pub winners_30d: Vec<WinnerCombo>,
    pub open_opportunities: Vec<MarketIntelItem>,
    pub cold_start: bool,
    pub cold_start_reason: Option<String>,
}
```

The CLI command `spur growth summary` returns this as JSON. Skills consume it.

### Skill ↔ CLI integration

```markdown
# growth-engine/SKILL.md — Step ② consult-analytics
Invoke: `spur growth summary --format=json`
Parse: typed DailyBriefing (schema in crates/spur-growth-engine/src/briefing.rs).
Compose: a 5–10 line markdown briefing for the day's artifact.
DO NOT bypass `spur growth` and read JSONL directly. Store invariants are crate-enforced.
```

---

## 8. Testing, Success Criteria, Migration Sequence

### Testing strategy (layered)

```
Manual smoke (real Buffer, occasional)         ← slowest, last
E2E with mocked HTTP (~5 tests)
Integration: query views over fixture (~10)
Property tests: extractor + store (~30)
Unit tests: bucket, hash, type conversion (~50)  ← fastest, most
```

**Key contracts:**

- `store::append` is the single write point. Property tests assert: (a) appending already-present `(post_id, bucket)` is no-op, (b) concurrent appends never duplicate, (c) crash during write leaves file valid (atomic O_APPEND ≤ pipe buffer, or temp+rename for larger).
- Each extractor: ≥3 fixture tests (happy / deleted / 429).
- Each query view: ≥1 known-input → known-output test.
- Token redaction tested in every error path.

### Success criteria — phased

**Phase 1 — Capture (Week 1)**
- ✅ `spur growth capture` runs against real Buffer
- ✅ ≥1 outcome captured (status=ok)
- ✅ Re-run no duplicates
- ✅ Property + extractor fixture tests pass
- ✅ growth-engine skill calls `spur growth capture` in step ①

**Phase 2 — Query (Week 2)**
- ✅ `spur growth summary --format=json` returns typed DailyBriefing
- ✅ DuckDB views run against real JSONL
- ✅ Daily growth-loop step 0.5 consumes briefing; artifact cites `active_strategy_ref:` + quadrant
- ✅ Cold-start mode tested

**Phase 3 — Publish/Schedule (Week 3)**
- ✅ `spur growth publish <artifact>` matches `scripts/publish-to-buffer.mjs` (parallel run, same Buffer draft IDs)
- ✅ `spur growth schedule <artifact> --due-at <iso>` works
- ✅ One supervised cutover; behavior identical
- ✅ Old `.mjs` deleted; `growth-loop-publish` skill points at Rust path

**Phase 4 — Weekly Strategy Proposal (Week 4+)**
- ✅ `spur growth propose-weekly` runs Sundays via cron
- ✅ ≥4 weeks data accumulated before first non-cold-start
- ✅ First user-approved `active-*.md` cycle completes
- ✅ Daily run consumes active strategy

### Migration sequence

```
Today      → Today's scheduled Buffer posts continue firing
             scripts/publish-to-buffer.mjs runs the daily publish path
             NO Rust code yet

Week 1     → Build crates/spur-growth-engine with capture + store
             Wire to growth-engine skill (Bash invocation in step ①)
             Start collecting outcome data IMMEDIATELY (data needs to accrue)

Week 2     → Add query/ module + daily summary command
             Daily growth-loop step 0.5 consumes spur growth summary
             First data-driven daily artifacts produced

Week 3     → Add publish + schedule commands
             Parallel run with publish-to-buffer.mjs for 7 days
             After 7 clean days: cut over (Rust publishes, JS dry-runs)
             After 7 more clean days: delete .mjs

Week 4+    → Add strategy.rs (propose-weekly)
             Register Sunday cron
             First non-cold-start proposal (4+ weeks of data)
             First user approval cycle
```

### Risk register

| Risk | Likelihood | Mitigation |
|---|---|---|
| Buffer analytics fields not exposed on free tier | Medium | Phase 1 task 1: 5-min curl probe. If no: manual-capture fallback. |
| Buffer non-RFC-compliant JSON responses | Confirmed | Tolerant `serde_json` parsing. Fixture test with malformed body. |
| Buffer rate limit (100/day free) | Medium | Steady state: ~30 reqs/day. Well under limit. |
| `duckdb-rs` crate maturity | Low | Verify version before commit. Alternative: shell to `duckdb` CLI. |
| Token in shell history | Low | Env only; never `--token` flag. |
| Concurrent capture races | Low | Single-process via cron. If concurrent needed: `fs2` file lock. |

### Out of scope (v1)

- Multi-user / multi-org
- Web dashboard or UI beyond `--format=table`
- A/B testing infrastructure
- Cross-channel attribution to product telemetry
- PostHog integration (v2 when product launches)
- Auto-reply / auto-respond
- Reddit publishing (Buffer doesn't connect)
- Self-rescheduling cron

---

## Open Questions for Implementation

These need resolution during Phase 1 but don't block this spec:

1. **Buffer analytics on free tier:** does the GraphQL schema expose post-level + channel-level analytics without a paid plan? Resolve with a curl probe in Phase 1 task 1.
2. **`duckdb-rs` crate vs CLI shell-out:** verify crate maturity + license against SPUR's other dependencies. If problematic, shell to `duckdb` CLI binary (still typed at the Rust layer; just slower).
3. **Atomic append size limit:** for outcomes < 4KB (typical), `O_APPEND` is atomic on POSIX. Larger writes (raw API blobs) may need temp+rename. Phase 1 task: confirm max row size and pick the strategy.
4. **Cron substrate:** local cron on user's machine (machine-locked, simple) or remote `CronCreate` routine (needs the 71-commit-ahead origin/main pushed)? Phase 1 default: local cron; revisit when product ships.
5. **`active-*.md` lookup mechanic:** by filename date prefix (simple `max_by(prefix)`) or by YAML frontmatter `status: active` field? Spec says filename; can revise if scanning frontmatter is cleaner.

---

## Approval / Sign-Off

Designed via `superpowers:brainstorming` 8-section flow. All 8 sections approved by user (founder) during session 2026-05-20. Next step: invoke `superpowers:writing-plans` to produce phased implementation plan with concrete task DAG.

**Approved:** 2026-05-20 (chat session)
