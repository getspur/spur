# Journey: explore-browser-open (JOURNEYS.md)
# Source of truth: scripts/e2e/vhs/tapes/explore-browser-open.tape

Output out/04-explore-browser-open.mp4
Output out/04-explore-browser-open.gif

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 720
Set Padding 12
Set Margin 0
Set TypingSpeed 30ms
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
Wait+Screen@20s /Go to/
Hide
Type "Explore"
Enter
Sleep 300ms
Show
# shell-use journey waits on "never synced"; current chrome says "Esc dashboard"
# (golden vhs tape still uses "Esc back" — media follows the behavioral wait string)
Wait+Screen@20s /never synced/
Sleep 2s
