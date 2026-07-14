# TUI live project — UAT + demo capture

Capture **SPUR TUI on a real project** (this monorepo by default, or any path
with `.spur/`). Unlike Arc A fixture demos (`demos/tui-journeys/`), this pack:

| | Fixture Arc A | Live project (this pack) |
|--|---------------|---------------------------|
| Workspace | empty temp + `no-agents` | real repo + real `.spur/` |
| Agents | none / fake stub | your configured brains/workers |
| Sessions | empty or seeded fixture | real session history / lineage |
| Safety | fully isolated | **navigation-only** by default |
| Cleanup | temp dir wiped | **never** deletes project files |

## What you will see

Storyboard is **navigation-only** (no typed prompts, no dispatches):

| # | Journey | Keys | Wait anchors |
|---|---------|------|----------------|
| 1 | `lineage-dashboard` | launch `--dashboard` | `Lineage`, `Activity`, `INSERT` |
| 2 | `sessions-picker` | `s` | `Sessions`, `TODAY` |
| 3 | `palette-open` | `Ctrl+K` | `Go to`, `esc dismiss` |

On a busy SPUR monorepo this shows live **Lineage** (running brains/execs),
**Activity** log, real **session titles**, and a palette filled with workers /
sessions — i.e. practical TUI use, not the empty first-run landing.

## Quick start

```bash
# Optional: pin binary
export SPUR_BIN="$(command -v spur)"
# Optional: another checkout
# export SPUR_DEMO_PROJECT=/path/to/your/repo

cd scripts/e2e/demos/tui-live

./uat.sh --list
./uat.sh --mode uat        # shell-use behavioral checks on live project
./uat.sh --mode capture    # VHS → out/*.mp4 *.gif
./uat.sh                   # UAT then capture
```

## Safety / policy

1. **Default landing is `tui --dashboard`** — avoids auto-resuming a prior chat
   that might immediately reconnect a brain turn. Live lineage may still show
   already-running project sessions (that is intentional for realism).
2. **Tapes and UAT never type a user prompt** and never press Enter on the
   composer. Adding “send a message” beats requires an explicit new journey and
   cost awareness.
3. **No fixture isolation, no `rm -rf` of the project.** The live launcher only
   `cd`s into `SPUR_DEMO_PROJECT` and runs `spur`.
4. **Not CI-default.** Media and live UAT are opt-in (machine + secrets +
   running agents vary).
5. Quit dialogs differ from empty fixtures when brains are attached (“agent
   subprocess will be terminated”). Live UAT accepts either quit chrome.

## Override TUI args

```bash
# Attach sessions picker on launch
SPUR_DEMO_TUI_ARGS='tui --sessions' ./bin/run-spur-tui-live.sh

# Force brand-new dashboard decision
SPUR_DEMO_TUI_ARGS='tui --new' ./bin/run-spur-tui-live.sh
```

## Related

- Fixture / first-run demos: `scripts/e2e/demos/tui-journeys/`
- Journey catalog: `scripts/e2e/JOURNEYS.md`
- Shared e2e launcher (fixtures only): `scripts/e2e/vhs/bin/run-spur-tui.sh`
