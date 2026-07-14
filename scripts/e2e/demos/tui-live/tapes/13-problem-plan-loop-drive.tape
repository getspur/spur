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

# STORY: HOOK — submit_plan should not become an invisible brain↔worker black box.
Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@30s /Lineage/
Sleep 3s

# STORY: ORIENTATION — learn the controls, then find campaigns created by the brain.
Hide
Type "?"
Show
Wait+Screen@12s /Dashboard/
Sleep 3s
Hide
Escape
Show
Sleep 1.5s

# STORY: ACTION — inspect plan state, then trace BRAIN → EXEC through lineage.
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
Wait+Screen@10s /Progress|No plans found/
Sleep 3.5s
Wait+Screen@10s /Work [Ii]tem|No plans found|No plans match/
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

# STORY: PROOF — BRAIN owns the loop; EXEC/Running exposes delegated work.
Wait+Screen@10s /BRAIN|brain/
Hide
Type "j"
Show
Sleep 2s
Hide
Type "j"
Show
Wait+Screen@10s /EXEC|Running|Succeeded|Cancelled/
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
Wait+Screen@10s /attempts|artifacts/
Sleep 2s
Hide
Type "l"
Show
Wait+Screen@10s /task|review/
Sleep 2s

# --- second node (brain ↔ worker hop) ---
Hide
Escape
Sleep 800ms
Type "k"
Enter
Show
Sleep 3s

# STORY: RESOLUTION — Activity closes the trace: campaign, roles, output, review, timeline.
Hide
Escape
Sleep 600ms
Tab
Show
Sleep 2.5s
Wait+Screen@10s /Activity/
Sleep 3s
