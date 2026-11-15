# 0055 Lines with text are numbered in the viewer and the editor; j/k go by line; narrow tables stay boxed

- **Date:** 2026-11-15 (after Phase 5)
- **Status:** Accepted.

## Context

The user reported five problems with the viewer's gutter
([0053](0053-viewer-gutter-and-line-copy.md)) and tables:

1. A window narrower than a table dropped the table's box: its rows
   wrapped unpadded with `│` between cells, which looked broken.
2. Line numbers counted blank lines, so they jumped (1, 3, 5) over the
   blank line between paragraphs.
3. j/k (and the mouse wheel, which the terminal sends as arrows) stopped
   on every row: blank rows, wrapped rows and table borders.
4. The editor had no line numbers ([0045](0045-simple-editor.md) left
   them out), so the viewer's numbers named nothing the editor showed.
5. Mouse clicks did not move the cursor.

Point 5 needs the mouse captured, which [0052](0052-viewer-cursor-and-folding.md)
declined (drag-selection, a parser for mouse reports, a mode to undo on
every exit). It was weighed again: about 250 lines, no security
benefit. The user skipped it, and kept the part that needs no mouse:
the cursor's `>` and number highlighted in orange.

## Decision

- **Narrow tables stay boxed.** If a table is wider than the screen, the
  columns that fit their share of the room keep their width, and the
  wider ones split what is left (the widest taking the odd columns).
  Each cell wraps inside its column, as a paragraph wraps; a table row is
  as tall as its tallest cell, and the other cells are padded, keeping
  their alignment. Borders and the `│ … │` margins are as at full width.
  Only if a column would keep fewer than 3 columns is the table drawn
  unboxed as before. The first row of a table row carries its number.
- **Only lines with text are numbered**, counted from 1: a blank line
  (only spaces and tabs before its line break) has no number and is not
  counted. One module, `model::lines`, numbers for both views, so the
  rule is the same on the raw note (editor) and the sanitized one
  (viewer): the sanitizer turns tabs into spaces and `\r\n` into `\n`,
  and spells out a lone `\r`, which is text in both. `y` copies the line
  by this number. Replaces 0053's "the editor's line numbers" (every line
  counted).
- **j/k and Up/Down go from one numbered line to the next**, past blank
  rows, wrapped rows and table borders; past the last numbered line, one
  row at a time, so a table's bottom border can still be reached. When
  the cursor lands on a line that wraps, the screen scrolls on until all
  its rows are shown, if they fit and the cursor stays on screen. The
  mouse wheel follows, through the terminal's arrows: nothing is
  captured. Space, PgDn/PgUp and g/G are unchanged.
- **The editor has the same gutter:** dim numbers right-aligned to the
  last one, a space, then the text; none on blank lines or wrapped rows.
  Left out when fewer than 20 columns would stay for the text, as in
  the viewer. Replaces 0045's "line numbers" in its list of things left
  out.
- **The cursor stands out in the viewer:** its `>` and its row's
  number are bold orange (256-color 208); the other numbers stay dim. In
  the editor the numbers stay dim (the user chose the viewer only).
- **No mouse.** 0052's reasons stand.

## Alternatives

- **Mouse clicks move the cursor** (point 5): declined again, see
  Context. Without capture, the wheel already moves the cursor by line.
- **The cursor's number in orange in the editor too:** about 5 lines;
  the user chose the viewer only.
- **Keep counting every line** (0053), and only make the editor match:
  the numbers would still jump. The user chose the count of lines with
  text, accepting that it no longer matches `wc -l` or a grep's line
  numbers.
- **Shrink every column by the same share:** wraps short columns that
  would fit. Declined for the share of what is left.
- **A `set visible_line_numbers on/off` command in the shell:** would be
  lost at every lock (no config file is written, and view settings do
  not belong in the vault), plus a command, completion and a setting
  reaching both views. Skipped for now; if wanted, one key that hides the
  gutter for the session is the small form.
- **A kept count in the editor**, so a frame does not count the note's
  lines: would have to follow every edit, paste and undo. Declined; see
  Consequences for the cost.

## Consequences

- About 120 lines in the renderer (column widths, wrapping runs to a
  column, trimming the space a cell wraps at), about 80 in
  `model::lines`, 50 in the pager and 30 in the editor's view, plus
  tests. The escape corpus's goldens show hostile cells spelled out
  inside a box.
- No new syscall, dependency, state or path for note text: numbers are
  drawn from integers, a wrapped cell is ranges of the same sanitized
  text, padding and borders are fixed text.
- The editor counts the note's numbered lines on every frame, for the
  gutter's width and the first number on screen: under 1 ms for 8 MiB of
  long lines, about 18 ms a pass for 8 MiB of the shortest (release
  build). Real notes are far smaller.
- A table row can take many screen rows in a narrow window.
- A paragraph taller than the screen is passed by one j; Space reads it.
- A mouse selection in the editor now carries the gutter too, as in the
  viewer.
