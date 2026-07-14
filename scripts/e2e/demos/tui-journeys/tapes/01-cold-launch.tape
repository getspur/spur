# Journey: cold-launch (JOURNEYS.md) — no-agents landing
# Source of truth for keys/waits: scripts/e2e/vhs/tapes/cold-launch.tape
# Media packaging for demos; not part of the golden visual suite.

Output out/01-cold-launch.mp4
Output out/01-cold-launch.gif

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 720
Set Padding 12
Set Margin 0
Set TypingSpeed 0ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "export SPUR_E2E_FIXTURE=no-agents"
Enter
Type "./bin/run-spur-tui.sh"
Enter
Show
Wait+Screen@20s /No agents configured/
Sleep 2s
