# Marketing capture: external_knowledge_context → external_code_read
# Fixture mode by default (reproducible). For live: SPUR_DEMO_MODE=live + API key.

Output out/02-external-tools.mp4
Output out/02-external-tools.gif

Set Shell bash
Set FontSize 15
Set Width 1280
Set Height 720
Set Padding 12
Set Margin 0
Set TypingSpeed 10ms
Set Theme "Catppuccin Mocha"
Set Framerate 24
Set PlaybackSpeed 1.0

Hide
Type "export SPUR_DEMO_PAUSE=0.4"
Enter
Type "export SPUR_DEMO_MODE=fixture"
Enter
Type "./bin/demo-external-tools.sh"
Enter
Show
Sleep 18s
