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
        assert len(rows) == sum(1 for p in DS_DIR.iterdir() if (p / "DESIGN.md").is_file()), \
            "every design-systems/<id>/DESIGN.md must have exactly one index entry"
        assert all(r["id"] and r["title"] and r["category"] for r in rows), "missing id/title/category"
        assert all(len(r["swatches"]) <= 6 for r in rows), "swatches capped at 6"
        assert all(s.startswith("#") for r in rows for s in r["swatches"]), "swatches must be hex"
        print(f"OK: index.json in sync, {len(rows)} design systems")
        return 0
    INDEX.write_text(payload, encoding="utf-8")
    print(f"wrote {INDEX} ({len(rows)} design systems)")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
