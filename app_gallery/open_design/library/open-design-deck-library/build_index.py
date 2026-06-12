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
