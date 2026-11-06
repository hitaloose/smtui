# Recording for the README. Regenerate with:
#   cargo build --release && vhs docs/demo.tape
Output docs/demo.gif
Require vhs

Set Shell "bash"
Set FontSize 15
Set Width 1200
Set Height 720
Set Padding 12
Set Framerate 24
Set TypingSpeed 70ms
Set Theme { "name": "smtui", "background": "#14161a", "foreground": "#e6e8eb", "cursor": "#5eead4", "selection": "#262a32", "black": "#14161a", "red": "#f87171", "green": "#5eead4", "yellow": "#fbbf24", "blue": "#93c5fd", "magenta": "#f9a8d4", "cyan": "#5eead4", "white": "#e6e8eb", "brightBlack": "#5b636e", "brightRed": "#f87171", "brightGreen": "#5eead4", "brightYellow": "#fbbf24", "brightBlue": "#93c5fd", "brightMagenta": "#f9a8d4", "brightCyan": "#5eead4", "brightWhite": "#ffffff" }
Env COLORTERM "truecolor"

Hide
Type 'ZDOTDIR="$PWD/docs/demo" ./target/release/smtui --demo'
Enter
Sleep 2s
Show

# Terminal mode: real shells in tabs
Sleep 1s
Type "echo 'real shells, one per tab'"
Sleep 300ms
Enter
Sleep 1.2s
Ctrl+]
Type "t"
Sleep 1.2s
Type "uptime"
Sleep 300ms
Enter
Sleep 1.5s

# Switch to Files
Ctrl+]
Type "2"
Sleep 1.5s

# Open a folder and come back
Enter
Sleep 1.2s
Backspace
Sleep 1s

# Select two items and copy them from the context menu
Space
Sleep 400ms
Down 5
Sleep 300ms
Space
Sleep 800ms
Type "o"
Sleep 1.5s
Enter
Sleep 1.5s

# Paste into another folder
Up 4
Sleep 400ms
Enter
Sleep 1s
Type "v"
Sleep 2.2s

# Shortcuts menu
Type "?"
Sleep 2s
Escape
Sleep 500ms

# Delete the pasted items, with typed confirmation
Type "d"
Sleep 1.5s
Type "DELETE"
Sleep 800ms
Enter
Sleep 2.2s

Ctrl+q
Sleep 500ms
