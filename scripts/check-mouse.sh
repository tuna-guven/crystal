#!/bin/sh
# Manual check of the mouse in the editor and the viewer (decisions
# 0073, 0045 and 0076, docs/development/testing.md): what CI cannot do is
# click in Ptyxis. CI already checks, through the binary, that clicks,
# drags, double and triple clicks edit the right text, that a click in the
# viewer or the live editor follows a link or folds a heading, and that
# reports are off in the shell and at prompts (tests/vault.rs,
# mouse_clicks_and_drags_in_the_editor_only,
# viewer_clicks_follow_links_and_fold, live_editor_follows_links); this
# checks by hand what the terminal itself does.
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
 3a. Link suggestions (decision 0045): at a line's end type  [[ : a gray
     list of the demo notes opens under the cursor, the first one
     reversed. Type a letter: it narrows. Turn the wheel over it: the
     highlight moves, the text does not scroll. Click a name: it becomes
     [[name]], the cursor after it. Type  [[  again and click on the
     text beside the list: the list closes, the cursor moves.
 3b. Headings and blocks (decision 0075): type  [[guide#  : the list
     shows  # Guide  and  # Keys , the key line starts with  Headings .
     Click  # Keys : [[guide#Keys]]. Type  [[guide#^ : the list shows
     Press q. ^keys  (the id dimmed), the key line starts with  Blocks .
     Turn the wheel over it, then click it: [[guide#^keys]]. Type
     [[welcome#^ and click a paragraph: a notice says demo notes are
     never saved, and nothing is written.
  4. Paste a long text (Ctrl+Shift+V, more lines than the window): the
     wheel moves the cursor 3 lines a step, and the text scrolls.
  5. Shift+drag: Ptyxis's own selection (it ignores Crystal's);
     Ctrl+Shift+C copies it, line numbers included. A plain click does
     not clear it; Shift+click does.
  6. Ctrl+E: the preview (decision 0076). No marker left of the
     headings. Move the mouse over  Welcome : a bold orange  v  shows
     left of it; away: it goes. Click the heading: the section folds
     ( … ) and the marker becomes  > ; again: it unfolds,  v .
     Shift+drag over a line: Ptyxis selects it. The wheel scrolls.
     Click  guide#Keys : the guide opens at  Keys ; Backspace: back.
     Ctrl+E again: back in the editor, clicks work again.
 6a. Ctrl+Q twice, then  view links : click  good : a notice says to
     Ctrl+click it, nothing opens. Ctrl+click it: Ptyxis opens it
     (decision 0054; checked 2026-11-19). Shift+drag
     over a line: Ptyxis selects it. q: the shell.
 6b. Live mode (decision 0045): type  edit --live welcome . Move the
     mouse over  Welcome : the bold orange  v  shows in the gutter;
     click it: the section folds ( … ),  > . Click it again: open.
     Fold it once more and press Down: the section opens, the cursor
     inside. Click  guide#Keys : the guide opens live at  Keys .
     Ctrl+Q, then  edit --live links : Ctrl+click  good : Ptyxis
     opens it; a plain click only says to Ctrl+click. Ctrl+Q.
  7. In the shell, where 6b ended. A click and a drag type
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
