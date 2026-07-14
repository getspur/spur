# Long product E2E film (real project):
#   lineage → sessions switch → explore adopt → worker cascade
# Prefer `n` new-session for reliable attach; TypingSpeed 80ms for @worker cascade.
# Story pacing: proof dwells ≥2–4s (agent= model= effort= hold is the aha).

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

# === beat 1: land — "I need a specialist without losing context" ===
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 3s

# === beat 2: sessions picker (context continuity) ===
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions/
Sleep 2.5s
# show list motion
Hide
Down
Down
Show
Sleep 2s
# new session is the reliable attach path on busy machines
Hide
Type "n"
Show
Sleep 2.5s
Wait+Screen@25s /INSERT|Session|Lineage/
Sleep 2.5s

# === beat 3: explore adopt skill + agent (specialist supply) ===
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
Wait+Screen@20s /synced/
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
Type "m"
Show
Sleep 2.5s
Hide
Escape
Sleep 800ms
Escape
Show
Sleep 2s

# === beat 4: worker cascade proof (agent= model= effort=) ===
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@12s /Sessions/
Hide
Type "n"
Show
Sleep 2.5s
Wait+Screen@25s /INSERT/
Sleep 2s
# Clear any accidental chars, then cascade (80ms TypingSpeed → not paste-burst)
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
