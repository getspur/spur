# Long product E2E film (real project):
#   lineage → sessions switch → explore adopt → worker cascade
# Prefer `n` new-session for reliable attach; TypingSpeed 80ms for @worker cascade.

Output out/09-product-e2e-flow.mp4
Output out/09-product-e2e-flow.gif

Set Shell bash
Set FontSize 14
Set Width 1440
Set Height 860
Set Padding 8
Set Margin 0
Set TypingSpeed 80ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

# === beat 1: land ===
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 1.5s

# === beat 2: sessions picker (palette) + new session ===
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions/
Sleep 1s
# show list motion
Hide
Down
Down
Show
Sleep 800ms
# new session is the reliable attach path on busy machines
Hide
Type "n"
Show
Sleep 2s
Wait+Screen@25s /INSERT|Session|Lineage/
Sleep 1.2s

# === beat 3: explore adopt skill + agent ===
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
Sleep 1s
Hide
Type "/"
Type "accessibility"
Enter
Show
Sleep 700ms
Hide
Type " "
Show
Sleep 500ms
Hide
Tab
Show
Sleep 700ms
Hide
Type " "
Show
Sleep 500ms
Hide
Enter
Show
Wait+Screen@15s /Gate/
Sleep 1s
Hide
Type "c"
Show
Sleep 800ms
Hide
Enter
Show
Wait+Screen@25s /applied/
Sleep 2s
Hide
Type "m"
Show
Sleep 1.5s
Hide
Escape
Sleep 400ms
Escape
Show
Sleep 1s

# === beat 4: worker cascade (profile / model / effort) ===
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
Sleep 2s
Wait+Screen@25s /INSERT/
Sleep 1s
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
Sleep 1s
Hide
Tab
Show
Sleep 1.1s
Hide
Tab
Show
Sleep 1.1s
Hide
Tab
Show
Wait+Screen@20s /agent=/
Sleep 3s
