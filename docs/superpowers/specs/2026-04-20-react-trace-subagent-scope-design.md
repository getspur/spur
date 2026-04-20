# React-trace sub-agent scope rendering — design

*Date: 2026-04-20*
*Status: approved (brainstorm gate); revised after codex-acp + opencode-acp review; awaiting user re-approval before writing-plans handoff*
*Surface: `crates/spur-tui/src/components/react_trace/`*
*Review rounds applied:*
- *codex-acp (Rust/systems): `dirty_from` framing corrected (§3.2); `TraceKind::ScopeTerminal` non-Act variant introduced (§3.1, §3.2); late-child and late-ToolCallUpdate semantics specified (§3.2, §5.10, §5.11); `orphan_warned` dedup state owned by `ReactTrace` (§3.3.1); glyph near-miss `◈`/`◆` documented (§7, §3.6); depth-clamp warn added (§3.6, §5.4); test scaffolding gap flagged (§7).*
- *opencode-acp (UX): orphan wording `(detached child)` → `(orphan)` (§3.4); warning `Style` in `trace_format.rs` (§3.4); `NO_COLOR` explicit handling (§3.5, §5.12); bracketed badge retained at 72–79 cols (§3.5); sticky-header and tree-summary-pane added to deferred (§6).*

## 1. Problem

Claude Code's `Task` tool spawns sub-agents whose tool calls stream into the same ACP session as the parent. Today `react_trace/dispatch.rs:69-80` attributes each child by reading `SpurToolMeta.parent_tool_use_id`, incrementing a depth counter, and prepending `"  "` × depth to the tool name. That is the whole visual treatment.

The resulting stream mixes parent messages, parent thoughts, parent tool calls, and child tool calls — distinguishable only by a two-space indent. Consequences:

- Parent thoughts that arrive *during* a `Task` turn look like they belong to the Task.
- Concurrent parallel Tasks are indistinguishable from each other.
- Orphan children (parent pointer references an unknown id) silently render at depth 0 — misattributed.
- The `Task` start and end have no visual framing, so a user reading a long trace cannot see "where did the sub-agent start / where did it end".

The component needs a richer visual vocabulary for sub-agent scope while preserving the append-only data model, the forward-monotonic cache invariant, and live-streaming behavior.

## 2. Constraints (invariants preserved)

- **Append-only entries.** `ReactTrace.entries: Vec<TraceEntry>` grows only; no reorder.
- **Forward-monotonic `dirty_from`.** Cache invalidation only jumps forward. Any design that forces backward invalidation is rejected.
- **Scroll-anchor stability.** Anchors resolve via `row_to_anchor` keyed on entry index; entry indices cannot be reassigned.
- **Dual render surfaces.** Both `render.rs` (full) and `compact_render.rs` (compact) must stay viable; compact is row-budgeted at one row per entry.
- **Agent-agnostic degradation.** Non-Claude agents (Codex, Kiro, Generic) return `SpurToolMeta.parent_tool_use_id = None`; their rendering must be unchanged.
- **PR #855 forward-compatibility.** When ACP upstream lands `parentSessionId` / `ToolKind::Subagent`, the correlation key changes but the rendering semantics must not.

## 3. Design

### 3.1 Data model additions (`react_trace/types.rs`)

```rust
#[derive(Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeId(pub String);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeRole {
    /// The Task tool call itself — visually framed as a section opener.
    Header,
    /// A child tool call inside the Task's scope.
    Child,
    /// The Task's terminal ToolCallUpdate — visually framed as a section closer with child count.
    Terminal,
    /// Child whose claimed parent id was never seen — visual warning marker.
    Orphan,
}

#[derive(Clone)]
pub struct Scope {
    pub id: ScopeId,
    pub depth: u8,     // capped at 8
    pub role: ScopeRole,
}

pub struct TraceEntry {
    // ... existing fields
    pub scope: Option<Scope>,   // None for parent-session output
}

// New TraceKind variant for the Terminal row.
// Deliberately NOT an `Act` variant so `find_act_by_id_mut`
// (mod.rs:834-855) cannot resolve late `ToolCallUpdate` events to it.
pub enum TraceKind {
    // ... existing variants (Act, Think, Done, …)
    ScopeTerminal {
        scope_id: ScopeId,
        status: ActStatus,      // completed | failed
        child_count: u32,
        duration_ms: Option<u64>,
    },
}
```

`ScopeId` is a newtype wrapper consistent with the in-tree `SessionId` / `DelegationId` / `BrainSessionId` pattern in `crates/spur-acp/src/types.rs:9-62` and `crates/spur-acp/src/domain/delegation.rs:19-32`. Transparent serde keeps it wire-compatible with a bare string, which matters for the eventual PR #855 migration.

Today `ScopeId(parent_tool_use_id)` is written by `dispatch.rs`. Post-#855, the source changes to `parentSessionId`; the rest of the rendering stack does not care.

### 3.2 Dispatch logic (`react_trace/dispatch.rs:69-80` — replaces current depth block)

For `SessionUpdate::ToolCall(tc)`:

- Extract `meta = adapter::extract_tool_meta(tc, ctx.agent_kind)` (unchanged).
- Classify:
  - If `tc.title == "Task"` (or future `ToolKind::Subagent`): role = `Header`, depth = parent depth (0 if no parent), `ScopeId` = `tc.tool_call_id`.
  - Else if `meta.parent_tool_use_id = Some(pid)` and `pid ∈ ctx.tool_depth`: role = `Child`, depth = parent depth + 1 (capped at 8), `ScopeId` = `pid`.
  - Else if `meta.parent_tool_use_id = Some(pid)` and `pid` unknown: role = `Orphan`, depth = 1, `ScopeId` = `pid`; emit `tracing::warn!` once per pid.
  - Else: `scope = None` (parent-session output, unchanged rendering).
- Register `ctx.tool_depth.insert(tc.tool_call_id, depth)`.

For `SessionUpdate::ToolCallUpdate(tcu)` with a terminal status on a Task id:

- **Synthesize** a new `TraceEntry` carrying `TraceKind::ScopeTerminal { scope_id = tcu.tool_call_id, status, child_count, duration_ms }` and `scope: Some(Scope { id, depth, role: Terminal })`, appended after the last child. The existing Header `Act` entry keeps its own status-update path (unchanged) so the two entries visually frame the scope — Header at open, Terminal at close, children between.
- **Terminal-row lookup isolation.** The Terminal entry uses `TraceKind::ScopeTerminal`, NOT `TraceKind::Act`. `find_act_by_id_mut` (`mod.rs:834-855`) ignores non-Act kinds, so a late `ToolCallUpdate` carrying the Task's `tool_call_id` resolves to the Header Act entry (as today), never to the synthesized Terminal. This closes the "late ToolCallUpdate hijacks Terminal" hole.
- **Child-count source:** counted forward from the Header entry's index at terminal emission time only. The Header row is never mutated to reflect child counts. Once the Terminal is synthesized, its `child_count` is frozen — it does NOT update on late-arriving children (see next bullet).
- **Late-child-after-Terminal.** If a child `ToolCall` arrives after its parent scope's Terminal has been synthesized, it still renders with the scope's gutter glyph, color, and badge, with an inline `(late)` suffix on the child row. The Terminal row's `child_count` is not revised — revising it would require a backward `dirty_from` jump across all rows between the late child and the Terminal. The `(late)` suffix signals the discrepancy to the user without invalidating any earlier rendering.
- **`dirty_from` discipline.** The spec does NOT claim `dirty_from` is strictly forward-monotonic — it is not, and neither is the current code at `mod.rs:834-855`, which sets `dirty_from = min(dirty_from, idx)` on every `ToolCallUpdate`. The real invariant: every cache-invalidation index is **≥ the Header's creation index** for that scope. Header-status updates dirty from `header_idx`; Terminal appends dirty from `terminal_idx`; late-child appends dirty from `late_child_idx`. No invalidation reaches any row strictly before `header_idx`. This is the existing ToolCallUpdate behavior — the new design adds no new backward-jump pathology.
- Rejected alternative: mutating the Header row's role from `Header` → `Terminal` on terminal arrival. Rejected because it collapses the scope's visual framing into a single morphing row, making "where did this Task begin?" unrecoverable after completion.

### 3.3 Color allocator (`react_trace/scope_colors.rs` — new module)

```rust
pub struct ScopeColorAllocator {
    next: u8,
    assigned: HashMap<ScopeId, Color>,
}

impl ScopeColorAllocator {
    pub fn color_for(&mut self, id: &ScopeId) -> Color { ... }
    pub fn reset(&mut self) { ... }
}
```

- Assignment-on-first-sight, modulo a fixed palette of ratatui `Color` values (proposed: Cyan, Magenta, Yellow, Green, Blue, LightRed, LightCyan, LightMagenta — 8 values, avoids pure Red/White reserved for errors/normal text).
- Stable within a session; reset when the session resets.
- Lives on `ReactTrace` (session-scoped), borrowed `&mut` during dispatch and render — no `Arc<Mutex<>>` needed since UI thread is single-writer.
- Rejected alternatives: pure hash modulo palette (risks collisions between concurrent Tasks); random (non-reproducible for debugging).

### 3.3.1 Orphan-warn dedup state (`react_trace/mod.rs`)

Adds a single field to `ReactTrace`:

```rust
pub(super) orphan_warned: HashSet<ScopeId>,
```

- Dispatch path: when an Orphan is classified, `if self.orphan_warned.insert(pid.clone()) { tracing::warn!(...) }` fires the log exactly once per unknown parent id, for the life of the trace.
- Reset policy: cleared together with `entries` on any session reset code path that already clears the trace (grep `self.entries.clear()` will identify every such site; `orphan_warned.clear()` must be added adjacent to each).
- Not serialized; transient per running session.

### 3.4 Rendering treatment (`render.rs` + `compact_render.rs`)

For each entry with `Some(scope)`:

**Gutter column.** One cell wide, prepended before all other content. Glyph depends on role:

| Role | Glyph | Color |
|---|---|---|
| Header | `◢` | scope color |
| Child | `┃` | scope color |
| Terminal | `◆` | scope color |
| Orphan | `⚠` | yellow (overrides scope color) |

**Badge.** Short identifier derived from the scope's assignment order: `[T1]`, `[T2]`, … Not the full `ScopeId.0` (UUIDs are too long). Allocator tracks the short label alongside the color.

**Header row format:**

```
◢ Subagent [T1]  <title>                    <status>
```

**Child row format:**

```
┃ [T1] ▸ <tool>  <input_summary>            <status>
```

**Terminal row format:**

```
◆ Subagent [T1]                             completed · N calls · <duration>
```

**Orphan row format:**

```
⚠ [?]  ▸ <tool>  <input_summary>            <status>   (orphan)
```

**Late-child row format** (child arrives after its Terminal fired):

```
┃ [T1] ▸ <tool>  <input_summary>            <status>   (late)
```

**Warning style reuse.** The orphan `⚠` glyph and `(orphan)` suffix use a new `warning` `Style` added to `trace_format.rs` alongside the existing family/outcome styles — Yellow/Amber foreground, no background. Keeps the orphan marker visually consistent with any future warning treatment SPUR adds to other trace surfaces.

### 3.5 Width-tiered degradation

Locate width check in the render helpers where `last_render_width` is known. Every tier retains **at least one non-color identity carrier** so `NO_COLOR` / 8-color / dim-terminal users are never left with color as the sole disambiguator:

| Width | Treatment | NO_COLOR identity fallback |
|---|---|---|
| ≥ 80 | Full: gutter + bracketed badge `[T1]` + terminal child count + duration | badge |
| 72–79 | Gutter + bracketed badge `[T1]` (brackets retained — they read as a distinct token, whereas bare `T1` bleeds into tool names like `Task` / `Test`) + terminal child count | badge |
| 60–71 | Gutter + **numeric-augmented glyph** `┃1` / `┃2` / … (single digit appended into the gutter column pair) | numeric suffix |
| < 60 | Depth indent (`"  "` × depth) + numeric suffix only: `  1▸ <tool>` | numeric suffix |

**`NO_COLOR` environment variable.** Honored by respecting ratatui's existing `Color` → no-op conversion when the detector trips. Since every tier carries a non-color identity token (badge or numeric suffix), the rendering remains correct — only the gutter/badge *color* is suppressed, never the glyph or token.

**Width detection source.** `last_render_width` on `ReactTrace` already tracks the most-recent paint width. The width-tier function is a pure `(u16) -> Tier` mapping; easy to unit-test independently.

### 3.6 Invariant handling summary

| Concern | Resolution |
|---|---|
| Orphan child | Explicit `ScopeRole::Orphan` + dedup warn via `ReactTrace.orphan_warned: HashSet<ScopeId>` (§3.3.1) |
| Parallel Tasks | Per-scope gutter color + bracketed badge + width-tier non-color fallback (§3.5) |
| Nested sub-agents (depth > 1) | Recursive depth via `ctx.tool_depth` lookup, capped at 8 |
| Depth > 8 clamp | One-time `tracing::warn!(tool_call_id = ?, depth = actual)` on first observation per tool_call_id; `Scope.depth` saturates at 8 |
| Task terminal before last child | Depth map is keyed on tool_call_id; not cleared on Terminal. Late children render with `(late)` suffix; Terminal `child_count` is frozen at synthesis |
| Late `ToolCallUpdate` after Terminal | Terminal uses `TraceKind::ScopeTerminal` (non-Act); `find_act_by_id_mut` resolves to Header only |
| `tool_depth` memory growth | Soft-cap warning (Phase 1); LRU eviction (Phase 2 — defer until observed) |
| Cache invalidation | `dirty_from` may jump to `header_idx` on Header status update, matching existing `ToolCallUpdate` behavior at `mod.rs:834-855`. No backward jump past any Header's own creation index. |
| Glyph collisions | Confirmed no exact collision with `trace_format.rs:20-51` (`⚙ ✎ ✗ → 🔎 $ ◈ ↯ ⇄ ▸ ⧉ 🔧` / `✓ ✗ ?`). Near-miss: `◈` (thinking, `trace_format.rs:28`) vs `◆` (Terminal) in some terminal fonts — snapshot-test required; if fails, swap Terminal glyph to `▣` or `■` |
| `NO_COLOR` degradation | Every width tier carries a non-color identity token (badge or numeric suffix); color is additive only (§3.5) |

## 4. Alternatives considered (rejected)

### 4.1 Grouped contiguous block (tree rendering)

Buffer child entries and render them under their Task parent with `├ └` connectors. Rejected because:

- Reorder breaks the append-only invariant, invalidates scroll anchors, and forces wholesale cache rebuild on every Task terminal.
- Streaming behavior degrades: a tree cannot render until the Task completes (the last child determines `└`). In parallel-Task scenarios the user stares at a half-built tree while another Task is still live.
- Reorder is a timeline lie: parent thoughts that were genuinely concurrent with the Task get pushed after it.

### 4.2 Hybrid inline-then-collapse

Render inline during streaming (like 3.4), then collapse children into a folded row on Task terminal. Rejected **as Phase 1** because:

- Adds fold state, input handling for expand/collapse, and an additional cache invariant.
- Preserving fold state across scroll and search adds a parallel layer to the cache architecture.
- When reframed as "Option A + hide-flag", the fold feature becomes strictly additive on top of the current design. Therefore kept as Phase 2 (see §6).

## 5. Acceptance criteria

1. A single-Task trace with three children and an interleaved parent thought renders as in the example of §3.4 at ≥80-col width, with visually distinct gutter for all three child rows and the intervening parent thought carrying no gutter.
2. Two concurrent Tasks render with distinct colors and distinct `[T1]` / `[T2]` badges; children are attributable to the correct Task by gutter color OR (under `NO_COLOR`) by badge token alone.
3. An orphan child (crafted fixture where the parent id is absent) renders with `⚠` gutter and `(orphan)` annotation, and emits exactly one `tracing::warn!` per orphan id for the life of the trace. Verified by driving the dispatch twice with the same orphan id and asserting only one warn fires.
4. Depth > 8 inputs saturate at 8 in the rendered indent; a `tracing::warn!` fires exactly once per `tool_call_id` whose depth was clamped; no panic, no overflow.
5. Non-Claude fixtures (any existing `tests/fixtures/notifications/` entry without `_meta.claudeCode.parentToolUseId`) render identically to their pre-change output. Captured via a golden-output test.
6. Cache invariants: in a 1000-entry stream test, every `dirty_from` value observed is ≥ the Header creation index of any open scope. Verified by instrumenting `mark_dirty_from_for_update` in a test build. The spec does NOT require `dirty_from` strictly forward-monotonic — that would contradict the existing `ToolCallUpdate` behavior at `mod.rs:834-855`.
7. Compact surface at 72-col width: gutter + bracketed `[T1]` badge fits on one row alongside tool name and status; no horizontal overflow.
8. Compact surface at 60-col width: gutter column renders with numeric suffix (`┃1`); content fits; scope identity recoverable without color.
9. Compact surface at `<60` width: depth indent + numeric suffix only; content fits; scope identity recoverable without color.
10. **Late-child after Terminal.** Drive sequence `ToolCall(Task) → ToolCall(child A) → ToolCallUpdate(Task=completed) → ToolCall(child B)`. Assert: Terminal row's `child_count == 1` and does NOT change; child B renders with scope gutter + `(late)` suffix; Terminal row remains positioned between child A and child B in the entry Vec.
11. **Late ToolCallUpdate after Terminal.** Drive sequence `ToolCall(Task) → ToolCallUpdate(Task=completed) → ToolCallUpdate(Task=<arbitrary field>)`. Assert: the second update resolves to the Header `Act` entry (not the `ScopeTerminal` entry); `ScopeTerminal.status` and `child_count` remain unchanged; `find_act_by_id_mut` continues to skip the ScopeTerminal.
12. **NO_COLOR rendering.** With `NO_COLOR=1` in the environment (or the equivalent ratatui flag), §3.5 width tiers still produce a non-empty scope identity token on every row with `Some(scope)`.
13. `scripts/check-no-vendor-meta-leak.sh` still passes (the new code must consume `SpurToolMeta`, not raw `_meta`).

## 6. Deferred (Phase 2+ / YAGNI)

- **Fold/collapse affordance.** Press `z` on a completed scope to hide its children; press again to unfold. Requires a `HashSet<ScopeId>` fold-state on `ReactTrace` and input routing; render pass skips entries whose `scope.id` is folded. Data model unchanged.
- **Sticky scope header.** When the user scrolls inside a Task scope spanning >1 screen, the Header row leaves the viewport — "where am I?" is lost. Phase 2 can sticky-pin the nearest open Header to the pane top, or show `[T1] Task <title>` in the pane block title (`render.rs:276-281` is the location). Requires a reverse "which scope is the current top row inside?" lookup; not done in Phase 1.
- **Task legend footer.** A small footer listing `[T1] = cyan · [T2] = magenta · ...` to help users reconcile colors at a glance when many parallel Tasks are active.
- **Tree-summary pane.** Option B (grouped tree rendering) rejected for the live trace surface, but could live peacefully as a post-session summary pane — "What did each sub-agent do?" — keyed on the completed scopes. Separate surface, separate component.
- **`tool_depth` LRU eviction.** Phase 1 only adds a soft-cap warning. Promote to LRU when long-session growth is observed.
- **Cross-agent parent extraction.** Today only Claude populates `parent_tool_use_id`. When Codex/Kiro adapters gain equivalents, `adapter/mod.rs:188` is the natural seam; no rendering change needed.
- **PR #855 migration.** When upstream lands `parentSessionId` on `ToolCall`, change the `ScopeId` source in `dispatch.rs` from `meta.parent_tool_use_id` to `tc.parent_session_id`. Zero change to `Scope`, `ScopeRole`, rendering, or caches.

## 7. Risks

- **Glyph near-miss: `◈` (thinking, `trace_format.rs:28`) vs `◆` (Terminal).** Visually distinct in most fonts but can blur in dim terminals or certain CJK-oriented fallback fonts. Mitigation: snapshot-test both glyphs at render time; if the test fails on the project's reference terminal, swap Terminal glyph to `▣` or `■` before merge. Exact collisions already confirmed absent against `trace_format.rs:20-51` (`⚙ ✎ ✗ → 🔎 $ ◈ ↯ ⇄ ▸ ⧉ 🔧` / `✓ ✗ ?`).
- **Color allocator exhaustion at >8 concurrent Tasks.** Palette recycles after 8; distinct color no longer guaranteed. Acceptable for Phase 1 — Claude rarely runs more than 2–3 parallel Tasks — but documented; the bracketed badge `[Tn]` still gives unique identity through at least 99 concurrent scopes.
- **Terminal child-count accuracy.** Counting forward from Header scans entries; cost is O(Task-span). For a long-running Task with many children, that is still bounded and only runs once at terminal emission. Late children after Terminal intentionally do NOT update the count (see §3.2) — the `(late)` suffix on their rows communicates the discrepancy.
- **`ScopeColorAllocator` borrow shape.** The allocator lives on `ReactTrace` behind `&mut`. Dispatch (write) and render (read-only after initial assignment) must not interleave mid-operation. Safe under the current UI-thread serialization guarantees; confirm no future async render path introduces a conflicting borrow.
- **Near-zero existing test coverage for `ToolCall → Terminal → late` sequences.** `streaming_tests.rs:1811-1868,2062-2107` covers message/thought chunks and single-Act status mutation but not the synthesized-Terminal sequences §5 criteria 10–11 depend on. Test scaffolding must be added as part of this work.
- **Color-free identity still relies on an 8-value palette.** At very high concurrency (10+ Tasks), two badges reuse the same color. Numeric suffix continues to distinguish them at <80 cols; at ≥80 cols the bracketed badge is always present. Risk only manifests if both badges scroll off at once — low likelihood.

## 8. Files touched

- `crates/spur-tui/src/components/react_trace/types.rs` — add `ScopeId`, `ScopeRole`, `Scope`, extend `TraceEntry`, add `TraceKind::ScopeTerminal`.
- `crates/spur-tui/src/components/react_trace/dispatch.rs:69-80` — replace depth-bare block with scope classification; hook Terminal synthesis into `SessionUpdate::ToolCallUpdate` terminal-status path.
- `crates/spur-tui/src/components/react_trace/scope_colors.rs` — new module with `ScopeColorAllocator`.
- `crates/spur-tui/src/components/react_trace/mod.rs` — add `ScopeColorAllocator` and `orphan_warned: HashSet<ScopeId>` fields on `ReactTrace`; clear both in every session-reset code path.
- `crates/spur-tui/src/components/react_trace/render.rs` — gutter + badge + Header/Terminal row treatment; `find_act_by_id_mut` already skips non-Act kinds (§3.2 Terminal-row lookup isolation), verify unchanged.
- `crates/spur-tui/src/components/react_trace/compact_render.rs` — same, with width-tier degradation.
- `crates/spur-tui/src/components/trace_format.rs` — add `warning` `Style` (Yellow/Amber) for orphan rendering; keep existing family/outcome glyph tables unchanged.
- `crates/spur-tui/src/components/react_trace/streaming_tests.rs` — golden-output tests for §5 criteria, including new sequences for criteria 10–12.
- (Fixtures) `crates/spur-acp/tests/fixtures/notifications/claude-code-acp/tool_call_subagent_task.json` — already covers Claude case; add a new orphan fixture `tool_call_orphan_child.json`.

## 9. Out of scope

- Changes to `spur-acp` data model or the `SpurToolMeta` contract.
- Changes to `adapter/claude.rs` extraction logic.
- The `0.26.0 → 0.30.0` version bump (already landed in `.spur/config.toml`).
- Any cross-agent promotion of `parent_tool_use_id` into `codex`/`kiro`/`generic` adapters.
- Any change to `AgentKind` or `ToolFamily` classification.
