# PROBLEM: multi-agent work is opaque / keybindings unknown
# Features: lineage, help, palette, Agents tree + detail tabs
# Story pacing: land → help → palette → Agents → stream (proof dwells ≥2–3.5s)

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

# STORY: HOOK — multi-agent work is dangerous when I cannot see what is running.
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 3s

# STORY: ORIENTATION — Help teaches the controls before the operator changes state.
Hide
Type "?"
Show
Wait+Screen@15s /Dashboard/
Sleep 3s
Hide
Escape
Show
Sleep 1.5s

# STORY: ACTION — Go to finds the surface; Esc → Tab gives the Agents tree control.
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Sleep 2.5s
Hide
Escape
Show
Wait+Screen@12s /Lineage/
Sleep 2s

# Navigate → Agents → tree hop
Hide
Escape
Sleep 600ms
Tab
Show
Sleep 2s
Hide
Type "j"
Type "j"
Show
Sleep 2s
Hide
Enter
Show
# STORY: PROOF — worker output and detail tabs replace guesswork with evidence.
Wait+Screen@12s /stream/
Sleep 3.5s
Hide
Type "l"
Show
Sleep 2s
Hide
Type "l"
Show
Sleep 2.5s

# STORY: RESOLUTION — return to Activity: the operator now has a live timeline.
Hide
Escape
Sleep 800ms
Tab
Show
Wait+Screen@12s /Activity/
Sleep 3.5s
