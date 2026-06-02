# Animus install + first-workflow demo
# Recorded with vhs (https://github.com/charmbracelet/vhs)
#
# Usage:
#   brew install vhs
#   cd assets/
#   vhs install-demo.tape          # produces install-demo.gif
#
# Storyboard (~45s):
#   1. animus doctor              (env is ready)
#   2. animus init                (scaffold a project)
#   3. animus subject create      (queue a task)
#   4. animus daemon start        (autonomous mode on)
#   5. animus daemon stream       (show the workforce shipping)
#
# Run this against a fresh tmp project so the cast is reproducible.

Output install-demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 1200
Set Height 700
Set Theme "Catppuccin Mocha"
Set TypingSpeed 50ms
Set Padding 20

# Reset to a clean demo project
Hide
Type "cd /tmp && rm -rf animus-demo && mkdir animus-demo && cd animus-demo && git init -q"
Enter
Sleep 500ms
Show

Type "# Animus: kernel + flavors. One Rust daemon, your workforce."
Enter
Sleep 1s

Type "animus doctor"
Enter
Sleep 2s

Type "animus init --template task-queue --non-interactive"
Enter
Sleep 2s

Type "animus subject create --kind task --title 'Add rate limiting' --priority p1"
Enter
Sleep 2s

Type "animus daemon start --autonomous"
Enter
Sleep 2s

Type "# The workforce is shipping. Go to the gym."
Enter
Sleep 1500ms

Type "animus daemon stream --limit 8"
Enter
Sleep 5s

Type "# That's it. Six lines."
Enter
Sleep 2s
