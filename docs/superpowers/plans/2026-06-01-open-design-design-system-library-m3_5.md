# Open Design on Jute — M3.5 Design-System Library Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Land Open Design's 148 design systems as a committed, validated **library** with a generated `index.json`, and document the brand→palette selection procedure in the `open-design` skill — without shipping any runtime install path, MCP tooling, or mode skills (those are M4).

**Architecture:** The 148 `DESIGN.md` files have already been vendored (by the brain, since their source `resources/open-design/` is gitignored and absent from worker trees) into `crates/spur-notebook/assets/open-design-library/design-systems/<id>/DESIGN.md`. This plan adds (1) a variance-tolerant index generator + committed `index.json`, (2) a self-check that keeps the index in sync with the on-disk set, and (3) a skill reference doc documenting the index schema + selection procedure. Access is via the agent's plain `Read` (the portable default from the asset-library spec); the runtime install location and any MCP surface are deferred to M4.

**Tech Stack:** Python 3 (generator + self-check — `python3` is on PATH), Markdown, Rust (`spur-core` skills tests, established pattern). No new dependencies.

**Reference spec:** `docs/superpowers/specs/2026-06-01-open-design-asset-library-design.ipynb` (approved).

---

## Pre-vendored data (already committed on this branch — do NOT re-create)

`crates/spur-core/...` is untouched; the data lives under the notebook crate's assets:

```
crates/spur-notebook/assets/open-design-library/design-systems/<id>/DESIGN.md   × 148
```

Verified facts the generator must honour (measured across all 148):
- Every file has a `# <title>` H1 (148/148) and a `> Category: <name>` line (148/148).
- 147/148 contain `#hex` color tokens; **`bmw-m` has none** → swatches must be allowed to be empty.
- Color formatting **varies**: some files use `- **Primary:** \`#FF5701\` — …`, others `### Primary` + `- **Rausch** (\`#ff385c\`): …`. Do **not** parse a fixed line shape — scan for hex codes.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/spur-notebook/assets/open-design-library/design-systems/<id>/DESIGN.md` | **Pre-vendored (148).** Source data. Read-only for this plan. |
| `crates/spur-notebook/assets/open-design-library/build_index.py` | **New.** Generator + `--check` self-validation. |
| `crates/spur-notebook/assets/open-design-library/index.json` | **New (generated).** Committed index. |
| `crates/spur-notebook/assets/open-design-library/README.md` | **New.** What the library is, how to regenerate, schema. |
| `crates/spur-core/src/skills/open-design/references/design-systems.md` | **New.** Index schema + brand→palette selection procedure for the brain. |
| `crates/spur-core/src/skills/open-design/SKILL.md` | **Modify.** Direction step references the design-system library. |
| `crates/spur-core/src/skills/mod.rs` | **Modify.** One test asserting the skill references the library. |

**Out of scope (do NOT build):** runtime install path / copying the library into `.spur/open-design/`, any `open_design_*` MCP tool or MCP Resource, the ~30 mode skills, deck themes, editing the 148 `DESIGN.md` files.

---

## Task 1: Index generator + committed `index.json` + self-check

**Files:**
- Create: `crates/spur-notebook/assets/open-design-library/build_index.py`
- Create: `crates/spur-notebook/assets/open-design-library/index.json` (generated)
- Create: `crates/spur-notebook/assets/open-design-library/README.md`

- [ ] **Step 1: Write the generator (this is the exact implementation — use it verbatim)**

Create `crates/spur-notebook/assets/open-design-library/build_index.py`:

```python
#!/usr/bin/env python3
"""Generate/verify index.json for the Open Design design-system library.

Scans design-systems/<id>/DESIGN.md and emits a compact index. Mirrors Open
Design's apps/daemon/src/design-systems.ts parsing, tolerant of format variance.

Usage:
  python3 build_index.py          # (re)write index.json
  python3 build_index.py --check  # exit 1 if index.json is stale/out of sync
"""
import json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
DS_DIR = ROOT / "design-systems"
INDEX = ROOT / "index.json"

H1 = re.compile(r"^#\s+(.+?)\s*$", re.M)
CATEGORY = re.compile(r"^>\s*Category:\s*(.+?)\s*$", re.M)
HEX = re.compile(r"#[0-9A-Fa-f]{3,8}\b")
TITLE_PREFIX = re.compile(r"^(?:Design System(?:\s+Inspired by|:)?\s+)", re.I)

def clean_title(raw: str) -> str:
    return TITLE_PREFIX.sub("", raw).strip() or raw.strip()

def first_paragraph(text: str, after: int) -> str:
    # First non-empty, non-blockquote, non-heading paragraph after `after`.
    for block in re.split(r"\n\s*\n", text[after:]):
        b = block.strip()
        if not b or b.startswith(">") or b.startswith("#"):
            continue
        return re.sub(r"\s+", " ", b)[:240]
    return ""

def swatches(text: str, limit: int = 6) -> list[str]:
    seen, out = set(), []
    for m in HEX.finditer(text):
        v = m.group(0).lower()
        if v not in seen:
            seen.add(v); out.append(v)
        if len(out) >= limit:
            break
    return out

def build() -> list[dict]:
    rows = []
    for d in sorted(p for p in DS_DIR.iterdir() if p.is_dir()):
        md = d / "DESIGN.md"
        if not md.is_file():
            continue
        text = md.read_text(encoding="utf-8")
        h1 = H1.search(text)
        cat = CATEGORY.search(text)
        title = clean_title(h1.group(1)) if h1 else d.name
        rows.append({
            "id": d.name,
            "title": title,
            "category": cat.group(1).strip() if cat else "",
            "summary": first_paragraph(text, h1.end() if h1 else 0),
            "swatches": swatches(text),
        })
    return rows

def main() -> int:
    rows = build()
    payload = json.dumps({"version": 1, "kind": "design-systems", "count": len(rows),
                          "items": rows}, indent=2, ensure_ascii=False) + "\n"
    if "--check" in sys.argv:
        current = INDEX.read_text(encoding="utf-8") if INDEX.exists() else ""
        if current != payload:
            print("index.json is stale — run: python3 build_index.py", file=sys.stderr)
            return 1
        # Invariants
        assert rows, "no design systems found"
        assert all(r["id"] and r["title"] and r["category"] for r in rows), "missing id/title/category"
        assert any(r["id"] == "bmw-m" and r["swatches"] == [] for r in rows), \
            "bmw-m must be present with empty swatches (no-hex edge case)"
        print(f"OK: index.json in sync, {len(rows)} design systems")
        return 0
    INDEX.write_text(payload, encoding="utf-8")
    print(f"wrote {INDEX} ({len(rows)} design systems)")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
```

- [ ] **Step 2: Generate the index**

Run: `python3 crates/spur-notebook/assets/open-design-library/build_index.py`
Expected: `wrote …/index.json (148 design systems)`

- [ ] **Step 3: Run the self-check to verify it fails when stale, passes when fresh**

Run: `python3 crates/spur-notebook/assets/open-design-library/build_index.py --check`
Expected: `OK: index.json in sync, 148 design systems` (exit 0).
Then sanity-check the edge case is captured:
Run: `python3 -c "import json;d=json.load(open('crates/spur-notebook/assets/open-design-library/index.json'));print(d['count']);print([r['id'] for r in d['items'] if not r['swatches']])"`
Expected: `148` and a list that includes `bmw-m`.

- [ ] **Step 4: Write the library README**

Create `crates/spur-notebook/assets/open-design-library/README.md`:

```markdown
# Open Design — Asset Library

Vendored from Open Design (`resources/open-design/`, gitignored upstream). M3.5 ships
the **design-systems** layer only; mode skills + themes arrive in M4.

## Layout
- `design-systems/<id>/DESIGN.md` — 148 branded design systems (palette + type + posture).
- `index.json` — generated discovery/selection metadata (`id`, `title`, `category`, `summary`, `swatches`).

## Regenerate
`python3 build_index.py`  · verify in CI/review with `python3 build_index.py --check`.

`index.json` is committed; `--check` fails if it drifts from the `DESIGN.md` set.
Runtime install location and access surface (Read vs MCP) are finalized in M4.
```

- [ ] **Step 5: Commit**

```bash
git add crates/spur-notebook/assets/open-design-library/build_index.py \
        crates/spur-notebook/assets/open-design-library/index.json \
        crates/spur-notebook/assets/open-design-library/README.md
git commit -m "feat(open-design): design-system library index generator + index.json"
```

---

## Task 2: Document the selection procedure in the `open-design` skill

**Files:**
- Create: `crates/spur-core/src/skills/open-design/references/design-systems.md`
- Modify: `crates/spur-core/src/skills/open-design/SKILL.md` (the Direction step)
- Modify: `crates/spur-core/src/skills/mod.rs` (one test)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/spur-core/src/skills/mod.rs`:

```rust
    #[test]
    fn open_design_references_design_system_library() {
        let fake = PathBuf::from("/nonexistent");
        let body = load_skill("open-design", &fake).unwrap();
        assert!(
            body.contains("references/design-systems.md"),
            "Direction step must point at the design-system library reference"
        );
        // The reference doc itself ships beside the skill source.
        let refs = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/skills/open-design/references/design-systems.md");
        let text = std::fs::read_to_string(&refs).expect("design-systems.md must exist");
        assert!(
            text.contains("index.json") && text.contains("swatches"),
            "reference must document the index schema"
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --lib skills::tests::open_design_references_design_system_library`
Expected: FAIL — `design-systems.md must exist` (not created yet).

- [ ] **Step 3: Create the reference doc**

Create `crates/spur-core/src/skills/open-design/references/design-systems.md`:

```markdown
# Open Design — Design-System Library

148 branded design systems (Linear, Stripe, Vercel, Airbnb, …) vendored under the
Open Design asset library: `assets/open-design-library/design-systems/<id>/DESIGN.md`,
with a compact `index.json` beside them.

## index.json schema
`{ version, kind: "design-systems", count, items: [ { id, title, category, summary, swatches[] } ] }`
- `swatches` are up to 6 lowercase hex codes in document order; may be empty (e.g. `bmw-m`).

## Selecting a design system (Direction step)
1. If the user names a brand or a strong visual reference, scan `index.json` `items`
   by `id` / `title` / `category` / `summary` for the closest match.
2. `Read` that system's `design-systems/<id>/DESIGN.md` for the full palette, type
   stack, and posture, and bind it to the artifact's CSS `:root`.
3. If no brand fits, fall back to the 5 directions in `references/directions.md`.

> Runtime install location and any search tool / MCP Resource surface are finalized
> in M4. For now, selection is `Read`-driven against the committed library + index.
```

- [ ] **Step 4: Wire the Direction step in SKILL.md**

In `crates/spur-core/src/skills/open-design/SKILL.md`, in the `### 2. Direction` step, add this bullet (keep the existing directions bullet):

```markdown
- If the user names a **brand** or strong visual reference, consult the design-system
  library first — see `references/design-systems.md` (search `index.json`, then `Read`
  the chosen `DESIGN.md` and bind its palette). Otherwise use the 5 directions below.
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-core --lib skills::tests::open_design_references_design_system_library`
Expected: PASS.

- [ ] **Step 6: Run the full skills suite (no regressions)**

Run: `cargo test -p spur-core --lib skills`
Expected: all passing (the 4 existing open-design tests + this new one + the pre-existing suite).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/skills/open-design/references/design-systems.md \
        crates/spur-core/src/skills/open-design/SKILL.md \
        crates/spur-core/src/skills/mod.rs
git commit -m "feat(open-design): wire Direction step to the design-system library"
```

---

## Task 3: Provenance + spec/plan status

**Files:**
- Modify: `crates/spur-core/src/skills/open-design/CREATION-LOG.md`

- [ ] **Step 1: Append the M3.5 entry**

Append to `crates/spur-core/src/skills/open-design/CREATION-LOG.md`:

```markdown

- **2026-06-01** — M3.5: vendored 148 design systems under
  `crates/spur-notebook/assets/open-design-library/design-systems/`, added
  `build_index.py` + committed `index.json`, and wired the Direction step to the
  design-system library via `references/design-systems.md`. Read-driven selection;
  runtime install path + MCP surface deferred to M4. Spec:
  `docs/superpowers/specs/2026-06-01-open-design-asset-library-design.ipynb`.
```

- [ ] **Step 2: Re-run the self-check and skills suite as a final gate**

Run: `python3 crates/spur-notebook/assets/open-design-library/build_index.py --check && cargo test -p spur-core --lib skills`
Expected: `OK: index.json in sync, 148 design systems` then all skills tests passing.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/skills/open-design/CREATION-LOG.md
git commit -m "docs(open-design): record M3.5 design-system library"
```

---

## Self-Review Notes

- **Spec coverage:** M3.5 spec row = "vendor 148 DESIGN.md + generate index + wire Direction step." Data vendoring done by brain (gitignore constraint, stated). Index = Task 1. Direction wiring = Task 2. Runtime install path + MCP access surface explicitly deferred to M4 per the spec's own milestone split.
- **Gitignore constraint honoured:** the 148 source files are pre-committed on this branch; no task reads `resources/open-design/` (absent from worker trees).
- **Variance tolerance proven:** generator scans for hex anywhere (not a fixed line shape) and the `--check` asserts the `bmw-m` no-hex edge case — matching the measured reality (147/148 have hex; airbnb vs agentic differ).
- **Type consistency:** test fn names, `load_skill`/`all_bundled_raw`/`env!("CARGO_MANIFEST_DIR")` usage match `crates/spur-core/src/skills/mod.rs` conventions and the M1 tests.
- **No placeholders:** the generator is given in full; the reference doc and SKILL.md bullet are given verbatim.
