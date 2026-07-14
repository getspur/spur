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

# STORY: HOOK — a multi-task campaign is unmanageable when its state is scattered.
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below/
Sleep 2.5s

# STORY: ORIENTATION — Plans gathers campaign history into one control surface.
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan/
Sleep 2.5s

# STORY: ACTION — move selection to connect a campaign row to its summary.
Hide
Down
Show
Sleep 2.5s

# STORY: PROOF — Progress plus Work item/empty state makes the next decision legible.
Wait+Screen@10s /Progress|No plans found/
Wait+Screen@10s /Work [Ii]tem|No plans found|No plans match/
# STORY: RESOLUTION — campaign state is now one readable decision surface.
Sleep 4s
