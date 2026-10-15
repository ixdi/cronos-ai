# Drives the real Cronos-AI TUI through a task's whole workflow. Run it through record.sh.
Output tui-demo.mp4
Output tui-demo.gif

Set Shell bash
Set FontSize 16
Set Width 1400
Set Height 800
Set Padding 16
Set TypingSpeed 60ms
Set Theme "Catppuccin Mocha"

Hide
Type `export PS1='$ '; cd "$CRONOS_AI_DEMO_REPO"; clear`
Enter
Show

Sleep 1s
Type "cronos-ai"
Sleep 500ms
Enter
Sleep 2.5s

# Open the Storefront project.
Down
Sleep 600ms
Down
Sleep 900ms
Enter
Sleep 3s

# Add a requirement and let the planner propose the tasks that cover it.
Type "g"
Sleep 2.5s
Type "n"
Sleep 800ms
Type "Shoppers receive an order confirmation email with a tracking link."
Sleep 800ms
Enter
Sleep 1.5s
Type "p"
Wait+Screen@30s /y create planned/
Sleep 4s
Type "y"
Sleep 2s
Backspace
Sleep 2.5s

# Start the next ready task and follow it.
Type "s"
Sleep 2s
Down
Sleep 400ms
Down
Sleep 400ms
Down
Sleep 900ms
Enter
Sleep 2s
Wait+Screen@60s /y approve/
Sleep 2.5s

# Read the specification, then approve it.
Type "v"
Sleep 4s
PageDown
Sleep 2.5s
Backspace
Sleep 1s
Type "y"

# Two writers, then two reviewers, run in parallel; then verification and the check command.
Wait+Screen@120s /Step: final-approval/
Sleep 3s

# Review the final code file by file, then approve it.
Type "v"
Sleep 2s
Type "]"
Sleep 2s
Type "]"
Sleep 2s
Type "]"
Sleep 2.5s
Backspace
Sleep 1s
Type "y"

# Documentation, then the local merge.
Wait+Screen@60s /merged locally/
Sleep 3s
Backspace
Sleep 2s
Type "b"
Sleep 5s
Type "q"
Sleep 1s
