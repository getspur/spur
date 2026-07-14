#!/usr/bin/env bash
# Render VHS marketing captures + extract still frames for Higgsfield refs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$ROOT/../../.." && pwd)"
cd "$REPO_ROOT"

OUT="$ROOT/out"
FRAMES="$OUT/frames"
mkdir -p "$OUT" "$FRAMES"

if ! command -v vhs >/dev/null 2>&1; then
  echo "error: vhs not on PATH (need charmbracelet vhs + ttyd + ffmpeg)" >&2
  exit 1
fi
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "error: ffmpeg not on PATH" >&2
  exit 1
fi

# Ensure tapes resolve relative Output paths under demo root.
cd "$ROOT"

echo "==> smoke: demo scripts"
SPUR_DEMO_PAUSE=0 ./bin/demo-context-setup.sh >/dev/null
SPUR_DEMO_PAUSE=0 SPUR_DEMO_MODE=fixture ./bin/demo-external-tools.sh >/dev/null

echo "==> VHS: 01-context-setup"
vhs -q tapes/01-context-setup.tape

echo "==> VHS: 02-external-tools"
vhs -q tapes/02-external-tools.tape

echo "==> extract stills from GIFs (Pillow; more reliable than ffmpeg seek here)"
python3 - <<'PY'
from pathlib import Path
from PIL import Image

out = Path("out")
frames = out / "frames"
frames.mkdir(parents=True, exist_ok=True)

def grab(src: str, dst: str, idx: int) -> None:
    im = Image.open(out / src)
    n = getattr(im, "n_frames", 1)
    idx = min(max(0, idx), n - 1)
    im.seek(idx)
    im.convert("RGB").save(frames / dst)
    print(f"  {dst}: frame {idx}/{n}")

grab("01-context-setup.gif", "01-setup-mid.png", 120)
grab("01-context-setup.gif", "01-setup-late.png", 280)
grab("02-external-tools.gif", "02-knowledge-mid.png", 160)
grab("02-external-tools.gif", "02-read-late.png", 300)
grab("02-external-tools.gif", "02-cta-end.png", 420)

names = [
    "01-setup-mid.png",
    "02-knowledge-mid.png",
    "02-read-late.png",
    "02-cta-end.png",
]
imgs = [Image.open(frames / n) for n in names]
w, h = imgs[0].size
sheet = Image.new("RGB", (w * 2, h * 2))
sheet.paste(imgs[0], (0, 0))
sheet.paste(imgs[1], (w, 0))
sheet.paste(imgs[2], (0, h))
sheet.paste(imgs[3], (w, h))
sheet.save(out / "contact-sheet.png")
print("  contact-sheet.png")
PY

echo
echo "Artifacts:"
ls -la "$OUT"/*.{mp4,gif,png} 2>/dev/null || ls -la "$OUT"
ls -la "$FRAMES"
echo
echo "Next: ./generate-higgsfield.sh"
