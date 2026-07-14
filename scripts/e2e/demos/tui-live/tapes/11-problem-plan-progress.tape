# PROBLEM: multi-task campaign progress is opaque
# Features: plan browser + progress/summary
# Story pacing: hold Progress / Work item as the aha

Output out/11-problem-plan-progress.mp4
Output out/11-problem-plan-progress.gif

Set Shell bash
Set FontSize 18
Set Width 2560
Set Height 1600
Set Padding 10
Set Margin 0
Set TypingSpeed 40ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 2.5s
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan/
Sleep 2s
Wait+Screen@10s /Progress/
Sleep 4s
# Nudge selection for summary
Hide
Down
Show
Sleep 3.5s
