# TUI E2E Journeys

| Journey | User story | Side | Fixture | Wait strings | Owning file |
|---|---|---|---|---|---|
| `cold-launch` | As a new user without configured agents, I see the no-agents landing state and setup hint. | behavioral | `no-agents` | `No agents configured`; `SPUR`; `spur init`; `Quit spur?` | `scripts/e2e/shell-use/journeys/cold-launch.sh` |
| `help-overlay` | As a keyboard user, I can open help from the no-agents dashboard and see mode/navigation guidance. | behavioral | `no-agents` | `No agents configured`; `Dashboard — Modes`; `Dashboard — Navigation`; `Toggle verbose mode`; `Quit spur?` | `scripts/e2e/shell-use/journeys/help-overlay.sh` |
| `clean-quit` | As a user, I can request quit from the no-agents dashboard, confirm, and exit cleanly. | behavioral | `no-agents` | `No agents configured`; `Quit spur?` | `scripts/e2e/shell-use/journeys/clean-quit.sh` |
| `cold-launch` | As a new user without configured agents, the first screen visually matches the no-agents landing golden. | visual | `no-agents` | `No agents configured` | `scripts/e2e/vhs/tapes/cold-launch.tape` |
| `help-overlay` | As a keyboard user, the help overlay visually matches the dashboard help golden. | visual | `no-agents` | `No agents configured`; `Dashboard — Modes` | `scripts/e2e/vhs/tapes/help-overlay.tape` |
| `clean-quit` | As a user, the quit confirmation and shell-after-exit screens visually match their goldens. | visual | `no-agents` | `No agents configured`; `Quit spur\?`; `VHS_SPUR_EXITED status=0` | `scripts/e2e/vhs/tapes/clean-quit.tape` |
