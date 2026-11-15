#!/bin/sh
# Manual check of clickable links (decision 0054,
# docs/development/testing.md): what CI cannot do is click. CI already
# checks, through the binary, that only the good target reaches the
# terminal inside OSC 8 (tests/vault.rs,
# only_clickable_targets_reach_the_terminal); this checks by eye that
# Ptyxis opens it, and nothing else.
#
# Usage: scripts/check-links.sh VAULT
# Runs the debug build with its in-memory demo notes (never saved) on an
# existing vault; you type its passphrase as usual.
set -eu

if [ $# -ne 1 ]; then
    echo "usage: $0 VAULT" >&2
    exit 2
fi

cargo build --locked
cat <<'EOF'

After unlocking, type:  view links

The note shows three links on line 3:

  good   clickable: underlined, and Ctrl+click opens
         https://example.org/ok in the browser; no address after it
  bad    NOT clickable: its address is shown, with <U+001B> and <U+0007>
         spelled out; Ctrl+click does nothing
  local  NOT clickable: "(file:///etc/passwd)" is shown; Ctrl+click
         does nothing, and no file manager or editor opens

Also check:
  - Hovering over "good" shows the hand cursor or underline Ptyxis uses
    for links; hovering over "bad" or "local" does not.
  - Press q, then quit: the shell prompt after Crystal is not underlined
    or clickable (every link was closed).

Press Enter to start.
EOF
read -r _
exec target/debug/crystal --demo-notes-for-test "$1"
