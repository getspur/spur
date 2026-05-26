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

### One new MCP tool (to be verified)

```
mcp__notebook__set_cell_metadata(notebook_id, cell_id, patch: object, merge: bool = true)
```

Merge-patches `cell.metadata.jute_deck`. Lives in the existing notebook MCP crate. Small addition.

**Verify during planning:** whether `mcp__notebook__write_cell` already supports a metadata-only merge mode. If yes, skip the new tool. The design assumes it does not, based on the current MCP surface.

### Why this design works

- **Async + observable for free.** Spur's worker dispatch already streams progress events. The jute UI subscribes via the same pattern used elsewhere in spur-core and shows "drafting slide 3 of 10..." without new plumbing.
- **Cancelable.** `mcp__spur-mcp__cancel_delegation` already exists.
- **Reviewable.** Worker mutations land as real cell edits in the notebook. The user sees each insert/edit as it happens. Undo = jute's existing cell-level history.
- **Safe.** Tool allowlist + no shell means the agent can only edit the notebook.

## Implementation risks

These are explicitly carried into the implementation plan.

1. **Notebook MCP metadata merge.** Verify whether a new `set_cell_metadata` tool is required or whether existing `write_cell` already merges metadata cleanly.
2. **Layout inference false positives.** Markdown cells with mixed content (`# H1` plus paragraphs plus bullets) need a clear precedence rule. Current spec: first-matching-rule-wins by table order. Confirm during implementation that real notebooks classify intuitively.
3. **Asset paths for image outputs in present mode.** Generated images saved by code cells use kernel-side paths; the render layer must resolve them against jute's existing image-asset pipeline. Confirm path resolution works when the present-mode route renders the slide.
4. **Output renderer reuse.** Present mode reuses jute's existing output renderer for code-cell outputs (Plotly, dataframes, etc.). Confirm it can be embedded inside the slide layout without notebook-specific chrome (cell number, exec count, toolbar).

## Out of scope (v2 candidates)

- **Any export** — PDF, PPTX, HTML bundle. Sharing in v1 = share the `.ipynb`.
- User-supplied themes / CSS.
- Agent-driven layout-suggestion review queue (Q6/D).
- Multi-user collaborative editing of the deck view.
- Slide transitions beyond fragment reveal.
- Speaker view on a second monitor / second window.

## Files touched (rough)

- `crates/spur-notebook/jute-notebook/src/ui/deck/` (new) — layout components, present-mode route, keyboard nav.
- `crates/spur-notebook/jute-notebook/src/bindings/` — TS types for cell + notebook metadata.
- Notebook MCP crate (location to confirm in plan) — optional `set_cell_metadata` tool.
- `crates/spur-notebook/jute-notebook/src/agent/` — four command entry points, prompt templates, delegation dispatcher.

## Open questions for planning

- Notebook MCP crate location and metadata-merge support (risk #1).
- Whether agent prompt templates live in jute TS or in `crates/spur-core/src/skills/`. Lean: TS in jute for v1 to iterate faster; move to skills if prompts stabilize.
- Whether the command palette already exists in jute or needs a small addition.
