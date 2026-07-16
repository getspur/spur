# Long product E2E film — Session Detail home:
#   n=new session (reliable under VHS) → explore adopt → @worker cascade
# STORY: HOOK → ORIENTATION → ACTION → PROOF → RESOLUTION
#
# Attach path (VHS-hardened):
#   Go to Sessions → Type n (new) → Type y (unsent-draft confirm if shown)
#   Wait Session · / INSERT
# Prefer n over resuming held/draft rows — VHS cannot soft-branch on dialogs.

Output out/09-product-e2e-flow.mp4
Output out/09-product-e2e-flow.gif

Set Shell bash
Set FontSize 18
Set Width 2560
Set Height 1600
Set Padding 10
Set Margin 0
Set TypingSpeed 90ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

# STORY: HOOK
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@35s /Lineage|Type a task below|SPUR|Sessions/
Sleep 2s

# STORY: ORIENTATION — new session (reliable attach)
Hide
Ctrl+K
Show
Wait+Screen@15s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@20s /Sessions|Start new session|TODAY/
Sleep 2s
Hide
Type "n"
Show
Sleep 1.2s
# unsent draft confirm (no-op if dialog absent — y is harmless on empty search?)
# If focused on Search, y might type into search — press Esc first to clear
Hide
Escape
Show
Sleep 400ms
# Re-open sessions if Esc dismissed
Hide
Ctrl+K
Show
Wait+Screen@10s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@12s /Sessions/
Sleep 1s
Hide
Type "n"
Show
Sleep 1s
Hide
Type "y"
Show
Sleep 4s
Wait+Screen@50s /Session ·|INSERT/
Sleep 3s

# STORY: ACTION — explore adopt
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
Wait+Screen@25s /synced|Explore|catalog|Skills/
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
Wait+Screen@20s /Gate/
Sleep 2.5s
Hide
Type "c"
Show
Sleep 2s
Hide
Enter
Show
Wait+Screen@30s /applied/
Sleep 4s
Hide
Escape
Sleep 700ms
Escape
Show
Sleep 2s

# STORY: PROOF — new clean session for cascade
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions|Start new session/
Sleep 1.5s
Hide
Type "n"
Show
Sleep 1s
Hide
Type "y"
Show
Sleep 4s
Wait+Screen@50s /Session ·|INSERT/
Sleep 2.5s

# Cascade
Hide
Backspace
Backspace
Backspace
Backspace
Backspace
Backspace
Type "@worker:codex"
Show
Wait+Screen@30s /Mentions/
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
Wait+Screen@25s /agent=/
Wait+Screen@12s /model=/
Wait+Screen@12s /effort=/
Sleep 4.5s

# STORY: RESOLUTION
Wait+Screen@12s /Session ·|INSERT|agent=/
Sleep 3s
