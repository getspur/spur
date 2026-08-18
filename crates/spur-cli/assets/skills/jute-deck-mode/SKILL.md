---
name: jute-deck-mode
description: "Use when authoring, restructuring, or polishing a presentation inside a Jupyter notebook served by jute-notebook. Establishes the cell-as-slide model — `.ipynb` is the single source of truth; deck-only data lives in `cell.metadata.jute_deck` and `notebook.metadata.jute_deck` — and prescribes how to use `notebook.set_cell_metadata` (with `expected_version`), the layout-inference contract, themes, fragments, speaker notes, and Present-mode keyboard nav. Equivalent to a `presenton`-style tool, but the notebook IS the deck — never produce a separate slide file."
role: both
---

# Jute Notebook Deck Mode

In jute-notebook, **every notebook cell is a slide.** `.ipynb` is the only source of truth — there is no separate "deck" file, no export step (v1 is view-only), and no parallel slide model that drifts from the notebook. Everything that makes a cell render *as a slide* lives in `cell.metadata.jute_deck` (and notebook-level concerns in `notebook.metadata.jute_deck`), so a deck round-trips through any Jupyter tool unchanged.

This skill is the equivalent of presenton's authoring loop, but adapted to that constraint. **Don't try to escape the constraint.** If you find yourself wanting "a slide that isn't a cell," stop and re-read this skill.

## When to use this skill

- The user asks to **draft / restructure / polish / annotate a slide deck** inside a notebook.
- The user mentions "present mode," "slides," "deck," "speaker notes," "jute-deck," or `/present`.
- You're a worker dispatched by the deck command palette (⌘⇧P → Deck → Draft/Restructure/Polish/Notes).
- You're about to call `notebook.set_cell_metadata` and need to know what keys mean what.

If the request is "make a PDF/PPTX/HTML export," this skill does **not** apply — v1 is view-only and there is no export pipeline. Push back and offer the in-jute Present mode instead.

## The two invariants (do not violate)

1. **Cells are slides, 1:1.** Never reorder, merge, or split slides by editing metadata alone — do it by inserting/deleting/moving *cells*. Adding `cell.metadata.jute_deck.layout = "title"` does not make a new slide; it changes how an existing cell renders. If you want N slides, the notebook needs N non-hidden cells.

2. **All `set_cell_metadata` calls require `expected_version`.** This is the same optimistic-concurrency protocol as `write_cell`. Read the cell's current `version` (e.g. via `notebook.read_cell` or `notebook_get_notebook`), pass it as `expected_version`, and if you get a conflict, **re-read and retry** — do not blind-overwrite. The handler is atomic on the TS side (`getCellSnapshotById` → `mergeCellJuteDeckMetadata` with no `await` between), so collisions only happen at the MCP boundary.

## Data model — the only schemas that matter

### Per-cell: `JuteDeckCellMetadata`

Path: `cell.metadata.jute_deck` (TS: `JuteDeckCellMetadata`, Rust: `crates/spur-notebook/jute-notebook/src-tauri/src/backend/notebook.rs::JuteDeckCellMetadata`).

| Field | Type | Meaning |
|---|---|---|
| `layout` | `JuteDeckLayout` (enum) | Force a specific layout. Omit (or set `"auto"`) to let inference decide. Values: `"auto"`, `"title"`, `"section"`, `"content"`, `"bullets"`, `"code"`, `"output"`, `"code-output"` (CodeOutput), `"two-col"` (TwoCol), `"image"`, `"blank"`. **Note the kebab-case for `code-output` and `two-col` in the TS binding.** |
| `hidden` | `boolean` | `true` → cell is skipped in deck/present mode but kept in the notebook. Use this to keep working notes inline without polluting the deck. `cellToSlide` returns `null` for hidden cells; Present mode filters them out. |
| `speaker_notes` | `string` (markdown) | Shown only when the presenter toggles the notes overlay with **S** in Present mode. Never rendered on the slide itself. |
| `theme_override` | `string` (theme id) | Per-slide override of the notebook-level theme. Theme ids: `"minimal-light"` (default), `"minimal-dark"`, `"spur-brand"`. Unknown ids fall back to `"minimal-light"` via `resolveTheme`. |
| `fragments` | `boolean` | When `true` on a `bullets` slide, Present mode reveals bullets one-by-one as the user presses Space/→. Counted by `countFragments(slide)` from `PresentPage.tsx`. |
| `background` | `string` | Per-slide background color (CSS color) or image URL. Applied by `SlideFrame`. |

### Per-notebook: `JuteDeckNotebookMetadata`

Path: `notebook.metadata.jute_deck` (Rust: same file, `JuteDeckNotebookMetadata`).

| Field | Type | Meaning |
|---|---|---|
| `theme` | `string` | Default theme id for the whole deck. Falls back to `"minimal-light"`. |
| `aspect` | `string` | Slide aspect ratio (e.g. `"16:9"`). |
| `title` | `string` | Deck title override (defaults to filename). |
| `author` | `string` | Deck author display name. |

**Caveat:** as of the current implementation, `PresentPage.tsx` calls `cellToSlide(cell, undefined)` — it does **not** thread notebook-level metadata yet. So `notebook.metadata.jute_deck.theme` is read by `cellToSlide` *in theory* but unused in Present mode today. Per-slide `theme_override` works; whole-deck theme switching is a follow-up. Don't rely on the notebook theme propagating until that lands.

## Layout inference — what the renderer picks when `layout` is omitted

`cellToSlide` (`crates/spur-notebook/jute-notebook/src/ui/deck/cellToSlide.ts`) runs `inferLayout` when `jute_deck.layout` is missing or `"auto"`. First-matching-rule-wins, in this exact order:

1. **Code cell** → `output` (the cell's executed output is the slide; the code itself is hidden).
2. **Raw / HTML cell** → `blank`.
3. **Markdown cell starting with a lone `# H1`** (no other content) → `title`.
4. **Markdown cell starting with a lone `## H2`** → `section`.
5. **Markdown cell containing any `# H1` anywhere** → `title` (intentional: an author who typed `#` almost certainly meant a title slide, even if they added body text).
6. **Markdown cell containing list bullets (`-`, `*`, or `1.`)** → `bullets`.
7. **Everything else** → `content`.

Implications for authoring:

- To get a **bullets** slide, write `## My Topic\n\n- item one\n- item two` — the `##` is not a title trigger (rule 3 requires a *lone* H1; rule 5 only triggers on `#`, not `##`). To get a section divider with no body, write just `## My Topic`.
- To get a **code + output** slide instead of output-only, set `layout: "code-output"` explicitly. Inference always picks `output` for code cells.
- To get a **title with subtitle**, set `layout: "title"` explicitly on a markdown cell with both H1 and body — inference would already pick `title` (rule 5), but being explicit prevents future inference changes from re-routing the slide.
- To get a **two-column** or **image** slide, you must set `layout` explicitly — there's no inference path to either.

## The three authoring workflows

The deck command palette (⌘⇧P → "Deck") dispatches one of four prompts (`crates/spur-notebook/jute-notebook/src/agent/deck/prompts.ts`) to a coder worker with `toolAllowlist: ["mcp__notebook__*"]`. As that worker, your prompts will arrive pre-framed; this section is what you actually do in each.

### Draft (build a deck from scratch)

```
1. Read the user's request.
2. notebook_insert_cell one markdown cell per slide, in order.
   - First cell: `# <Deck title>` plus author/date if known → layout will infer "title".
   - Section dividers: `## <Section name>` alone → infers "section".
   - Bullet slides: `## <Slide title>\n\n- ...\n- ...` → infers "bullets".
   - Content slides: prose with one `#` → infers "title" (this is intentional). For prose
     without a hero title, omit the H1 and rely on rule 7 → "content".
3. notebook.set_cell_metadata only when you need to OVERRIDE inference
   (e.g. force "two-col", "image", or attach speaker_notes/fragments).
   - Do NOT call set_cell_metadata for slides where inference is already correct.
4. Aim for 6–12 slides unless the user specifies otherwise (per the dispatch prompt).
5. Do NOT call notebook_write_cell after notebook_insert_cell on the same cell —
   the insert source IS the slide content. Doing both wastes a version bump and
   risks a race.
```

### Restructure (reorder / split / merge / delete)

```
1. Read the notebook (notebook_get_notebook) to learn cell order, ids, and current
   versions.
2. To reorder: delete + reinsert at the new position, or use the move ops in
   notebook MCP (consult notebook_list_recents/get_notebook output).
3. To split: notebook_insert_cell after the current cell with the bottom half,
   then notebook_write_cell to trim the top half.
4. To merge: notebook_write_cell on the survivor with combined source, then
   notebook_delete_cell on the loser.
5. Preserve code cells unless the user explicitly says to remove them.
6. After structural moves, if layout changes (e.g. you split a bullets slide and
   the second half no longer starts with bullets), update layout via
   set_cell_metadata.
```

### Polish (rewrite prose only)

```
1. Find target cells (user-specified, or all markdown cells if unspecified).
2. notebook_write_cell with rewritten source. Do NOT change:
   - cell order
   - layouts
   - hidden flags
   - speaker_notes (unless user asked)
3. Keep titles ≤ 8 words, bullets ≤ 14 words (per the dispatch prompt).
4. Match the requested tone.
```

### Notes (write speaker notes)

```
1. For each non-hidden cell:
   notebook.set_cell_metadata id=<cellId>
                              patch={ "speaker_notes": "..." }
                              expected_version=<current version>
2. Notes should ADD CONTEXT (transitions, anecdotes, numbers), not repeat the
   visible slide.
3. 1–3 sentences per slide.
4. Do NOT modify cell source. This task is metadata-only.
```

## The atomic-handler invariant for `set_cell_metadata`

The TS handler at `crates/spur-notebook/jute-notebook/src/agent/handlers.ts` (case `"notebook.set_cell_metadata"`) does:

```
1. getCellSnapshotById(id)            // read current { source, version, metadata }
2. compare snapshot.version === expected_version → if not, return conflict
3. mergeCellJuteDeckMetadata(id, patch)   // MUST be synchronous-after-step-2 (no await)
```

**There must be no `await` between the version check and the merge.** If you're editing this handler or adding a sibling op, preserve that ordering. Any async work (translating, validating against external schemas, etc.) must happen *before* step 1 or *after* step 3.

For callers (you, the agent): the protocol is the same as `write_cell`:

```
read cell → grab version V
call set_cell_metadata(id, patch, expected_version=V)
if response.ok=false (version conflict): re-read, recompute patch, retry
```

If you need to make N metadata changes to the same cell, **batch them into one patch** instead of N sequential calls — both for fewer version bumps and to avoid intra-task races.

## Present mode — what your changes look like to the audience

Route: `/present?path=<encoded>` (registered in `App.tsx`). The page builds slides via:

```ts
cells.map((cell) => cellToSlide(cell, undefined)).filter((s) => s !== null)
```

then renders one `SlideFrame` at a time. Keyboard nav (`PresentPage.tsx`):

| Key | Action |
|---|---|
| `→`, `Space`, `PageDown` | Next fragment if `slide.fragments`, else next slide |
| `←`, `PageUp` | Previous fragment, else previous slide |
| `Home` | First slide |
| `End` | Last slide |
| `S` | Toggle speaker-notes overlay |
| `B` | Toggle blackout (presses again to restore) |
| `Esc` | Return to `/notebook?path=<encoded>` |

`fragmentIndex` resets to 0 on every slide change. The blackout overlay is purely visual — it does not unmount the slide, so `useState` inside layout components survives the toggle.

## Themes — three presets only

`crates/spur-notebook/jute-notebook/src/ui/deck/themes.ts`:

- `minimal-light` (default) — white bg, slate-900 text, blue accent.
- `minimal-dark` — slate-900 bg, slate-50 text, blue accent.
- `spur-brand` — indigo→violet gradient, white text, amber accent.

`resolveTheme(id)` returns `minimal-light` for unknown ids; don't invent ids. To customize beyond this, the right path is **add a theme to `THEMES`** in `themes.ts` and ship a code change — not stuff CSS into `cell.metadata.jute_deck.background` and hope.

Per-slide background (`jute_deck.background`) takes a single CSS color string or image URL and is applied by `SlideFrame`. Use it for accent slides, not for theming.

## Anti-patterns

- **Calling `write_cell` and `set_cell_metadata` in sequence on the same cell without re-reading.** Each call bumps `version`; the second one will fail with a conflict. Either re-read between calls or batch (write source first, then read fresh version, then set metadata).
- **Setting `layout` on every cell "to be explicit."** Inference is well-defined and stable; explicit layouts are noise unless they override inference. Reserve them for `two-col`, `image`, `code-output`, or cases where inference is wrong.
- **Using `hidden: true` to "comment out" a draft slide you intend to revisit.** It works, but it leaves a confusing "phantom cell" in edit mode. Better: delete the cell and re-insert later, or move the draft into a `## Scratch` section at the bottom and hide that section.
- **Treating `notebook.metadata.jute_deck.theme` as the live notebook-level theme switch.** Present mode currently passes `undefined` for the deck-meta arg of `cellToSlide`, so notebook-level theme is read but inert. Use `theme_override` per slide until that's wired through. (Tracked as a v1 follow-up in the merge commit.)
- **Speaker notes that repeat the slide.** Notes are surfaced *only* via S; if they restate visible content they waste the channel. Use them for transitions, numbers you can't fit on the slide, and "wait for the laugh" stage directions.
- **Fragments on non-bullet slides.** `countFragments(slide)` only counts bullet items; setting `fragments: true` on a content/title/code slide is a no-op (`maxFragments` is 0) but adds confusion when somebody else reads the metadata.
- **Inline backticks inside bullets.** `BulletsLayout` rolls its own `renderInlineMarkdown` that only handles `**bold**` and `*italic*` — historically it did **not** parse `` `code` `` spans (a follow-up to that limitation is tracked; check the current `BulletsLayout.tsx` before assuming the gap). If you're authoring against an older deck-mode build and a bullet needs inline code, either drop the backticks (use **bold** instead) or force `layout: "content"` on that cell so the full `MarkdownRenderer` runs. `ContentLayout` handles backticks, links, em-dashes, and the rest.
- **Trying to export to PDF/PPTX/HTML.** No export path exists in v1. If the user insists, the supported workflow is screenshot Present mode, or wait for the export track to ship.
- **Producing a separate `.md` / `.json` deck file alongside the notebook.** The notebook IS the deck. Two sources of truth diverge; we'd rather have one ugly notebook than two beautiful files that lie to each other.

## File map (for navigation and edits)

```
crates/spur-notebook/jute-notebook/
├── src-tauri/src/backend/notebook.rs        # JuteDeckCellMetadata, JuteDeckLayout,
│                                             # JuteDeckNotebookMetadata (Rust, ts-rs source)
├── src/bindings/
│   ├── JuteDeckCellMetadata.ts              # generated; do not hand-edit
│   ├── JuteDeckLayout.ts
│   ├── JuteDeckNotebookMetadata.ts
│   └── CellMetadata.ts                      # cell.metadata = { spur?, jute_deck? }
├── src/stores/notebook.ts                   # mergeCellJuteDeckMetadata (zustand+immer)
├── src/agent/
│   ├── handlers.ts                          # notebook.set_cell_metadata atomic handler
│   ├── types.ts                             # AgentSetCellMetadata, AgentBridgeRequest
│   └── deck/
│       ├── prompts.ts                       # PROMPTS: draft|restructure|polish|notes
│       ├── dispatch.ts                      # dispatchDeckCommand → spur_delegate_to_worker
│       └── index.ts
├── src/ui/deck/
│   ├── types.ts                             # SlideSpec, Block, ResolvedLayout
│   ├── cellToSlide.ts                       # pure transform + inferLayout (regex rules)
│   ├── themes.ts                            # THEMES + resolveTheme
│   ├── SlideFrame.tsx                       # theme + background frame
│   └── layouts/                             # Title/Section/Bullets/Content/Code/
│                                             # Output/CodeOutput/TwoCol/Image/Blank + index
├── src/ui/notebook/NotebookCommandMenu.tsx  # ⌘⇧P palette with "Deck" group
└── src/pages/PresentPage.tsx                # /present route + keyboard nav

crates/spur-notebook/src/mcp/tools/
└── set_cell_metadata.rs                     # MCP tool: validates expected_version >= 1,
                                              # bridges to TS handler over BRIDGE_TIMEOUT
```

## Key principles

- **One source of truth: the `.ipynb`.** No sidecar slide files, no "deck DB," no per-session in-memory state that the notebook doesn't reflect.
- **Insert/delete to add/remove slides; `set_cell_metadata` to change how a slide renders.** Confusing these is the most common authoring bug.
- **Inference is your friend.** Write good markdown and let `inferLayout` do its job. Override only when inference is wrong or you need `two-col`/`image`/`code-output`.
- **Every `set_cell_metadata` carries `expected_version`.** Treat conflicts as "re-read and retry," never as "blind retry."
- **Speaker notes are an overlay (S key), not a slide footer.** Write them to *add* context, not to repeat the slide.
- **Themes are a closed enum** (three presets). To customize, add to `THEMES`; do not abuse `background`.
- **Fragments only matter on bullet slides.** The reveal counter ignores everything else.
- **No export in v1.** If the user wants PDF/PPTX, the answer is "Present mode for now," not "let me cobble something together."

## TL;DR

```
0. .ipynb is the deck. No exports, no sidecar files.
1. One cell = one slide. Use insert/delete to change slide count.
2. cell.metadata.jute_deck.{layout,hidden,speaker_notes,theme_override,fragments,background}
   controls how a slide renders. notebook.metadata.jute_deck.{theme,aspect,title,author}
   sets deck-wide defaults (theme not yet propagated in Present mode — use theme_override).
3. notebook.set_cell_metadata REQUIRES expected_version; on conflict, re-read and retry.
4. Layout inference: code→output, raw/html→blank, lone H1→title, lone H2→section,
   any H1→title, has bullets→bullets, else→content.
5. Set layout explicitly only for two-col, image, code-output, or when inference is wrong.
6. Present mode: /present?path=… with → / Space / S / B / Esc keys.
7. Three themes: minimal-light, minimal-dark, spur-brand. Add to THEMES to extend.
```
