# PROBLEM: multi-agent work is opaque / keybindings unknown
# Features: lineage, activity, help, palette

Output out/10-problem-ops-visibility.mp4
Output out/10-problem-ops-visibility.gif

Set Shell bash
Set FontSize 14
Set Width 1440
Set Height 860
Set Padding 8
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
Sleep 1.5s
# Help — how do I drive this?
Hide
Type "?"
Show
Wait+Screen@15s /Dashboard/
Sleep 2s
Hide
Escape
Show
Sleep 800ms
# Palette hub
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Sleep 2s
Hide
Escape
Show
Wait+Screen@12s /Lineage/
Sleep 1.5s
