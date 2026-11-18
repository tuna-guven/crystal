#!/bin/sh
# Manual check of the mouse in the editor (decision 0073,
# docs/development/testing.md): what CI cannot do is click in Ptyxis.
# CI already checks, through the binary, that clicks, drags, double and
# triple clicks edit the right text and that reports are on only while
# the editor shows (tests/vault.rs,
# mouse_clicks_and_drags_in_the_editor_only); this checks by hand what
# the terminal itself does.
#
# Usage: scripts/check-mouse.sh VAULT
# Runs the debug build with its in-memory demo notes (never saved) on an
# existing vault, locking after 60 seconds without input; you type its
# passphrase as usual. Nothing is written to the vault.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 VAULT" >&2
    exit 2
fi

cargo build --locked
cat <<'EOF'

After unlocking, type:  edit welcome
(A demo note: Ctrl+S refuses to save it, so nothing you do here lasts.)

In the editor:
  1. Click on a letter: the cursor moves there. Click on a line number,
     past a line's end, below the text: line start, line end, text end.
  2. Drag across two lines: they are selected; type x: replaced.
     Ctrl+Z undoes it.
  3. Double-click a word: it is selected. Triple-click: the whole line.
     Click once more: only the cursor again.
  4. Paste a long text (Ctrl+Shift+V, more lines than the window): the
     wheel moves the cursor 3 lines a step, and the text scrolls.
  5. Shift+drag: Ptyxis's own selection (it ignores Crystal's);
     Ctrl+Shift+C copies it, line numbers included. A plain click does
     not clear it; Shift+click does.
  6. Ctrl+E: the preview. The wheel moves the > marker; a click changes
     nothing. Ctrl+E again: back in the editor, clicks work again.
  7. Ctrl+Q twice (discarding): the shell. A click and a drag type
     nothing at the prompt; the wheel steps through the command history
     (Ptyxis sends it as Up and Down, as before the mouse work).

Auto-lock (Crystal locks after 60 seconds without input):
  8. Type  edit hostile , click into it, then wait a minute without
     touching anything. The lock screen says your edits are lost (demo
     notes are never saved).
  9. Click, drag and turn the wheel on the lock screen: nothing is
     typed, the prompt stays (the wheel's arrow keys are skipped,
     decision 0074). Your passphrase still unlocks; Esc quits.

After Crystal:
 10. Start this script again, open the editor, then in another terminal:
         pkill -9 -x crystal
     Back here, click: note what is printed (expected: reports such as
     ^[[<0;12;5M, since the terminal still sends them). Type  reset  and
     Enter: clicks print nothing afterwards.

Report each number that did not do what it says.

Press Enter to start.
EOF
read -r _
exec target/debug/crystal --demo-notes-for-test --lock-after-for-test 60 unlock "$1"
