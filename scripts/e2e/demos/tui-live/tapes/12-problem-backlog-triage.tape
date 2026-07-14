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

# STORY: HOOK — the backlog firehose hides the issue that needs action now.
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below/
Sleep 2.5s

# STORY: ORIENTATION — Issues replaces noise with a priority/status surface.
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

# STORY: ACTION — isolate open P0 work, then open the issue decision context.
Wait+Screen@10s /P0/
Wait+Screen@10s /open/
Sleep 3s
Hide
Enter
Show
# STORY: PROOF — identity, status, and priority explain why this item is urgent.
Wait+Screen@12s /status: open/
Wait+Screen@12s /priority: P0/
# STORY: RESOLUTION — the fire is isolated without leaving the control plane.
Sleep 4s
