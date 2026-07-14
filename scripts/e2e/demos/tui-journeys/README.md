# TUI journeys — UAT + demo capture

Dual-purpose runner for **Arc A** (first-run / empty workspace). Story points
come only from [`JOURNEYS.md`](../../JOURNEYS.md); this directory packages them
for:

| Mode | Tool | Purpose |
|------|------|---------|
| **UAT** | `shell-use` journeys | Behavioral feature acceptance |
| **capture** | VHS tapes → `out/*.mp4` `out/*.gif` | Demo / marketing assets later |

Do **not** invent new product flows here. Promote a journey from
`scripts/e2e/shell-use/journeys/` (and optionally `scripts/e2e/vhs/tapes/`) by
adding a row to `journeys.conf` and a media tape that reuses the same keys and
wait strings.

## Layout

```text
journeys.conf          # name | fixture | shell-use script | vhs stem
uat.sh                 # --mode uat|capture|all
render.sh              # VHS media only
bin/run-spur-tui.sh    # → scripts/e2e/vhs/bin/run-spur-tui.sh
tapes/                 # marketing packaging of existing e2e story points
out/                   # gitignored mp4/gif
```

## Arc A storyboard

| # | Journey | Fixture | UAT owner | Media tape |
|---|---------|---------|-----------|------------|
| 1 | `cold-launch` | `no-agents` | `shell-use/journeys/cold-launch.sh` | `01-cold-launch` |
| 2 | `help-overlay` | `no-agents` | `…/help-overlay.sh` | `02-help-overlay` |
| 3 | `palette-open` | `no-agents` | `…/palette-open.sh` | `03-palette-open` |
| 4 | `explore-browser-open` | `no-agents` | `…/explore-browser-open.sh` | `04-explore-browser-open` |
| 5 | `clean-quit` | `no-agents` | `…/clean-quit.sh` | `05-clean-quit` |

Shared launcher env (see `scripts/e2e/vhs/bin/run-spur-tui.sh`):

- `SPUR_E2E_FIXTURE` — fixture name (default `no-agents`)
- `SPUR_E2E_TUI_ARGS` — e.g. `--dashboard` (worker Arc B)
- `SPUR_E2E_SEED_CATALOG=1` — seed fake codex catalog (worker Arc B)

## Quick start

```bash
# From repo root — need a local spur binary
SPUR_REMOTE=0 scripts/spur-cargo build -p spur-cli

cd scripts/e2e/demos/tui-journeys

# List Arc A rows
./uat.sh --list

# Feature UAT only (shell-use)
./uat.sh --mode uat

# Demo media only (vhs + ttyd + ffmpeg; pin check via scripts/e2e/vhs/check-vhs.sh)
./uat.sh --mode capture
# or: ./render.sh

# Full path: UAT then capture (skips capture if UAT fails)
./uat.sh
```

## Policy

- **Not** wired into `scripts/e2e/run-all.sh` / CI visual goldens by default.
  Media is heavy and intentionally opt-in.
- Golden text snapshots stay in `scripts/e2e/vhs/` (`run-vhs-suite.sh`).
- Demo tapes share capture geometry with `tui-live` via
  `scripts/e2e/demos/geometry.env` (Mac Air M2 / wide iTerm defaults:
  **2560×1600**, FontSize **18**, shell-use PTY **200×50** — not 720p).
  Re-stamp with `../apply-geometry.sh` after editing geometry.env.
  Theme may still be Catppuccin while wait strings match the journey owners.
- When a golden tape exists, treat it as the key-sequence source of truth;
  when only shell-use exists (`palette-open`), the journey script is the source.

## Arc B (later)

Worker story points (`worker-mention-cascade`, `session-detail-reply`, …)
reuse the same pattern with:

```bash
export SPUR_E2E_FIXTURE=worker-mentions
export SPUR_E2E_TUI_ARGS=--dashboard
export SPUR_E2E_SEED_CATALOG=1
```

Add rows to `journeys.conf` and tapes; do not fork a second fixture tree.

## Real project (practical demos)

For TUI against a real `.spur/` (lineage, session history, live workers), use:

```bash
cd scripts/e2e/demos/tui-live
./uat.sh --mode capture
```

See that pack’s README — navigation-only, no fixture isolation.
