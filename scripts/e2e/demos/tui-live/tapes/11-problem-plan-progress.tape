# PROBLEM: multi-task campaign progress is opaque
# HOME: Session Detail → Plans
# STORY: HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION

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

# STORY: HOOK
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below|SPUR/
Sleep 2s

# STORY: ORIENTATION — Session Detail
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions|Start new session|No sessions yet|Failed/
Sleep 1.5s
Hide
Type "n"
Show
Sleep 2s
Wait+Screen@50s /Session ·|INSERT/
Sleep 2.5s

# STORY: ACTION — Plans from session
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan|Plans/
Sleep 2s
Wait+Screen@10s /Progress|No plans found/
Sleep 4s
Hide
Down
Show
Sleep 3.5s

# STORY: PROOF
Wait+Screen@10s /Progress|No plans found|Work/
Sleep 2s
# STORY: RESOLUTION
Sleep 2s
