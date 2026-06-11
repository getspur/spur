# Jute Notebook — Multi-Notebook Tabs Design

- **Status:** Approved (UI/UX approved; structural review complete)
- **Date:** 2026-06-09
- **Surface:** `crates/spur-notebook/jute-notebook` (Tauri + React frontend) and the
  daemon/MCP layer in `crates/spur-notebook/jute-notebook/src-tauri/` + `crates/spur-notebook/src/mcp/`
- **Approved visual:** scratch notebook `~/.spur/scratch/Untitled106.ipynb` (interactive
  desktop mock — tab strip beside the macOS traffic lights, 4 kernel states, agent-focus
  ring, notebook-scoped toolbar, toggleable design-note callouts)
- **Provenance:** produced via `/open-design`; structural analysis via `spur-analyst`
  against `.spur/analyst.duckdb` graph hash `7f051bb4…`; first-principles review via
  sequential-thinking

## 1. Problem & Goal

Today Jute is **one notebook per OS window**. Opening a notebook calls the Rust daemon
(`daemonControl({command:"open"})`), which spawns a fresh `WebviewWindow` at
`/notebook?path=…`. This is the README's design principle #1 ("the kernel as a window").
It does not scale to the workflows SPUR now creates: a person flipping between an analysis
notebook, an ETL pipeline, and an App-mode dashboard; or an agent orchestrating several
notebooks. Window-per-notebook means OS-window juggling, no at-a-glance view of which
notebooks hold live kernels, and (see §3) a **latent correctness hazard** when more than
one window is open at once.

**Goal:** one window hosts **N notebooks as tabs**, each preserving the
`1 notebook : 1 kernel : 1 lifecycle` invariant — closing a tab tears down its kernel.
This *generalizes* principle #1 from "kernel as a window" to "kernel as a tab" rather than
violating it. The agent's notion of "the current notebook" is **redefined** from "the one
loaded document" to "the focused tab."

### Non-goals (out of MVP scope)

- Split panes / side-by-side notebooks in one window. Tabs only.
- Cross-window tab drag (tear-off into a new window). Defer.
- Real-time multiplayer on a shared tab set.
- Replacing native macOS window tabbing — see §8 (it may be a cheaper partial win, but the
  backend work below is required regardless).

## 2. Grounding (verified against code + live `.spur/analyst.duckdb`)

This composes existing primitives; the kernel layer is **already** multi-notebook.

**Frontend is notebook-context-scoped — so the tab strip is cheap.** Every notebook
component reads its notebook via `NotebookContext` / `useNotebook()`
(`src/stores/notebook.ts`). `pages/NotebookPage.tsx` instantiates exactly one
`useMemo(() => new Notebook(), [])` per window, keyed off the `path` URL search param, and
registers it with `setActiveAgentNotebook(notebook)`. Wrapping N `Notebook` instances and
swapping the active one through the same context requires **no change to consuming
components**.

**The daemon document store is single, process-wide.**
`src-tauri/src/state.rs`: `State.notebook: Arc<Mutex<Option<Arc<NotebookStore>>>>` and
`State.datasource_catalog: Arc<Mutex<DatasourceCatalog>>` ("for the active notebook").
`State::get_notebook()` lazily initializes **one** store; `NotebookStore::load` /
`replace` (`commands.rs::replace_notebook_and_hydrate_catalog`, `handle_daemon_control_inner`
`LoadNotebook` arm) **overwrite** it.

**The kernel layer is already keyed — this is the template.** `State.kernels:
DashMap<String, KernelSlot>` keyed by path-derived slot IDs. `state.rs::notebook_slot_id`
has **22 callers**; `slot_id_for` / `slot_id_for_spec` / `notebook_path_from_slot_id`
round-trip path↔slot. Per-notebook kernel teardown is already expressible.

**`get_notebook` production blast radius is broader than the first pass showed.** The
initial structural review saw the obvious control-path sites, but current `main` also routes
agent/MCP and DAG execution through `state.get_notebook()`:
- `commands.rs::handle_daemon_control_inner` (the control dispatch)
- `commands.rs::replace_notebook_and_hydrate_catalog`
- `commands.rs::resolve_run_cell_dispatch`
- **cross-crate** `crates/spur-notebook/src/mcp/mod.rs::NotebookDaemonControl::persist_catalog_to_current_notebook`
- `crates/spur-notebook/src/mcp/tools/run_cell.rs`, `notebook_push_source.rs`,
  `notebook_dag_status.rs`, and `save.rs`
- `crates/spur-notebook/src/dag/run_context.rs` and `dag/engine.rs`

Treat Phase 1 as a registry substrate refactor, not a four-callsite patch. Every production
path that snapshots, loads, applies run events, saves, or mutates the document store must
either resolve through the focus pointer or accept an explicit `NotebookId` override.

**There are TWO single-"current"-notebook authorities, not one.** Besides the TS bridge's
module-global `activeNotebook` (`src/agent/bridge.ts`, which fires
`invoke("notebook_active_changed")`), the **Rust MCP server** owns its own current:
`crates/spur-notebook/src/mcp/mod.rs` has `current_path` (field **and** method),
`current_path_for_recents_event`, `close_current_window`, `persist_catalog_to_current_notebook`,
`kernel_alive_for_notebook`, `LastNotebookRecord`, `clear_last_notebook`. Every
`notebook_*` MCP tool the agent calls resolves against this single current.

**Mutating control commands carry no notebook id — they are implicitly targeted.**
`DaemonControlCommand::WriteCell { id, source, expected_version, last_edited_by }` (and
`InsertCell`, `DeleteCell`, `ReadCell`, set-metadata) hit `state.get_notebook()`. This
implicit-target ergonomics is *why* `notebook_write_cell` etc. take no notebook arg.

**Deltas are broadcast to all windows, filtered inbound by path.**
`stores/notebook.ts::notebookDeltaIsForPath(notebookPath, deltaPath)` drops deltas whose
path ≠ this window's. Its own comment: "the daemon holds a single process-wide store but
broadcasts every delta to all windows." This guards **inbound** rendering only.

**Co-change confirms a tight cross-crate / cross-language ring** (90-day,
`v_file_cochange`): `commands.rs ↔ mcp/mod.rs` **42×** (static edge), `commands.rs ↔
stores/notebook.ts` **27×** (no static edge = the IPC/bindings seam), `notebook_store.rs ↔
mcp/mod.rs` 14×, `state.rs ↔ mcp/mod.rs` 13×. Any change to the notebook-store model ripples
Rust daemon ↔ SPUR MCP ↔ TS frontend together by construction.

## 3. Latent correctness hazard (reproduce before relying on it)

**Static-analysis hypothesis — not yet reproduced.** With two windows open: `LoadNotebook`
replaces the single `NotebookStore`; opening notebook Y flips the daemon store from X to Y.
A subsequent `WriteCell` from window A (showing X) carries no path, so it lands on the store
now holding **Y**. `notebookDeltaIsForPath` only filters **inbound** deltas to frontends; it
does **not** guard the **outbound** write hitting the wrong document.

This is direct evidence the keying refactor (§5) is plausibly a **bug fix worth doing
independent of tabs**. Per the repo's verification discipline, the first task (§6, Phase 0)
is a **failing reproduction test**, not a fix — it either confirms or refutes this before
any design weight rests on it.

> Live confirmation seen while authoring the approved mock: `notebook_new` created the
> scratch file, but `write_cell`/`read_cell` returned `cell_not_found` until an explicit
> `notebook_open` — because the MCP tools target the daemon's single loaded "current"
> notebook. The single-current assumption is real and observable today.

## 4. Approved UI/UX

Reference: `Untitled106.ipynb`. Direction is locked to Jute's real design language (no
freestyle): white canvas, `gray-50` surfaces, `gray-200` hairlines, `gray-900` ink,
`gray-500` icon buttons (`hover:bg-gray-100`, `active:scale-110`), green/orange kernel dots,
Fira Code mono, macOS overlay title bar.

**Tabs are window-level chrome; the toolbar is notebook-level chrome.**

- **Placement.** The tab strip lives in the title-bar region, beside the macOS traffic
  lights (Safari/Finder-native). The existing `NotebookHeader` (run/restart, kernel pill +
  `gen N`, CPU/RAM, Notebook/DAG/App segmented, settings/home) sits below it and **switches
  with the focused tab**.
- **Tab anatomy.** `kernel-dot · lang badge · filename · trailing slot`. Max ~188px,
  ellipsized. Active tab is white and visually merges with the toolbar below; inactive tabs
  are muted on the chrome.
- **Kernel dot reuses Jute's convention.** green = live, **pulsing** green = running,
  orange = no kernel — so all tabs' kernel state is scannable at a glance.
- **Agent-focus pointer (◎).** The focused tab shows a small violet ring marking it as the
  agent's *current notebook* (the §5 focus-pointer model, surfaced as UI).
- **Dirty indicator.** A soft dot marks unsaved work; it swaps to the close `✕` on hover so
  the slot never changes width.
- **Close = kernel teardown.** Honors principle #1; running/dirty tabs prompt to confirm.
- **New-tab `+` (⌘T).** Takes over the header's old `+`/Home shortcut; opens the Home
  picker inside the tab.
- **Overflow.** Tabs scroll, then collapse into a `▾` tab-list dropdown when they exceed the
  strip width.
- **Keyboard.** ⌘T new · ⌘W close · ⌘1–9 jump · ⌘⌥←/→ move between tabs.

Violet is the single accent reserved for "the new tab/agent layer" (focus ring, dirty-on-
active, annotations). Everything else stays in Jute's existing gray + kernel-dot palette.

## 5. Architecture: keyed registry + focus pointer

The core move is **not** UI; it is completing the half-finished multi-notebook keying and
redefining "current."

### 5.1 Daemon: single store → keyed registry

Mirror the proven kernel `DashMap` pattern.

- `State.notebook: Arc<Mutex<Option<Arc<NotebookStore>>>>` → `notebooks: DashMap<NotebookId,
  Arc<NotebookStore>>`.
- `State.datasource_catalog` → per-notebook (keyed by `NotebookId`).
- `NotebookId`: a stable semantic document identity. Current code has two path-derived
  identifiers with different semantics: `state.rs::notebook_slot_id(path)` is a raw
  `notebook:{path}` kernel-slot key, while `ports.rs::notebook_id_for_path(path)` is a
  hashed `nb-...` storage id. Do **not** casually reuse one as the other. Introduce one
  canonical `NotebookId` type/helper and derive kernel-slot IDs, port roots, store keys, and
  delta routing from it.
- Saved notebooks derive `NotebookId` from the normalized/canonical path; scratch notebooks
  get a UUID-backed `NotebookId` until save-as. Save-as/path rename is an explicit identity
  migration: move the store/catalog/focus/delta route and decide whether the kernel slot
  follows the document identity or restarts under the new path.
- A daemon-held **focus pointer**: `State.focused: Mutex<Option<NotebookId>>`.
- `get_notebook()` resolves through the focus pointer by default, but the refactor must cover
  all production store access paths named in §2, including MCP tool modules and DAG runtime
  paths. Helpers should make the target explicit where the request already has a notebook
  path/id, and fall back to focus only for genuinely implicit operations.

### 5.2 "Current" is redefined, not deleted

From first principles, the implicit-target ergonomics are a **good** affordance and must be
preserved: `notebook_*` MCP tools and `daemon_control` mutations stay **implicitly targeted
at the focused notebook**. We do **not** add a required notebook-id to every command (that
would break every existing prompt/skill and churn all `bindings/`).

- New control command: `SetFocus { notebook_id }` (or `Focus`), emitted by the frontend on
  tab switch and by the agent when it wants to operate on a different tab. Cheap; one new
  command, not a per-command field.
- **Optional** explicit `notebook_id` override on mutating commands, for the headless /
  agent-orchestration case (drive a background tab without focusing it). **Additive** — the
  field is optional and defaults to the focus pointer, so existing callers and prompts are
  unchanged.
- The Rust `NotebookMcpServer.current_path` and the TS `activeNotebook` both become views of
  the focus pointer rather than independent authorities. `notebook_active_changed` generalizes
  to carry the focused `NotebookId`.
- Request-level binding is required: when the frontend receives an `agent://request`, it
  captures the focused `NotebookId`/`Notebook` at dispatch start and uses that target for the
  whole request. A human tab switch during an in-flight request must not retarget later
  handler steps to the newly focused tab. Long-running MCP/server requests need the same
  snapshot semantics, or they must carry the optional `notebook_id` override explicitly.

**Tradeoff (stated honestly):** with implicit focus, the agent acts on what the human sees
by default. Operating on a background tab requires `SetFocus` first, or the optional
`notebook_id` override. This is the right default for an interactive human-in-the-loop
product and still supports headless multi-notebook orchestration via the override.

### 5.3 Frontend

- A `NotebookTabs` store (zustand): ordered `[{ id, path, title, dirty, kernelState, mode }]`
  + `activeTabId`. Each open tab owns a **live, kept-mounted** `Notebook` instance (do not
  unmount on switch — that would drop kernel/edit state and pay reload cost).
- A `TabStrip` component fused into the title-bar region, matching the chrome tokens above.
- `NotebookContext.Provider` supplies the active tab's `Notebook` — consuming components are
  untouched.
- Tab switch → set `activeTabId`, send `SetFocus`, update `setActiveAgentNotebook`.
- Close → confirm if dirty/running → tear down the tab's kernel slot (`notebook_slot_id`
  keying already supports per-notebook teardown) → drop the registry entry.

### 5.4 Delta routing

Keep the path/`NotebookId` tag on every delta. Inbound filtering stays
(`notebookDeltaIsForPath` generalized to id). Outbound mutations now resolve to the correct
store via the registry + focus/override, closing the §3 hazard.

## 6. Build sequence (de-risked: backend correctness first, UI last)

**Phase 0 — Reproduce the hazard (test only).** Failing integration test in
`src-tauri` (or `crates/spur-notebook/tests`): open A, open B, mutate A, assert A's
authoritative document is correct and untouched by B. Confirms or refutes §3. *(Stands alone;
no UI.)*

**Phase 1 — Keyed registry (backend correctness, no UI change).** Promote `State.notebook`
and `datasource_catalog` to keyed maps; introduce the canonical `NotebookId`; add the focus
pointer; make the full production store-access surface in §2 resolve through explicit target
or focus. Phase 0's test must now pass. Multi-*window* becomes correct as a side effect.

**Phase 2 — Focus protocol.** Add `SetFocus` control command; redefine `current_path` /
`activeNotebook` as focus views; generalize `notebook_active_changed`; add the **optional**
`notebook_id` override; bind each request to one target at dispatch start. New
`DaemonControlCommand` variants / fields require serde round-trip coverage in the Jute/notebook
layer and regenerated ts-rs `bindings/`; ACP round-trip tests are only needed if ACP event
types change. Run the notebook Rust tests through `scripts/spur-cargo` and frontend tests /
typecheck through `scripts/spur-pnpm`.

**Phase 3 — Tab UI (frontend composition).** `NotebookTabs` store + `TabStrip`; kept-mounted
per-tab `Notebook` instances; tab switch → `SetFocus`; close → kernel teardown; overflow +
keyboard. Pure composition over `NotebookContext`.

**Phase 4 — Product-principle decision (late, cheap, reversible).** With the backend done,
choose between leaning on native macOS window tabbing (§8) vs the in-app `TabStrip`, or
shipping both. By here it is a small frontend bet, not a gating decision.

All compiles/tests go through `scripts/spur-cargo` and `scripts/spur-pnpm` (never bare
`cargo`/`pnpm`), per repo guidelines.

## 7. Testing

- **Phase 0:** the concurrent-window write-isolation test (the linchpin).
- **Registry:** unit tests that two `NotebookId`s map to two independent `NotebookStore`s and
  catalogs; load/replace of one never mutates another.
- **Focus:** `SetFocus` round-trip serialization; implicit-target resolves to focus; optional
  `notebook_id` override targets a background tab without changing focus; an in-flight
  request remains bound to its starting notebook when focus changes mid-request.
- **Identity:** one canonical `NotebookId` helper covers store keys, port roots, delta tags,
  and kernel-slot derivation; save-as/path migration preserves or deliberately restarts the
  associated kernel according to the chosen lifecycle policy.
- **Frontend:** `TabStrip` switch keeps each tab's edit buffer / kernel state (kept-mounted);
  close prompts on dirty/running and tears down only that slot; `notebookDeltaIsForPath`
  (id-generalized) still isolates inbound deltas. Use `scripts/spur-pnpm test`.
- **Bindings:** regenerated ts-rs types compile and typecheck (`scripts/spur-pnpm run typecheck`).

## 8. Alternatives considered

- **A — Windows-only, done right.** Skip in-app tabs; fix the single-store leak (Phases 0–1)
  and rely on the OS, including native macOS window tabbing (Tauri windows partly get it).
  Most faithful to README #1/#2; lowest UI surface. **Shares the same backend prerequisite**
  as tabs, so it is not a competing track — it is Phases 0–2 without Phase 3.
- **B — In-app tab strip (approved).** Best for juggling many small notebooks / dashboards and
  for agent-driven multi-notebook work. Chosen.
- **C — Hybrid workspace.** Tabs within a window, each still 1 notebook + 1 kernel; closing the
  window tears down all its tabs' kernels. Effectively B with explicit workspace framing.
- **Rejected: notebook-id on every command.** Breaks implicit-target ergonomics, churns all
  bindings, breaks existing agent prompts. Replaced by the focus-pointer + optional override.

## 9. Product-principle reconciliation

- **#1 "kernel as a window."** Preserved and generalized: tab-close tears down the kernel, so
  the `1:1:1` invariant holds with "tab" substituted for "window."
- **#2 "we removed JupyterLab tabs / anti-clutter."** The genuine tension. Mitigations: tabs
  are window chrome (not a second left rail), reuse existing tokens, show only kernel-relevant
  state, and collapse to a dropdown on overflow. This is a taste call, settled by the approved
  mock — not an architecture blocker.

## 10. Open questions

- Kernel lifecycle nuance: does tab-close **always** kill the kernel, or offer "keep warm" for
  recents (which already track `kernelAlive`)?
- Agent addressing depth: is the optional `notebook_id` override exposed on all mutating
  `notebook_*` MCP tools, or only a curated subset?
- Present / App mode: do those surfaces get tabs, or stay full-window per tab?
- Scratch notebook identity stability across save-as (path change ⇒ `NotebookId` change) and
  its effect on the kernel slot and focus pointer.
- Whether `NotebookId` should be user-visible/debuggable (`notebook:{path}` style) or compact
  and storage-safe (`nb-...` hash style). The implementation must not keep both as competing
  identities.

## Appendix — key file/symbol references

- Routing / shell: `src/App.tsx`, `src/pages/NotebookPage.tsx`, `src/ui/notebook/NotebookHeader.tsx`,
  `src/ui/notebook/NotebookView.tsx`
- Frontend store / bridge: `src/stores/notebook.ts` (`notebookDeltaIsForPath`,
  `NotebookContext`/`useNotebook`), `src/agent/bridge.ts` (`activeNotebook`,
  `setActiveAgentNotebook`)
- Window spawning: `src-tauri/src/window.rs` (`open_notebook_path`, `initialize_builder`,
  `attach_hide_on_close`)
- Daemon state: `src-tauri/src/state.rs` (`State`, `get_notebook`, `notebook_slot_id`,
  `KernelSlot`, `DatasourceCatalog`), `src-tauri/src/ports.rs` (`notebook_id_for_path`)
- Control dispatch: `src-tauri/src/commands.rs` (`handle_daemon_control_inner`,
  `replace_notebook_and_hydrate_catalog`, `resolve_run_cell_dispatch`, `DaemonControlCommand`)
- MCP/DAG store access: `crates/spur-notebook/src/mcp/tools/run_cell.rs`,
  `notebook_push_source.rs`, `notebook_dag_status.rs`, `save.rs`,
  `crates/spur-notebook/src/dag/run_context.rs`, `dag/engine.rs`
- MCP current-notebook authority: `crates/spur-notebook/src/mcp/mod.rs` (`NotebookMcpServer`,
  `NotebookDaemonControl`, `current_path`, `close_current_window`,
  `persist_catalog_to_current_notebook`, `LastNotebookRecord`)
- Bindings (ts-rs): `src/bindings/DaemonControlCommand.ts`, `NotebookDelta.ts`
