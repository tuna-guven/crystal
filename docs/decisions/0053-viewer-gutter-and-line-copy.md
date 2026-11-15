# 0053 The viewer has a gutter with the note's line numbers; y copies a line

- **Date:** 2026-11-15 (after Phase 5)
- **Status:** Accepted. The numbering is replaced: only lines with text
  are numbered, in the viewer and the editor alike, and j/k move by
  numbered line ([0055](0055-line-numbers-and-narrow-tables.md)).

## Context

The viewer's cursor ([0052](0052-viewer-cursor-and-folding.md)) was a
dark-gray background across the line. The user asked for a `>` marker
in front of the line instead, and for line numbers. Numbers only help if
they name the same line as the editor: a line written as one long
paragraph should carry one number, with its wrapped rows unnumbered, and
a folded section should skip its numbers. The user also asked that `y`
copy the line the cursor is on, not the whole note.

Markdown joins consecutive lines into one paragraph (a soft break shows
as a space), so a line of the note could start in the middle of a row
and have nowhere to put its number.

## Decision

- **A gutter** left of the note: `>` on the cursor's row (bold), a
  space, the line number right-aligned and dim, a space (`> 12 `). The
  number column is as wide as the note's last line number, fixed when the
  note is rendered, so text does not shift while scrolling. If fewer than
  20 columns would stay for the note, the numbers are dropped and only
  `> ` stays. The dark-gray line highlight is gone; every cell keeps the
  colors the renderer gave it.
- **The editor's line numbers:** each row records the note line (from 1,
  counted as the editor counts: `\n` and `\r\n` end a line) on which its
  first note text begins, from the parser's source offsets. A row shows
  that number only if no earlier row did: a wrapped row has none. Rows
  without note text have none: the blank row between blocks, a blank line
  inside code or a `.txt` note, table borders and padding. Lines that make
  no row (a code fence, a table's `|-|` line) are skipped. Rows under a
  fold are not drawn, so their numbers are skipped too.
- **A line break in the note stays a line break** in the viewer, as in
  Obsidian's reading view (its default, "Strict line breaks" off), so
  every line of a paragraph starts a row and has its number.
- **y copies the cursor's line** as written (Markdown included, without
  its line break) into Crystal's own clipboard; on a wrapped row, the
  whole line it belongs to; on a folded heading, the heading's line only.
  On a row without note text it says "No line here to copy." **Y copies
  the whole note**, as y did before. In a preview, both copy from the
  editor's unsaved text. The clipboard is still Crystal's own
  ([0041](0041-internal-clipboard-only.md)): pasted with Ctrl+V in the
  editor, never reaching another program.
- The key line reads "… · y/Y copy line/note · Ctrl+E edit".

## Alternatives

- **Rendered-row numbers:** no renderer change, but the numbers change
  with the window's width and match nothing in the editor. Declined.
- **Joined paragraphs, numbering only each paragraph's first line:**
  numbers would jump (5 to 7) for no visible reason. Declined.
- **Numbers on blank lines in code and `.txt` notes:** about 20 more
  lines to track where an empty row starts; left out, so numbers skip
  blank lines everywhere alike.
- **A key to hide the gutter**, for a clean mouse selection: one more
  state and key. The user chose the gutter always on.
- **Copying the hidden text of a fold with a mouse selection:** the
  terminal copies only what is on screen, and Crystal is not told about
  a selection. It would need the mouse captured
  ([0052](0052-viewer-cursor-and-folding.md)) and a way to the system
  clipboard ([0041](0041-internal-clipboard-only.md)). Declined; unfold
  first, or `Y`.

## Consequences

- About 60 lines in the renderer (offsets from the parser, one number per
  row, a line counter that counts on from its last answer), about 50 in
  the viewer and the app, plus tests. One `u32` per row: an 8 MiB note of
  empty paragraphs costs about 16 MB more.
- No new syscall, dependency, state outside the viewer, or path for note
  text: the numbers are drawn from integers, and `y` copies a slice of
  the note into the clipboard that already held the whole note.
- A mouse selection over several rows carries the gutter (`  12 ` and
  `>`) with the text; block selection, where the terminal has it, can
  leave it out.
- Notes that relied on joined lines (hard-wrapped paragraphs) show their
  line breaks, as Obsidian does by default.
