# 0041 Crystal's own clipboard only; copy out with the terminal's selection

- **Date:** 2026-11-09 (Phase 3)
- **Status:** Accepted. Replaces the clipboard records 0035 (two
  clipboards, OSC 52) and 0039 (filtered system copies), now removed.

## Context

A notes app needs copy and paste. A terminal program has two ways to
reach the system clipboard itself:

- **OSC 52** (`ESC ] 52 ; c ; <base64> BEL`): the terminal sets the
  clipboard. VTE-based terminals (GNOME Terminal, and Ptyxis, the default
  on secureblue and the terminal Crystal is used in) ignore it, so a copy
  would silently do nothing. Where it works, note text sits on the
  clipboard where every program can read it, and Crystal can only clear
  it blindly after a timeout.
- **The Wayland or X11 clipboard protocol:** a socket the sandbox blocks,
  a protocol implementation or library, and Crystal serving pastes; or
  `wl-copy`, external software, which Crystal does not use.

Crystal does not capture the mouse, so the terminal's own selection works
on everything Crystal shows, on the alternate screen too.

## Decision

- **Crystal's own clipboard:** `y` in the pager copies the note into
  zeroizing, locked memory that never leaves the process. The editor
  (Phase 4) pastes from it, and every selection there goes into it
  automatically. It is wiped on quit (and on lock, Phase 5).
- **No system clipboard from Crystal:** no OSC 52, no protocol, no
  external program. To copy to another program, select the text with the
  mouse in the terminal and use its copy key (Ctrl+Shift+C in Ptyxis).
  That copies what is shown: wrapped lines, and hidden characters as
  their spelled-out `<U+XXXX>` ([0037](0037-visible-escapes.md)), so no
  escape sequence or BiDi trick reaches the other program.
- Pasting into Crystal from other programs is the terminal's paste, which
  arrives as bracketed paste and is always text, never keys
  ([0033](0033-shell.md)).
- A note can never set or read the clipboard: the sanitizer and the grid
  spell out ESC, so a note's own OSC 52 is inert text.

## Alternatives

- **OSC 52, as first decided:** does nothing in VTE terminals; where it
  works, it hands note text to every program and needs a timed clear that
  cannot check what it clears.
- **Wayland or X11 clipboard, `wl-copy`:** a socket through the sandbox,
  a library or a lot of code, or external software.

## Consequences

- Crystal writes nothing to the terminal but frames: no clipboard
  sequence, no countdown, no timer for it.
- What the user copies with the terminal's selection is the user's to
  manage: Crystal cannot clear it, and a clipboard manager may keep it.
  The setup guide says so.
- A copy through the terminal's selection follows the screen's wrapping;
  the editor (Phase 4) is where whole notes are copied and pasted.
