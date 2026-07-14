# Journey: palette-open (JOURNEYS.md)
# Source of truth: scripts/e2e/shell-use/journeys/palette-open.sh
# (no golden visual tape yet — demo media is the first VHS packaging)

Output out/03-palette-open.mp4
Output out/03-palette-open.gif

Set Shell bash
Set FontSize 18
Set Width 2560
Set Height 1600
Set Padding 10
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
Ctrl+K
Show
Wait+Screen@20s /esc dismiss/
Sleep 2s
