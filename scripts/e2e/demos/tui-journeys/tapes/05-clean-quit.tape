# Journey: clean-quit (JOURNEYS.md)
# Source of truth: scripts/e2e/vhs/tapes/clean-quit.tape

Output out/05-clean-quit.mp4
Output out/05-clean-quit.gif

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
Ctrl+C
Show
Wait+Screen@20s /Quit spur\?/
Sleep 600ms
Hide
Type "y"
Enter
Show
Wait+Screen@20s /VHS_SPUR_EXITED status=0/
Sleep 1s
