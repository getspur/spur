# PROBLEM: multi-task campaign progress is opaque
# Features: plan browser + progress/summary

Output out/11-problem-plan-progress.mp4
Output out/11-problem-plan-progress.gif

Set Shell bash
Set FontSize 14
Set Width 1440
Set Height 860
Set Padding 8
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
Sleep 1s
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan/
Sleep 1s
Wait+Screen@10s /Progress/
Sleep 2.5s
# Nudge selection for summary
Hide
Down
Show
Sleep 1.5s
