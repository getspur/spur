# Polyglot Notebook — Cell Identity & Kernel-Routing UI (Design)

**Date:** 2026-06-03
**Visual board:** rendered design board in notebook (`Untitled78.ipynb`, artifact cell) — the source-of-truth visual.
**Status:** approved (full system).

## Problem

The notebook is polyglot end-to-end. Every code cell carries
`cell.metadata.spur.code_type ∈ {python, javascript, rust, go}` and the kernel layer
routes each to a distinct kernel via `code_type_kernelspec`
(`crates/spur-notebook/src/dag/cell_runner.rs:191`):

| `code_type` | kernelspec |
|---|---|
| python | `python3` |
| javascript | `deno` |
| rust | `evcxr` |
| go | `gonb` |
| *(spur AI cell)* | `spur` |

`code_type_for_spec` (`backend/notebook.rs:273`) is the inverse map. The canonical Rust
`CodeType` enum (`backend/notebook.rs:251`) already includes all four languages.

**But the frontend renders it monolingual:**

1. `CellInput.tsx:83` `extensionForLanguage` hardcodes `python()` for *every* code cell —
   Rust/Go/JS all get Python highlighting. Only `lang-python` + `lang-markdown` are installed.
2. No language is ever shown. A grep of `src/ui/` for `codeType`/`rust`/`go`/`javascript`
   returns zero render sites. The only per-cell identity is the `✦ AI` chrome.
3. No switcher. The single per-cell toggle (`CellInputAside`) only flips code↔markdown.
4. The generated `CodeType.ts` binding is **stale** — `python|javascript|rust`, missing `go`.

**Risk:** low. `extensionForLanguage` has 1 caller; `CellInput`/`NotebookCells` are leaf
components (blast score ≤ 2.3 via `v_blast_radius`).

## Design — language/kernel as first-class cell identity

Unify all five routing targets into one visual + interaction system, with the `✦ AI`
cell as one entry, reusing the violet chrome already merged (`AiCellHeader`).

### 1. Language token (single source of truth)
A `cellLanguage.ts` module maps `CodeType | "spur"` → `{ label, glyph, kernelspec,
accent, chip colors, glyph bg }`. Consolidates today's `isAiCell` logic. Tokens:

| id | glyph | label | accent |
|---|---|---|---|
| python | `Py` | Python | `#3776AB` |
| javascript | `JS` | JavaScript | `#8A6D00` |
| rust | `Rs` | Rust | `#CE422B` |
| go | `Go` | Go | `#00ADD8` |
| spur | `✦` | AI Agent | `#7C3AED` (violet) |

### 2. Chip + accent bar (identity)
Every code cell gets a chip (glyph + label) in the header zone the `✦ AI` cell already
uses, plus a 3px left **accent bar** so language is scannable down the document. The
gutter execution marker is tinted by the token accent; the AI cell keeps `✦[n]` and its
`manual` / `● LIVE` pill.

### 3. Chip = switcher (control)
Clicking the chip opens a kernel/type menu — the five language tokens + a divider +
Markdown / Raw — replacing the binary code↔markdown toggle. Selecting:
- a code language → writes `code_type` (`setCellCodeType`), ensuring `type==="code"`.
- Markdown / Raw → writes cell `type` (existing `setCellType`).
- **AI Agent → present but disabled** (tooltip: agent cells need backend wiring). Assigning
  the `spur` kernelspec is a backend concern, deferred with the backend-surface epic.

### 4. Per-`code_type` highlighting
`CellInput` reads `codeType` from the store and selects the CodeMirror language:
`python()` / `javascript()` / `rust()` / Go via `StreamLanguage` (`@codemirror/legacy-modes`).
Spur/AI cells (prompt text) get no code grammar. The `language` compartment reconfigures on
`codeType` change.

## Scope boundary

**Frontend now (this plan):** token module, chip + accent bar + tinted gutter, switcher menu
(Py/JS/Rs/Go + Markdown/Raw), per-language highlighting + deps, regenerate `CodeType.ts`
(adds `go`).

**Backend-gated (deferred, pairs with `bd-1bpb` + backend-surface epic):** AI `● LIVE`
auto-run cascade & `ai_live` persistence; switching a cell *into* an AI/`spur` cell
(kernelspec + agent assignment); agent name / usage / cached output. The chip surfaces these
controls disabled.

## File map

- Create `crates/spur-notebook/jute-notebook/src/ui/notebook/cellLanguage.ts` (+ test)
- Create `crates/spur-notebook/jute-notebook/src/ui/notebook/CellLanguageMenu.tsx` (+ test)
- Modify `crates/spur-notebook/jute-notebook/src/ui/notebook/NotebookCells.tsx` (+ test) —
  chip header, accent bar, tinted gutter (generalize `AiCellHeader`)
- Modify `crates/spur-notebook/jute-notebook/src/ui/notebook/CellInput.tsx` — codeType-driven highlighting
- Modify `crates/spur-notebook/jute-notebook/package.json` — `@codemirror/lang-javascript`,
  `@codemirror/lang-rust`, `@codemirror/legacy-modes`
- Regenerate `crates/spur-notebook/jute-notebook/src/bindings/CodeType.ts` via `ts-rs-export`
