# Palette End-to-End Integration Design

> Status: design approved — ready for implementation plan
> Author: brainstormed via multi-round MCTS evaluation
> Date: 2026-04-20
> Scope: `crates/spur-tui/src/components/palette.rs` and its upstream/downstream seams

## Summary

The Ctrl+K command palette ships today with a clean ranker (`palette.rs`, fenced by a 2-allocation rerank invariant) but only *one of four* payload variants — `Session` — is fully functional end-to-end. `Command` and `Trace` are silent no-ops on Enter; `Worker` switches view but does not pre-select the node; the `Command` source upstream drops agent-static and agent-dynamic entries because `open_palette()` constructs a throwaway `CommandRegistry::new()` instead of borrowing the live one.

This design closes the broken seams, adds search-by-id, and layers on four bounded UX improvements that make the palette discoverable, forgiving, and honest about its current capabilities — all without introducing new public types, new `Action` variants, or cross-crate ripples.

## Background

`palette.rs` is a context-free ranker over a snapshot bag of `PaletteResult`. The performance contract — O(N) rerank, exactly 2 allocations on the non-empty-query path, zero clones of result fields — is fenced by `tests/palette_rerank_bench_smoke.rs`. The ranker itself is correct; the integration risk lives at two seams in `app.rs`:

- **Seam A — `open_palette()` (`crates/spur-tui/src/app.rs:343-368`)**: snapshot construction. Synchronous, eager, all-sources-always.
- **Seam B — `result_to_action()` (`crates/spur-tui/src/app.rs:2116-2139`)**: intent → `Action` translation.

A 4-payload × 2-seam audit shows three of four payloads are partially or fully non-functional downstream:

|              | Upstream                                              | Downstream                                         |
| ------------ | ----------------------------------------------------- | -------------------------------------------------- |
| **Command**  | Broken — fresh `CommandRegistry::new()` drops agent commands | Broken — arm returns `None` ("Phase F1.5") |
| **Session**  | OK                                                    | OK — `Action::ResumeSession` over mpsc              |
| **Worker**   | OK (live lineage)                                     | Partial — `NavigateTo(SessionDetail)`, no node pre-select |
| **Trace**    | OK but unbounded                                      | Broken — arm returns `None`; no `Action::ScrollToTraceEntry` |

Plus one cross-cutting hole: `pattern.score` runs over `entry.label` only (`palette.rs:117`), so typing a session-id substring (id lives in `subtitle`) matches nothing.

## Goals

- Make all four payload variants either fully functional or honestly hidden.
- Add subtitle-aware fuzzy search without breaking the rerank invariant.
- Make the empty-query state self-documenting so first-time users see what the palette does without typing.
- Add tracing instrumentation sufficient to make a future "lazy population policy" decision empirically.
- Reuse existing primitives: `submit_router::route`, the `Action` enum + `process_action` loop, the `tracing::debug!` convention.

## Non-Goals

- Trace-jump dispatch (`Action::ScrollToTraceEntry`). Requires a stable-id design for `TraceEntry`; deferred to a follow-up. This design *prepares* for it via U3c (forward-compatible "coming soon" placeholder).
- Lazy / streaming `PaletteSource` trait. Premature without measurement; tracing in this design is the prerequisite that unblocks it later.
- `SessionId` type unification across `spur-cli` / `spur-core`. Cross-crate ripple not justified standalone.
- Worker fine-grained node pre-selection (akin to `Action::InspectWorkers` at `app.rs:1147-1163`). Belongs to a separate "`NavigateTo(SessionDetail)` should always pre-select the right node, regardless of source" spec.
- Prefix syntax (`>command`, `:session`, `@worker`). Increases novice cognitive load; rejected on the user-friendliness axis.

## Design

### Engine layer

#### C1a — Borrow live registry in `open_palette`

Today (`app.rs:350`):

```rust
let cmd_registry = crate::commands::registry::CommandRegistry::new();
```

This produces a registry containing only `SpurLocalSource` entries (via `ensure_cache()` at `commands/registry.rs:75-134`, which always prepends spur-local). Agent-static and agent-dynamic commands are silently absent.

Replacement: borrow from the active `SessionDetailView` if present, fall back to a fresh registry. `CommandRegistry` is **not `Clone`** (no `#[derive(Clone)]` on the struct at `commands/registry.rs:15-22`, no manual impl), so we use a borrow-with-owned-fallback shim:

```rust
let owned_fallback;
let cmd_registry: &CommandRegistry = match self.session_detail.as_ref() {
    Some(view) => &view.command_registry,
    None => {
        owned_fallback = CommandRegistry::new();
        &owned_fallback
    }
};
let commands = CommandSource::new(cmd_registry).collect();
```

`SpurLocalSource` entries are unconditionally included regardless of which path is taken, because they are baked into `ensure_cache()` (`registry.rs:99`). When no session is active the palette shows spur-local commands only; when a session is active it shows the full merged set (spur-local + that session's static + dynamic).

#### C1b — Route `Command` accept through `submit_router::route`

Today (`app.rs:2116-2139`), `result_to_action` is a free function and the `Command` arm returns `None`. There is **no** generic `Action::RunCommand`. The slash-command dispatch model is *registry-resolution-at-parse-time*: `submit_router::route` (the input bar's existing primitive) resolves a slash command to a concrete semantic `Action` (`Action::ShowHelp`, `Action::ClearSession`, `Action::SendMessage`, `Action::VendorExec`).

Refactor `result_to_action` from a free function to a `&self` method on `App`, and route the `Command` arm through the same primitive:

```rust
fn result_to_action(&self, result: PaletteResult) -> Option<Action> {
    use crate::commands::submit_router::{route, SubmitDecision};
    match result.payload {
        PalettePayload::Session { session_id } => {
            Some(Action::ResumeSession { session_id })
        }
        PalettePayload::Worker { session_id } => {
            Some(Action::NavigateTo(ViewId::SessionDetail(session_id)))
        }
        PalettePayload::Command { name } => {
            let view = self.session_detail.as_ref()?;
            let registry = &view.command_registry;
            match route(&format!("/{name}"), &[], registry, false) {
                SubmitDecision::Local { action } => Some(action),
                SubmitDecision::Send { blocks, interrupt } => {
                    let session = self.current_session_id()?;
                    Some(Action::SendMessage { session, blocks, interrupt })
                }
                SubmitDecision::VendorExec { method, params } => {
                    let session = self.current_session_id()?;
                    Some(Action::VendorExec { session, method, params })
                }
            }
        }
        PalettePayload::Trace { .. } => None, // U3c: trace results are not surfaced; see below
    }
}
```

Notes:

- Spur-local commands (`/help`, `/clear`, `/cost`, `/vim`, etc.) dispatch identically to how the input bar dispatches them today — same `Action`, same `process_action` handler.
- Agent-static and agent-dynamic commands return `Action::SendMessage` / `Action::VendorExec`, both of which require an active session (`current_session_id()`). The `?` early-returns when no session is present.
- `Action::ClearSession` and similar session-scoped local actions are already defensive in `process_action` (they're invoked from the input bar in the same SessionDetailView context); palette dispatch reuses that defensiveness.
- Zero new `Action` variants. Zero new traits. Single dispatch source of truth.

#### F1 — Subtitle-aware fuzzy scoring

Today, `rerank` (`palette.rs:115-121`) scores only `entry.label`. Change to score both `label` and `subtitle`, take the weighted max, and reuse `self.scratch` between the two scorings to preserve the 2-allocation invariant:

```rust
for (i, entry) in self.raw.iter().enumerate() {
    self.scratch.clear();
    let label = Utf32Str::new(&entry.label, &mut self.scratch);
    let label_score = pattern.score(label, &mut self.matcher);

    self.scratch.clear();
    let sub = Utf32Str::new(&entry.subtitle, &mut self.scratch);
    let sub_score = pattern.score(sub, &mut self.matcher);

    let best = match (label_score, sub_score) {
        (Some(a), Some(b)) => Some(a.max((b as f32 * 0.7) as u32)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some((b as f32 * 0.7) as u32),
        (None, None) => None,
    };
    if let Some(score) = best {
        tmp.push((score, i as u32));
    }
}
```

Allocation accounting (must stay 2 on non-empty-query path):

1. `Pattern::parse(...)` — unchanged.
2. `Vec<(u32, u32)>` scratch for scoring — unchanged.

Per-entry: two `Utf32Str::new` calls each write into `self.scratch` (reused, capacity-only growth, never freed). Two `pattern.score` calls allocate nothing. **Allocation budget: still 2.** The smoke test must continue to pass; if it doesn't, the fix is the implementation, not the threshold.

The 0.7× subtitle weight biases toward label matches (which represent the user-friendly identifier for each kind) while still allowing strong subtitle matches (e.g., a unique session-id prefix) to dominate weak label matches. Subject to empirical tuning during implementation; a regression test must capture expected ranking for representative queries.

#### Tracing instrumentation

Three `tracing::debug!` points, no per-entry logs:

- `open_palette` start — modal opening.
- `extend_raw` end — per-source result counts (`commands`, `sessions`, `workers`, `trace_count_skipped`).
- `rerank` end — `query_len`, `N` (raw size), `M` (matched size), `elapsed_us`.

These are sufficient to identify the population-time perf cliff the rerank invariant doesn't cover (long-running sessions with thousands of trace entries) and to make a data-driven decision later about lazy / per-source-gated population.

### UX layer

#### U1 — Empty-query grouped view, recency-sorted

When `state.query().is_empty()`, render with section headers and per-kind caps:

```
> ▮
─────────────────────────────────────────
COMMANDS
  /help         cmd · show help
  /clear        cmd · clear session
  /cost         cmd · session cost
  ...

SESSIONS
  refactor-tree-walker   session · 7f3b…  (last opened just now)
  fix-palette-perf       session · 8c2a…  (last opened 2h ago)
  ...

WORKERS
  codex                  worker · running
  claude-code            worker · idle
  ...

TRACE — coming soon
─────────────────────────────────────────
↑↓ select   ⏎ accept   esc dismiss
```

When `!state.query().is_empty()`, fall back to today's flat scored render. Two render paths in `palette_overlay.rs`, both consuming `state.iter_ranked()`.

**Recency sort lives at source-side `collect()` time, not in rerank.** Each `XxxSource::collect()` returns a `Vec<PaletteResult>` already sorted by recency:

- `SessionSource`: sort by `SessionEntry.last_opened_at` descending. The field is an ISO-8601 string (`session_metadata.rs:24`); lexicographic sort = chronological sort. **Verified — no scope deferral.**
- `WorkerSource`: sort by lineage temporal order (most-recent activity first), using the existing `SpurEvent.seq` ordering on the lineage projection.
- `CommandSource`: keep the order returned by `CommandRegistry::list()` (spur-local first by `SpurLocalSource::entries()` definition, then static, then dynamic). Commands have no recency concept.
- `TraceSource`: skipped entirely (see U3c).

This preserves the rerank invariant verbatim: rerank still does `order.extend(0..self.raw.len() as u32)` on the empty-query path, which is O(N) extend. The cost of recency sorting is paid once per `open_palette()`, not per keystroke, and is bounded by source size (sessions: hundreds typical; workers: <50).

**Per-kind cap policy.** Default cap: 5 rows per kind. If the modal's available result-area height (modal_height − query_row − blank_row − hints_row − section_headers) cannot fit `kinds * (cap + header_row)`, scale the cap down to fit, minimum 2. This handles small terminals (≤24-row SSH sessions) gracefully without truncating the modal itself.

#### U2 — Empty-result hint

When `!state.query().is_empty() && state.ranked_len() == 0`, render a one-line hint in the result area:

- Default: `No matches. Try shorter or different keywords.`
- If the query starts with `/` AND no `session_detail` is active: `Slash commands need an active session.`

Both are render-side only (no state changes). The second variant requires the overlay to know whether a session is active, which can be passed in via the existing `PaletteOverlay` borrow signature.

#### U3c — Hide Trace from results, show hint in empty state

Until trace dispatch (`Action::ScrollToTraceEntry`) lands with a stable-id design, do not surface Trace results — they would be silent no-ops on Enter and erode user trust. Concretely:

- In `open_palette()`, omit the `TraceSource` batch from `extend_raw`. Tracing logs a `trace_count_skipped` count for visibility.
- In the U1 empty grouped view, render `TRACE — coming soon` as a one-line section placeholder (no rows). This preserves discoverability of the future feature.
- The `PalettePayload::Trace` variant remains in the type — removing it would be a public API change and a regression for the upcoming follow-up. The `Trace { .. }` arm in `result_to_action` returns `None` with a `// TODO(palette-trace-dispatch): wire when stable-id design lands` comment. This arm is unreachable in practice (TraceSource is omitted at `extend_raw` time) but is kept as a type-exhaustiveness anchor and a forward-compat hook.

Forward compatibility: when the trace-dispatch follow-up lands, lift the omission in `open_palette` and replace the placeholder with real rows in U1's grouped render. No throwaway code.

#### U7 — Hints row content

Today the last row of the modal is reserved for hints. Tighten the content to a single, accurate line:

```
↑↓ select   ⏎ accept   esc dismiss
```

If the hint row already shows this exact content, no change. Otherwise, update it.

## Data flow

```
Ctrl+K
  └─→ App::handle_crossterm_event (app.rs:483)
       └─→ App::open_palette (app.rs:343)
            ├─ CommandSource::new(&registry).collect()         [recency-ordered: no, list() order]
            ├─ SessionSource::from_metadata(...).collect()     [recency-sorted by last_opened_at]
            ├─ WorkerSource::from_lineage(&lineage).collect()  [recency-sorted by lineage seq]
            └─ TraceSource: SKIPPED (U3c)
            ↓
       PaletteState::extend_raw(batches)  →  rerank() with empty query
            ↓
       palette_visible = true

[user types query]
  └─→ PaletteState::push_char → rerank()
       (label + 0.7×subtitle scoring; 2-alloc invariant preserved)

[user presses Enter]
  └─→ PaletteState::handle_key returns PaletteIntent::Accept(result)
       └─→ App::result_to_action(&self, result)  ← now &self method
            ├─ Session  → Action::ResumeSession { session_id }
            ├─ Worker   → Action::NavigateTo(ViewId::SessionDetail(session_id))
            ├─ Command  → submit_router::route("/<name>", &[], &registry, false)
            │              └─→ SubmitDecision::{Local, Send, VendorExec}
            │                  → Action::{ShowHelp | ClearSession | … | SendMessage | VendorExec}
            └─ Trace    → None (deferred; not reachable since TraceSource skipped)
            ↓
       App::process_action(action) → existing dispatch loop
```

## Invariants and tests

### Invariants preserved

- **Rerank performance contract** (`palette.rs:30-49`, fenced by `tests/palette_rerank_bench_smoke.rs`):
  - O(N) time per rerank.
  - Exactly 2 allocations on the non-empty-query path.
  - Zero clones of `PaletteResult` fields.
- **Context-free ranker.** No view-state coupling inside `palette.rs`. All source ordering and policy decisions live at source-side `collect()` or in `app.rs::open_palette`.
- **Single dispatch source of truth.** `submit_router::route` is the only registry-resolver; the palette becomes its second consumer.

### Invariants updated

One line in the `palette.rs` module-level doc comment: note that `extend_raw` now expects sources to return recency-sorted batches, and that the rerank invariant continues to apply unchanged.

### New / modified tests

- `tests/palette_rerank_bench_smoke.rs` — must continue to pass with F1's two-Utf32Str-per-entry scoring. If it fails, fix the implementation; do not loosen the threshold.
- `tests/palette_state.rs` — add cases asserting subtitle-driven matches (e.g., a query matching only the session-id substring in the subtitle ranks the result; a label match still beats a weaker subtitle match given the 0.7× weight).
- `tests/palette_dispatch.rs` — add cases asserting the `Command` arm returns:
  - `Some(Action::ShowHelp)` for `/help` (spur-local, session-less).
  - `Some(Action::SendMessage { … })` for an agent-static command (session present).
  - `None` for an agent-static command when no session is active.
- `tests/palette_integration.rs` — Ctrl+K open with no session shows spur-local commands only; with session shows merged set.
- `tests/palette_render.rs` — new cases for the empty grouped view (kind headers, per-kind cap, "TRACE — coming soon" line) and the no-match hint (default + `/`-prefix variant).
- `tests/palette_sources.rs` — assert each source returns recency-sorted output where applicable.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| F1's 0.7× subtitle weight is empirically wrong | Regression tests in `palette_state.rs` capture expected ranking for representative queries; tunable in one place. |
| `submit_router::route` has side effects | Verify during implementation that `route` is a pure resolver (no mutation, no logging). If it logs, accept the duplicate-with-input-bar entries (low cost) or wrap in a quiet variant. |
| `process_action(Action::ClearSession)` invoked from sessionless palette | Verify the handler is no-op-defensive when invoked without a session. Today the input bar can't dispatch sessionless slash commands so this path is new for `process_action`; add a sessionless guard if missing. |
| Empty grouped view doesn't fit small terminals | Per-kind cap auto-scales down to a minimum of 2 based on available result-area height. |
| Recency sort changes user expectation of stable result order | Acceptable: empty-query order is currently insertion order, which is also "unstable" (depends on source ingestion order). Recency is strictly more useful. |
| `CommandRegistry::ensure_cache()` is `RefCell`-based; concurrent calls during render | Single-threaded TUI render loop; no concurrency. |

## Out of scope (deferred to follow-ups)

- **C2 — Trace dispatch (`Action::ScrollToTraceEntry`).** Requires stable-id design for `TraceEntry`; the index field today is positional and would lie if the trace ring-buffers or compacts.
- **C3 — Worker fine-grained node pre-selection.** Belongs to a session-detail spec ("`NavigateTo(SessionDetail)` should always pre-select the right node regardless of source"), not a palette spec.
- **C4′ — Per-source enable policy / lazy population.** Tracing in this design is the measurement prerequisite. Decision unblocked once we have data.
- **C5 — `SessionId` type unification across `spur-cli` / `spur-core`.** Should ride along with a broader cleanup, not standalone for the palette.
- **U5 — Prefix syntax (`>`, `:`, `@`).** Increases novice cognitive load; rejected on the user-friendliness axis.
- **U6 — Selected-result preview pane.** Marginal value; modal geometry pressure.

## Files touched

| File | Changes |
|---|---|
| `crates/spur-tui/src/components/palette.rs` | F1 (subtitle scoring); 1-line invariant doc note for source-side recency sorting |
| `crates/spur-tui/src/components/palette_overlay.rs` | U1 (empty grouped render), U2 (no-match hint), U7 (hints row content), pass session-presence flag from `App` |
| `crates/spur-tui/src/components/palette_sources.rs` | Recency sort in `SessionSource::from_metadata` (by `last_opened_at` desc) and `WorkerSource::from_lineage` (by lineage seq desc) |
| `crates/spur-tui/src/app.rs` | C1a (borrow live registry with owned fallback in `open_palette`); C1b (`result_to_action` → `&self` method routing through `submit_router::route`); skip `TraceSource` from `extend_raw` (U3c); tracing instrumentation |
| `crates/spur-tui/tests/palette_state.rs` | F1 ranking assertions |
| `crates/spur-tui/tests/palette_dispatch.rs` | C1b dispatch cases (spur-local, agent-static with/without session) |
| `crates/spur-tui/tests/palette_integration.rs` | Sessionless-vs-active command-source coverage |
| `crates/spur-tui/tests/palette_render.rs` | U1 grouped render, U2 hints, U7 hints row |
| `crates/spur-tui/tests/palette_sources.rs` | Recency-sort assertions |

**Public API surface added: zero.** No new `Action` variants, no new traits, no new public types, no cross-crate ripples.
