# Live: resume real session → message history / transcript

Output out/04-session-resume.mp4
Output out/04-session-resume.gif

Set Shell bash
Set FontSize 18
Set Width 2560
Set Height 1600
Set Padding 10
Set Margin 0
Set TypingSpeed 0ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "./bin/run-spur-tui-live.sh"
Enter
Show
Wait+Screen@25s /Lineage/
Sleep 800ms
Hide
Type "s"
Show
Wait+Screen@15s /Sessions/
Sleep 600ms
# Prefer mid-list rows (top sessions often attach-held by other TUIs)
Hide
Down
Down
Down
Down
Show
Sleep 400ms
Hide
Enter
Show
Wait+Screen@25s /Session/
Sleep 3s
