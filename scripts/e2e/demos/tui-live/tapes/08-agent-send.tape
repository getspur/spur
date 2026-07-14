# Live: opt-in agent send — REAL model spend
# Only run when SPUR_DEMO_ALLOW_AGENT_SEND=1 (render.sh / uat.sh gate).

Output out/08-agent-send.mp4
Output out/08-agent-send.gif

Set Shell bash
Set FontSize 15
Set Width 1400
Set Height 800
Set Padding 10
Set Margin 0
Set TypingSpeed 35ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@25s /Lineage/
Sleep 1s
# Resume a mid-list session (more reliable send surface)
Hide
Type "s"
Show
Wait+Screen@15s /Sessions/
Hide
Down
Down
Down
Down
Enter
Show
Wait+Screen@25s /Session/
Sleep 1s
Hide
Type "demo capture ping - reply with only the word ok"
Show
Sleep 800ms
Hide
Enter
Show
Wait+Screen@60s /YOU/
Sleep 1s
Wait+Screen@60s /ok/
Sleep 3s
