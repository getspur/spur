# Marketing capture: spur context CLI setup surface
# Render: vhs tapes/01-context-setup.tape  (from demo root)

Output out/01-context-setup.mp4
Output out/01-context-setup.gif

Set Shell bash
Set FontSize 16
Set Width 1280
Set Height 720
Set Padding 12
Set Margin 0
Set TypingSpeed 12ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "export SPUR_DEMO_PAUSE=0.45"
Enter
Type "./bin/demo-context-setup.sh"
Enter
Show
Sleep 14s
