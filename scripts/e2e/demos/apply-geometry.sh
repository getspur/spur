#!/usr/bin/env bash
# Apply geometry.env into all demo VHS tapes under scripts/e2e/demos/.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
source "$ROOT/geometry.env"

export SPUR_VHS_WIDTH SPUR_VHS_HEIGHT SPUR_VHS_FONT_SIZE SPUR_VHS_PADDING

python3 - "$ROOT" <<'PY'
import os, re, sys
from pathlib import Path

root = Path(sys.argv[1])
w = os.environ["SPUR_VHS_WIDTH"]
h = os.environ["SPUR_VHS_HEIGHT"]
fs = os.environ["SPUR_VHS_FONT_SIZE"]
pad = os.environ.get("SPUR_VHS_PADDING", "10")

tapes = list(root.rglob("*.tape"))
changed = 0
for tape in tapes:
    text = tape.read_text()
    orig = text
    text = re.sub(r"^Set FontSize \d+\s*$", f"Set FontSize {fs}", text, flags=re.M)
    text = re.sub(r"^Set Width \d+\s*$", f"Set Width {w}", text, flags=re.M)
    text = re.sub(r"^Set Height \d+\s*$", f"Set Height {h}", text, flags=re.M)
    text = re.sub(r"^Set Padding \d+\s*$", f"Set Padding {pad}", text, flags=re.M)
    if text != orig:
        tape.write_text(text)
        changed += 1
        print(f"updated {tape.relative_to(root)}")
print(f"tapes_updated={changed} total={len(tapes)} geometry={w}x{h} font={fs}")
PY
