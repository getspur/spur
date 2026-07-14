# PROBLEM: multi-agent work is opaque / keybindings unknown
# Features: lineage, help, palette, Agents tree + detail tabs

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

Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 1.2s
# Help
Hide
Type "?"
Show
Wait+Screen@15s /Dashboard/
Sleep 1.5s
Hide
Escape
Show
Sleep 600ms
# Palette
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Sleep 1.2s
Hide
Escape
Show
Wait+Screen@12s /Lineage/
Sleep 600ms
# Navigate mode → Agents → focus node → detail
Hide
Escape
Sleep 350ms
Tab
Show
Sleep 700ms
Hide
Type "j"
Type "j"
Show
Sleep 500ms
Hide
Enter
Show
Wait+Screen@12s /stream/
Sleep 1s
Hide
Type "l"
Show
Sleep 800ms
Hide
Type "l"
Show
Sleep 1.2s
