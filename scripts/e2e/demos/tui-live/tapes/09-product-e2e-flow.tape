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

# STORY: HOOK — I need a specialist without losing the context already in flight.
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage|Type a task below/
Sleep 3s

# STORY: ORIENTATION — session history is the continuity layer, not disposable chrome.
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@15s /Sessions/
Wait+Screen@10s /TODAY/
Sleep 2.5s
# show list motion
Hide
Down
Down
Show
Sleep 2s
# resume the highlighted prior session — this is the context-continuity proof
Hide
Enter
Show
Sleep 2.5s
Wait+Screen@25s /Session ·/
Sleep 2.5s

# STORY: ACTION — source a specialist, pass the trust gate, and apply it locally.
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

# STORY: PROOF — the dispatch atom must expose agent=, model=, and effort= together.
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
# STORY: RESOLUTION — the right specialist is ready, and no send/spend occurred.
Sleep 4.5s
