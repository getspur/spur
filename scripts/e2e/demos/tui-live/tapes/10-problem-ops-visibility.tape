# PROBLEM: multi-agent work is opaque from the session I work in
# HOME: Session Detail (composer + ReAct + workers)
# STORY: HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION

Output out/10-problem-ops-visibility.mp4
Output out/10-problem-ops-visibility.gif

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

# STORY: HOOK — cold start (dashboard is only the safe entry)
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below|SPUR/
Sleep 2s

# STORY: ORIENTATION — enter Session Detail (operator home)
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
# Prefer new session for reliable attach on busy machines
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
Sleep 3s
Hide
Escape
Show
Sleep 1.5s

# STORY: ACTION — workers panel (Alt+d)
Hide
Alt+d
Show
Sleep 2.5s

# STORY: ACTION — Go to hub from session
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Sleep 2.5s
Hide
Escape
Show
Sleep 2s

# STORY: ACTION — optional lineage: select EXEC worker (not BRAIN), stream + task
Hide
Escape
Sleep 600ms
Show
Sleep 1s
Hide
Escape
Sleep 400ms
Tab
Show
Sleep 2s
# skip BRAIN root → first worker child
Hide
Type "j"
Show
Sleep 1.2s
Hide
Type "j"
Show
Sleep 1.5s
Hide
Enter
Show
Wait+Screen@15s /stream/
# hold stream panel while content streams / loads (VHS: no Ctrl+digit)
Sleep 4s
# cycle tabs: stream → artifacts → attempts → task (three l presses)
Hide
Type "l"
Show
Sleep 1.2s
Hide
Type "l"
Show
Sleep 1.2s
Hide
Type "l"
Show
Wait+Screen@12s /task/
Sleep 4.5s

# STORY: PROOF — worker detail held
Sleep 2s
# STORY: RESOLUTION
Hide
Escape
Show
Sleep 2s
