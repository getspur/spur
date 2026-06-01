# Open Design on Jute — M2a Native Deck Track Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Teach the `open-design` skill to build **native Jute decks** for `kind: deck` — one cell per slide with `jute_deck` metadata driven through `notebook_*` + `set_cell_metadata` — and give it a deck-aware critique. Pure skill-side wiring; **no `jute-notebook` code changes** (the deck mode, 12 layouts, present chrome, and the `jute_deck` facet already exist and work).

**Architecture:** Jute already renders decks natively: `cellToSlide()` maps each cell to a slide via the `metadata.jute_deck` facet (which `set_cell_metadata` already merges), `JuteDeckLayout` defines 12 layouts, and present mode / speaker notes / fragments are built. M2a adds a reference doc (`references/deck-mode.md`) teaching the brain to drive that, wires the `open-design` skill's Artifact step to use it on `kind: deck`, and adds deck-specific critique. The artifact-deck track (OD's 51 `html-ppt-*` themes) and reconciling `dispatchDeckCommand` are explicitly deferred (M2c / open decision).

**Tech Stack:** Markdown (skill reference content), Rust (`spur-core` skills tests, established pattern). No new dependencies, no jute/TS changes.

**Reference spec:** `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb` (approved).

---

## Verified facts the content must honour (read from `src/ui/deck/*`, `src/bindings/JuteDeck*`)

- **`JuteDeckLayout`** (12): `auto · title · section · content · bullets · code · output · code-output · two-col · image · blank`. `auto` = infer.
- **`metadata.jute_deck`** per-cell: `layout`, `hidden`, `speaker_notes`, `theme_override`, `fragments`, `background`.
- **Notebook `metadata.jute_deck`**: `theme` (default `minimal-light`), `aspect` (default `16:9`), `title`, `author`.
- **`set_cell_metadata(id, patch, expected_version)`** merges into `cell.metadata.jute_deck` — no new tool needed.
- **`cellToSlide` inference**: `# H1` → `title`, `## H2` → `section`, lines with `- `/`* ` bullets → `bullets`; explicit `layout` overrides.
- **3 built-in themes**: `minimal-light`, `minimal-dark`, `spur-brand` (more ported in M2b).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/spur-core/src/skills/open-design/references/deck-mode.md` | **New.** Native-deck authoring guide (layouts, `jute_deck`, present). |
| `crates/spur-core/src/skills/open-design/SKILL.md` | **Modify.** Artifact step routes `kind: deck` to the native flow. |
| `crates/spur-core/src/skills/open-design/references/critique.md` | **Modify.** Add a "Deck-specific checks" section. |
| `crates/spur-core/src/skills/open-design/CREATION-LOG.md` | **Modify.** M2a provenance entry. |
| `crates/spur-core/src/skills/mod.rs` | **Modify.** Two tests (deck-mode wiring; deck critique). |

**Out of scope (do NOT build):** any change under `crates/spur-notebook/jute-notebook/` (deck mode already works), the artifact-deck track / OD `html-ppt-*` themes (M2c), reconciling `dispatchDeckCommand` (open decision), new themes (M2b), export.

---

## Task 1: `deck-mode.md` reference + wire the Artifact step

**Files:**
- Create: `crates/spur-core/src/skills/open-design/references/deck-mode.md`
- Modify: `crates/spur-core/src/skills/open-design/SKILL.md`
- Modify: `crates/spur-core/src/skills/mod.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn open_design_deck_mode_native_flow() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/deck-mode.md"),
            "Artifact step must route kind:deck to the native deck guide"
        );
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references/deck-mode.md");
        let text = std::fs::read_to_string(&refs).expect("deck-mode.md must exist");
        for marker in ["jute_deck", "set_cell_metadata", "title", "section", "bullets", "speaker_notes"] {
            assert!(text.contains(marker), "deck-mode.md must document `{marker}`");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib skills::tests::open_design_deck_mode_native_flow`
Expected: FAIL — `deck-mode.md must exist`.

- [ ] **Step 3: Create `references/deck-mode.md` (verbatim)**

```markdown
# Open Design — Native Deck Mode

When the brief is a deck (`kind: deck`), do NOT emit a single HTML blob. Build a
**native Jute deck**: the notebook IS the deck, one cell per slide. Jute renders it
via `cellToSlide` + layout components, with present mode, speaker notes, and bullet
reveal already built.

## Set up the deck (notebook-level)
Set `metadata.jute_deck` on the notebook: `{ theme: "minimal-light", aspect: "16:9",
title: "<deck title>", author?: "<name>" }`. (More themes arrive in M2b; the 3
built-ins are `minimal-light`, `minimal-dark`, `spur-brand`.)

## One cell per slide
- `notebook_insert_cell(kind="markdown", source="...")` for prose slides (the common case);
  `kind="code"` only when the slide shows live code/output.
- Then `set_cell_metadata(id, patch={ ... }, expected_version)` to set the slide's
  `jute_deck` facet. The patch merges into `cell.metadata.jute_deck`.

## Per-slide `jute_deck` fields
- `layout`: one of `title · section · content · bullets · code · output · code-output ·
  two-col · image · blank` (omit or `auto` to infer).
- `speaker_notes`: markdown shown only via the `S` overlay in present mode.
- `fragments`: `true` to reveal markdown bullets one at a time.
- `background`: per-slide color or image URL.
- `theme_override`: a theme id for this slide only.
- `hidden`: `true` to keep a cell in the notebook but skip it in the deck.

## Layout inference (so you can often skip `layout`)
- `# H1` (one line) → `title`
- `## H2` (one line) → `section`
- lines starting `- ` / `* ` → `bullets`
- code cell → `code` (or `code-output` when it has output)
- otherwise → `content`; use `two-col` / `image` explicitly when relevant.

## Flow
1. Set notebook `jute_deck` (theme, aspect, title).
2. For each slide: `notebook_insert_cell` → `set_cell_metadata(jute_deck:{layout, speaker_notes?, fragments?})`.
3. Keep one idea per slide; let inference pick the layout unless you need an explicit one.
4. Critique with the **Deck-specific checks** in `references/critique.md`, then revise the slide cells.

> The polished/branded "artifact deck" track (OD's magazine/launch HTML themes) lands in
> M2c. For now, native deck mode is the path for every `kind: deck`.
```

- [ ] **Step 4: Wire the Artifact step in SKILL.md**

In `crates/spur-core/src/skills/open-design/SKILL.md`, under `### 4. Artifact`, insert this as the FIRST bullet (before the existing `notebook_insert_cell(kind="code", …)` bullet):

```markdown
- **If the brief is a deck (`kind: deck`)**, do NOT emit a single HTML blob — build a
  native Jute deck instead: see `references/deck-mode.md` (one cell per slide +
  `jute_deck` metadata via `set_cell_metadata`). The bullets below are for non-deck,
  single-HTML artifacts.
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_deck_mode_native_flow`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/deck-mode.md \
        crates/spur-core/src/skills/open-design/SKILL.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "feat(open-design): native deck-mode reference + Artifact-step wiring (M2a)"
```

---

## Task 2: Deck-aware critique

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/references/critique.md`
- Modify: `crates/spur-core/src/skills/mod.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn open_design_critique_has_deck_checks() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references");
        let critique = std::fs::read_to_string(dir.join("critique.md")).unwrap();
        assert!(
            critique.contains("Deck-specific checks"),
            "critique.md must include deck-specific checks"
        );
        for marker in ["one idea per slide", "theme rhythm", "slide counter"] {
            assert!(critique.contains(marker), "deck checks must cover `{marker}`");
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib skills::tests::open_design_critique_has_deck_checks`
Expected: FAIL — assertion on `Deck-specific checks`.

- [ ] **Step 3: Append the deck-checks section to `critique.md` (verbatim)**

Append to `crates/spur-core/src/skills/open-design/references/critique.md`:

```markdown

## Deck-specific checks (run for `kind: deck`)

Apply these in addition to the 5-dimensional critique:

- **One idea per slide** — if a slide makes two points, split it.
- **Readable from the back row** — headlines ≥ 36px, body ≥ 22px.
- **Theme rhythm** — no 3+ consecutive slides on the same layout; break up content slides
  with `section` covers.
- **Slide counter present** — the audience can always see position (native present mode shows it).
- **Speaker notes, not slide clutter** — move detail into `jute_deck.speaker_notes`, keep the
  slide sparse.
- **One accent, used sparingly** — same restraint as the anti-AI-slop checklist above.
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_critique_has_deck_checks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/critique.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "feat(open-design): deck-aware critique checks (M2a)"
```

---

## Task 3: Provenance + final gate

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/CREATION-LOG.md`

- [ ] **Step 1: Append the M2a entry**

Append to `crates/spur-core/src/skills/open-design/CREATION-LOG.md`:

```markdown

- **2026-06-01** — M2a: native deck track. Added `references/deck-mode.md` (one cell per
  slide + `jute_deck` via `set_cell_metadata`, 12 layouts, present mode), routed the Artifact
  step to it for `kind: deck`, and added deck-specific critique checks. No jute-notebook
  changes (deck mode already exists). Artifact-deck track + `html-ppt-*` themes = M2c;
  `dispatchDeckCommand` reconciliation = open decision. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.
```

- [ ] **Step 2: Run the full skills suite as the final gate**

Run: `cargo test -p spur-core --lib skills`
Expected: all passing (the prior open-design tests + the two new M2a tests + the pre-existing suite).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/skills/open-design/CREATION-LOG.md
git commit -m "docs(open-design): record M2a native deck track"
```

---

## Self-Review Notes

- **Spec coverage:** M2a row = "wire `open-design` deck flow to native deck mode; deck-aware critique." Task 1 = reference + SKILL.md wiring; Task 2 = critique; Task 3 = provenance + gate. Artifact track (M2c), theme port (M2b), and `dispatchDeckCommand` reconciliation are explicitly deferred per the spec's milestones/open-decisions.
- **No jute changes:** every fact (12 layouts, `jute_deck` fields, `set_cell_metadata` merge, inference rules) is documented from the existing code; M2a ships zero TS/Rust changes to `jute-notebook`.
- **No gitignored deps:** all content is authored inline here; nothing reads `resources/open-design/`.
- **Type consistency:** test fn names + `load_skill`/`env!("CARGO_MANIFEST_DIR")` usage match the M1/M3.5 tests in `crates/spur-core/src/skills/mod.rs`.
- **No placeholders:** `deck-mode.md`, the SKILL.md bullet, the critique section, and both tests are given verbatim.
