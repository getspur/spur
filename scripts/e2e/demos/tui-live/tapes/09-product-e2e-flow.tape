# Long product E2E film — Session Detail home:
#   attach session → explore adopt → worker cascade in composer
# STORY: HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION

Output out/09-product-e2e-flow.mp4
Output out/09-product-e2e-flow.gif

Set Shell bash
Set FontSize 18
Set Width 2560
Set Height 1600
Set Padding 10
Set Margin 0
Set TypingSpeed 80ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

# STORY: HOOK
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below|SPUR/
Sleep 2.5s

# STORY: ORIENTATION — Session Detail (context continuity)
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions|Start new session|No sessions yet|Failed|TODAY/
Sleep 2s
# Prefer existing free session (Down past +New) to avoid "Failed to initialize brain"
Hide
Down
Show
Sleep 800ms
Hide
Enter
Show
Sleep 2.5s
Wait+Screen@25s /Session ·|INSERT|another window|Failed to initialize/
# If attach conflict or init failed, fall back to new session
Hide
Type "n"
Show
Sleep 2s
Wait+Screen@25s /Session ·|INSERT|Failed to initialize|Lineage/
Sleep 3s

# STORY: ACTION — explore adopt skill + agent
Hide
Escape
Escape
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Explore"
Enter
Show
Wait+Screen@20s /synced|Explore|catalog/
Sleep 2.5s
Hide
Type "/"
Type "accessibility"
Enter
Show
Sleep 2s
Hide
Type " "
Show
Sleep 1.5s
Hide
Tab
Show
Sleep 2s
Hide
Type " "
Show
Sleep 1.5s
Hide
Enter
Show
Wait+Screen@15s /Gate/
Sleep 2.5s
Hide
Type "c"
Show
Sleep 2s
Hide
Enter
Show
Wait+Screen@25s /applied/
Sleep 4s
Hide
Escape
Sleep 800ms
Escape
Show
Sleep 2s

# STORY: PROOF — worker cascade in Session Detail composer
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@12s /Sessions|Start new session|Failed/
Hide
Type "n"
Show
Sleep 2.5s
Wait+Screen@25s /Session ·|INSERT|Failed to initialize/
Sleep 2s
Hide
Backspace
Backspace
Backspace
Backspace
Backspace
Type "@worker:codex"
Show
Wait+Screen@20s /Mentions/
Sleep 2.5s
Hide
Tab
Show
Sleep 2s
Hide
Tab
Show
Sleep 2s
Hide
Tab
Show
Wait+Screen@20s /agent=/
Wait+Screen@10s /model=/
Wait+Screen@10s /effort=/
Sleep 4.5s

# STORY: RESOLUTION — still in Session Detail
Wait+Screen@10s /Session ·|INSERT|agent=/
Sleep 3s
