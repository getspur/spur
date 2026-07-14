# PROBLEM: submit_plan multi-agent loop is opaque — drive it from TUI
# Features: help, plan browser, lineage Agents j/k/Enter/detail tabs, activity
#
# Story arc: land → drive keys → plan campaigns → BRAIN/EXEC tree → stream → tabs → Activity
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

# --- land: control plane, not a button ---
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 3s

# --- how do I drive this? ---
Hide
Type "?"
Show
Wait+Screen@12s /Dashboard/
Sleep 3s
Hide
Escape
Show
Sleep 1.5s

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
Sleep 2s
Wait+Screen@10s /Progress/
Sleep 3.5s
Hide
Down
Down
Show
Sleep 3s
Hide
Escape
Show
Wait+Screen@15s /Lineage/
Sleep 2s

# --- lineage: Esc Navigate → Tab Agents ---
Hide
Escape
Sleep 600ms
Tab
Show
Sleep 2.5s

# --- hop tree (prefer BRAIN then EXEC rows when present) ---
Hide
Type "j"
Show
Sleep 2s
Hide
Type "j"
Show
Sleep 2s

# --- open node detail: stream is the aha ---
Hide
Enter
Show
Wait+Screen@15s /stream/
Sleep 3.5s

# --- cycle detail tabs: attempts → task/review ---
Hide
Type "l"
Show
Sleep 2s
Hide
Type "l"
Show
Sleep 2s

# --- second node (brain ↔ worker hop) ---
Hide
Escape
Sleep 800ms
Type "k"
Enter
Show
Sleep 3s

# --- Activity log: auto-loop visibility ---
Hide
Escape
Sleep 600ms
Tab
Show
Sleep 2.5s
Wait+Screen@10s /Activity|Lineage/
Sleep 3s
