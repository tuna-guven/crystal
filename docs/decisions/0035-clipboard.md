# 0035 Two clipboards: internal, and the system clipboard through OSC 52

- **Date:** 2026-11-08 (Phase 3 redesign)
- **Status:** Accepted; "ESC becomes U+FFFD" in safeguard 3 is now
  "ESC is spelled out" ([0037](0037-visible-escapes.md))

## Context

A notes app without copy and paste is hardly usable. A terminal program
has two ways to reach the system clipboard: the OSC 52 escape sequence,
which asks the terminal to set it, or the Wayland or X11 clipboard
protocol. The protocols need a socket (which the sandbox blocks), either
a protocol library or a lot of code, and a process that stays alive to
serve pastes; the alternative, `wl-copy`, is external software, which
Crystal does not use. Anything on the system clipboard can be read by
every program you run, and clipboard managers keep histories.

## Decision

- **Internal clipboard (the default):** `y` in the pager, `Ctrl+C` in
  the editor copy into a zeroizing buffer in Crystal's locked memory;
  `Ctrl+V` in the editor pastes from it. It never leaves the process and
  is wiped on lock, exit and suspend.
- **System clipboard, only by a separate, explicit action:** `Y` in the
  pager, `Alt+C` in the editor, `copy NAME` in the shell. Crystal writes
  `ESC ] 52 ; c ; <base64> BEL` to the terminal.
- **Safeguards:**
  1. The status line shows that the system clipboard holds note text and
     counts down.
  2. It is cleared after 45 seconds, and on lock, exit and suspend, by an
     empty OSC 52. Crystal cannot read the clipboard back to check that
     it still holds its own text (most terminals refuse OSC 52 queries),
     so the clear is unconditional.
  3. Only Crystal can produce the sequence: the sanitizer and the grid
     turn ESC from note text into U+FFFD, so a note can neither write nor
     query the clipboard.
  4. The base64 is encoded straight into the wiped output chunk
     ([0031](0031-own-grid.md)), with no separate copy.
  5. At most 64 KiB per copy; terminals drop larger payloads silently.
  6. Under tmux or screen (`TMUX`, `STY` set), a warning: tmux keeps
     copied text in its own buffers.
  7. A setting in `~/.config/crystal` turns the system clipboard off
     entirely ([0032](0032-fixed-locations.md)), once settings exist.
- **Pasting from other programs** uses the terminal's own paste, which
  arrives as bracketed paste; Crystal never reads the system clipboard.
- The base64 encoder is about 20 lines of Crystal code; no dependency.

## Alternatives

- **No clipboard:** makes Crystal impractical.
- **Wayland or X11 protocol:** a socket the sandbox would have to allow,
  a protocol implementation or library, and Crystal serving pastes.
- **`wl-copy` or `xclip`:** external software, and `execve` after
  unlock, which the sandbox denies.
- **System clipboard only:** every copy, even between two notes, would
  expose text to other programs.

## Consequences

- Copying to the system clipboard works only in terminals that support
  OSC 52 (foot, kitty, Alacritty, WezTerm, Ghostty, xterm with
  `allowWindowOps`); others ignore it. VTE-based terminals (GNOME
  Terminal, Ptyxis) have historically ignored it. The setup guide gives a
  one-line check; Crystal says so the first time a copy may not have
  arrived.
- While the system clipboard holds note text, any program running as you
  can read it (A5, A6), and a clipboard manager may keep it after the
  clear. OSC 52 cannot mark text as sensitive the way password managers
  do. This is a deliberate, visible user action; the guide says so.
- The event loop needs a timer for the clear: the existing `ppoll` gets
  a timeout.
