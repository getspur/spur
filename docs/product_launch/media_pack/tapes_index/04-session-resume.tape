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
Wait+Screen@35s /Session ·|INSERT/
Sleep 2s
Hide
Ctrl+K
Show
Wait+Screen@15s /Go to/
Hide
Type "Sessions"
Enter
Show
Wait+Screen@20s /Sessions|TODAY|Start new session/
Sleep 1s
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
Wait+Screen@25s /Session ·|INSERT/
Sleep 3s
