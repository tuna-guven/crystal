# 0047 A changed paste is reported; pastes stay plain text

- **Date:** 2026-11-14
- **Status:** Accepted. Extends the paste rules of
  [0045](0045-simple-editor.md), which stay in force.

## Context

Text pasted with the terminal's paste key (Ctrl+Shift+V) reaches Crystal
as a bracketed paste ([0041](0041-internal-clipboard-only.md)): plain
text from whatever program or web page filled the clipboard, so it is
untrusted (A3 in the [threat model](../design/threat-model.md)).
`shell::keys` already cleans it before anything sees it: control
characters and BiDi overrides are dropped, broken UTF-8 becomes U+FFFD,
`\r\n` and `\r` become `\n`, and invisible characters stay in the text
but are shown as `<U+XXXX>` ([0046](0046-invisible-characters-spelled-out.md)).
All of this happens silently: the user cannot tell that the text in the
note differs from what was copied.

Two wishes came up: a warning and a check before pasting, and HTML
copied from a browser turned into Markdown on paste.

## Decision

- **A changed paste is reported, after the fact,** with one fixed line:
  "Untrusted input sanitized and pasted safely." It shows when the
  cleaning dropped a control or BiDi character or replaced broken bytes
  (`Key::Paste`'s `cleaned`), in the editor's status line until the next
  key, and in the shell as an output line. A clean paste shows nothing;
  line breaks turned into `\n` are expected and do not count. Invisible
  characters are not reported: they stay in the text and are already
  shown as `<U+XXXX>`. No counts and no pasted text: what matters is
  that the text differs from what was copied, and Ctrl+Z undoes the
  paste whole.
- **No question before pasting.** The paste is inserted at once, as one
  undo step, as before.
- **Pastes stay plain text.** Crystal never reads the HTML (or any other)
  form of the system clipboard and converts nothing.

## Alternatives

- **Ask before every paste:** the user has just chosen to paste, sees
  the result at once and can undo it; a question on every paste is
  answered without reading. It also needs a pending-paste state holding
  the text until the answer. No security gain.
- **Ask only when the paste was changed:** smaller, but the same answer
  as the report, one key later. Declined for the report alone.
- **Report counts** (how many characters removed or replaced): more
  words in a status line for numbers that change nothing the user does.
- **HTML to Markdown through the compositor's clipboard:** the terminal
  pastes only the clipboard's plain-text form, so the HTML form needs a
  Wayland connection of Crystal's own, open all session, since a paste
  can come at any time. That keeps `socket` and `connect` in the seccomp
  allowlist and the compositor's socket reachable through Landlock after
  the sandbox ([Sandboxing](../design/sandboxing.md)); a client that can
  read the clipboard can also set it, so a compromised Crystal would gain
  a channel to every other program, where today only the screen leads
  out. Data arrives over pipes passed as file descriptors (`recvmsg`,
  `SCM_RIGHTS`) from the copying program; GNOME lets only a focused
  window read the clipboard, so Crystal would need a window of its own
  and the protocols for it (hundreds of lines, or `wayland-client` and
  its dependencies); `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` would join
  the environment allowlist. On X11, any client can read every other
  client's keys and screen.
- **HTML through `wl-paste`:** spawns a process during the session, which
  CLAUDE.md forbids (only the age plugin, at unlock, before the sandbox);
  it needs `execve` and `clone`, both denied, and the child inherits
  Crystal's sandbox, so the socket would have to be open anyway. It is
  also external software Crystal would depend on.
- **Convert HTML source pasted as plain text:** a parser for untrusted
  HTML: `html5ever` and a dozen crates, or about 400 lines of Crystal's
  own with many edge cases (nesting, tables, entities, broken markup),
  more input for the Phase 6 fuzzing, and no security gain.

## Consequences

- About 20 lines in `shell::keys`, the editor and the shell, plus tests
  (steps P1 to P3 in the [roadmap](../development/roadmap.md)).
- To keep formatting from a web page, convert it before pasting (a
  browser's "copy as Markdown" or `pandoc`), outside Crystal.
- A clipboard whose text holds the paste end marker (`ESC [ 201 ~`) would
  end the bracketed paste early in a terminal that passes ESC through:
  what follows would reach Crystal as typed keys, bypassing both the
  cleaning and the report. Only the terminal can prevent this. Checked
  2026-11-14 (step P5) with Ptyxis 50.1 on VTE 0.84.1: it pastes each ESC
  as U+241B `␛` (the symbol for escape), so the end marker cannot be
  forged, and only the real one ends the paste. That `␛` is a printable
  symbol: Crystal keeps it, and such a paste is not reported as cleaned.
  The [threat model](../design/threat-model.md) notes this under the
  terminal's trust boundary.
