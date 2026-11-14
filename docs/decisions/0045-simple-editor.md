# 0045 A simple modeless editor with common Ctrl keys

- **Date:** 2026-11-10 (Phase 4, step E0)
- **Status:** Accepted. Replaces the nano-like key scheme first planned in
  the roadmap's Phase 4, and the part of
  [0041](0041-internal-clipboard-only.md) that copied every editor
  selection into the clipboard automatically. Drops the per-note merge of
  sync conflicts that [0001](0001-single-vault-blob.md) and
  [0023](0023-unlock.md) planned: a conflict is refused, never merged.

## Context

Phase 4 adds `edit NAME` and `new NAME` ([0033](0033-shell.md)): an
editor inside Crystal, on its own grid ([0031](0031-own-grid.md)), because
an external `$EDITOR` writes temp, swap and undo files in plaintext.

Every key the editor knows is code that handles note text, which may be
hostile ([A3](../design/threat-model.md)). A Helix-style modal editor was
designed and partly written first (2026-11-10): three modes, word classes,
selection-first motions and a command line. It was removed before it was
committed: far more code and state than a notes editor needs, against
Crystal's aim of being minimal and auditable. The editor is modeless and
uses the keys most editors share.

Crystal reads the terminal in raw mode, so the kernel turns no key into a
signal. Ctrl+Z reached Crystal as a byte, and `term.rs` suspended Crystal
when a read held it, before any key reached the shell. Undo needs that
key.

Bracketed pastes were cut at 4 KiB, silently: enough for the shell's
prompt, not for pasting a note.

Saving needs the vault's recipient, which `unlock` derived from the
identity and dropped with it. While Crystal is open, Syncthing or a
second Crystal may replace `vault.age`; a save that only wrote the
in-memory notes would silently drop those changes.

A per-note merge of two vault versions (Syncthing's conflict copies, or
a `vault.age` replaced during a session) was planned for Phase 4: newest
note wins, the other kept as "… (conflict)". It would be about 150 to
250 lines, a second decrypt path, deleting conflict copies inside the
vault directory, and the identity kept in memory for the whole session,
only to spare a user who edits one vault on two devices at once from
choosing a version by hand. It was dropped on 2026-11-11.

## Decision

- **One mode, these keys:**

  | Key | Does |
  | --- | --- |
  | Arrows, Home, End, PgUp, PgDn | Move |
  | Shift + arrows, Shift+Home, Shift+End | Select |
  | Ctrl+A | Select all |
  | Ctrl+C / Ctrl+X | Copy / cut the selection into Crystal's clipboard |
  | Ctrl+V | Paste Crystal's clipboard |
  | Text, Enter, Tab | Insert; a selection is replaced |
  | Backspace / Delete | Delete the selection, or one character |
  | Ctrl+Z / Ctrl+Y | Undo / redo |
  | Ctrl+S | Save |
  | Ctrl+E | Switch to the viewer, and from the viewer back to the editor |
  | Ctrl+Q | Close; with unsaved changes it warns first, and a second Ctrl+Q discards them |

  Any other key does nothing. The status line shows the note's name, `[+]`
  for unsaved changes, and the keys.
- **Copying is explicit:** only Ctrl+C and Ctrl+X put text into Crystal's
  clipboard ([0041](0041-internal-clipboard-only.md)); selecting copies
  nothing. The system clipboard stays the terminal's: Ctrl+Shift+C copies
  what the mouse selected, and Ctrl+Shift+V pastes into Crystal as a
  bracketed paste. Crystal never sees or sets the system clipboard. The
  pager keeps `y`.
- **Ctrl+E, editor and viewer:** in the editor it shows the note rendered,
  in the pager ([0033](0033-shell.md)), from the editor's current text,
  unsaved changes included; nothing is written. Links resolve against the
  saved notes. Ctrl+E in the viewer returns to the editor with its edits
  and cursor, or, from `view NAME`, opens the editor on that note.
- **Ctrl+Z is a key, not a suspend:** keyboard suspend is dropped, along
  with `term.rs`'s check for the byte in each read (so a paste holding
  0x1a no longer suspends). A `SIGTSTP` sent from outside still suspends
  through the normal path. The key that locks Crystal is chosen in
  Phase 5, with auto-lock. In the shell and the pager, Ctrl+Z does
  nothing.
- **Undo:** a run of typing is one step; any other key ends it. At most
  200 steps and 64 MiB of text (eight full notes), kept as a log of edits
  (position, removed and inserted text), never as copies of the note; the
  oldest steps go first. All memory is locked, so an unbounded log could
  pass the memlock limit and abort Crystal.
- **Pastes up to 8 MiB,** the body limit, with line breaks kept (`\r\n`
  and `\r` become `\n`) and tabs kept; the shell's prompt turns them into
  spaces. In the editor, a paste that is longer, or that does not fit in
  the note, is not pasted at all, and Crystal says so: a cut paste would
  look whole. The shell's 4 KiB line takes what fits and says the paste
  was cut. The paste buffer is zeroizing and sized once.
- **The same editor for Markdown and plain text** ([0038](0038-plain-text-notes.md)):
  it edits the text as written and renders nothing.
- **Text in zeroizing memory, grown by hand:** the note is edited in a
  zeroizing buffer. When it fills, the text is copied into a new buffer
  of twice the size and the old one is wiped; growth is never left to
  `realloc`, which would leave unwiped copies until the zeroizing
  allocator (Phase 5). Freed bytes are overwritten. Capped at the 8 MiB
  body limit; the cursor stays on grapheme boundaries.
- **Shown safely:** soft wrap at grapheme boundaries; a tab advances to
  the next multiple of 4 columns; control and BiDi characters are drawn
  spelled out as dimmed `<U+XXXX>` ([0037](0037-visible-escapes.md)), one
  cursor step each. So is a whole grapheme that carries an invisible
  character behind a visible one (tag characters, variation selectors,
  zero-width characters, word joiner, byte order mark, combining grapheme
  joiner, Hangul fillers), so text that looks the same is the same
  ([0046](0046-invisible-characters-spelled-out.md), which the pager
  follows too). The joiner between two emoji and U+FE0F stay; the price is that the few
  flags built from tag characters (England, Scotland, Wales) are spelled
  out. All of it goes through the grid.
- **Saving:** Ctrl+S saves the whole vault at once (one blob,
  [0001](0001-single-vault-blob.md)): generation + 1, the new `modified`
  time, `VERSION` written first when the directory still says format 1
  ([0034](0034-vault-format-2.md)), seal, `vault::atomic_write`, then the
  counter ([0019](0019-generation-counters.md)), then the indexes are
  rebuilt. A failed save keeps the edits in memory and says so.
- **Only the recipient stays in memory:** unlock derives the vault's
  recipient from the identity, then wipes the identity as before. The
  recipient seals each save; it stays in locked memory, private, never
  written, shown or logged ([0044](0044-vault-recipient-private.md)), and
  is wiped on quit (and on lock, Phase 5).
- **No merge; a changed `vault.age` is not overwritten:** after sealing
  and before writing anything, Crystal compares `vault.age`'s stat with
  the one recorded when it read the file. If it changed (Syncthing brought
  a newer version, or a second Crystal saved), the save is refused and
  says so; nothing is written, and the edits stay in the editor only until
  it closes. Close and reopen the vault to see the other version. The
  check is not atomic with the rename: a file that lands between the two
  is overwritten. Closing that gap needs a lock both writers honour, which
  Syncthing does not.
- **Sync conflict copies stay refused at unlock,** as since
  [0023](0023-unlock.md): Crystal does not open a vault while a
  `vault.sync-conflict-*.age` is beside it. To resolve it, keep one
  version and move the other out of the vault directory. To look at the
  conflict copy first, move `vault.age` out and rename the copy to
  `vault.age`; if its generation is older than this device has seen,
  unlock asks for `restore` ([0036](0036-backups-external.md)).

## Alternatives

- **A Helix-style modal editor:** written in part, then removed; three
  modes, word motions and a command line are much more code on hostile
  text, for power a notes editor does not need.
- **nano's keys (Alt+U for undo and similar):** Alt arrives as Esc and a
  key, which the decoder drops; less familiar than Ctrl+Z, Ctrl+C and
  Ctrl+V.
- **Keep Ctrl+Z as suspend, undo on Ctrl+U:** keeps a special case in
  `term.rs` for a feature (backgrounding Crystal) that keeps every note
  decrypted in memory anyway.
- **Every selection copied automatically (0041 as planned):** a selection
  made only to delete or replace text would overwrite what was copied.
- **Ctrl+E saves first, or refuses with unsaved changes:** a full vault
  write for every look at the rendered note, or an extra step for it.
- **A per-note merge (newest note wins, the other kept as "… (conflict)"):**
  no edit lost when one vault is edited on two devices at once, but about
  150 to 250 lines, a second decrypt path, deletions in the vault
  directory and the identity in memory all session, for no security gain.
- **Overwrite a changed `vault.age`:** simplest, but loses the other
  device's edits silently.

## Consequences

- About 850 lines for buffer, keys and view, without tests, a third of
  them comments (first estimated at 500; corrected in step E4, approved
  2026-11-11). Most of the size is safety: spelled-out hidden characters,
  the cursor kept on grapheme boundaries (also across `\r\n`), and
  scrolling that never wraps the whole note per key; soft wrap is about
  120 lines. Saving, `mv` and `rm` are session code apart from
  it.
- Left out, each addable later in its own step: word motions, search in
  the editor, indenting, line numbers, syntax colors, the mouse.
- Crystal can no longer be suspended from the keyboard; quit, or send
  `SIGTSTP` from another terminal.
- The editor's buffer, undo log and pastes are more places note text sits
  in memory: bounded, zeroizing, wiped when the editor closes.
- Only the recipient outlives unlock; Phase 5's lock must wipe it.
- Editing one vault on two devices at the same time ends in a refused
  save or a conflict copy, which the user resolves by choosing a
  version. Nothing is lost silently, and nothing is merged.
