# Journey: help-overlay (JOURNEYS.md)
# Source of truth: scripts/e2e/vhs/tapes/help-overlay.tape + shell-use/journeys/help-overlay.sh

Output out/02-help-overlay.mp4
Output out/02-help-overlay.gif

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
Sleep 800ms
Hide
Type "?"
Show
Wait+Screen@20s /Dashboard — Modes/
Sleep 2s
