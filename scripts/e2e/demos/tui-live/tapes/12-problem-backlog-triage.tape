# PROBLEM: backlog firehose — what is P0 open work?
# Features: issues list + detail
# Story pacing: list → P0 → detail hold

Output out/12-problem-backlog-triage.mp4
Output out/12-problem-backlog-triage.gif

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
Type "Issues"
Enter
Show
Wait+Screen@20s /Issues/
Sleep 2s
Wait+Screen@10s /P0/
Sleep 3s
Hide
Enter
Show
Wait+Screen@12s /status:|priority:|bd-/
Sleep 4s
