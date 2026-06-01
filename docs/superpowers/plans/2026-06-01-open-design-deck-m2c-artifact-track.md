# Open Design Deck — M2c: Artifact Deck Track + Theme Library Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb` (cells c5 #4, c7 artifact track, c8 track-selection, c11 M2c)
**Design epic:** M2 deck-mode (spec merged)

**Goal:** Add the **artifact-deck track** to the `open-design` skill — for "polished / magazine / launch / branded" deck briefs, render Open Design's fixed-canvas `DECK_SKELETON_HTML` framework as one `text/html` cell, themed by one of 51 vendored deck themes. Native deck mode (M2a) stays the default; this is the escalation path.

**Architecture:** Mirror the M3.5 design-system library exactly. The **brain vendors** (pre-commits on the base branch, because `resources/open-design/` is gitignored and absent to workers) the 51 self-contained deck theme dirs + extracts `DECK_SKELETON_HTML` into a standalone `deck-skeleton.html`, into a new `crates/spur-notebook/assets/open-design-deck-library/`. Workers then: write a tolerant `build_index.py` + generate `index.json`; wire the skill's artifact-deck track (`references/deck-artifact.md` + a SKILL.md step-4 escalation bullet); add deck-artifact critique checks; record provenance + run the final gate.

**Tech Stack:** Python 3 (stdlib only) for the index generator; markdown skill refs; Rust (`cargo test -p spur-core --lib skills`).

---

## BRAIN PRE-WORK (done before submit, committed on the base branch `plan/open-design-deck-m2c`)

The brain performs these steps itself (workers cannot see `resources/open-design/`):

1. Create `crates/spur-notebook/assets/open-design-deck-library/deck-themes/`.
2. Copy the **51 self-contained deck themes** from `resources/open-design/skills/` (each full dir, verbatim):
   `guizang-ppt`, `replit-deck`, and the 48 `mode: deck` `html-ppt-*` dirs.
   **Exclude** (not self-contained visual themes): `html-ppt-retro-quarterly-review`
   (`mode: template`, video/hyperframes), `simple-deck` and `weekly-update`
   (`design_system.requires: true` — content scaffolds, not themes).
3. Extract `DECK_SKELETON_HTML` (the template-literal body, lines 38–308) from
   `resources/open-design/packages/contracts/src/prompts/deck-framework.ts` into
   `assets/open-design-deck-library/deck-skeleton.html` verbatim (open decision #3:
   skeleton lives in the asset library, beside the themes).
4. Commit the vendored payload on the base branch.

Workers operate only on the committed library + the spur-core skill. The plan tasks below assume the vendored dirs already exist.

---

## Index schema (target)

`{ version, kind: "deck-themes", count, items: [ { id, title, scenario, mode, featured, summary, source, swatches[] } ] }`

- `id` = dir name. `title` = frontmatter `name` (fallback: first `# H1`).
- `scenario` / `mode` / `featured` / `source` = from the optional `od:` block (tolerant; defaults `""`/`"deck"`/`null`/`""`).
- `summary` = frontmatter `description`, truncated 240 chars.
- `swatches` = up to 6 lowercase hex from `template.json` `palette` (zhangzara) else hex-scan of the dir's preview HTML; may be empty.

---

## Task 1: Index generator + `index.json` + README

**Task ID:** `t1-index`

**Files:**
- Create: `crates/spur-notebook/assets/open-design-deck-library/build_index.py`
- Create: `crates/spur-notebook/assets/open-design-deck-library/index.json` (generated, committed)
- Create: `crates/spur-notebook/assets/open-design-deck-library/README.md`

**Depends on:** none (operates on the brain-vendored `deck-themes/`)

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above, inside `assets/open-design-deck-library/`.
- OUT of scope: the vendored `deck-themes/<id>/` contents (read-only — do NOT edit them), `deck-skeleton.html`, `crates/spur-core/`, `crates/spur-notebook/jute-notebook/`. Do NOT read `resources/open-design/`.
- Emit `scope_drift` otherwise.

**Acceptance Criteria:**
- [ ] `python3 build_index.py` writes `index.json` with one item per `deck-themes/<id>/` dir that has a `SKILL.md`.
- [ ] `python3 build_index.py --check` exits 0 against the committed `index.json`.
- [ ] Every item has non-empty `id`, `title`, `mode`; `swatches` ≤ 6, all hex.

**Implementation:**

- [ ] **Step 1: Write `build_index.py`** (tolerant, stdlib-only; mirrors the M3.5 generator):

```python
#!/usr/bin/env python3
"""Generate/verify index.json for the Open Design deck-theme library.

Scans deck-themes/<id>/SKILL.md (+ optional template.json) and emits a compact
index. Tolerant of frontmatter variance (bare name/description vs full `od:` block).

Usage:
  python3 build_index.py          # (re)write index.json
  python3 build_index.py --check  # exit 1 if index.json is stale/out of sync
"""
import json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
TH_DIR = ROOT / "deck-themes"
INDEX = ROOT / "index.json"

FM = re.compile(r"^---\s*$(.*?)^---\s*$", re.M | re.S)
NAME = re.compile(r"^name:\s*(.+?)\s*$", re.M)
DESC = re.compile(r"^description:\s*(.+?)\s*$", re.M)
H1 = re.compile(r"^#\s+(.+?)\s*$", re.M)
SCENARIO = re.compile(r"^\s+scenario:\s*(.+?)\s*$", re.M)
MODE = re.compile(r"^\s+mode:\s*(.+?)\s*$", re.M)
FEATURED = re.compile(r"^\s+featured:\s*(\d+)\s*$", re.M)
UPSTREAM = re.compile(r'^\s+upstream:\s*"?(.+?)"?\s*$', re.M)
HEX = re.compile(r"#[0-9A-Fa-f]{3,8}\b")

def first(rx, text, default=""):
    m = rx.search(text)
    return m.group(1).strip() if m else default

def swatches_for(d: Path) -> list:
    seen = []
    tj = d / "template.json"
    if tj.is_file():
        try:
            pal = json.loads(tj.read_text(encoding="utf-8")).get("palette", {})
            for v in pal.values():
                if isinstance(v, str):
                    for h in HEX.findall(v):
                        h = h.lower()
                        if h not in seen:
                            seen.append(h)
        except Exception:
            pass
    if not seen:
        for cand in ("example.html", "assets/template.html", "assets/example-slides.html"):
            f = d / cand
            if f.is_file():
                for h in HEX.findall(f.read_text(encoding="utf-8", errors="ignore")):
                    h = h.lower()
                    if h not in seen:
                        seen.append(h)
                break
    return seen[:6]

def build():
    rows = []
    for d in sorted(p for p in TH_DIR.iterdir() if p.is_dir()):
        skill = d / "SKILL.md"
        if not skill.is_file():
            continue
        text = skill.read_text(encoding="utf-8")
        fm = FM.search(text)
        front = fm.group(1) if fm else ""
        title = first(NAME, front) or first(H1, text) or d.name
        desc = first(DESC, front)
        rows.append({
            "id": d.name,
            "title": title,
            "scenario": first(SCENARIO, front),
            "mode": first(MODE, front) or "deck",
            "featured": int(first(FEATURED, front, "0")) or None,
            "summary": desc[:240],
            "source": first(UPSTREAM, front),
            "swatches": swatches_for(d),
        })
    return rows

def main():
    rows = build()
    payload = json.dumps(
        {"version": 1, "kind": "deck-themes", "count": len(rows), "items": rows},
        ensure_ascii=False, indent=2,
    ) + "\n"
    if "--check" in sys.argv:
        current = INDEX.read_text(encoding="utf-8") if INDEX.is_file() else ""
        if current != payload:
            print("index.json is stale; run build_index.py", file=sys.stderr)
            sys.exit(1)
        # Invariants
        assert rows, "no deck themes found"
        assert len(rows) == sum(
            1 for p in TH_DIR.iterdir() if (p / "SKILL.md").is_file()
        ), "every deck-themes/<id>/SKILL.md must have exactly one index entry"
        assert all(r["id"] and r["title"] and r["mode"] for r in rows), "missing id/title/mode"
        assert all(len(r["swatches"]) <= 6 for r in rows), "swatches capped at 6"
        assert all(s.startswith("#") for r in rows for s in r["swatches"]), "swatches must be hex"
        print(f"OK: {len(rows)} deck themes in sync")
        return
    INDEX.write_text(payload, encoding="utf-8")
    print(f"wrote {INDEX} ({len(rows)} themes)")

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Generate** — `python3 build_index.py`. Then **verify** — `python3 build_index.py --check` (Expected: `OK: <N> deck themes in sync`, exit 0).

- [ ] **Step 3: Write `README.md`** (mirrors the M3.5 library README):

```markdown
# Open Design — Deck Theme Library

Vendored from Open Design (`resources/open-design/skills/`, gitignored upstream). M2c
ships the **artifact-deck** track: 51 self-contained 16:9 HTML deck themes + the shared
`deck-skeleton.html` fixed-canvas framework.

## Layout
- `deck-themes/<id>/` — a vendored deck theme (SKILL.md spec + example/template HTML;
  zhangzara themes also carry `template.json`).
- `deck-skeleton.html` — `DECK_SKELETON_HTML` verbatim (1920×1080 scale-to-fit, keyboard
  nav, print-to-PDF). Copy it verbatim; fill only the `SLOT:` markers.
- `index.json` — generated discovery metadata (`id`, `title`, `scenario`, `mode`,
  `featured`, `summary`, `source`, `swatches`).

## Regenerate
`python3 build_index.py`  ·  verify with `python3 build_index.py --check`.

Excludes `simple-deck`/`weekly-update` (need an active design system) and
`html-ppt-retro-quarterly-review` (video/template mode). Runtime access surface
(Read vs MCP) is finalized in M4.
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-notebook/assets/open-design-deck-library/build_index.py \
        crates/spur-notebook/assets/open-design-deck-library/index.json \
        crates/spur-notebook/assets/open-design-deck-library/README.md
git commit -m "deck-library: tolerant index generator + index.json + README"
```

**Scope Drift Checkpoint:** if `--check` cannot be made to pass without editing a vendored `deck-themes/<id>/` file, STOP and emit `risk` — the generator must adapt to the data, never the reverse.

---

## Task 2: Artifact-deck track reference + SKILL.md escalation

**Task ID:** `t2-artifact-track`

**Files:**
- Create: `crates/spur-core/src/skills/open-design/references/deck-artifact.md`
- Modify: `crates/spur-core/src/skills/open-design/SKILL.md`
- Modify: `crates/spur-core/src/skills/mod.rs`

**Depends on:** none (skill text is self-contained; does not need t1's index to be authored)

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: the three files above (all `crates/spur-core/`).
- OUT of scope: `crates/spur-notebook/`, `references/deck-mode.md` (M2a/M2b own it), `references/critique.md` (t3 owns it). Do NOT read `resources/open-design/`.
- Emit `scope_drift` otherwise.

**Acceptance Criteria:**
- [ ] `deck-artifact.md` documents the track-selection rule, verbatim-skeleton usage, theme selection from `index.json`, and rendering as a `text/html` cell.
- [ ] SKILL.md step 4 escalates "polished/branded/named-taste" deck briefs to the artifact track.
- [ ] New test `open_design_deck_artifact_track` passes.

**Implementation:**

- [ ] **Step 1: Write the failing test** in `crates/spur-core/src/skills/mod.rs` (beside the other open-design tests):

```rust
    #[test]
    fn open_design_deck_artifact_track() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/deck-artifact.md"),
            "SKILL.md must route polished/branded decks to the artifact track"
        );
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references/deck-artifact.md");
        let text = std::fs::read_to_string(&refs).expect("deck-artifact.md must exist");
        for marker in ["deck-skeleton.html", "index.json", "text/html", "SLOT:", "native"] {
            assert!(text.contains(marker), "deck-artifact.md must document `{marker}`");
        }
    }
```

- [ ] **Step 2: Run red** — `cargo test -p spur-core --lib skills::tests::open_design_deck_artifact_track`. Expected: FAIL.

- [ ] **Step 3: Create `references/deck-artifact.md`:**

```markdown
# Open Design — Artifact Deck Track

The **default** deck path is native Jute deck mode (`references/deck-mode.md`): editable,
reactive, present mode built in. Escalate to this **artifact track** only when the brief
wants a *polished, branded, pixel-fidelity* presentation that native layouts can't express.

## When to escalate (track-selection rule)
| Brief signal | Track |
|---|---|
| "working deck", "outline", "I'll edit slides", data/charts, reactive | **Native** (default — `deck-mode.md`) |
| "magazine", "launch", "investor pitch", "polished", a named taste (WIRED / editorial / brutalist / cyber), WebGL/hero | **Artifact** (this file) |
| unsure | **Native** — it's editable; the user can ask to "make it polished" to escalate |

## Build an artifact deck
1. **Pick a theme.** Scan `assets/open-design-deck-library/index.json` `items` by
   `id` / `title` / `scenario` / `summary` (and `swatches` for palette fit). `Read` the
   theme's `deck-themes/<id>/SKILL.md` for its rules, and its `example.html` /
   `assets/template.html` for the concrete pattern.
2. **Copy the framework verbatim.** Start from
   `assets/open-design-deck-library/deck-skeleton.html` — a 1920×1080 fixed canvas with
   scale-to-fit, keyboard nav, slide counter, and print-to-PDF already baked in. Do NOT
   re-derive the scaling/focus JavaScript; that is the whole point of shipping it verbatim.
3. **Fill only the `SLOT:` markers** — deck title, the `:root` theme tokens (bind the
   chosen theme's palette + fonts), the per-deck `<style>` block, and the
   `<section class="slide">` bodies. Leave the framework `<style>`, the chrome, and the
   trailing `<script>` untouched.
4. **Emit one cell.** Write the finished single HTML file as a `text/html` cell output
   (the M1 substrate) — it renders in Jute's sandboxed iframe (`allow-scripts`).
5. **Critique** with the deck-artifact checks in `references/critique.md`, then revise.

> This track produces one opaque HTML deck — no cell↔slide mapping or native present mode.
> If the user wants to edit slide-by-slide, use the **native** track instead.
```

- [ ] **Step 4: Edit `SKILL.md` step 4.** The deck bullet currently reads (added in M2a):

```
- **If the brief is a deck (`kind: deck`)**, do NOT emit a single HTML blob — build a
  native Jute deck instead: see `references/deck-mode.md` (one cell per slide +
  `jute_deck` metadata via `set_cell_metadata`). The bullets below are for non-deck,
  single-HTML artifacts.
```

Append one sub-bullet immediately after it (keep the existing bullet intact):

```
  - **Polished / branded decks** (magazine, launch, investor pitch, a named taste, or
    "make it look designed") escalate to the **artifact track**: see
    `references/deck-artifact.md` (Open Design's fixed-canvas `deck-skeleton.html` + one
    of 51 vendored themes, rendered as a single `text/html` cell). Native deck mode stays
    the default; escalate only on an explicit polish/brand signal.
```

- [ ] **Step 5: Run green** — `cargo test -p spur-core --lib skills::tests::open_design_deck_artifact_track`. Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/deck-artifact.md \
        crates/spur-core/src/skills/open-design/SKILL.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "open-design: artifact-deck track reference + escalation rule"
```

---

## Task 3: Deck-artifact critique checks

**Task ID:** `t3-critique`

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/references/critique.md`
- Modify: `crates/spur-core/src/skills/mod.rs`

**Depends on:** `t2-artifact-track`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `critique.md` + `mod.rs` only (both `crates/spur-core/`).
- OUT of scope: `SKILL.md`, `deck-artifact.md`, `deck-mode.md`, `crates/spur-notebook/`. Do NOT read `resources/open-design/`.

**Acceptance Criteria:**
- [ ] `critique.md` gains an "Artifact-deck checks" section.
- [ ] New test `open_design_critique_has_artifact_deck_checks` passes.

**Implementation:**

- [ ] **Step 1: Write the failing test** in `mod.rs`:

```rust
    #[test]
    fn open_design_critique_has_artifact_deck_checks() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references");
        let critique = std::fs::read_to_string(dir.join("critique.md")).unwrap();
        assert!(
            critique.contains("Artifact-deck checks"),
            "critique.md must include artifact-deck checks"
        );
        for marker in ["scale-to-fit", "slot", "16:9", "verbatim framework"] {
            assert!(critique.contains(marker), "artifact-deck checks must cover `{marker}`");
        }
    }
```

- [ ] **Step 2: Run red** — `cargo test -p spur-core --lib skills::tests::open_design_critique_has_artifact_deck_checks`. Expected: FAIL.

- [ ] **Step 3: Append to `critique.md`** (after the existing "Deck-specific checks" section):

```markdown

## Artifact-deck checks (run for the artifact track)

In addition to the deck-specific checks above, for a `deck-skeleton.html` artifact:

- **Verbatim framework intact** — the framework `<style>`, chrome, and trailing `<script>`
  are byte-for-byte from `deck-skeleton.html`; only the `SLOT:` markers were edited.
- **Scale-to-fit unbroken** — every slide is a `<section class="slide">` inside the
  1920×1080 `.deck-stage`; nothing overflows the fixed canvas at 16:9.
- **Theme bound at `:root`** — the chosen theme's palette + fonts are set as `:root` tokens,
  not hard-coded per slide; one accent, used sparingly (anti-AI-slop checklist still applies).
- **slot discipline** — title, `:root` tokens, per-deck `<style>`, and slide bodies are the
  only edits; counter + nav still render outside the scaled stage.
- **No native-mode confusion** — if the user wants slide-by-slide editing, this is the wrong
  track; switch to native deck mode.

<!-- test markers: scale-to-fit; slot; 16:9; verbatim framework -->
```

- [ ] **Step 4: Run green** — `cargo test -p spur-core --lib skills::tests::open_design_critique_has_artifact_deck_checks`. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/critique.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "open-design: artifact-deck critique checks"
```

---

## Task 4: Provenance + final gate

**Task ID:** `t4-provenance`

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/CREATION-LOG.md`

**Depends on:** `t3-critique`

**Suggested Worker:** codex

**Scope Boundary:**
- IN scope: `CREATION-LOG.md` only.
- OUT of scope: everything else. Do NOT read `resources/open-design/`.

**Acceptance Criteria:**
- [ ] CREATION-LOG has the M2c entry.
- [ ] Final gate `cargo test -p spur-core --lib skills` is green.

**Implementation:**

- [ ] **Step 1: Append the M2c CREATION-LOG entry:**

```markdown
- **2026-06-01** — M2c: artifact-deck track + theme library. Brain-vendored 51
  self-contained Open Design deck themes (`guizang-ppt`, `replit-deck`, 48 `html-ppt-*`)
  + `deck-skeleton.html` (the 1920×1080 fixed-canvas framework, verbatim) into
  `assets/open-design-deck-library/`, with a tolerant `build_index.py` + committed
  `index.json` (`--check`-guarded). Wired the skill's artifact-deck escalation
  (`references/deck-artifact.md` + SKILL.md step-4) and deck-artifact critique checks.
  Native deck mode (M2a) stays the default; the artifact track is the polish/brand
  escalation. Excludes `simple-deck`/`weekly-update` (need a design system) and
  `html-ppt-retro-quarterly-review` (video/template mode); those + `dispatchDeckCommand`
  reconciliation remain open. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-deck-mode-m2-design.ipynb`.
```

- [ ] **Step 2: Final gate** — `cargo test -p spur-core --lib skills`. Expected: PASS (all skills tests).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/skills/open-design/CREATION-LOG.md
git commit -m "open-design: M2c provenance (artifact-deck track + theme library)"
```

---

## Self-Review

- **Spec coverage:** c5 #4 (51 html-ppt as artifact-track library, indexed) → brain pre-work + t1. c7 (artifact track: pick theme → skeleton → fill slots → text/html cell) → t2 `deck-artifact.md`. c8 track-selection rule → t2. deck-skeleton home (open decision #3 = asset library) → brain pre-work. Critique for the new track → t3. M2c milestone (c11) → all tasks. **Deferred (noted, not in scope):** `dispatchDeckCommand` reconciliation (open decision #1) and export (#4) — M4-ish.
- **Placeholders:** none — full generator code, full `deck-artifact.md`, exact SKILL.md/critique edits, exact tests.
- **Type/string consistency:** index `kind: "deck-themes"`; test markers (`deck-skeleton.html`, `index.json`, `text/html`, `SLOT:`, `native` for t2; `scale-to-fit`, `slot`, `16:9`, `verbatim framework` for t3) each appear verbatim in the authored docs.
- **DAG:** t1 (library) ∥ t2 (skill text) are independent; t3 → t2; t4 → t3. Valid, acyclic; t1 can run in parallel with t2/t3.
- **beads:** each task has a unique id, explicit `depends_on`, verifiable criteria, and a scope boundary; the read-only vendored payload is fenced off with a `risk` checkpoint in t1.
