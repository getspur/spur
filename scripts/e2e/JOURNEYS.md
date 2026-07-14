# TUI E2E Journeys

Journey rows are the single story catalog for TUI e2e. Consumers:

| Consumer | Path | Role |
|---|---|---|
| Behavioral UAT | `scripts/e2e/shell-use/journeys/` | shell-use asserts (feature acceptance) |
| Visual goldens | `scripts/e2e/vhs/tapes/` | text golden regression |
| Demo + Arc A UAT package | `scripts/e2e/demos/tui-journeys/` | shell-use UAT + VHS mp4/gif for demos |

Arc A dual runner (first-run / `no-agents`):

```bash
cd scripts/e2e/demos/tui-journeys
./uat.sh --list
./uat.sh --mode uat       # shell-use only
./uat.sh --mode capture   # VHS media → out/
./uat.sh                  # UAT then capture
```

Live-project dual runner (real `.spur/`, navigation-only):

```bash
cd scripts/e2e/demos/tui-live
# optional: SPUR_DEMO_PROJECT=/path/to/repo
./uat.sh --mode uat       # shell-use on real project
./uat.sh --mode capture   # VHS media of lineage / sessions / palette
```

Do not invent demo beats outside this table — add a journey row first.

| Journey | User story | Side | Fixture | Wait strings | Owning file |
|---|---|---|---|---|---|
| `cold-launch` | As a new user without configured agents, I see the no-agents landing state and setup hint. | behavioral | `no-agents` | `No agents configured`; `SPUR`; `spur init`; `Quit spur?` | `scripts/e2e/shell-use/journeys/cold-launch.sh` |
| `help-overlay` | As a keyboard user, I can open help from the no-agents dashboard and see mode/navigation guidance. | behavioral | `no-agents` | `No agents configured`; `Dashboard — Modes`; `Dashboard — Navigation`; `Toggle verbose mode`; `Quit spur?` | `scripts/e2e/shell-use/journeys/help-overlay.sh` |
| `clean-quit` | As a user, I can request quit from the no-agents dashboard, confirm, and exit cleanly. | behavioral | `no-agents` | `No agents configured`; `Quit spur?` | `scripts/e2e/shell-use/journeys/clean-quit.sh` |
| `cold-launch` | As a new user without configured agents, the first screen visually matches the no-agents landing golden. | visual | `no-agents` | `No agents configured` | `scripts/e2e/vhs/tapes/cold-launch.tape` |
| `help-overlay` | As a keyboard user, the help overlay visually matches the dashboard help golden. | visual | `no-agents` | `No agents configured`; `Dashboard — Modes` | `scripts/e2e/vhs/tapes/help-overlay.tape` |
| `clean-quit` | As a user, the quit confirmation and shell-after-exit screens visually match their goldens. | visual | `no-agents` | `No agents configured`; `Quit spur\?`; `VHS_SPUR_EXITED status=0` | `scripts/e2e/vhs/tapes/clean-quit.tape` |
| `worker-mention-cascade` | As a user with a configured worker, typing `@worker:codex` walks the cascading worker → agent → model → effort picker and composes a fully enriched mention atom. | behavioral | `worker-mentions` | `Type a task below`; `rust-reviewer`; `GPT-5 Codex`; `e2e deep reasoning`; `model=gpt-5-codex`; `Quit spur?` | `scripts/e2e/shell-use/journeys/worker-mention-cascade.sh` |
| `worker-mention-send` | As a user, sending a message with an enriched worker mention prepends the `[UI hint]` worker-preference block and lands the message in a session against the fake ACP worker. | behavioral | `worker-mentions` | `Type a task below`; `model=gpt-5-codex`; `User-suggested workers`; `agent=rust-reviewer, model=gpt-5-codex, effort=high`; `parser bug`; `Quit spur?` | `scripts/e2e/shell-use/journeys/worker-mention-send.sh` |
| `worker-mention-slots` | As a user, I can double-comma-skip the agent slot, see an ambiguous slot prefix stay open as a filter, auto-advance on a unique refinement, and delete a composed atom with one Backspace. | behavioral | `worker-mentions` | `Type a task below`; `GPT-5 Codex`; `codex model=gpt-5-codex`; `Enter to submit`; `rust-tester`; `rust-reviewer`; `agent=rust-reviewer`; `Quit spur?` | `scripts/e2e/shell-use/journeys/worker-mention-slots.sh` |
| `worker-mention-probe` | As a user mentioning a worker with no cached catalog, the popup title shows `fetching codex models…`, the mention degrades to agent-only, and the background probe against the live fake worker unlocks the model/effort slots for later mentions. | behavioral | `worker-mentions` | `Type a task below`; `rust-reviewer`; `fetching codex models`; `agent=rust-reviewer`; `Enter to submit`; `GPT-5 Codex`; `model=gpt-5-codex`; `Quit spur?` | `scripts/e2e/shell-use/journeys/worker-mention-probe.sh` |
| `agent-config-open` | As a user with a configured worker, I can open the codex agent configuration browser and see the worker settings. | behavioral | `worker-mentions` | `Type a task below`; `Settings: codex`; `codex`; `skip_permissions`; `Esc back`; `Quit spur?` | `scripts/e2e/shell-use/journeys/agent-config-open.sh` |
| `insights-open` | As a user, I can open Insights from the no-agents dashboard and return without disrupting the dashboard. | behavioral | `no-agents` | `No agents configured`; `Analytics feature disabled`; `Refreshing...`; `Quit spur?` | `scripts/e2e/shell-use/journeys/insights-open.sh` |
| `interrupt-quit-prompt` | As a user with an in-flight fake-worker turn, pressing Ctrl+C opens the quit prompt, declining it keeps the session alive, and the reply still renders. | behavioral | `worker-mentions` | `Type a task below`; `rust-reviewer`; `GPT-5 Codex`; `e2e deep reasoning`; `model=gpt-5-codex`; `Quit spur?`; `e2e canned reply` | `scripts/e2e/shell-use/journeys/interrupt-quit-prompt.sh` |
| `issue-browser-open` | As a user, I can open the issue browser from the no-agents dashboard and see its not-configured state. | behavioral | `no-agents` | `No agents configured`; `Go to`; `No issue tracker configured`; `Quit spur?` | `scripts/e2e/shell-use/journeys/issue-browser-open.sh` |
| `palette-open` | As a keyboard user, I can open the command palette from the no-agents dashboard and dismiss it cleanly. | behavioral | `no-agents` | `No agents configured`; `esc dismiss`; `Quit spur?` | `scripts/e2e/shell-use/journeys/palette-open.sh` |
| `plan-browser-open` | As a user, I can open the plan browser from the no-agents dashboard and see the brain-session selection empty state. | behavioral | `no-agents` | `No agents configured`; `Go to`; `Select a brain session first (S)`; `Quit spur?` | `scripts/e2e/shell-use/journeys/plan-browser-open.sh` |
| `resize` | As a user, resizing the terminal while the no-agents dashboard is open keeps the dashboard rendered and responsive. | behavioral | `no-agents` | `No agents configured`; `Quit spur?` | `scripts/e2e/shell-use/journeys/resize.sh` |
| `session-detail-reply` | As a user, after sending a message to the fake worker, I can see the canned agent reply render in the session transcript. | behavioral | `worker-mentions` | `Type a task below`; `rust-reviewer`; `GPT-5 Codex`; `e2e deep reasoning`; `model=gpt-5-codex`; `e2e canned reply from fake worker`; `Quit spur?` | `scripts/e2e/shell-use/journeys/session-detail-reply.sh` |
| `session-picker-open` | As a user, I can open the session picker from the no-agents dashboard and return to the dashboard cleanly. | behavioral | `no-agents` | `No agents configured`; `Go to`; `Sessions`; `Quit spur?` | `scripts/e2e/shell-use/journeys/session-picker-open.sh` |
| `loop-browser-open` | As a user, I can open the loop browser from the no-agents dashboard and see its empty state. | behavioral | `no-agents` | `No agents configured`; `Go to`; `No loops found.`; `Quit spur?` | `scripts/e2e/shell-use/journeys/loop-browser-open.sh` |
| `session-picker-populated` | As a user with session history, I can open the session picker and see a seeded session from today. | behavioral | `worker-mentions` | `Type a task below`; `Go to`; `Sessions`; `TODAY`; `e2e picker seeded prompt`; `Quit spur?` | `scripts/e2e/shell-use/journeys/session-picker-populated.sh` |
| `explore-browser-open` | As a user, I can open the explore browser from the no-agents dashboard and see the never-synced catalog banner. | behavioral | `no-agents` | `No agents configured`; `Go to`; `never synced`; `Quit spur?` | `scripts/e2e/shell-use/journeys/explore-browser-open.sh` |
| `explore-browser-open` | As a user, opening the explore browser via the palette visually matches the never-synced browse-stage golden. | visual | `no-agents` | `No agents configured`; `Go to`; `never synced` | `scripts/e2e/vhs/tapes/explore-browser-open.tape` |
| `cold-launch` | Arc A demo media: no-agents landing (marketing mp4/gif). | demo | `no-agents` | `No agents configured` | `scripts/e2e/demos/tui-journeys/tapes/01-cold-launch.tape` |
| `help-overlay` | Arc A demo media: dashboard help overlay. | demo | `no-agents` | `No agents configured`; `Dashboard — Modes` | `scripts/e2e/demos/tui-journeys/tapes/02-help-overlay.tape` |
| `palette-open` | Arc A demo media: command palette open. | demo | `no-agents` | `No agents configured`; `esc dismiss` | `scripts/e2e/demos/tui-journeys/tapes/03-palette-open.tape` |
| `explore-browser-open` | Arc A demo media: explore browser never-synced. | demo | `no-agents` | `No agents configured`; `Go to`; `never synced` | `scripts/e2e/demos/tui-journeys/tapes/04-explore-browser-open.tape` |
| `clean-quit` | Arc A demo media: quit confirm and clean exit. | demo | `no-agents` | `No agents configured`; `Quit spur\?`; `VHS_SPUR_EXITED status=0` | `scripts/e2e/demos/tui-journeys/tapes/05-clean-quit.tape` |
| `lineage-dashboard` | Live project: open TUI and show lineage/activity surface. | demo-live | real project | `Lineage`; `Activity`; `INSERT` | `scripts/e2e/demos/tui-live/tapes/01-lineage-dashboard.tape` |
| `sessions-picker` | Live project: open sessions picker with real history. | demo-live | real project | `Lineage`; `Sessions`; `TODAY` | `scripts/e2e/demos/tui-live/tapes/02-sessions-picker.tape` |
| `palette-open` | Live project: command palette with real sessions/workers. | demo-live | real project | `Lineage`; `Go to`; `esc dismiss` | `scripts/e2e/demos/tui-live/tapes/03-palette-open.tape` |
| `session-resume` | Live project: resume a session and load transcript history. | demo-live | real project | `Sessions`; `Session ·`; `INSERT` | `scripts/e2e/demos/tui-live/tapes/04-session-resume.tape` |
| `explore-browser` | Live project: explore synced ecosystem catalog (skills). | demo-live | real project | `synced`; `catalog`; `Skills`; `Sources` | `scripts/e2e/demos/tui-live/tapes/05-explore-browser.tape` |
| `explore-agents-tab` | Live project: explore Agents tab after skills. | demo-live | real project | `synced`; `Agents` | `scripts/e2e/demos/tui-live/tapes/06-explore-agents-tab.tape` |
| `composer-draft` | Live project: type composer draft without sending. | demo-live | real project | `INSERT`; `draft only` | `scripts/e2e/demos/tui-live/tapes/07-composer-draft.tape` |
| `agent-send` | Live project: opt-in minimal brain turn (real spend). | demo-live | real project | `YOU`; `THINK`/`ok`; `Session ·` | `scripts/e2e/demos/tui-live/tapes/08-agent-send.tape` |
| `product-e2e-flow` | Problem: need specialist+model+effort without losing context. Sessions → explore adopt → `@worker` cascade; optional send. | demo-live | real project | `applied`; `Mentions`; `agent=`; `model=`; `effort=` | `scripts/e2e/demos/tui-live/tapes/09-product-e2e-flow.tape` |
| `problem-ops-visibility` | Problem: multi-agent work is opaque. Lineage/Activity + Help + Palette hub. | demo-live | real project | `Lineage`; `Activity`; `Dashboard`; `Go to` | `scripts/e2e/demos/tui-live/tapes/10-problem-ops-visibility.tape` |
| `problem-plan-progress` | Problem: campaign progress opaque. Plan browser Progress + summary. | demo-live | real project | `Plan`; `Progress`; `awaiting`/`complete`; `Work item` | `scripts/e2e/demos/tui-live/tapes/11-problem-plan-progress.tape` |
| `problem-backlog-triage` | Problem: backlog firehose. Issues P0 open list + detail. | demo-live | real project | `Issues`; `P0`; `open`; `bd-`; `status:`/`priority:` | `scripts/e2e/demos/tui-live/tapes/12-problem-backlog-triage.tape` |
