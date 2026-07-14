# PROBLEM: submit_plan multi-agent loop is opaque — drive it from TUI
# Features: help, plan browser, lineage Agents j/k/Enter/detail tabs, activity
#
# Navigate mode: Esc first so j/k hit Agents tree (not INSERT composer).
# Tab focuses Agents panel ('1' would re-enter Compose).

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

# --- land ---
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 1.2s

# --- how do I drive this? ---
Hide
Type "?"
Show
Wait+Screen@12s /Dashboard/
Sleep 1.5s
Hide
Escape
Show
Sleep 600ms

# --- plan browser (submit_plan campaigns) ---
Hide
Ctrl+K
Show
Wait+Screen@12s /Go to/
Hide
Type "Plan"
Enter
Show
Wait+Screen@20s /Plan/
Sleep 800ms
Wait+Screen@10s /Progress/
Sleep 1.2s
Hide
Down
Down
Show
Sleep 1.5s
Hide
Escape
Show
Wait+Screen@15s /Lineage/
Sleep 800ms

# --- lineage: Esc Navigate → Tab Agents → j/k → Enter → detail tabs ---
Hide
Escape
Sleep 400ms
Tab
Show
Sleep 700ms
Hide
Type "j"
Type "j"
Show
Sleep 600ms
Hide
Enter
Show
Wait+Screen@15s /stream/
Sleep 1s
Hide
Type "l"
Show
Sleep 800ms
Hide
Type "l"
Show
Sleep 800ms
Hide
Escape
Sleep 400ms
Type "k"
Enter
Show
Sleep 1.2s
Hide
Escape
Sleep 400ms
Tab
Show
Sleep 1.2s
Wait+Screen@10s /Activity|Lineage/
Sleep 2s
