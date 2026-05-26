# jute-deck — Notebook-Native Presentations

**Status:** Design approved 2026-05-26. Awaiting implementation plan.
**Owner:** Kevin Truong
**Working name:** `jute-deck` (a mode in jute-notebook, not a separate app)

## Summary

A presentation feature built into `jute-notebook` where **each notebook cell renders as one slide**. The `.ipynb` file is the single source of truth — no new file format. Slide-specific config lives in cell metadata. v1 ships **present mode only**, viewed inside jute. No export. A small agent layer reuses Spur's existing worker delegation to draft, restructure, polish, and annotate decks via the notebook MCP.

Inspired by [Presenton](https://github.com/presenton/presenton), but inverted: instead of an AI-first generator that emits HTML decks, this is a notebook-first authoring surface where the deck is a *view* of the notebook, optionally shaped by an agent.

## Goals & non-goals

**Goals**
- Cell = slide, 1:1. The notebook is the deck.
- Live code outputs (charts, dataframes, generated images) embed in slides natively.
- Present mode inside jute — deterministic and fast.
- Optional agent assistance (draft / restructure / polish / speaker-notes) using existing Spur infrastructure — no new MCP server, no new worker type.
- `.ipynb` files stay compatible with other Jupyter tooling (unknown metadata is ignored).

**Non-goals (v1)**
- **No export of any kind.** No PDF, no PPTX, no HTML bundle. The deck is viewed inside jute. Sharing = share the `.ipynb`.
- A new `.jdeck` artifact format. The notebook is the artifact.
- A template/layout library system (Presenton-style `layouts/*.html`). Convention + cell metadata covers v1.
- User-supplied custom themes at runtime. Three built-in themes; user themes are a v2 extension point.
- A multi-turn agent chat panel inside jute. Agent runs as one-shot worker delegations.
- Multi-user real-time collaboration on the deck (separate jute concern).

## Architectural decisions (with rationale)

| # | Decision | Rationale |
|---|---|---|
| Q1 | **Hybrid**: notebook is the working surface; "compile to deck" produces a slide view of the same file. | User picked C. Keeps notebook authoring intact, lets the deck be a non-destructive projection. |
| Q2 | **Manual-first + optional agent pass.** The agent writes the same cell + metadata shape a human would. | User picked C. Deterministic core; agent is a power tool, not a dependency. |
| Q3 | **Cell = slide, 1:1.** `.ipynb` is the source of truth. No new file format. | User confirmed. Eliminates an entire schema, persistence, sync, and migration surface. |
| Q4 | **Present mode only, inside jute.** No export of any kind in v1. | User scoped down from B to view-only. Smallest possible surface; eliminates headless-print risk; ships fastest. |
| Q5 | **Convention over config**, with cell-metadata override as escape hatch. | User picked A-with-B-escape-hatch. Most cells get a correct layout for free; full control when needed. |
| Q6 | Agent does **Draft (A) + Restructure (B) + Polish (C) + Speaker notes (E)**. No layout suggestions queue (D) in v1. | User approved. All four share one tool surface; the suggestion queue needs a separate UX. |

## Architecture overview

Three layers, all inside the existing `crates/spur-notebook/jute-notebook` directory. No new crates.

```
┌─────────────────────────────────────────────────────────┐
│  Agent layer (no new infra)                             │
│    Command palette → delegate_to_worker → notebook MCP  │
└──────────────────────┬──────────────────────────────────┘
                       │  cell edits via existing MCP
                       ▼
┌─────────────────────────────────────────────────────────┐
│  .ipynb (single source of truth)                        │
│    cells[].metadata.jute_deck = { layout, hidden, ... } │
│    notebook.metadata.jute_deck = { theme, aspect, ... } │
└──────────────────────┬──────────────────────────────────┘
                       │  read on load + edit subscription
                       ▼
┌─────────────────────────────────────────────────────────┐
│  Render layer                                           │
│    cellToSlide(cell, deckMeta) → SlideSpec              │
│      └─► Present mode (Tauri webview, /present/:id)     │
└─────────────────────────────────────────────────────────┘
```

## Schema

### Cell-level metadata
Stored at `cell.metadata.jute_deck`. Jupyter ignores unknown metadata, so other tools tolerate it.

```ts
type JuteDeckCellMeta = {
  layout?: "auto" | "title" | "section" | "content" | "bullets"
         | "code" | "output" | "code-output" | "two-col" | "image" | "blank";
  hidden?: boolean;          // skip in deck, keep in notebook
  speaker_notes?: string;    // markdown; notes view only
  theme_override?: string;   // per-slide theme id
  fragments?: boolean;       // bullet-by-bullet reveal (markdown bullets only)
  background?: string;       // color or image URL
};
```

### Notebook-level metadata
Stored at `notebook.metadata.jute_deck`.

```ts
type JuteDeckNotebookMeta = {
  theme?: string;            // default "minimal-light"
  aspect?: "16:9" | "4:3" | "16:10";  // default "16:9"
  title?: string;            // defaults to filename
  author?: string;
};
```

### Layout inference (when `layout` is absent or `"auto"`)

| Cell shape | Inferred layout |
|---|---|
| markdown, single `# H1` (optional subtitle line) | `title` |
| markdown, single `## H2` only | `section` |
| markdown, contains a bulleted list | `bullets` |
| markdown, anything else | `content` |
| code cell | `output` (output rendered; source hidden) |
| raw or HTML cell | `blank` (raw HTML fills the slide) |

Inference is a pure function. Results are cached in memory but **never written back to the file** — the cell stays clean unless the user (or agent) explicitly sets a layout.

### Themes (v1)

Three built-in: `minimal-light`, `minimal-dark`, `spur-brand`. Each is a Tailwind utility preset compiled into the jute bundle. No runtime CSS-in-JS in v1; no user-supplied stylesheets.

## Render layer

### Pure transform + present-mode renderer

```ts
function cellToSlide(cell: Cell, deckMeta: JuteDeckNotebookMeta): SlideSpec | null;

type SlideSpec = {
  id: string;
  layout: ResolvedLayout;            // inferred or explicit, never "auto"
  blocks: Block[];
  speaker_notes?: string;
  theme: string;
  background?: string;
  fragments: boolean;
};

type Block =
  | { kind: "heading"; level: 1|2|3; text: string }
  | { kind: "markdown"; md: string }
  | { kind: "bullets"; items: string[]; fragments: boolean }
  | { kind: "code"; lang: string; source: string }
  | { kind: "output"; mime_bundles: MimeBundle[] }
  | { kind: "html"; html: string }
  | { kind: "image"; src: string; alt?: string };
```

Returns `null` if the cell has `hidden: true`. Layout templates are plain React components consuming `blocks[]`, one file per layout under `src/ui/deck/layouts/`.

### Present mode

- New route `/present/:notebookId` inside the existing jute React Router setup.
- Opens in the **same Tauri window**, replacing the editor surface. Esc returns. Avoids second-window state-sync.
- **Keyboard:**
  - `→ / Space / PgDn` — next
  - `← / PgUp` — previous
  - `Home / End` — first / last
  - `B` — blackout
  - `S` — toggle speaker-notes overlay
  - `Esc` — exit present mode
- **Mouse:** click anywhere advances; corner overlay shows slide N / total.
- **Live outputs:** if the kernel is running and a code cell re-executes, the slide's output updates in place. Free feature of cell-as-slide.

## Agent layer

No new infrastructure. Four prompted worker entry points, all using existing Spur tooling.

### Commands (in command palette, or "Deck → AI" menu)

| Command | What it does |
|---|---|
| **Draft deck** (Q6/A) | "Describe what you want; I'll create the cells." Worker reads any existing cells + user prompt, inserts new markdown/code cells. |
| **Restructure deck** (Q6/B) | "Tighten / split / reorder." Worker edits, reorders, inserts, deletes cells. |
| **Polish slides** (Q6/C) | "Rewrite bullets for X audience." Selection-aware. Text-only edits to markdown cells. |
| **Generate speaker notes** (Q6/E) | Fills `cell.metadata.jute_deck.speaker_notes` for selected or all slides. |

Each opens a small inline prompt input (no modal). Submit → dispatches a Spur worker delegation.

### Worker dispatch shape

Each command builds a delegation via `mcp__spur-mcp__delegate_to_worker` with:
- **Task prompt:** a short canned prompt per command + the user's free-text + a *summary* of the notebook (cell id, type, layout, first 80 chars). Full cell content is fetched on demand by the worker through MCP, not pre-stuffed into the prompt.
- **Worker type:** the existing default coder worker (confirm during planning by inspecting `crates/spur-core/src/workers/`).
- **Tool allowlist:** `mcp__notebook__*` only. No shell, no filesystem outside the notebook, no kernel execution. The agent can only mutate the notebook.

### One new MCP tool — confirmed required

```
mcp__notebook__set_cell_metadata(id, patch: JuteDeckCellMetadata, expected_version)
```

Atomic check-and-merge into `cell.metadata.jute_deck`. Follows the same `expected_version` protocol as `write_cell` (per the atomic-handler invariant in `handlers.ts:14-17`). Requires changes at three layers: Rust MCP tool, TS agent handler, daemon notebook store (see Codebase cross-map).

### Why this design works

- **Async + observable for free.** Spur's worker dispatch already streams progress events. The jute UI subscribes via the same pattern used elsewhere in spur-core and shows "drafting slide 3 of 10..." without new plumbing.
- **Cancelable.** `mcp__spur-mcp__cancel_delegation` already exists.
- **Reviewable.** Worker mutations land as real cell edits in the notebook. The user sees each insert/edit as it happens. Undo = jute's existing cell-level history.
- **Safe.** Tool allowlist + no shell means the agent can only edit the notebook.

## Codebase cross-map (verified 2026-05-27 against indexed graph)

This section replaces several "to verify" placeholders in the original draft. Every claim below is grounded to a concrete file path.

### Notebook MCP layer — confirmed

- **Tool registration:** `crates/spur-notebook/src/mcp/tools/mod.rs` — one module per tool. Existing tools: `read_cell`, `write_cell`, `insert_cell`, `delete_cell`, `get_notebook`, `save`, `snapshot`, `run_cell`, `interrupt`, `kernel_info`, `start_kernel`, `stop_kernel`, `restart_kernel`, `venv_*`, `daemon_*`.
- **`write_cell` does NOT accept metadata.** `WriteCellParams` at `crates/spur-notebook/src/mcp/tools/write_cell.rs:15` is `{ id, source, expected_version }`. The bridge call sends those plus `last_edited_by`. **The spec's "needs a new tool" assumption holds.**
- **Three layers to extend** (not just one) when adding `set_cell_metadata`:
  1. **Rust MCP tool**: new file `crates/spur-notebook/src/mcp/tools/set_cell_metadata.rs` + register in `mod.rs::tools()`.
  2. **TS agent handler**: extend the `notebook.*` dispatch switch in `crates/spur-notebook/jute-notebook/src/agent/handlers.ts` (currently routes `notebook.snapshot|export|flush_pending|read_cell|insert_cell|write_cell|delete_cell`).
  3. **Daemon notebook store**: `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` — currently only `set_cell_spur_metadata` writes the `spur` namespace; needs a sibling op for the `jute_deck` namespace, or a generic metadata-merge op gated by namespace.

### Cell-metadata schema — already structured, extend it

- `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs:198` —
  ```rust
  pub struct CellMetadata {
      pub spur: Option<SpurCellMetadata>,
      #[serde(flatten)] #[ts(skip)]
      pub other: Map<String, Value>,
  }
  ```
- **Add `jute_deck: Option<JuteDeckCellMetadata>` as a typed sibling to `spur`.** Follows the existing pattern; auto-generates TS bindings via `ts-rs` (`crates/spur-notebook/jute-notebook/src/bindings/CellMetadata.ts` is auto-generated).
- Constructor `empty_cell_metadata()` at `notebook_store.rs:411` needs the new field set to `None`.

### Routing — wouter, not React Router

- `crates/spur-notebook/jute-notebook/src/App.tsx:18` uses **`wouter`** with `<Switch>` + `<Route>`. Single existing route: `<Route path="/notebook" component={NotebookPage}>`. `NotebookPage` reads `path` / `inline` via query string (`useSearch`), not URL params.
- **Spec update:** the proposed `/present/:notebookId` should be `<Route path="/present" component={PresentPage}>` with `?path=...` matching the existing `NotebookPage` pattern. No need for path params.

### Command palette — already exists, just add items

- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx:18` is a full cmdk-based palette bound to ⌘K, with grouped `Command.Item`s ("Execution", etc.). Mounted by `NotebookPage`.
- **Big win:** the four agent commands (Draft / Restructure / Polish / Speaker notes) and "Enter present mode" become new `<Command.Item>` entries under a new "Deck" group. No new palette infrastructure.

### Output renderer — reusable

- `crates/spur-notebook/jute-notebook/src/ui/notebook/OutputView.tsx` exists, alongside `MarkdownRenderer.tsx` and `RenderMarkdownCell.tsx`. These cover the rendering surface present-mode needs.
- **Open question for planning** (now scoped, not unknown): confirm whether `OutputView` carries notebook-specific chrome (cell number, execution count, toolbar). If yes, add a `chromeless` prop. If no, drop in directly.

### Spur worker delegation — confirmed

- `mcp__spur-mcp__delegate_to_worker` resolved to `crates/spur-mcp/src/tools.rs:371`. Use as-is from the agent commands.

### Existing agent bridge — the integration surface

- `crates/spur-notebook/jute-notebook/src/agent/` is the existing seam for "Spur agent talks to jute's in-memory notebook." Files: `bridge.ts`, `types.ts`, `handlers.ts`, `events.ts`, `events.contract.ts`. The four deck-AI commands plug in here (new request types, new handler cases, new dispatch entries from the command menu).

## Updated open questions for planning (narrowed)

1. **`OutputView` chrome.** Read the component; decide between `chromeless` prop vs. wrapping.
2. **Image-asset path resolution in present mode.** Confirm that generated-image outputs (kernel-saved files) resolve correctly when rendered from the new `/present` route. Most likely fine since the asset pipeline is route-agnostic, but worth a one-cell smoke test.
3. **Layout inference precedence on mixed-content markdown.** First-matching-rule-wins by table order; verify on real notebooks.

## Implementation risks

These are explicitly carried into the implementation plan.

1. **Three-layer metadata write** (Rust MCP tool + TS handler + daemon store). Spec originally framed this as "one new MCP tool"; reality is one tool entry plus matching changes at the TS handler layer and the daemon notebook store. Each layer has its own concurrency invariants (the TS handler comment in `handlers.ts:14-17` explicitly warns "Atomic-handler invariant: handlers that perform version checks and mutations must keep check + mutation synchronous, with no await between them"). Follow the same pattern for metadata writes.
2. **Layout inference false positives.** Markdown cells with mixed content (`# H1` plus paragraphs plus bullets) need a clear precedence rule. Current spec: first-matching-rule-wins by table order. Confirm during implementation that real notebooks classify intuitively.

## Out of scope (v2 candidates)

- **Any export** — PDF, PPTX, HTML bundle. Sharing in v1 = share the `.ipynb`.
- User-supplied themes / CSS.
- Agent-driven layout-suggestion review queue (Q6/D).
- Multi-user collaborative editing of the deck view.
- Slide transitions beyond fragment reveal.
- Speaker view on a second monitor / second window.

## Files touched (rough)

### New
- `crates/spur-notebook/jute-notebook/src/ui/deck/` — layout components (`TitleLayout.tsx`, `BulletsLayout.tsx`, `OutputLayout.tsx`, `CodeOutputLayout.tsx`, `TwoColLayout.tsx`, `ImageLayout.tsx`, `BlankLayout.tsx`), the `cellToSlide` transform, present-mode keyboard handler.
- `crates/spur-notebook/jute-notebook/src/pages/PresentPage.tsx` — the `/present` route component.
- `crates/spur-notebook/src/mcp/tools/set_cell_metadata.rs` — new MCP tool.
- `crates/spur-notebook/jute-notebook/src/agent/deck/` — four deck-command entry points, prompt templates, delegation dispatcher.

### Modified
- `crates/spur-notebook/jute-notebook/src/App.tsx` — add `<Route path="/present" component={PresentPage}>`.
- `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCommandMenu.tsx` — add "Deck" `Command.Group` with five items (4 AI commands + "Enter present mode").
- `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs` — add `JuteDeckCellMetadata` struct + `JuteDeckNotebookMeta`; add `jute_deck` fields to `CellMetadata` and notebook-root metadata.
- `crates/spur-notebook/jute-notebook/src-tauri/src/notebook_store.rs` — update `empty_cell_metadata()`; add metadata-merge op for the `jute_deck` namespace following the `set_cell_spur_metadata` pattern.
- `crates/spur-notebook/jute-notebook/src/agent/handlers.ts` — add `notebook.set_cell_metadata` case to the dispatch switch, with the same atomic-handler invariant as existing write paths.
- `crates/spur-notebook/jute-notebook/src/agent/types.ts` — add `AgentSetCellMetadata` type.
- `crates/spur-notebook/src/mcp/tools/mod.rs` — register the new tool module.
- TS bindings (`crates/spur-notebook/jute-notebook/src/bindings/`) — auto-regenerated by `ts-rs`; no hand edits.

### Possibly modified (decide during planning)
- `crates/spur-notebook/jute-notebook/src/ui/notebook/OutputView.tsx` — add `chromeless` prop only if it currently renders notebook-specific chrome.

## Resolved during cross-map

- ~~Notebook MCP crate location~~ → `crates/spur-notebook/src/mcp/tools/`.
- ~~Whether the command palette exists~~ → yes, `NotebookCommandMenu.tsx`, cmdk-based, ⌘K-bound. Add a "Deck" group.
- ~~Whether `write_cell` already supports metadata merging~~ → no. New tool needed across all three layers.

## Remaining open question

- Agent prompt templates: live in jute TS (`agent/deck/prompts/`) or in `crates/spur-core/src/skills/`? Lean: TS in jute for v1 to iterate faster; promote to skills if prompts stabilize.
