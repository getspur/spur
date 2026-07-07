Output actual/raw/clean-quit.txt

Set Shell bash
Set Width 1067
Set Height 600
Set FontSize 20
Set Padding 0
Set Margin 0
Set TypingSpeed 0ms

Hide
Type "./bin/run-spur-tui.sh"
Enter
Show
Wait+Screen@15s /No agents configured/
Hide
Ctrl+C
Show
Wait+Screen@15s /Quit spur\?/
Hide
Type "y"
Enter
Show
Wait+Screen@15s /VHS_SPUR_EXITED status=0/
