# PROBLEM: submit_plan loop is opaque — drive it from Session Detail
# HOME: Session Detail (YOU/THINK/DELEGATE + workers + Alt+p)
# STORY: HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION
# Navigate: session is primary; Plans is a hub, not the home.

Output out/13-problem-plan-loop-drive.mp4
Output out/13-problem-plan-loop-drive.gif

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

# STORY: ORIENTATION — Session Detail home
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions|Start new session|No sessions yet|Failed/
Sleep 2s
Hide
Type "n"
Show
Sleep 2.5s
Wait+Screen@25s /Session ·|INSERT|Failed to initialize/
Sleep 3s

# STORY: ACTION — session help
Hide
Escape
Type "?"
Show
Sleep 2.5s
Hide
Escape
Show
Sleep 1s

# STORY: ACTION — workers panel
Hide
Alt+d
Show
Sleep 2.5s

# STORY: ACTION — Alt+p plan inspector (soft if no plan)
Hide
Alt+p
Show
Sleep 3s
Hide
Escape
Show
Sleep 1.5s

# STORY: ACTION — Plans hub from session
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan|Plans|Progress|No plans found/
Sleep 3.5s
Hide
Escape
Show
Sleep 2s

# STORY: PROOF — back home Session Detail
Wait+Screen@15s /Session ·|INSERT|Lineage/
Sleep 2s
# STORY: RESOLUTION
Sleep 2s
