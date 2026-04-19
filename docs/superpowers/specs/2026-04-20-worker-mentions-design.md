# Worker Mentions in the TUI `@`-Picker

**Status:** approved (design)
**Date:** 2026-04-20
**Owner:** TUI
**Related code:** `crates/spur-tui/src/mentions/`, `crates/spur-tui/src/components/query_source.rs`, `crates/spur-tui/src/app.rs`
**Supersedes nothing.** Extends `2026-04-13-chat-input-commands-mentions-design.md` (the trait-based `MentionSource` registry it set up is the seam this spec plugs into).

## 1. Goal & scope

Make worker agents first-class citizens of the TUI `@`-mention picker so the user can compose a brain-session prompt like:

> Refactor `@crates/spur-acp/src/registry.rs`. `@worker:claude-code` would be a good fit.

…and have the brain LLM treat the worker mention as a **strong preference** (not a hard override) when it decides whether and where to delegate.

### In scope (this spec)

- A `WorkerMentionSource` that emits one mention entry per agent whose `role ∈ {worker, both}`.
- Per-session source registration: brain sessions get `[FileMentionSource, WorkerMentionSource]`; direct (single-agent) sessions keep `[FileMentionSource]` only.
- Picker ranking refinements so workers are visible on first `@` keypress and surface above tied file matches without overriding clear file-specific queries.
- Send-time prepend of a one-line preference hint as the first `ContentBlock::Text` of the outgoing user message when worker mentions are present.
- Visual differentiation in the picker (icon + tier tag + description) using existing `RetrievalRow.{primary, secondary, tag}` slots.

### Explicitly out of scope (future)

- **Hard-route bypass.** Workers in mentions never bypass the brain. Direct dispatch is already covered by direct (non-brain) sessions.
- **Structured ACP envelope field.** No new field on `Action::SendMessage` or any ACP type; the hint travels as an in-band prepended `ContentBlock::Text`.

- **Per-turn system-prompt injection.** The orchestrator builds the brain's system prompt once at session creation (`Orchestrator::build_brain_prompt_v1`, `crates/spur-core/src/orchestrator.rs:2467`). There is no per-turn system-prompt seam in the TUI, and we are not adding one. The hint rides the user turn instead.
- **Sectioned picker (`── Workers ──` / `── Files ──` headers).** Would require changes to `RetrievalRow` and `PickerShell` selection/scroll math; revisit when mention kinds grow to 3+.
- **Worker info expansion** (e.g., `@worker:foo!` to inline its capability sheet). Layered feature; not core to "mention a worker for tasks".
- **Recency / usage-based ranking** of workers within the picker.

## 2. Motivation

The `delegate_to_worker` MCP tool already lets the brain dispatch tasks to specific workers. Today the user's only way to influence that choice is to write English in the prompt ("use claude-code please"). That has two problems:

1. **No discoverability.** The user must already know which workers exist and what their handles are.
2. **No structural elevation.** A buried English sentence competes with the rest of the prompt; the brain may interpret it loosely or miss it.

A `@`-picker entry for each worker fixes (1) (autocomplete teaches the catalog) and a one-line system-prompt addendum at send fixes (2) (the preference is hoisted into a deterministic instruction the brain sees in its system context).

The brain remains authoritative — it can still refuse the suggestion based on `delegation.avoid_for` matches or its own task analysis. We are not bypassing the orchestrator's routing; we are giving the user a structured way to *suggest* into it.

## 3. Architecture

### 3.1 Data flow

```
.spur/config.toml ─┐
                   ▼
         Vec<AgentConfig>          (filtered by role ∈ {worker, both})
                   │
                   ▼
        Vec<WorkerMentionDescriptor>   (snapshot held by app.rs)
                   │
                   ▼
        WorkerMentionSource ──┐
                              │
        FileMentionSource  ───┴──▶ MentionRegistry (per session)
                                          │
                          query("...")    │
                                          ▼
                                   Vec<MentionEntry>
                                          │
                                          ▼
                          MentionQuerySource → RetrievalRow → PickerShell
                                          │
                              user picks ─┘
                                          ▼
                  RetrievalAccept::InsertAtom { uri: "worker://<name>", … }
                                          │
                                          ▼
                          InputBar atom in prompt buffer
                                          │
                              user submits prompt
                                          ▼
              SendPath (session_detail.rs:921):
              scan ProtectedRange.uri for worker:// prefixes
                                          │
                                          ▼
        prepend_worker_hint() inserts a ContentBlock::Text
        as blocks[0]; worker atoms also remain inline as
        ResourceLink (transcript fidelity preserved)
                                          │
                                          ▼
                    Action::SendMessage { blocks, … }
                                          │
                                          ▼
                          Brain LLM sees hint as first
                          item in the user turn
                                          │
                                          ▼
                          delegate_to_worker(...) — brain's choice
```

### 3.2 Module layout (no new crates)

```
crates/spur-tui/src/mentions/
├── entry.rs           ← extend MentionKind, MentionEntry
├── file_source.rs     ← unchanged
├── worker_source.rs   ← NEW
├── registry.rs        ← per-session source list, ranking refinements
└── mod.rs             ← re-export new types
```

```
crates/spur-tui/src/app.rs                 ← snapshot construction + plumbing into SessionDetailView
crates/spur-tui/src/views/session_detail.rs ← snapshot field, brain-vs-direct registry constructor selection,
                                              prepend_worker_hint() call site at line ~921
crates/spur-tui/src/mentions/hint.rs       ← NEW (small, testable): prepend_worker_hint helper
crates/spur-tui/src/components/
└── query_source.rs                        ← plumb secondary/tag into RetrievalRow
```

`submit_router::route` is **not** modified. The hint is layered onto its `SubmitDecision::Send` output in the view, keeping the router signature stable.

## 4. Component designs

### 4.1 `MentionEntry` extensions (`mentions/entry.rs`)

Add a `Worker` variant and two optional display slots. File entries leave the new fields empty, preserving today's render exactly.

```rust
pub enum MentionKind {
    File,
    Directory,
    Worker,                              // NEW
}

pub struct MentionEntry {
    pub kind: MentionKind,
    pub uri: String,                     // file://… or worker://…
    pub display: String,                 // "src/foo.rs" or "worker:claude-code"
    pub secondary: Option<String>,       // NEW — worker description; None for files
    pub tag: Option<String>,             // NEW — "specialist" / "generalist"; None for files
}
```

The existing `entry_for_path` helper continues to construct `File`/`Directory` entries with `secondary = None, tag = None`.

### 4.2 `WorkerMentionSource` (`mentions/worker_source.rs`)

Holds an immutable snapshot of worker descriptors. `build()` ignores `cwd` — workers are global.

```rust
#[derive(Debug, Clone)]
pub struct WorkerMentionDescriptor {
    pub name: String,                    // unique slug, e.g. "claude-code"
    pub description: Option<String>,     // delegation.description
    pub tier: Option<String>,            // "specialist" | "generalist"
}

pub struct WorkerMentionSource {
    snapshot: Vec<WorkerMentionDescriptor>,
}

impl MentionSource for WorkerMentionSource {
    fn name(&self) -> &'static str { "worker" }

    fn build(&mut self, _cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        Ok(self.snapshot.iter().map(|d| MentionEntry {
            kind: MentionKind::Worker,
            uri: format!("worker://{}", d.name),
            display: format!("worker:{}", d.name),
            secondary: d.description.clone(),
            tag: d.tier.clone(),
        }).collect())
    }
}
```

Snapshot construction lives in `app.rs`, populated at app construction from the same `Vec<AgentConfig>` the orchestrator uses, filtered by `role ∈ {worker, both}` — identical to the filter `McpServer::set_workers` uses (`crates/spur-mcp/src/server.rs:640`). The snapshot is held in `App` state alongside the existing agent registry handle.

On agent-registry reload (rare; user-initiated config change): rebuild the snapshot, then drop and rebuild the per-session `MentionRegistry` for any open brain session (cheaper than a surgical cache invalidation and rare enough that the cost is irrelevant). A small `MentionRegistry::clear_cache()` method is added for this; it empties the `HashMap<String, CachedIndex>`.

### 4.3 `MentionRegistry` per-session source list (`mentions/registry.rs`)

Today `MentionRegistry::new()` constructs a fixed `[FileMentionSource]`. Replace with two constructors:

```rust
impl MentionRegistry {
    /// Source list for sessions where brain delegation is meaningful.
    pub fn for_brain_session(workers: Vec<WorkerMentionDescriptor>) -> Self { … }

    /// Source list for direct (single-agent) sessions; no worker mentions.
    pub fn for_direct_session() -> Self { … }
}
```

The cache key remains the `SessionId`; the source list lives on the registry. Constructors are called wherever a `SessionDetailView` (or its picker harness) is built — today `Rc<RefCell<MentionRegistry>>` is passed into `MentionQuerySource::new` (`query_source.rs:270`). The view chooses which constructor to call based on whether the session's executor role is `brain`.

If a session's role is unknown at registry-build time (edge case), default to `for_direct_session()`. Worker mentions are an additive affordance; missing them is harmless.

**Snapshot plumbing.** `SessionDetailView::new` (currently 5 parameters: `session_id, agent_name, role, cwd, agent_cfg` — see `session_detail.rs:108`) gains a 6th: `worker_snapshot: Vec<WorkerMentionDescriptor>`. The single production call site is `app.rs:760`; four test call sites in `session_detail.rs` (lines 1657, 1779, 1816, plus any in test modules) pass `Vec::new()`. The view stores the snapshot, derives `known_worker_names: HashSet<String>` once, and uses both: the snapshot to construct `MentionRegistry::for_brain_session(snapshot.clone())`, the name set to drive `prepend_worker_hint`.

### 4.4 Picker ranking refinements (`registry.rs::query`)

**Empty-query branch** (today: `entries.iter().take(limit).cloned()` then sort by `display.len()`):

1. Take all worker entries from the cached index, capped at `WORKER_PIN_CAP = 6`, stable-sorted by `display.len()` then `name`.
2. Take files sorted by `display.len()` to fill the remaining `limit - workers_taken` slots.
3. Concatenate `[workers, files]` and return.

**Typed-query branch** (today: nucleo score → sort desc, tiebreak by `display.len()`):

1. Score every entry as today.
2. After scoring, apply a multiplicative boost for worker entries: `score = score * WORKER_SCORE_NUM / WORKER_SCORE_DEN` where `(NUM, DEN) = (5, 4)` (≈ +25 %).
3. Sort by adjusted score desc; tiebreak by `display.len()` asc (existing rule).
4. Take `limit`.

Both constants are `pub(super) const` at the top of `registry.rs` with a doc comment explaining the rationale and how to tune.

Rationale for the boost magnitude: nucleo scores for short exact matches dwarf 25 %, so `@src/spur-acp/foo.rs` still picks the file. But for ambiguous queries like `@cla` where multiple paths and `worker:claude-code` all match, the boost reliably surfaces the worker because its display starts with `worker:` (full substring match for `cla` is a `c`-class hit either way) — empirical tuning in tests will validate.

### 4.5 Picker render (`components/query_source.rs::MentionQuerySource::refresh`)

Today rows render as `📁 @path/` or `📄 @path` with empty `secondary` and `tag`. Extend:

```rust
let (icon, tag_render) = match m.kind {
    MentionKind::Directory => ("\u{1F4C1}", String::new()),
    MentionKind::File      => ("\u{1F4C4}", String::new()),
    MentionKind::Worker    => ("\u{1F916}", m.tag.clone()
        .map(|t| format!("\u{27E8}{}\u{27E9}", t))   // ⟨specialist⟩
        .unwrap_or_default()),
};

RetrievalRow {
    primary:   format!("{} @{}", icon, m.display),  // "🤖 @worker:claude-code"
    secondary: m.secondary.clone().unwrap_or_default(),
    tag:       tag_render,
    atoms:     Vec::new(),
}
```

`accept(row_idx)` continues to construct `RetrievalAccept::InsertAtom { text: format!("@{}", hit.display), uri: hit.uri.clone(), name: hit.display.clone(), … }`. For workers: `text = "@worker:claude-code"`, `uri = "worker://claude-code"`, `name = "worker:claude-code"`. The `InputBar` already renders this as a single-unit atom (existing `ProtectedRange` machinery).

### 4.6 Send-time hint prepend (`session_detail.rs` + `mentions/hint.rs`)

**Architectural note (corrects an earlier draft).** The brain's "system prompt" is built once per session by `Orchestrator::build_brain_prompt_v1` (`crates/spur-core/src/orchestrator.rs:2467`). There is no per-turn system-prompt seam in the TUI, and adding one would require a cross-crate orchestrator/ACP change (out of scope per §1). The hint therefore travels **as the first `ContentBlock::Text` of the user turn** — in-band, unambiguously attributable to the UI, and reversible by a single-line deletion.

The seam is `crates/spur-tui/src/views/session_detail.rs:921`, immediately before the `SubmitDecision::Send { blocks, interrupt }` is converted into `Action::SendMessage`. The view owns:

- `worker_snapshot: Vec<WorkerMentionDescriptor>` — the same snapshot threaded into the `MentionRegistry` constructor (§4.3).
- `known_worker_names: HashSet<String>` — derived once from `worker_snapshot`, kept in sync.

Helper, in a new tiny module `crates/spur-tui/src/mentions/hint.rs` for unit testability:

```rust
use std::collections::HashSet;
use crate::components::input_bar::ProtectedRange;
use spur_acp::{ContentBlock, TextContent};

/// If the outgoing message has any worker:// atoms whose names are
/// in `known_workers`, prepend a single ContentBlock::Text hint to
/// `blocks` and return true. Otherwise leave `blocks` untouched and
/// return false.
///
/// Worker atoms are NOT removed from the message body — they continue
/// to serialize as ResourceLink via the existing `assemble_blocks`,
/// preserving transcript fidelity.
pub fn prepend_worker_hint(
    blocks: &mut Vec<ContentBlock>,
    ranges: &[ProtectedRange],
    known_workers: &HashSet<String>,
) -> bool {
    let mut names: Vec<&str> = ranges
        .iter()
        .filter_map(|r| r.uri.strip_prefix("worker://"))
        .filter(|n| known_workers.contains(*n))
        .collect();
    names.sort();
    names.dedup();
    if names.is_empty() {
        return false;
    }
    let hint = format!(
        "[UI hint] User-suggested workers for delegation this turn: {} \
         (preference, not override; honor unless `delegation.avoid_for` clearly matches).",
        names.join(", ")
    );
    blocks.insert(0, ContentBlock::Text(TextContent::new(hint)));
    true
}
```

Call site in `session_detail.rs` (sketch — exact match to existing match-arm structure):

```rust
SubmitDecision::Send { mut blocks, interrupt } => {
    if self.role == "brain" {
        let _ = crate::mentions::hint::prepend_worker_hint(
            &mut blocks,
            self.input_bar.protected_ranges(),  // existing accessor or add a small one
            &self.known_worker_names,
        );
    }
    Some(Action::SendMessage {
        session: self.session_id.clone(),
        blocks,
        interrupt,
    })
}
```

If `protected_ranges()` is not already exposed on `InputBar`, add a minimal `pub fn protected_ranges(&self) -> &[ProtectedRange]` accessor. The field is already `pub(crate)`-reachable via existing methods like `range_at_cursor`; this is a tiny additive change.

**Direct sessions skip the helper entirely** (`role != "brain"`). Worker mentions can't be picked there (per §4.3 source-list selection), so the guard is defense in depth against pasted text and a clear signal in code.

**Behavior summary:**

| Mention pattern in outgoing message | Resulting `blocks` |
|---|---|
| No worker atoms | unchanged: `[Text, ResourceLink?, Text?, …]` (today's behavior) |
| One known worker | `[Text("[UI hint] … claude-code …"), <today's blocks…>]` |
| Multiple known workers | `[Text("[UI hint] … claude-code, codex …"), …]` (deduped, sorted) |
| Worker atom whose name is unknown to the registry | unknown name dropped from hint; if all unknown, no hint prepended |
| Worker mention in a direct session (e.g., pasted) | helper not invoked; literal text passes through |

## 5. Error handling & edge cases

| Case | Behavior |
|---|---|
| Worker name in atom no longer in registry | Drop from hint silently. Literal `@worker:foo` text passes through. |
| Same worker mentioned multiple times | Dedupe in `worker_preference_hint`. |
| Multiple distinct workers mentioned | Comma-list in the hint. Brain may interpret as candidate set or as parallel-delegation signal. |
| Worker mention pasted into a direct session | `WorkerMentionSource` is not registered, so it can't be picked. If pasted text contains `@worker:foo`, the input is plain text — no atom, no hint injection. Harmless. |
| Agent registry reload mid-session | Rebuild the worker descriptor snapshot in `App`; call `MentionRegistry::clear_cache()` so the next `query()` rebuilds with the new worker list. |
| `role = "brain"`-only agents | Filtered out at snapshot build (only `worker` and `both` are included). |
| Empty workers list (no agents have worker role) | `WorkerMentionSource::build` returns `vec![]`. Picker shows files only. |

## 6. Testing strategy

Unit tests in `crates/spur-tui/src/mentions/`:

1. **`worker_source_emits_one_entry_per_descriptor`** — descriptor list of 3 → 3 `MentionEntry` rows with `MentionKind::Worker`, correct `uri` / `display` / `tag`.
2. **`registry_empty_query_pins_workers_first`** — registry seeded with 6 workers and 100 files; empty query → first N rows are workers, rest are files in `display.len()` order.
3. **`registry_empty_query_caps_workers_at_pin_cap`** — 10 workers in source; only `WORKER_PIN_CAP` appear at the top of the empty-query result.
4. **`registry_typed_query_boosts_workers_in_ties`** — query `cla` with `worker:claude-code` and `claudia.rs` both present; worker ranks first.
5. **`registry_typed_query_does_not_clobber_strong_file_matches`** — query `src/spur-acp/registry.rs` (a real file present); the file is row 0 despite worker presence.
6. **`prepend_worker_hint_dedupes_and_validates`** — given ranges `[worker://a, worker://a, worker://missing, worker://b]` and known `{a, b, c}`, returns `true` and `blocks[0]` is a `Text` whose body lists `a, b` exactly once.
7. **`prepend_worker_hint_noop_when_no_worker_ranges`** — ranges list contains only `file://` URIs → returns `false`, `blocks` unchanged.
8. **`prepend_worker_hint_noop_when_all_unknown`** — only worker URIs whose names aren't in `known_workers` → returns `false`, `blocks` unchanged.

Integration touchpoints:

9. **Picker smoke test** in the existing session-detail / picker tests: open `@` picker in a brain session, confirm worker rows appear in the empty state; type `@wo`, confirm `worker:*` rows rank at top; pick one, confirm a `ProtectedRange` with `uri = "worker://…"` is inserted into the input buffer.
10. **Send-path integration test** (new file under `crates/spur-tui/tests/`): construct a `SessionDetailView` with role `"brain"` and a snapshot of two workers; insert text + a worker atom into its `InputBar`; trigger submit; assert the resulting `Action::SendMessage { blocks, … }` has `blocks[0] == ContentBlock::Text(t)` where `t.text` starts with `"[UI hint]"` and contains the worker name; assert the original `ResourceLink` is preserved later in `blocks`.

**Empirical validation already performed.** A throwaway simulation under `cargo test -p spur-tui --test _nucleo_score_simulation` (deleted post-validation) confirmed §4.4's score-boost behavior with the live `nucleo-matcher 0.3` dependency. Concrete results recorded in §10 below.

## 7. Reversibility & migration

The whole feature collapses cleanly:

- Remove `MentionRegistry::for_brain_session` callers (use `for_direct_session` everywhere) → workers disappear from the picker.
- Delete the `prepend_worker_hint` call at `session_detail.rs:921` → no hint blocks are emitted; worker atoms still serialize as ResourceLink inline (Option A behavior).
- Keep `MentionEntry::{secondary, tag}` and the `Worker` variant — these are additive and don't constrain anything if unused.

No cross-crate schema migration. No ACP envelope changes. No orchestrator entry-point additions.

## 8. Future extensions

- **B2 — `list_available_workers` reordering.** If telemetry shows the brain ignoring the prose hint, reorder the tool's response so preferred workers appear first. Touches `spur-mcp`.
- **B3 — Structured envelope field.** A `preferred_workers: Vec<String>` field on the brain prompt envelope. Cross-crate; defer until B1 + B2 prove insufficient.
- **C-migration: sectioned picker.** When mention kinds grow to 3+ (e.g., adding inline slash-commands, recent threads, past sessions), introduce `RowKind::Header` to `RetrievalRow` and update `PickerShell` selection/scroll math. Scoped follow-up.
- **Worker info expansion.** `@worker:foo` followed by `?` (or similar) inlines the worker's full capability sheet (`good_for`, `avoid_for`, `output_shape`) into the prompt.

## 9. Non-goals reaffirmed

- No hard-route bypass of the brain.
- No new ACP, MCP, or orchestrator surface.
- No new picker abstractions (sections, headers, multiple triggers).
- No recency/usage ranking.
- No sub-arg autocomplete on worker mentions.

## 10. Implementability audit (pre-plan validation)

This spec was audited against the live codebase before producing an implementation plan. Each load-bearing assumption was either grounded in a file:line citation or empirically validated by simulation. Results:

| Assumption | Outcome | Evidence |
|---|---|---|
| `RetrievalRow` has `primary/secondary/tag/atoms` fields | ✅ verified | `crates/spur-tui/src/components/query_source.rs:25-35` |
| `InputBar` atoms preserve `{ uri, name }` | ✅ verified | `ProtectedRange { start, end, uri, name }` at `input_bar.rs:18-22`; `insert_atom(text, uri, name)` at `:1044` |
| Brain/direct distinction reachable at `MentionRegistry` construction | ✅ verified | `SessionDetailView::new(.., role: String, .., agent_cfg: Arc<AgentConfig>)` at `session_detail.rs:108` — both in scope |
| Per-turn brain system-prompt seam exists in TUI | ❌ **refuted** | Brain prompt is built once at session creation by `Orchestrator::build_brain_prompt_v1` (`crates/spur-core/src/orchestrator.rs:2467`). Spec was repaired in §4.6 to prepend an in-band `ContentBlock::Text` to the user turn at `session_detail.rs:921` instead. |
| `AgentRole` enum w/ `Brain/Worker/Both` and TUI-reachable filter | ✅ verified | `crates/spur-acp/src/types.rs:126`; `AgentRegistry::workers()` filter at `crates/spur-acp/src/registry.rs:85` |
| Nucleo `5/4` boost surfaces workers for ambiguous queries without clobbering strong file matches | ✅ empirically verified — see below |
| `MentionRegistry` cache trivially clearable | ✅ verified | `cache: HashMap<String, CachedIndex>` at `registry.rs:22` |
| `WORKER_PIN_CAP = 6` × `limit = 20` (call site `query_source.rs:300`) leaves ≥ 14 file slots | ✅ verified |
| MCP `worker_capable` filter mirrorable in TUI | ✅ verified | Same predicate as `registry.rs:85` |
| Adding `Option`-typed fields to `MentionEntry` is non-breaking | ✅ verified | One construction site (`entry_for_path` at `entry.rs:25`) |

### 10.1 Empirical nucleo simulation

A throwaway integration test (`crates/spur-tui/tests/_nucleo_score_simulation.rs`, deleted post-validation) was run against the live `nucleo-matcher = "0.3"` dependency with a 4-worker / 12-file fixture and the proposed `5/4` worker boost. Concrete observed scores:

| Query | Top result | Score | Comment |
|---|---|---|---|
| `cla` | `🤖 worker:claude-code` | 105 | Beats `📄 claudia.rs` (88) — ambiguous query surfaces worker. |
| `worker:co` | `🤖 worker:codex` | 305 | Worker namespace match wins decisively. |
| `src/spur-acp/registry.rs` | `📄 src/spur-acp/registry.rs` | 634 | Strong file match unaffected by worker boost. |
| `registry` | `📄 src/registry.rs` | 209 | Short substring still picks file (no workers contain the substring). |
| `toml` | `📄 Cargo.toml` | 104 | File-only query: no workers in result set. |

All 5 cases match the spec's §4.4 claims. The boost factor `(NUM, DEN) = (5, 4)` is approved as the implementation default; tests in §6 lock these expectations into the regression suite using the same fixture pattern.

### 10.2 Plumbing changes called out

- `SessionDetailView::new` signature gains a 6th parameter (`worker_snapshot: Vec<WorkerMentionDescriptor>`); 1 production + 4 test call sites updated (`app.rs:760`, `session_detail.rs:1657 / :1779 / :1816`, plus any subsequent test additions).
- `submit_router::route` is **not** modified.
- `InputBar` may need a tiny `pub fn protected_ranges(&self) -> &[ProtectedRange]` accessor if not already exposed; one-line additive change.

No other surface changes anticipated.
