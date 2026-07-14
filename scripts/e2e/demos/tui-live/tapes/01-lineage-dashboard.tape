# Live project: lineage + activity (real agents / sessions if running)
# Navigation only — no prompts typed.

Output out/01-lineage-dashboard.mp4
Output out/01-lineage-dashboard.gif

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
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@25s /Lineage/
Sleep 3s
